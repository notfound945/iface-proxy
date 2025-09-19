use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::io;

use anyhow::Result;
use tokio::net::{lookup_host, TcpSocket, TcpStream};

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
pub(crate) fn bind_iface_v4(fd: i32, iface: &str) -> Result<()> {
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

#[cfg(target_os = "macos")]
pub(crate) fn bind_iface_v6(fd: i32, iface: &str) -> Result<()> {
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

// 全局日志限频
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

pub(crate) fn log_throttled<F: FnOnce()>(f: F)
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
                log_log(format!("suppressed {} messages in last 1s", suppressed));
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

pub(crate) fn current_timestamp_prefix() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs() as i64;
    let millis = now.subsec_millis();
    let t: nix::libc::time_t = secs as nix::libc::time_t;
    let mut tm: nix::libc::tm = unsafe { std::mem::zeroed() };
    unsafe { let _ = nix::libc::localtime_r(&t, &mut tm); }
    let year = tm.tm_year + 1900;
    let month = tm.tm_mon + 1;
    let day = tm.tm_mday;
    let hour = tm.tm_hour;
    let min = tm.tm_min;
    let sec = tm.tm_sec;
    format!("[{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}.{millis:03}]")
}

pub(crate) fn log_info(message: impl AsRef<str>) {
    println!(
        "{} \x1b[32mINFO\x1b[0m {}",
        current_timestamp_prefix(),
        message.as_ref()
    );
}

pub(crate) fn log_log(message: impl AsRef<str>) {
    println!(
        "{} \x1b[36mLOG\x1b[0m {}",
        current_timestamp_prefix(),
        message.as_ref()
    );
}

pub(crate) fn log_error(message: impl AsRef<str>) {
    eprintln!(
        "{} \x1b[31mERROR\x1b[0m {}",
        current_timestamp_prefix(),
        message.as_ref()
    );
}

pub(crate) fn is_transient_anyhow_error(err: &anyhow::Error) -> bool {
    if let Some(ioe) = err.downcast_ref::<io::Error>() {
        return matches!(
            ioe.kind(),
            io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::TimedOut
                | io::ErrorKind::UnexpectedEof
        );
    }
    let s = err.to_string().to_lowercase();
    s.contains("broken pipe")
        || s.contains("connection reset")
        || s.contains("timed out")
        || s.contains("connection aborted")
        || s.contains("unexpected eof")
}

#[cfg(target_os = "macos")]
pub(crate) fn try_raise_nofile_limit(min_soft: u64) {
    unsafe {
        let mut lim = nix::libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        if nix::libc::getrlimit(nix::libc::RLIMIT_NOFILE, &mut lim) != 0 {
            log_error("getrlimit(RLIMIT_NOFILE) failed");
            return;
        }
        let mut new_lim = lim;
        if new_lim.rlim_cur < min_soft as u64 {
            new_lim.rlim_cur = min_soft as u64;
        }
        if new_lim.rlim_max < new_lim.rlim_cur {
            new_lim.rlim_max = new_lim.rlim_cur;
        }
        if nix::libc::setrlimit(nix::libc::RLIMIT_NOFILE, &new_lim) != 0 {
            log_log(format!(
                "NOFILE raise attempt failed; current soft={}, hard={}",
                lim.rlim_cur, lim.rlim_max
            ));
        } else {
            let mut after = nix::libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            let _ = nix::libc::getrlimit(nix::libc::RLIMIT_NOFILE, &mut after);
            log_log(format!(
                "NOFILE limit: soft={} hard={}",
                after.rlim_cur, after.rlim_max
            ));
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn try_raise_nofile_limit(_min_soft: u64) {
    // No-op on unsupported targets
}

pub(crate) async fn connect_outbound(host: &str, port: u16, iface: &str) -> Result<TcpStream> {
    let addrs = lookup_host((host, port)).await?;
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
    if let Some(e) = last_err {
        Err(e)
    } else {
        anyhow::bail!("no address")
    }
}


