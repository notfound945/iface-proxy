use anyhow::Result;

mod proxy;

#[tokio::main]
async fn main() -> Result<()> {
    // 解析命令行参数中的 --iface/-i，默认 en0
    let mut iface = String::from("en0");
    let mut listen = String::from("127.0.0.1:7891");
    let mut socks5_listen: Option<String> = None;
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
        } else if arg == "--socks5-listen" || arg == "-S" {
            if let Some(val) = args.next() { socks5_listen = Some(val); }
        } else if let Some(val) = arg.strip_prefix("--socks5-listen=") {
            socks5_listen = Some(val.to_string());
        }
    }

    let http_iface = iface.clone();
    let http_listen = listen.clone();
    let http_task = tokio::spawn(async move { proxy::run_http_proxy(&http_iface, &http_listen).await });

    if let Some(s5_addr) = socks5_listen {
        let s5_iface = iface.clone();
        tokio::spawn(async move { let _ = proxy::run_socks5_proxy(&s5_iface, &s5_addr).await; });
    }

    let _ = http_task.await;
    Ok(())
}