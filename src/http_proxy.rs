use anyhow::Result;
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::util::{connect_outbound, log_throttled};

// ---- Optional HTTP/2/h2c (CONNECT-only) via hyper ----
use hyper::{Body, Request, Response, StatusCode};
use hyper::service::service_fn;
use hyper::server::conn::Http;
use std::sync::Arc;
use tokio_rustls::rustls::{self, ServerConfig};
use tokio_rustls::TlsAcceptor;
use tokio::io::{AsyncRead, AsyncWrite};
use std::fs::File;
use std::io::BufReader;

async fn read_http_headers(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 { anyhow::bail!("client closed before headers"); }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") { return Ok(buf); }
        if buf.len() > 64 * 1024 { anyhow::bail!("headers too large"); }
    }
}

fn split_headers_body(buf: &[u8]) -> Option<(usize, &[u8])> {
    for i in 0..buf.len().saturating_sub(3) {
        if &buf[i..i+4] == b"\r\n\r\n" { return Some((i+4, &buf[i+4..])); }
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
        if let Some(rest) = line.strip_prefix("Host:") { return Some(rest.trim().to_string()); }
        if let Some(rest) = line.strip_prefix("host:") { return Some(rest.trim().to_string()); }
    }
    None
}

async fn handle_http_proxy(mut inbound: TcpStream, iface: &str) -> Result<()> {
    let raw = read_http_headers(&mut inbound).await?;
    let (header_end, body_start) = split_headers_body(&raw).ok_or_else(|| anyhow::anyhow!("bad headers"))?;
    let headers_str = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let (method, uri, version) = parse_request_line(&headers_str)?;

    if method.eq_ignore_ascii_case("CONNECT") {
        let mut hp = uri.split(':');
        let host = hp.next().unwrap_or("");
        let port: u16 = hp.next().unwrap_or("443").parse().unwrap_or(443);
        log_throttled(|| println!("HTTP CONNECT -> {}:{} (iface: {})", host, port, iface));
        let mut outbound = connect_outbound(host, port, iface).await?;
        inbound.write_all(b"HTTP/1.1 200 Connection Established\r\nProxy-Agent: iface-proxy\r\n\r\n").await?;
        let (c2s, s2c) = copy_bidirectional(&mut inbound, &mut outbound).await?;
        log_throttled(|| println!("HTTP CONNECT finished {}:{} (c->s: {} bytes, s->c: {} bytes)", host, port, c2s, s2c));
        return Ok(());
    }

    let (mut host, mut port, path) = if let Some(rest) = uri.strip_prefix("http://") {
        if let Some(pos) = rest.find('/') { (rest[..pos].to_string(), 80u16, rest[pos..].to_string()) } else { (rest.to_string(), 80u16, "/".to_string()) }
    } else if uri.starts_with('/') {
        (parse_host_from_headers(&headers_str).unwrap_or_default(), 80u16, uri.to_string())
    } else {
        anyhow::bail!("unsupported URI for HTTP proxy");
    };
    if let Some((h, p)) = host.clone().split_once(':') { host = h.to_string(); port = p.parse().unwrap_or(80); }

    log_throttled(|| println!("HTTP {} {} -> {}:{} (iface: {})", method, path, host, port, iface));
    let mut outbound = connect_outbound(&host, port, iface).await?;

    let mut lines = headers_str.split("\r\n");
    let _first = lines.next();
    let mut rebuilt = String::new();
    rebuilt.push_str(&format!("{} {} {}\r\n", method, path, version));
    let mut has_host = false;
    for line in lines {
        if line.is_empty() { continue; }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("host:") { has_host = true; }
        if lower.starts_with("proxy-connection:") || lower.starts_with("proxy-authorization:") { continue; }
        rebuilt.push_str(line);
        rebuilt.push_str("\r\n");
    }
    if !has_host { if port == 80 { rebuilt.push_str(&format!("Host: {}\r\n", host)); } else { rebuilt.push_str(&format!("Host: {}:{}\r\n", host, port)); } }
    rebuilt.push_str("\r\n");

    outbound.write_all(rebuilt.as_bytes()).await?;
    if !body_start.is_empty() { inbound.write_all(body_start).await?; }
    let (c2s, s2c) = copy_bidirectional(&mut inbound, &mut outbound).await?;
    log_throttled(|| println!("HTTP finished {} {} (c->s: {} bytes, s->c: {} bytes)", method, host, c2s, s2c));
    Ok(())
}

pub async fn run_http_proxy(iface: &str, listen: &str) -> Result<()> {
    let listener = TcpListener::bind(listen).await?;
    println!("HTTP proxy listening on {}, bound to {}", listen, iface);
    loop {
        let (inbound, peer_addr) = listener.accept().await?;
        let listen_for_log = listen.to_string();
        log_throttled(|| println!(
            "Incoming TCP connection from {} -> listening on {} (iface: {})",
            peer_addr, listen_for_log, iface
        ));
        let iface_for_task = iface.to_string();
        tokio::spawn(async move {
            if let Err(e) = handle_http_proxy(inbound, &iface_for_task).await {
                eprintln!("TCP handler error: {}", e);
            }
        });
    }
}

#[derive(Clone, Default)]
pub struct Http2Options {
    pub enable_h2: bool,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
}

fn build_tls(config: &Http2Options) -> Result<Option<TlsAcceptor>> {
    if let (Some(cert_path), Some(key_path)) = (config.tls_cert.as_ref(), config.tls_key.as_ref()) {
        let certs = {
            let mut rd = BufReader::new(File::open(cert_path)?);
            rustls_pemfile::certs(&mut rd)?.into_iter().map(rustls::Certificate).collect::<Vec<_>>()
        };
        let key = {
            let mut rd = BufReader::new(File::open(key_path)?);
            let keys = rustls_pemfile::pkcs8_private_keys(&mut rd)?;
            if keys.is_empty() { anyhow::bail!("no pkcs8 key found"); }
            rustls::PrivateKey(keys[0].clone())
        };
        let mut cfg = ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
        cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Some(TlsAcceptor::from(Arc::new(cfg))))
    } else {
        Ok(None)
    }
}

async fn serve_hyper<I>(io: I, iface: Arc<String>) -> Result<()>
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service = service_fn(move |req: Request<Body>| {
        let iface = iface.clone();
        async move {
            if req.method() == hyper::Method::CONNECT {
                // CONNECT host:port
                let authority = req.uri().authority().map(|a| a.as_str().to_string()).unwrap_or_default();
                let mut parts = authority.split(':');
                let host = parts.next().unwrap_or("");
                let port: u16 = parts.next().unwrap_or("443").parse().unwrap_or(443);
                log_throttled(|| println!("HTTP2 CONNECT -> {}:{} (iface: {})", host, port, iface));
                let outbound_res = connect_outbound(host, port, &iface).await;
                let mut resp = Response::new(Body::empty());
                match outbound_res {
                    Ok(mut outbound) => {
                        *resp.status_mut() = StatusCode::OK;
                        // Spawn upgrade tunnel
                        tokio::spawn(async move {
                            match hyper::upgrade::on(req).await {
                                Ok(mut upgraded) => {
                                    let _ = copy_bidirectional(&mut upgraded, &mut outbound).await;
                                }
                                Err(e) => eprintln!("upgrade error: {}", e),
                            }
                        });
                        Ok::<_, anyhow::Error>(resp)
                    }
                    Err(e) => {
                        *resp.status_mut() = StatusCode::BAD_GATEWAY;
                        *resp.body_mut() = Body::from(format!("connect failed: {}", e));
                        Ok(resp)
                    }
                }
            } else {
                // TODO: 可扩展为完整的 HTTP/2 正向代理，这里先返回 501
                let mut resp = Response::new(Body::from("HTTP/2 proxy only supports CONNECT in this mode"));
                *resp.status_mut() = StatusCode::NOT_IMPLEMENTED;
                Ok(resp)
            }
        }
    });

    Http::new().http2_only(false).serve_connection(io, service).await?;
    Ok(())
}

pub async fn run_http_proxy_h2(iface: &str, listen: &str, opts: Http2Options) -> Result<()> {
    let listener = TcpListener::bind(listen).await?;
    let tls = build_tls(&opts)?;
    let iface_arc = Arc::new(iface.to_string());
    if tls.is_some() {
        println!("HTTP/2(TLS) proxy listening on {}, bound to {}", listen, iface);
    } else {
        println!("HTTP/2(h2c) proxy listening on {}, bound to {}", listen, iface);
    }

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let iface = iface_arc.clone();
        let tls = tls.clone();
        log_throttled(|| println!("Incoming TCP connection from {} -> {}", peer_addr, listen));
        tokio::spawn(async move {
            let res: Result<()> = if let Some(tls_acceptor) = tls {
                match tls_acceptor.accept(stream).await {
                    Ok(tls_stream) => serve_hyper(tls_stream, iface).await,
                    Err(e) => { eprintln!("TLS accept error: {}", e); Ok(()) }
                }
            } else {
                serve_hyper(stream, iface).await
            };
            if let Err(e) = res { eprintln!("hyper serve error: {}", e); }
        });
    }
}


