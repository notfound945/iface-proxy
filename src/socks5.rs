use anyhow::Result;
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::{sleep, timeout, Duration};
use std::sync::Arc;

use crate::util::{connect_outbound, log_throttled, log_info, log_error, is_transient_anyhow_error};

async fn read_exact_into(stream: &mut TcpStream, buf: &mut [u8], read_timeout_ms: u64) -> Result<()> {
    timeout(Duration::from_millis(read_timeout_ms), stream.read_exact(buf))
        .await
        .map_err(|_| anyhow::anyhow!("read timeout"))??;
    Ok(())
}

async fn handle_socks5(
    mut inbound: TcpStream,
    iface: &str,
    user: Option<&str>,
    pass: Option<&str>,
    read_timeout_ms: u64,
    session_timeout_ms: u64,
) -> Result<()> {
    // Greeting
    let mut g = [0u8; 2];
    read_exact_into(&mut inbound, &mut g, read_timeout_ms).await?;
    if g[0] != 5 { anyhow::bail!("Invalid SOCKS5 version in greeting"); }
    let nmethods = g[1] as usize;
    let mut methods = vec![0u8; nmethods];
    if nmethods > 0 { read_exact_into(&mut inbound, &mut methods, read_timeout_ms).await?; }
    let need_auth = user.is_some() || pass.is_some();
    if need_auth {
        let use_userpass = methods.iter().any(|m| *m == 0x02);
        if use_userpass { inbound.write_all(&[0x05, 0x02]).await?; } else { inbound.write_all(&[0x05, 0xFF]).await?; anyhow::bail!("client doesn't support username/password auth"); }
        // subnegotiation
        let mut sb_ver = [0u8;1]; read_exact_into(&mut inbound, &mut sb_ver, read_timeout_ms).await?; if sb_ver[0] != 0x01 { anyhow::bail!("invalid auth subnegotiation version"); }
        let mut ulen_b = [0u8;1]; read_exact_into(&mut inbound, &mut ulen_b, read_timeout_ms).await?; let ulen = ulen_b[0] as usize;
        let mut ubytes = vec![0u8; ulen]; if ulen>0 { read_exact_into(&mut inbound, &mut ubytes, read_timeout_ms).await?; }
        let mut plen_b = [0u8;1]; read_exact_into(&mut inbound, &mut plen_b, read_timeout_ms).await?; let plen = plen_b[0] as usize;
        let mut pbytes = vec![0u8; plen]; if plen>0 { read_exact_into(&mut inbound, &mut pbytes, read_timeout_ms).await?; }
        let ok = user.map(|u| u.as_bytes().to_vec()).as_deref()==Some(&ubytes[..]) && pass.map(|p| p.as_bytes().to_vec()).as_deref()==Some(&pbytes[..]);
        if ok { inbound.write_all(&[0x01, 0x00]).await?; } else { inbound.write_all(&[0x01, 0x01]).await?; anyhow::bail!("invalid username/password"); }
    } else {
        inbound.write_all(&[0x05, 0x00]).await?;
    }

    // Request
    let mut h = [0u8; 4]; read_exact_into(&mut inbound, &mut h, read_timeout_ms).await?;
    if h[0] != 5 { anyhow::bail!("Invalid SOCKS5 version in request"); }
    let cmd = h[1]; let atyp = h[3];
    let (target_host, target_port) = match atyp {
        0x01 => { let mut v4=[0u8;4]; read_exact_into(&mut inbound,&mut v4, read_timeout_ms).await?; let ip=std::net::Ipv4Addr::new(v4[0],v4[1],v4[2],v4[3]); let mut p=[0u8;2]; read_exact_into(&mut inbound,&mut p, read_timeout_ms).await?; (ip.to_string(), u16::from_be_bytes(p)) }
        0x03 => { let mut l=[0u8;1]; read_exact_into(&mut inbound,&mut l, read_timeout_ms).await?; let len=l[0] as usize; let mut hb=vec![0u8;len]; if len>0 { read_exact_into(&mut inbound,&mut hb, read_timeout_ms).await?; } let host=String::from_utf8_lossy(&hb).to_string(); let mut p=[0u8;2]; read_exact_into(&mut inbound,&mut p, read_timeout_ms).await?; (host, u16::from_be_bytes(p)) }
        0x04 => { let mut v6=[0u8;16]; read_exact_into(&mut inbound,&mut v6, read_timeout_ms).await?; let ip=std::net::Ipv6Addr::from(v6); let mut p=[0u8;2]; read_exact_into(&mut inbound,&mut p, read_timeout_ms).await?; (ip.to_string(), u16::from_be_bytes(p)) }
        _ => anyhow::bail!("Unsupported ATYP"),
    };

    match cmd {
        0x01 => {
            log_throttled(|| log_info(format!("SOCKS5 CONNECT -> {}:{} (iface: {})", target_host, target_port, iface)));
            let mut outbound = connect_outbound(&target_host, target_port, iface).await?;
            inbound.write_all(&[0x05, 0x00, 0x00, 0x01, 0,0,0,0, 0,0]).await?;
            let (c2s, s2c) = timeout(Duration::from_millis(session_timeout_ms), copy_bidirectional(&mut inbound, &mut outbound)).await??;
            log_throttled(|| log_info(format!("SOCKS5 finished {}:{} (c->s: {} bytes, s->c: {} bytes)", target_host, target_port, c2s, s2c)));
            Ok(())
        }
        0x03 => { anyhow::bail!("UDP ASSOC not supported") }
        _ => { anyhow::bail!("Unsupported CMD") }
    }
}

pub async fn run_socks5_proxy_auth(iface: &str, listen: &str, user: Option<&str>, pass: Option<&str>, sem: Arc<Semaphore>, read_timeout_ms: u64, session_timeout_ms: u64) -> Result<()> {
    let listener = TcpListener::bind(listen).await?;
    log_info(format!("SOCKS5 proxy listening on {}, bound to {}", listen, iface));
    let mut backoff_ms: u64 = 50;
    loop {
        let (inbound, peer_addr) = match listener.accept().await {
            Ok(v) => { backoff_ms = 50; v }
            Err(e) => {
                log_error(format!("accept error: {}", e));
                sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms.saturating_mul(2)).min(1000);
                continue;
            }
        };
        let listen_for_log = listen.to_string();
        log_throttled(|| log_info(format!("Incoming TCP connection from {} -> listening on {} (iface: {})", peer_addr, listen_for_log, iface)));
        let iface_for_task = iface.to_string();
        let u = user.map(|s| s.to_string());
        let p = pass.map(|s| s.to_string());
        let sem_clone = sem.clone();
        match sem_clone.try_acquire_owned() {
            Ok(permit) => {
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = handle_socks5(inbound, &iface_for_task, u.as_deref(), p.as_deref(), read_timeout_ms, session_timeout_ms).await {
                        if is_transient_anyhow_error(&e) {
                            log_info(format!("SOCKS5 handler transient: {}", e));
                        } else {
                            log_error(format!("SOCKS5 handler error: {}", e));
                        }
                    }
                });
            }
            Err(_) => {
                log_throttled(|| log_info("too many concurrent connections; dropping new SOCKS5 connection"));
            }
        }
    }
}


