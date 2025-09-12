use anyhow::Result;

mod proxy;

#[tokio::main]
async fn main() -> Result<()> {
    // 解析命令行参数中的 --iface/-i，默认 en0
    let mut iface = String::from("en0");
    let mut listen = String::from("127.0.0.1:7891");
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
        }
    }

    proxy::run_http_proxy(&iface, &listen).await
}