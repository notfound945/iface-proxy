## 工具简介

一个支持 http_proxy/https_proxy 的本地代理，监听 `127.0.0.1:7890`，将客户端的 HTTP 请求或 HTTPS CONNECT 隧道转发到目标站点；转发前会将“外发连接”绑定到指定网卡，便于控制出站接口。

- **协议**: HTTP 代理（普通 HTTP 转发 + HTTPS 的 CONNECT 隧道）
- **监听**: 127.0.0.1:7890（固定）
- **出站绑定**: 通过 `--iface` 指定网卡（macOS 使用 IP_BOUND_IF，Linux 使用 SO_BINDTODEVICE）
- **日志**: 内置每秒限频（默认 50 条/秒），新秒开始会打印上一秒抑制数量

### 开发背景

多网口时，指定流量出口，通过 http_proxy/https_proxy 使用。

## 快速开始

### 编译
```bash
cargo build --release
# 产物：./target/release/iface-socks5
```

或使用 Makefile：
```bash
make release
# 运行发布二进制（可覆盖网卡名）
make run-release IFACE=en0
```

### 运行
```bash
./target/release/iface-socks5 --iface en0
# 日志示例：
# HTTP proxy listening on 127.0.0.1:7890, bound to en0
```

macOS 可用以下命令查看网卡名（常见为 `en0`/`en1`）：
```bash
networksetup -listallhardwareports
```
Linux 查看网卡：
```bash
ip link
```

## 使用方式

### curl 测试（HTTP）
```bash
curl -x http://127.0.0.1:7890 -I http://example.com -v --connect-timeout 5 --max-time 10
```

### curl 测试（HTTPS via CONNECT）
```bash
curl -x http://127.0.0.1:7890 -I https://example.com -v --connect-timeout 5 --max-time 10
```

### 通过环境变量（适用于多数 CLI）
```bash
export http_proxy=http://127.0.0.1:7890
export https_proxy=http://127.0.0.1:7890
curl -I https://example.com -v
```

## 行为说明

- 普通 HTTP 请求：解析绝对 URI 或基于 `Host` 头，重写为 `METHOD path HTTP/x.x` 后转发。
- HTTPS：处理 `CONNECT host:port`，返回 `200 Connection Established` 后透明转发 TLS 流量。
- 出站连接仅尝试 IPv4 地址，并在 `connect` 前绑定指定网卡。
- 日志输出有全局每秒限频（默认 50 条）。可在 `src/main.rs` 中调整 `LOGS_PER_SEC`。

## 权限与平台注意

- macOS：`--iface` 应填如 `en0` 的实际网卡名；通过 IP_BOUND_IF 绑定。
- Linux：使用 SO_BINDTODEVICE，通常需要 root 或 `CAP_NET_ADMIN` 权限。

## Makefile 速览

```bash
make build           # 调试构建
make release         # 发布构建
make run IFACE=en0   # 调试运行，指定网卡
make run-release IFACE=en0
make strip           # 去符号减小体积（macOS）
make linux-musl      # 构建 Linux musl 静态二进制
```

## 限制与路线图

- 仅支持 HTTP 代理与 HTTPS CONNECT；不支持 SOCKS5/UDP（已移除）。
- 仅尝试 IPv4 出站；后续可按需增加 IPv6。
- 暂无鉴权支持；如需用户名/密码鉴权可后续扩展。
- 监听地址目前固定为 `127.0.0.1:7890`，如需参数化可新增 `--listen` 选项。



