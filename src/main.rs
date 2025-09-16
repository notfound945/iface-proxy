use anyhow::Result;

mod util;
mod http_proxy;
mod socks5;

fn print_help() {
    println!("iface-proxy - 本地 HTTP/HTTPS 与 SOCKS5 代理 (仅 HTTP/1.x)\n\n用法:\n  iface-proxy [OPTIONS]\n\n常用参数:\n  -i, --iface <NAME>              指定外发网卡名称 (默认: en0)\n  -l, --listen <ADDR:PORT>        HTTP 代理监听地址 (默认: 127.0.0.1:7890，HTTP/1.x)\n      --socks5                    启用 SOCKS5 代理（默认关闭）\n  -S, --socks5-listen <ADDR:PORT> SOCKS5 监听地址 (默认: 127.0.0.1:7080，与 --socks5 配合使用)\n  -h, --help                      显示本帮助并退出\n\n说明:\n- 默认仅启动 HTTP(127.0.0.1:7890，HTTP/1.x)。使用 --socks5 才会启用 SOCKS5(默认 127.0.0.1:7080)。\n- 出站连接将绑定到指定网卡 (--iface)。\n示例:\n  iface-proxy --iface en0\n  iface-proxy --iface en0 --socks5\n  iface-proxy --iface en0 --socks5 --socks5-listen 127.0.0.1:1081\n  iface-proxy --iface en0 --listen 127.0.0.1:8080\n");
}

#[tokio::main]
async fn main() -> Result<()> {
    // 解析命令行参数中的 --iface/-i，默认 en0
    let mut iface = String::from("en0");
    let mut listen = String::from("127.0.0.1:7890");
    let mut socks5_listen: Option<String> = Some(String::from("127.0.0.1:7080"));
    let mut socks5_user: Option<String> = None;
    let mut socks5_pass: Option<String> = None;
    let mut enable_socks5 = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" { print_help(); return Ok(()); }
        if arg == "--iface" || arg == "-i" {
            if let Some(val) = args.next() { iface = val; }
        } else if let Some(val) = arg.strip_prefix("--iface=") {
            iface = val.to_string();
        } else if arg == "--listen" || arg == "-l" {
            if let Some(val) = args.next() { listen = val; }
        } else if let Some(val) = arg.strip_prefix("--listen=") {
            listen = val.to_string();
        } else if arg == "--socks5" {
            enable_socks5 = true;
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
    // 主端口固定 HTTP/1.x 代理
    let http_task = tokio::spawn(async move {
        if let Err(e) = http_proxy::run_http_proxy(&http_iface, &http_listen).await {
            eprintln!("{} HTTP proxy fatal error: {}", crate::util::current_timestamp_prefix(), e);
        }
    });

    if enable_socks5 {
        if let Some(s5_addr) = socks5_listen {
            let s5_iface = iface.clone();
            let s5_user_cloned = socks5_user.clone();
            let s5_pass_cloned = socks5_pass.clone();
            tokio::spawn(async move {
                if let Err(e) = socks5::run_socks5_proxy_auth(&s5_iface, &s5_addr, s5_user_cloned.as_deref(), s5_pass_cloned.as_deref()).await {
                    eprintln!("{} SOCKS5 proxy fatal error: {}", crate::util::current_timestamp_prefix(), e);
                }
            });
        }
    }

    if let Err(e) = http_task.await {
        eprintln!("{} HTTP proxy task panicked: {}", crate::util::current_timestamp_prefix(), e);
    }
    Ok(())
}