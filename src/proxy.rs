use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};

#[cfg(target_os = "macos")]
use nix::libc::{if_nametoindex, IPPROTO_IP, IP_BOUND_IF, IPPROTO_IPV6, IPV6_BOUND_IF};

#[cfg(target_os = "macos")]
fn iface_index(iface: &str) -> Result<u32> {
    let cstr = CString::new(iface)?;
    let idx = unsafe { if_nametoindex(cstr.as_ptr()) };
    if idx == 0 {
        anyhow::bail!("Invalid iface: {}", iface);
    }
    Ok(idx)
}

#[cfg(target_os = "macos")]
fn bind_iface_v4(fd: i32, iface: &str) -> Result<()> {
    let idx = iface_index(iface)?;
    let ret = unsafe {
        nix::libc::setsockopt(
            fd,
            IPPROTO_IP,
            IP_BOUND_IF,
            &idx as *const _ as *const nix::libc::c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    if ret != 0 {
        anyhow::bail!("setsockopt(IP_BOUND_IF) failed");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bind_iface_v4(fd: i32, iface: &str) -> Result<()> {
    use nix::sys::socket::setsockopt;
    use nix::sys::socket::sockopt::BindToDevice;
    setsockopt(fd, BindToDevice, iface.as_bytes())?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn bind_iface_v6(fd: i32, iface: &str) -> Result<()> {
    let idx = iface_index(iface)?;
    let ret = unsafe {
        nix::libc::setsockopt(
            fd,
            IPPROTO_IPV6,
            IPV6_BOUND_IF,
            &idx as *const _ as *const nix::libc::c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    if ret != 0 {
        anyhow::bail!("setsockopt(IPV6_BOUND_IF) failed");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bind_iface_v6(fd: i32, iface: &str) -> Result<()> {
    use nix::sys::socket::setsockopt;
    use nix::sys::socket::sockopt::BindToDevice;
    setsockopt(fd, BindToDevice, iface.as_bytes())?;
    Ok(())
}

// 全局日志限频（简单的每秒计数器）
const LOGS_PER_SEC: u64 = 50;
static LOG_WINDOW_SEC: AtomicU64 = AtomicU64::new(0);
static LOG_COUNT: AtomicU64 = AtomicU64::new(0);
static LOG_SUPPRESSED: AtomicU64 = AtomicU64::new(0);

fn now_sec() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn log_throttled<F: FnOnce()>(f: F)
where
    F: FnOnce(),
{
    let now = now_sec();
    let window = LOG_WINDOW_SEC.load(Ordering::Relaxed);
    if now != window {
        if LOG_WINDOW_SEC
            .compare_exchange(window, now, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let suppressed = LOG_SUPPRESSED.swap(0, Ordering::SeqCst);
            if suppressed > 0 {
                println!("[log] suppressed {} messages in last 1s", suppressed);
            }
            LOG_COUNT.store(0, Ordering::SeqCst);
        }
    }
    let c = LOG_COUNT.fetch_add(1, Ordering::SeqCst);
    if c < LOGS_PER_SEC {
        f();
    } else {
        LOG_SUPPRESSED.fetch_add(1, Ordering::SeqCst);
    }
}

async fn read_http_headers(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            anyhow::bail!("client closed before headers");
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(buf);
        }
        if buf.len() > 64 * 1024 {
            anyhow::bail!("headers too large");
        }
    }
}

fn split_headers_body(buf: &[u8]) -> Option<(usize, &[u8])> {
    for i in 0..buf.len().saturating_sub(3) {
        if &buf[i..i + 4] == b"\r\n\r\n" {
            return Some((i + 4, &buf[i + 4..]));
        }
    }
    None
}

fn parse_request_line<'a>(headers: &'a str) -> anyhow::Result<(&'a str, &'a str, &'a str)> {
    let mut lines = headers.split("\r\n");
    let line = lines.next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("bad request line"))?;
    let uri = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("bad request line"))?;
    let version = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("bad request line"))?;
    Ok((method, uri, version))
}

fn parse_host_from_headers(headers: &str) -> Option<String> {
    for line in headers.split("\r\n").skip(1) {
        if let Some(rest) = line.strip_prefix("Host:") {
            return Some(rest.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("host:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

async fn connect_outbound(host: &str, port: u16, iface: &str) -> Result<TcpStream> {
    let addrs = tokio::net::lookup_host((host, port)).await?;
    let mut last_err: Option<anyhow::Error> = None;
    for sa in addrs {
        match sa {
            std::net::SocketAddr::V4(v4) => {
                let socket = TcpSocket::new_v4()?;
                let fd = socket.as_raw_fd();
                if let Err(e) = bind_iface_v4(fd, iface) {
                    last_err = Some(e);
                    continue;
                }
                match socket.connect(std::net::SocketAddr::V4(v4)).await {
                    Ok(s) => return Ok(s),
                    Err(e) => {
                        last_err = Some(anyhow::Error::new(e));
                        continue;
                    }
                }
            }
            std::net::SocketAddr::V6(v6) => {
                let socket = TcpSocket::new_v6()?;
                let fd = socket.as_raw_fd();
                if let Err(e) = bind_iface_v6(fd, iface) {
                    last_err = Some(e);
                    continue;
                }
                match socket.connect(std::net::SocketAddr::V6(v6)).await {
                    Ok(s) => return Ok(s),
                    Err(e) => {
                        last_err = Some(anyhow::Error::new(e));
                        continue;
                    }
                }
            }
        }
    }
    if let Some(e) = last_err { Err(e) } else { anyhow::bail!("no address") }
}

async fn handle_http_proxy(mut inbound: TcpStream, iface: &str) -> Result<()> {
    // 读取并解析请求头
    let raw = read_http_headers(&mut inbound).await?;
    let (header_end, body_start) =
        split_headers_body(&raw).ok_or_else(|| anyhow::anyhow!("bad headers"))?;
    let headers_str = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let (method, uri, version) = parse_request_line(&headers_str)?;

    if method.eq_ignore_ascii_case("CONNECT") {
        // CONNECT host:port HTTP/1.1
        let mut hp = uri.split(':');
        let host = hp.next().unwrap_or("");
        let port: u16 = hp.next().unwrap_or("443").parse().unwrap_or(443);
        log_throttled(|| println!("HTTP CONNECT -> {}:{} (iface: {})", host, port, iface));
        let mut outbound = connect_outbound(host, port, iface).await?;
        inbound
            .write_all(
                b"HTTP/1.1 200 Connection Established\r\nProxy-Agent: iface-proxy\r\n\r\n",
            )
            .await?;
        let (c2s, s2c) = copy_bidirectional(&mut inbound, &mut outbound).await?;
        log_throttled(|| {
            println!(
                "HTTP CONNECT finished {}:{} (c->s: {} bytes, s->c: {} bytes)",
                host, port, c2s, s2c
            )
        });
        return Ok(());
    }

    // 普通 HTTP 代理：解析绝对 URI 或使用 Host 头
    // 支持方法：GET/POST/HEAD/PUT/DELETE/OPTIONS/PATCH 等
    let (mut host, mut port, path) = if let Some(rest) = uri.strip_prefix("http://") {
        if let Some(pos) = rest.find('/') {
            (rest[..pos].to_string(), 80u16, rest[pos..].to_string())
        } else {
            (rest.to_string(), 80u16, "/".to_string())
        }
    } else if uri.starts_with('/') {
        (
            parse_host_from_headers(&headers_str).unwrap_or_default(),
            80u16,
            uri.to_string(),
        )
    } else {
        // 不支持 https:// 直接方法式（应使用 CONNECT）或奇异 URI
        anyhow::bail!("unsupported URI for HTTP proxy");
    };
    // 拆 host:port（对克隆字符串 split，避免借用冲突）
    if let Some((h, p)) = host.clone().split_once(':') {
        host = h.to_string();
        port = p.parse().unwrap_or(80);
    }

    log_throttled(|| println!("HTTP {} {} -> {}:{} (iface: {})", method, path, host, port, iface));
    let mut outbound = connect_outbound(&host, port, iface).await?;

    // 重写请求行与头：METHOD path HTTP/x.x + Host 头（去除 Proxy-Connection 等）
    let mut lines = headers_str.split("\r\n");
    let _first = lines.next();
    let mut rebuilt = String::new();
    rebuilt.push_str(&format!("{} {} {}\r\n", method, path, version));
    let mut has_host = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("host:") {
            has_host = true;
        }
        if lower.starts_with("proxy-connection:") || lower.starts_with("proxy-authorization:") {
            continue;
        }
        rebuilt.push_str(line);
        rebuilt.push_str("\r\n");
    }
    if !has_host {
        if port == 80 {
            rebuilt.push_str(&format!("Host: {}\r\n", host));
        } else {
            rebuilt.push_str(&format!("Host: {}:{}\r\n", host, port));
        }
    }
    // 结束头
    rebuilt.push_str("\r\n");

    outbound.write_all(rebuilt.as_bytes()).await?;
    // 将已读缓冲中 header 之后的字节（可能是请求体开头）写给服务端
    if !body_start.is_empty() {
        outbound.write_all(body_start).await?;
    }
    // 然后开始双向转发
    let (c2s, s2c) = copy_bidirectional(&mut inbound, &mut outbound).await?;
    log_throttled(|| {
        println!(
            "HTTP finished {} {} (c->s: {} bytes, s->c: {} bytes)",
            method, host, c2s, s2c
        )
    });
    Ok(())
}

pub async fn run_http_proxy(iface: &str, listen: &str) -> Result<()> {
    let listener = TcpListener::bind(listen).await?;
    println!("HTTP proxy listening on {}, bound to {}", listen, iface);

    loop {
        let (inbound, peer_addr) = listener.accept().await?;
        let listen_for_log = listen.to_string();
        log_throttled(|| println!(
            "Incoming TCP connection from {} -> listening on {} (iface: {})",
            peer_addr, listen_for_log, iface
        ));
        let iface_for_task = iface.to_string();
        tokio::spawn(async move {
            if let Err(e) = handle_http_proxy(inbound, &iface_for_task).await {
                eprintln!("TCP handler error: {}", e);
            }
        });
    }
}

// -------------------- SOCKS5 Proxy --------------------

async fn read_exact_into(stream: &mut TcpStream, buf: &mut [u8]) -> Result<()> {
    stream.read_exact(buf).await?;
    Ok(())
}

async fn handle_socks5(mut inbound: TcpStream, iface: &str, user: Option<&str>, pass: Option<&str>) -> Result<()> {
    // Greeting: VER | NMETHODS | METHODS
    let mut g = [0u8; 2];
    read_exact_into(&mut inbound, &mut g).await?;
    if g[0] != 5 {
        anyhow::bail!("Invalid SOCKS5 version in greeting");
    }
    let nmethods = g[1] as usize;
    let mut methods = vec![0u8; nmethods];
    if nmethods > 0 { inbound.read_exact(&mut methods).await?; }
    let need_auth = user.is_some() || pass.is_some();
    if need_auth {
        // username/password method is 0x02
        let use_userpass = methods.iter().any(|m| *m == 0x02);
        if use_userpass { inbound.write_all(&[0x05, 0x02]).await?; } else { inbound.write_all(&[0x05, 0xFF]).await?; anyhow::bail!("client doesn't support username/password auth"); }
        // subnegotiation: VER=1 | ULEN | UNAME | PLEN | PASSWD
        let mut sb_ver = [0u8;1];
        read_exact_into(&mut inbound, &mut sb_ver).await?;
        if sb_ver[0] != 0x01 { anyhow::bail!("invalid auth subnegotiation version"); }
        let mut ulen_b = [0u8;1];
        read_exact_into(&mut inbound, &mut ulen_b).await?;
        let ulen = ulen_b[0] as usize;
        let mut ubytes = vec![0u8; ulen];
        if ulen>0 { inbound.read_exact(&mut ubytes).await?; }
        let mut plen_b = [0u8;1];
        read_exact_into(&mut inbound, &mut plen_b).await?;
        let plen = plen_b[0] as usize;
        let mut pbytes = vec![0u8; plen];
        if plen>0 { inbound.read_exact(&mut pbytes).await?; }
        let ok = user.map(|u| u.as_bytes().to_vec()).as_deref()==Some(&ubytes[..]) && pass.map(|p| p.as_bytes().to_vec()).as_deref()==Some(&pbytes[..]);
        if ok { inbound.write_all(&[0x01, 0x00]).await?; } else { inbound.write_all(&[0x01, 0x01]).await?; anyhow::bail!("invalid username/password"); }
    } else {
        // No auth
        inbound.write_all(&[0x05, 0x00]).await?;
    }

    // Request: VER | CMD | RSV | ATYP
    let mut h = [0u8; 4];
    read_exact_into(&mut inbound, &mut h).await?;
    if h[0] != 5 {
        anyhow::bail!("Invalid SOCKS5 version in request");
    }
    let cmd = h[1];
    let atyp = h[3];

    let (target_host, target_port) = match atyp {
        0x01 => {
            // IPv4
            let mut v4 = [0u8; 4];
            read_exact_into(&mut inbound, &mut v4).await?;
            let ip = std::net::Ipv4Addr::new(v4[0], v4[1], v4[2], v4[3]);
            let mut p = [0u8; 2];
            read_exact_into(&mut inbound, &mut p).await?;
            let port = u16::from_be_bytes(p);
            (ip.to_string(), port)
        }
        0x03 => {
            // Domain
            let mut l = [0u8; 1];
            read_exact_into(&mut inbound, &mut l).await?;
            let len = l[0] as usize;
            let mut host_bytes = vec![0u8; len];
            if len > 0 {
                inbound.read_exact(&mut host_bytes).await?;
            }
            let host = String::from_utf8_lossy(&host_bytes).to_string();
            let mut p = [0u8; 2];
            read_exact_into(&mut inbound, &mut p).await?;
            let port = u16::from_be_bytes(p);
            (host, port)
        }
        _ => anyhow::bail!("Unsupported ATYP"),
    };

    match cmd {
        0x01 => {
            // CONNECT
            log_throttled(|| println!(
                "SOCKS5 CONNECT -> {}:{} (iface: {})",
                target_host, target_port, iface
            ));
            let mut outbound = connect_outbound(&target_host, target_port, iface).await?;
            // Success reply: VER, REP=0x00, RSV, ATYP=IPv4, BND.ADDR=0.0.0.0, BND.PORT=0
            inbound
                .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await?;
            let (c2s, s2c) = copy_bidirectional(&mut inbound, &mut outbound).await?;
            log_throttled(|| println!(
                "SOCKS5 finished {}:{} (c->s: {} bytes, s->c: {} bytes)",
                target_host, target_port, c2s, s2c
            ));
            Ok(())
        }
        0x03 => {
            // UDP ASSOC - not supported here
            anyhow::bail!("UDP ASSOC not supported")
        }
        _ => anyhow::bail!("Unsupported CMD"),
    }
}

#[allow(dead_code)]
pub async fn run_socks5_proxy(iface: &str, listen: &str) -> Result<()> {
    let listener = TcpListener::bind(listen).await?;
    println!("SOCKS5 proxy listening on {}, bound to {}", listen, iface);
    loop {
        let (inbound, peer_addr) = listener.accept().await?;
        let listen_for_log = listen.to_string();
        log_throttled(|| println!(
            "Incoming TCP connection from {} -> listening on {} (iface: {})",
            peer_addr, listen_for_log, iface
        ));
        let iface_for_task = iface.to_string();
        tokio::spawn(async move {
            if let Err(e) = handle_socks5(inbound, &iface_for_task, None, None).await {
                eprintln!("SOCKS5 handler error: {}", e);
            }
        });
    }
}

pub async fn run_socks5_proxy_auth(iface: &str, listen: &str, user: Option<&str>, pass: Option<&str>) -> Result<()> {
    let listener = TcpListener::bind(listen).await?;
    println!("SOCKS5 proxy listening on {}, bound to {}", listen, iface);
    loop {
        let (inbound, peer_addr) = listener.accept().await?;
        let listen_for_log = listen.to_string();
        let iface_for_task = iface.to_string();
        let user_opt = user.map(|s| s.to_string());
        let pass_opt = pass.map(|s| s.to_string());
        log_throttled(|| println!(
            "Incoming TCP connection from {} -> listening on {} (iface: {})",
            peer_addr, listen_for_log, iface
        ));
        tokio::spawn(async move {
            let u = user_opt.as_deref();
            let p = pass_opt.as_deref();
            if let Err(e) = handle_socks5(inbound, &iface_for_task, u, p).await {
                eprintln!("SOCKS5 handler error: {}", e);
            }
        });
    }
}


