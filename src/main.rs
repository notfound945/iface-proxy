use std::os::fd::AsRawFd;
use std::ffi::CString;
use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::atomic::{AtomicU64, Ordering};
use anyhow::Result;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
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

/// 处理 SOCKS5 握手 & TCP 代理
async fn handle_socks5(mut inbound: TcpStream, iface: &str) -> Result<()> {
    let mut buf = [0u8; 262];
    inbound.read(&mut buf).await?; // 认证方法请求
    inbound.write_all(&[0x05, 0x00]).await?; // 不认证

    let n = inbound.read(&mut buf).await?;
    if n < 7 {
        anyhow::bail!("Invalid SOCKS5 request");
    }

    let cmd = buf[1];
    let atyp = buf[3];

    let addr = match atyp {
        0x01 => {
            let ip = std::net::Ipv4Addr::new(buf[4], buf[5], buf[6], buf[7]);
            let port = u16::from_be_bytes([buf[8], buf[9]]);
            format!("{}:{}", ip, port)
        }
        0x03 => {
            let len = buf[4] as usize;
            let host = String::from_utf8_lossy(&buf[5..5+len]).to_string();
            let port = u16::from_be_bytes([buf[5+len], buf[6+len]]);
            format!("{}:{}", host, port)
        }
        _ => anyhow::bail!("Unsupported ATYP"),
    };

    if cmd == 0x01 {
        log_throttled(|| println!("SOCKS5 CONNECT request -> {} (iface: {})", addr, iface));
        // TCP CONNECT
        let mut outbound = TcpStream::connect(&addr).await?;
        let fd = outbound.as_raw_fd();
        bind_iface(fd, iface)?;

        inbound.write_all(&[0x05, 0x00, 0x00, 0x01, 0,0,0,0, 0,0]).await?;
        log_throttled(|| println!("TCP CONNECT established -> {} (iface: {})", addr, iface));
        let (c2s, s2c) = copy_bidirectional(&mut inbound, &mut outbound).await?;
        log_throttled(|| println!(
            "TCP CONNECT finished -> {} (c->s: {} bytes, s->c: {} bytes)",
            addr, c2s, s2c
        ));
    } else if cmd == 0x03 {
        anyhow::bail!("UDP ASSOC not supported here (use udp server)");
    } else {
        anyhow::bail!("Unsupported CMD");
    }
    Ok(())
}

/// UDP 代理 (SOCKS5 UDP Associate 模式)
async fn udp_proxy(iface: &str) -> Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:7890").await?;
    let fd = socket.as_raw_fd();
    bind_iface(fd, iface)?;
    println!("UDP relay on 127.0.0.1:7890 (bound to {})", iface);

    let mut buf = [0u8; 1500];

    loop {
        let (len, src) = socket.recv_from(&mut buf).await?;
        log_throttled(|| println!("UDP datagram received: {} bytes from {}", len, src));
        if len < 10 {
            continue;
        }
        // SOCKS5 UDP header: RSV(2) + FRAG(1) + ATYP(1) + DST.ADDR + DST.PORT + DATA
        let atyp = buf[3];
        let (addr, offset) = match atyp {
            0x01 => {
                let ip = std::net::Ipv4Addr::new(buf[4], buf[5], buf[6], buf[7]);
                let port = u16::from_be_bytes([buf[8], buf[9]]);
                (format!("{}:{}", ip, port), 10)
            }
            0x03 => {
                let len = buf[4] as usize;
                let host = String::from_utf8_lossy(&buf[5..5+len]).to_string();
                let port = u16::from_be_bytes([buf[5+len], buf[6+len]]);
                (format!("{}:{}", host, port), 7+len)
            }
            _ => continue,
        };

        let data = &buf[offset..len];
        log_throttled(|| println!(
            "UDP forward -> {} (iface: {}, payload: {} bytes)",
            addr,
            iface,
            data.len()
        ));
        socket.send_to(data, &addr).await?;
        log_throttled(|| println!("UDP {} -> {}", src, addr));
    }
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

    // TCP
    let listener = TcpListener::bind("127.0.0.1:7890").await?;
    println!("SOCKS5 TCP listening on 127.0.0.1:7890, bound to {}", iface);

    // UDP
    let iface_for_udp = iface.clone();
    tokio::spawn(async move {
        if let Err(e) = udp_proxy(&iface_for_udp).await {
            eprintln!("UDP proxy error: {}", e);
        }
    });

    loop {
        let (inbound, peer_addr) = listener.accept().await?;
        log_throttled(|| println!(
            "Incoming TCP connection from {} -> listening on 127.0.0.1:7890 (iface: {})",
            peer_addr, iface
        ));
        let iface_for_task = iface.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_socks5(inbound, &iface_for_task).await {
                eprintln!("TCP handler error: {}", e);
            }
        });
    }
}