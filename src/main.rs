use anyhow::Result;

mod util;
mod http_proxy;
mod socks5;

#[tokio::main]
async fn main() -> Result<()> {
    // 解析命令行参数中的 --iface/-i，默认 en0
    let mut iface = String::from("en0");
    let mut listen = String::from("127.0.0.1:7890");
    let mut socks5_listen: Option<String> = Some(String::from("127.0.0.1:1080"));
    let mut socks5_user: Option<String> = None;
    let mut socks5_pass: Option<String> = None;
    let mut enable_h2 = true;
    let mut tls_cert: Option<String> = None;
    let mut tls_key: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--iface" || arg == "-i" {
            if let Some(val) = args.next() {
                iface = val;
            }
        } else if let Some(val) = arg.strip_prefix("--iface=") {
            iface = val.to_string();
        } else if arg == "--listen" || arg == "-l" {
            if let Some(val) = args.next() {
                listen = val;
            }
        } else if let Some(val) = arg.strip_prefix("--listen=") {
            listen = val.to_string();
        } else if arg == "--http2" {
            enable_h2 = true;
        } else if arg == "--tls-cert" {
            if let Some(val) = args.next() { tls_cert = Some(val); }
        } else if let Some(val) = arg.strip_prefix("--tls-cert=") {
            tls_cert = Some(val.to_string());
        } else if arg == "--tls-key" {
            if let Some(val) = args.next() { tls_key = Some(val); }
        } else if let Some(val) = arg.strip_prefix("--tls-key=") {
            tls_key = Some(val.to_string());
        } else if arg == "--socks5-listen" || arg == "-S" {
            if let Some(val) = args.next() { socks5_listen = Some(val); }
        } else if let Some(val) = arg.strip_prefix("--socks5-listen=") {
            socks5_listen = Some(val.to_string());
        } else if arg == "--socks5-user" {
            if let Some(val) = args.next() { socks5_user = Some(val); }
        } else if let Some(val) = arg.strip_prefix("--socks5-user=") {
            socks5_user = Some(val.to_string());
        } else if arg == "--socks5-pass" {
            if let Some(val) = args.next() { socks5_pass = Some(val); }
        } else if let Some(val) = arg.strip_prefix("--socks5-pass=") {
            socks5_pass = Some(val.to_string());
        }
    }

    let http_iface = iface.clone();
    let http_listen = listen.clone();
    let http_task = if enable_h2 {
        let opts = http_proxy::Http2Options { tls_cert, tls_key };
        tokio::spawn(async move { http_proxy::run_http_proxy_h2(&http_iface, &http_listen, opts).await })
    } else {
        tokio::spawn(async move { http_proxy::run_http_proxy(&http_iface, &http_listen).await })
    };

    if let Some(s5_addr) = socks5_listen {
        let s5_iface = iface.clone();
        let s5_user_cloned = socks5_user.clone();
        let s5_pass_cloned = socks5_pass.clone();
        tokio::spawn(async move { let _ = socks5::run_socks5_proxy_auth(&s5_iface, &s5_addr, s5_user_cloned.as_deref(), s5_pass_cloned.as_deref()).await; });
    }

    let _ = http_task.await;
    Ok(())
}