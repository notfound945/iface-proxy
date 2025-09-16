use anyhow::Result;
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::util::{connect_outbound, log_throttled, log_info, log_error};

async fn read_http_headers(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 { anyhow::bail!("client closed before headers"); }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") { return Ok(buf); }
        if buf.len() > 64 * 1024 { anyhow::bail!("headers too large"); }
    }
}

fn split_headers_body(buf: &[u8]) -> Option<(usize, &[u8])> {
    for i in 0..buf.len().saturating_sub(3) {
        if &buf[i..i+4] == b"\r\n\r\n" { return Some((i+4, &buf[i+4..])); }
    }
    None
}

fn parse_request_line<'a>(headers: &'a str) -> anyhow::Result<(&'a str, &'a str, &'a str)> {
    let mut lines = headers.split("\r\n");
    let line = lines.next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or_else(|| anyhow::anyhow!("bad request line"))?;
    let uri = parts.next().ok_or_else(|| anyhow::anyhow!("bad request line"))?;
    let version = parts.next().ok_or_else(|| anyhow::anyhow!("bad request line"))?;
    Ok((method, uri, version))
}

fn parse_host_from_headers(headers: &str) -> Option<String> {
    for line in headers.split("\r\n").skip(1) {
        if let Some(rest) = line.strip_prefix("Host:") { return Some(rest.trim().to_string()); }
        if let Some(rest) = line.strip_prefix("host:") { return Some(rest.trim().to_string()); }
    }
    None
}

async fn handle_http_proxy(mut inbound: TcpStream, iface: &str) -> Result<()> {
    let raw = read_http_headers(&mut inbound).await?;
    let (header_end, body_start) = split_headers_body(&raw).ok_or_else(|| anyhow::anyhow!("bad headers"))?;
    let headers_str = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let (method, uri, version) = parse_request_line(&headers_str)?;

    if method.eq_ignore_ascii_case("CONNECT") {
        let mut hp = uri.split(':');
        let host = hp.next().unwrap_or("");
        let port: u16 = hp.next().unwrap_or("443").parse().unwrap_or(443);
        log_throttled(|| log_info(format!("HTTP CONNECT -> {}:{} (iface: {})", host, port, iface)));
        let mut outbound = connect_outbound(host, port, iface).await?;
        inbound.write_all(b"HTTP/1.1 200 Connection Established\r\nProxy-Agent: iface-proxy\r\n\r\n").await?;
        let (c2s, s2c) = copy_bidirectional(&mut inbound, &mut outbound).await?;
        log_throttled(|| log_info(format!("HTTP CONNECT finished {}:{} (c->s: {} bytes, s->c: {} bytes)", host, port, c2s, s2c)));
        return Ok(());
    }

    let (mut host, mut port, path) = if let Some(rest) = uri.strip_prefix("http://") {
        if let Some(pos) = rest.find('/') { (rest[..pos].to_string(), 80u16, rest[pos..].to_string()) } else { (rest.to_string(), 80u16, "/".to_string()) }
    } else if uri.starts_with('/') {
        (parse_host_from_headers(&headers_str).unwrap_or_default(), 80u16, uri.to_string())
    } else {
        anyhow::bail!("unsupported URI for HTTP proxy");
    };
    if let Some((h, p)) = host.clone().split_once(':') { host = h.to_string(); port = p.parse().unwrap_or(80); }

    log_throttled(|| log_info(format!("HTTP {} {} -> {}:{} (iface: {})", method, path, host, port, iface)));
    let mut outbound = connect_outbound(&host, port, iface).await?;

    let mut lines = headers_str.split("\r\n");
    let _first = lines.next();
    let mut rebuilt = String::new();
    rebuilt.push_str(&format!("{} {} {}\r\n", method, path, version));
    let mut has_host = false;
    for line in lines {
        if line.is_empty() { continue; }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("host:") { has_host = true; }
        if lower.starts_with("proxy-connection:") || lower.starts_with("proxy-authorization:") { continue; }
        rebuilt.push_str(line);
        rebuilt.push_str("\r\n");
    }
    if !has_host { if port == 80 { rebuilt.push_str(&format!("Host: {}\r\n", host)); } else { rebuilt.push_str(&format!("Host: {}:{}\r\n", host, port)); } }
    rebuilt.push_str("\r\n");

    outbound.write_all(rebuilt.as_bytes()).await?;
    if !body_start.is_empty() { inbound.write_all(body_start).await?; }
    let (c2s, s2c) = copy_bidirectional(&mut inbound, &mut outbound).await?;
    log_throttled(|| log_info(format!("HTTP finished {} {} (c->s: {} bytes, s->c: {} bytes)", method, host, c2s, s2c)));
    Ok(())
}

pub async fn run_http_proxy(iface: &str, listen: &str) -> Result<()> {
    let listener = TcpListener::bind(listen).await?;
    log_info(format!("HTTP proxy listening on {}, bound to {}", listen, iface));
    loop {
        let (inbound, peer_addr) = listener.accept().await?;
        let listen_for_log = listen.to_string();
        log_throttled(|| log_info(format!(
            "Incoming TCP connection from {} -> listening on {} (iface: {})",
            peer_addr, listen_for_log, iface
        )));
        let iface_for_task = iface.to_string();
        tokio::spawn(async move {
            if let Err(e) = handle_http_proxy(inbound, &iface_for_task).await {
                log_error(format!("TCP handler error: {}", e));
            }
        });
    }
}
