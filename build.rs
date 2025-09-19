fn main() {
    // Prefer explicit IFACE_PROXY_VERSION, then CI tag, then Cargo package version
    let version = std::env::var("IFACE_PROXY_VERSION")
        .or_else(|_| std::env::var("GITHUB_REF_NAME"))
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());

    println!("cargo:rustc-env=IFACE_PROXY_VERSION={}", version);
}


