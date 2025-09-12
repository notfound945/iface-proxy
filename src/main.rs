use anyhow::Result;

mod util;
mod http_proxy;
mod socks5;

fn print_help() {
    println!("iface-proxy - 本地 HTTP/HTTPS 与 SOCKS5 代理\n\n用法:\n  iface-proxy [OPTIONS]\n\n常用参数:\n  -i, --iface <NAME>              指定外发网卡名称 (默认: en0)\n  -l, --listen <ADDR:PORT>        HTTP 代理监听地址 (默认: 127.0.0.1:7890，HTTP/1.x)\n  -S, --socks5-listen <ADDR:PORT> SOCKS5 监听地址 (默认: 127.0.0.1:1080)\n      --no-socks5                 禁用 SOCKS5 代理\n      --socks5-user <USER>        SOCKS5 用户名 (可选)\n      --socks5-pass <PASS>        SOCKS5 密码 (可选)\n      --tls-cert <FILE>           启用 h2(TLS) 时的证书 (PEM)\n      --tls-key  <FILE>           启用 h2(TLS) 时的私钥 (PEM)\n      --http2-listen <ADDR:PORT>  额外启用 h2c 专用端口 (仅 CONNECT)\n      --http2                     主端口启用 HTTP/2/h2c (默认关闭)\n  -h, --help                      显示本帮助并退出\n\n说明:\n- 默认启动 HTTP(127.0.0.1:7890，HTTP/1.x) 与 SOCKS5(127.0.0.1:1080)。可用 --no-socks5 禁用 SOCKS5。\n- 可通过 --http2 在主端口启用 h2c；提供 --tls-cert/--tls-key 则支持 h2(TLS+ALPN)。\n- 也可通过 --http2-listen 启动一个仅 h2c 的专用端口（仅 CONNECT）。\n- 出站连接将绑定到指定网卡 (--iface)。\n示例:\n  iface-proxy --iface en0\n  iface-proxy --iface en0 --listen 127.0.0.1:8080\n  iface-proxy --iface en0 --http2-listen 127.0.0.1:8081\n  iface-proxy --iface en0 --http2 --tls-cert cert.pem --tls-key key.pem\n");
}

#[tokio::main]
async fn main() -> Result<()> {
    // 解析命令行参数中的 --iface/-i，默认 en0
    let mut iface = String::from("en0");
    let mut listen = String::from("127.0.0.1:7890");
    let mut socks5_listen: Option<String> = Some(String::from("127.0.0.1:1080"));
    let mut socks5_user: Option<String> = None;
    let mut socks5_pass: Option<String> = None;
    let mut disable_socks5 = false;
    let mut enable_h2 = false;
    let mut tls_cert: Option<String> = None;
    let mut tls_key: Option<String> = None;
    let mut http2_listen: Option<String> = None;
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
        } else if arg == "--http2" {
            enable_h2 = true;
        } else if arg == "--no-http2" {
            enable_h2 = false;
        } else if arg == "--tls-cert" {
            if let Some(val) = args.next() { tls_cert = Some(val); }
        } else if let Some(val) = arg.strip_prefix("--tls-cert=") {
            tls_cert = Some(val.to_string());
        } else if arg == "--tls-key" {
            if let Some(val) = args.next() { tls_key = Some(val); }
        } else if let Some(val) = arg.strip_prefix("--tls-key=") {
            tls_key = Some(val.to_string());
        } else if arg == "--http2-listen" {
            if let Some(val) = args.next() { http2_listen = Some(val); }
        } else if let Some(val) = arg.strip_prefix("--http2-listen=") {
            http2_listen = Some(val.to_string());
        } else if arg == "--socks5-listen" || arg == "-S" {
            if let Some(val) = args.next() { socks5_listen = Some(val); }
        } else if let Some(val) = arg.strip_prefix("--socks5-listen=") {
            socks5_listen = Some(val.to_string());
        } else if arg == "--no-socks5" {
            disable_socks5 = true;
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

    if let Some(h2_addr) = http2_listen {
        let h2_iface = iface.clone();
        tokio::spawn(async move { let _ = http_proxy::run_http2_h2c(&h2_iface, &h2_addr).await; });
    }

    if !disable_socks5 {
        if let Some(s5_addr) = socks5_listen {
            let s5_iface = iface.clone();
            let s5_user_cloned = socks5_user.clone();
            let s5_pass_cloned = socks5_pass.clone();
            tokio::spawn(async move { let _ = socks5::run_socks5_proxy_auth(&s5_iface, &s5_addr, s5_user_cloned.as_deref(), s5_pass_cloned.as_deref()).await; });
        }
    }

    let _ = http_task.await;
    Ok(())
}