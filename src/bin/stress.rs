use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Clone, Copy, Debug)]
enum Mode { Http, Connect, Idle }

fn parse_args() -> (String, Mode, String, usize, u64) {
    // defaults
    let mut target = String::from("127.0.0.1:7890");
    let mut mode = Mode::Http;
    // for http: http uri; for connect: host:port; for idle: unused
    let mut payload = String::from("http://example.com/");
    let mut conns: usize = 500;
    let mut duration_secs: u64 = 60;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--target" { if let Some(v) = args.next() { target = v; } }
        else if let Some(v) = arg.strip_prefix("--target=") { target = v.to_string(); }
        else if arg == "--mode" { if let Some(v) = args.next() { mode = parse_mode(&v); } }
        else if let Some(v) = arg.strip_prefix("--mode=") { mode = parse_mode(v); }
        else if arg == "--payload" { if let Some(v) = args.next() { payload = v; } }
        else if let Some(v) = arg.strip_prefix("--payload=") { payload = v.to_string(); }
        else if arg == "--conns" { if let Some(v) = args.next() { conns = v.parse().unwrap_or(conns); } }
        else if let Some(v) = arg.strip_prefix("--conns=") { conns = v.parse().unwrap_or(conns); }
        else if arg == "--duration-secs" { if let Some(v) = args.next() { duration_secs = v.parse().unwrap_or(duration_secs); } }
        else if let Some(v) = arg.strip_prefix("--duration-secs=") { duration_secs = v.parse().unwrap_or(duration_secs); }
        else if arg == "-h" || arg == "--help" { print_help_and_exit(); }
    }
    (target, mode, payload, conns, duration_secs)
}

fn parse_mode(s: &str) -> Mode { match s { "http" => Mode::Http, "connect" => Mode::Connect, "idle" => Mode::Idle, _ => Mode::Http } }

fn print_help_and_exit() -> ! {
    eprintln!("stress - simple HTTP proxy stress tool\n\nOptions:\n  --target ADDR:PORT       Proxy address (default 127.0.0.1:7890)\n  --mode http|connect|idle Mode: http absolute-URI GET; connect sends CONNECT then closes; idle opens TCP and does nothing\n  --payload STR            http: URI (default http://example.com/); connect: host:port (default example.com:443)\n  --conns N                Concurrent connections (default 500)\n  --duration-secs S        Test duration in seconds (default 60)\n");
    std::process::exit(0)
}

async fn worker_http(target: &str, uri: &str) -> anyhow::Result<()> {
    let mut stream = TcpStream::connect(target).await?;
    let host = uri.strip_prefix("http://").and_then(|r| r.split('/').next()).unwrap_or("");
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        uri, host
    );
    stream.write_all(req.as_bytes()).await?;
    let mut buf = [0u8; 1024];
    let _ = stream.read(&mut buf).await; // best-effort
    Ok(())
}

async fn worker_connect(target: &str, authority: &str) -> anyhow::Result<()> {
    let mut stream = TcpStream::connect(target).await?;
    let req = format!(
        "CONNECT {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        authority, authority
    );
    stream.write_all(req.as_bytes()).await?;
    // read a small response then close
    let mut buf = [0u8; 128];
    let _ = stream.read(&mut buf).await;
    Ok(())
}

async fn worker_idle(target: &str) -> anyhow::Result<()> {
    // open and keep a short idle to exercise server read timeout
    let _stream = TcpStream::connect(target).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (target, mode, payload, conns, duration_secs) = parse_args();
    let payload = if matches!(mode, Mode::Connect) {
        if payload.is_empty() || !payload.contains(':') { "example.com:443".to_string() } else { payload }
    } else { payload };

    let stop = Arc::new(AtomicBool::new(false));
    let success = Arc::new(AtomicU64::new(0));
    let failures = Arc::new(AtomicU64::new(0));

    let start = Instant::now();
    let stop_clone = stop.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(duration_secs)).await;
        stop_clone.store(true, Ordering::SeqCst);
    });

    // stats ticker
    let s_succ = success.clone();
    let s_fail = failures.clone();
    tokio::spawn(async move {
        let mut prev_s = 0u64; let mut prev_f = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let s = s_succ.load(Ordering::Relaxed);
            let f = s_fail.load(Ordering::Relaxed);
            let ds = s - prev_s; let df = f - prev_f; prev_s = s; prev_f = f;
            eprintln!(
                "[{:?}] +ok={} +err={} total_ok={} total_err={}",
                start.elapsed(), ds, df, s, f
            );
        }
    });

    let mut tasks = Vec::with_capacity(conns);
    for _ in 0..conns {
        let target_c = target.clone();
        let payload_c = payload.clone();
        let stop_c = stop.clone();
        let succ_c = success.clone();
        let fail_c = failures.clone();
        tasks.push(tokio::spawn(async move {
            while !stop_c.load(Ordering::Relaxed) {
                let res = match mode {
                    Mode::Http => worker_http(&target_c, &payload_c).await,
                    Mode::Connect => worker_connect(&target_c, &payload_c).await,
                    Mode::Idle => worker_idle(&target_c).await,
                };
                match res {
                    Ok(_) => { succ_c.fetch_add(1, Ordering::Relaxed); }
                    Err(_) => { fail_c.fetch_add(1, Ordering::Relaxed); }
                }
            }
        }));
    }

    for t in tasks { let _ = t.await; }
    eprintln!("Finished in {:?}. ok={} err={}", start.elapsed(), success.load(Ordering::Relaxed), failures.load(Ordering::Relaxed));
    Ok(())
}


