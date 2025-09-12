use std::os::fd::AsRawFd;
use std::ffi::CString;
use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::atomic::{AtomicU64, Ordering};
use anyhow::Result;
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional};

#[cfg(target_os = "macos")]
use nix::libc::{if_nametoindex, IPPROTO_IP, IP_BOUND_IF};

#[cfg(target_os = "macos")]
fn bind_iface(fd: i32, iface: &str) -> Result<()> {
    let cstr = CString::new(iface)?;
    let idx = unsafe { if_nametoindex(cstr.as_ptr()) };
    if idx == 0 {
        anyhow::bail!("Invalid iface: {}", iface);
    }
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
fn bind_iface(fd: i32, iface: &str) -> Result<()> {
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
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn log_throttled<F: FnOnce()>(f: F)
where
    F: FnOnce(),
{
    let now = now_sec();
    let window = LOG_WINDOW_SEC.load(Ordering::Relaxed);
    if now != window {
        if LOG_WINDOW_SEC.compare_exchange(window, now, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
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

/// 读取完整的 HTTP 请求头（直到 \r\n\r\n），返回包含头部的原始字节
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
        if &buf[i..i+4] == b"\r\n\r\n" {
            return Some((i + 4, &buf[i+4..]));
        }
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
        if let Some(rest) = line.strip_prefix("Host:") {
            return Some(rest.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("host:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

async fn connect_outbound_ipv4(host: &str, port: u16, iface: &str) -> Result<TcpStream> {
    let addrs = tokio::net::lookup_host((host, port)).await?;
    let mut last_err: Option<anyhow::Error> = None;
    for sa in addrs {
        if let std::net::SocketAddr::V4(v4) = sa {
            let socket = TcpSocket::new_v4()?;
            let fd = socket.as_raw_fd();
            if let Err(e) = bind_iface(fd, iface) { last_err = Some(e); continue; }
            match socket.connect(std::net::SocketAddr::V4(v4)).await {
                Ok(s) => return Ok(s),
                Err(e) => { last_err = Some(anyhow::Error::new(e)); continue; }
            }
        }
    }
    if let Some(e) = last_err { Err(e) } else { anyhow::bail!("no ipv4 address") }
}

/// 处理 HTTP/HTTPS 代理
async fn handle_http_proxy(mut inbound: TcpStream, iface: &str) -> Result<()> {
    // 读取并解析请求头
    let raw = read_http_headers(&mut inbound).await?;
    let (header_end, body_start) = split_headers_body(&raw).ok_or_else(|| anyhow::anyhow!("bad headers"))?;
    let headers_str = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let (method, uri, version) = parse_request_line(&headers_str)?;

    if method.eq_ignore_ascii_case("CONNECT") {
        // CONNECT host:port HTTP/1.1
        let mut hp = uri.split(':');
        let host = hp.next().unwrap_or("");
        let port: u16 = hp.next().unwrap_or("443").parse().unwrap_or(443);
        log_throttled(|| println!("HTTP CONNECT -> {}:{} (iface: {})", host, port, iface));
        let mut outbound = connect_outbound_ipv4(host, port, iface).await?;
        inbound.write_all(b"HTTP/1.1 200 Connection Established\r\nProxy-Agent: iface-proxy\r\n\r\n").await?;
        let (c2s, s2c) = copy_bidirectional(&mut inbound, &mut outbound).await?;
        log_throttled(|| println!("HTTP CONNECT finished {}:{} (c->s: {} bytes, s->c: {} bytes)", host, port, c2s, s2c));
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
        (parse_host_from_headers(&headers_str).unwrap_or_default(), 80u16, uri.to_string())
    } else {
        // 不支持 https:// 直接方法式（应使用 CONNECT）或奇异 URI
        anyhow::bail!("unsupported URI for HTTP proxy");
    };
    // 拆 host:port（对克隆字符串 split，避免借用冲突）
    if let Some((h, p)) = host.clone().split_once(':') { host = h.to_string(); port = p.parse().unwrap_or(80); }

    log_throttled(|| println!("HTTP {} {} -> {}:{} (iface: {})", method, path, host, port, iface));
    let mut outbound = connect_outbound_ipv4(&host, port, iface).await?;

    // 重写请求行与头：METHOD path HTTP/x.x + Host 头（去除 Proxy-Connection 等）
    let mut lines = headers_str.split("\r\n");
    let _first = lines.next();
    let mut rebuilt = String::new();
    rebuilt.push_str(&format!("{} {} {}\r\n", method, path, version));
    let mut has_host = false;
    for line in lines {
        if line.is_empty() { continue; }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("host:") { has_host = true; }
        if lower.starts_with("proxy-connection:") || lower.starts_with("proxy-authorization:") {
            continue;
        }
        rebuilt.push_str(line);
        rebuilt.push_str("\r\n");
    }
    if !has_host {
        if port == 80 { rebuilt.push_str(&format!("Host: {}\r\n", host)); }
        else { rebuilt.push_str(&format!("Host: {}:{}\r\n", host, port)); }
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
    log_throttled(|| println!("HTTP finished {} {} (c->s: {} bytes, s->c: {} bytes)", method, host, c2s, s2c));
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // 解析命令行参数中的 --iface/-i，默认 en0
    let mut iface = String::from("en0");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--iface" || arg == "-i" {
            if let Some(val) = args.next() {
                iface = val;
            }
        } else if let Some(val) = arg.strip_prefix("--iface=") {
            iface = val.to_string();
        }
    }

    // TCP（HTTP/HTTPS 代理）
    let listener = TcpListener::bind("127.0.0.1:7891").await?;
    println!("HTTP proxy listening on 127.0.0.1:7891, bound to {}", iface);

    loop {
        let (inbound, peer_addr) = listener.accept().await?;
        log_throttled(|| println!(
            "Incoming TCP connection from {} -> listening on 127.0.0.1:7891 (iface: {})",
            peer_addr, iface
        ));
        let iface_for_task = iface.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_http_proxy(inbound, &iface_for_task).await {
                eprintln!("TCP handler error: {}", e);
            }
        });
    }
}