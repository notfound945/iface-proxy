
## 工具简介

一个支持 http_proxy/https_proxy 与 socks5 的本地代理，默认监听 `127.0.0.1:7890`（HTTP）。SOCKS5 默认关闭，可通过 `--socks5` 启用，并可用 `--socks5-listen` 指定监听地址；外发连接可绑定到指定网卡，便于控制出站接口。

- **协议**: HTTP 代理（仅 HTTP/1.x；HTTPS 的 CONNECT 隧道）、SOCKS5（支持无认证与用户名/密码认证）
- **监听**: HTTP 通过 `--listen` 指定（默认 127.0.0.1:7890 或你的传参）；SOCKS5 通过 `--socks5` 启用，默认 `127.0.0.1:7080`（可用 `--socks5-listen` 覆盖）

### 开发背景

多网口时，指定流量出口，通过 http_proxy/https_proxy 使用。

## 快速开始

### 一键安装

```bash
curl -fsSL https://raw.githubusercontent.com/notfound945/iface-proxy/refs/heads/master/scripts/install.sh | sh
## 或者 ##
wget -qO- https://raw.githubusercontent.com/notfound945/iface-proxy/refs/heads/master/scripts/install.sh | sh
```

### 默认参数与启用示例

- **HTTP 默认监听**: `127.0.0.1:7890`（或按 `--listen` 覆盖）
- **SOCKS5 默认监听**: `127.0.0.1:7080`（可按 `--socks5-listen` 覆盖）

```bash
# 查看版本
iface-proxy --version

# 查看帮助
iface-proxy --help

# 默认仅启用 HTTP
iface-proxy --iface en0

# 自定义 HTTP 监听
iface-proxy --iface en0 --listen 127.0.0.1:8080

# 启用 SOCKS5（默认 127.0.0.1:7080）
iface-proxy --iface en0 --socks5

# 自定义 SOCKS5 监听
iface-proxy --iface en0 --socks5 --socks5-listen 127.0.0.1:7081

# 启用 SOCKS5（用户名/密码）
iface-proxy --iface en0 --socks5 --socks5-listen 127.0.0.1:7080 \
  --socks5-user user --socks5-pass pass
```
- **出站绑定**: 通过 `--iface` 指定网卡（macOS 使用 IP_BOUND_IF）
- **日志**: 内置每秒限频（默认 50 条/秒），新秒开始会打印上一秒抑制数量

### 编译
```bash
cargo build --release
# 产物：./target/release/iface-proxy
```

或使用 Makefile：
```bash
make release
# 运行发布二进制（可覆盖网卡名）
make run-release IFACE=en0
```

### 运行
```bash
iface-proxy --iface en0 --listen 127.0.0.1:7890
# 日志示例：
# HTTP proxy listening on 127.0.0.1:7890, bound to en0
```

macOS 可用以下命令查看网卡名（常见为 `en0`/`en1`）：
```bash
networksetup -listallhardwareports
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



### curl 测试（SOCKS5）
```bash
# 无认证
curl --socks5-hostname 127.0.0.1:7080 -I https://example.com -v
# 用户名/密码
curl --proxy-user user:pass --socks5-hostname 127.0.0.1:7080 -I https://example.com -v
```

### 禁用 SOCKS5
不传 `--socks5` 即默认禁用。

### 通过环境变量（适用于多数 CLI）
```bash
export http_proxy=http://127.0.0.1:7890
export https_proxy=http://127.0.0.1:7890
curl -I https://example.com -v
```

## 行为说明

- 普通 HTTP 请求：解析绝对 URI 或基于 `Host` 头，重写为 `METHOD path HTTP/x.x` 后转发。
- HTTPS：处理 `CONNECT host:port`，返回 `200 Connection Established` 后透明转发 TLS 流量。
- SOCKS5：支持 CONNECT；可选用户名/密码认证。
- 出站连接支持 IPv4/IPv6，并在 `connect` 前绑定指定网卡。
- 日志输出有全局每秒限频（默认 50 条）。可在 `src/util.rs` 中调整 `LOGS_PER_SEC`。
- 日志自带本地时间戳与颜色分级（INFO/LOG/ERROR）。
- 监听 accept 出错（如 EMFILE）会指数退避并继续运行，避免进程退出。

## 权限与平台注意

- macOS：`--iface` 应填如 `en0` 的实际网卡名；通过 IP_BOUND_IF 绑定。

## Makefile 速览

```bash
make build           # 调试构建
make release         # 发布构建
make run IFACE=en0 LISTEN=127.0.0.1:7890 SOCKS5=1 USER=user PASS=pass
make run IFACE=en0 LISTEN=127.0.0.1:7890 SOCKS5=127.0.0.1:7081 USER=user PASS=pass
make run-release IFACE=en0 LISTEN=127.0.0.1:7890 SOCKS5=1 USER=user PASS=pass
make strip           # 去符号减小体积（macOS）

# 压测（内置 simple stress 工具）
make stress-build
make stress STRESS_TARGET=127.0.0.1:7890 STRESS_MODE=http STRESS_PAYLOAD=http://example.com/ STRESS_CONNS=1000 STRESS_DURATION=120
make stress-connect STRESS_TARGET=127.0.0.1:7890 STRESS_CONNS=2000
make stress-idle STRESS_TARGET=127.0.0.1:7890 STRESS_CONNS=5000 STRESS_DURATION=120
```

## 进阶参数与建议

- 并发与超时（启动参数）：
  - `--max-conns <N>`：最大并发连接数（默认 10000）。超限的新连接将被丢弃并记录日志。
  - `--read-timeout-ms <MS>`：读取请求首部/握手的超时（默认 10000）。
  - `--session-timeout-ms <MS>`：单连接转发会话的超时（默认 600000，10 分钟）。
- 文件描述符上限：
  - 建议在 shell 中提升：`ulimit -n 65536`
  - 程序启动会尝试提升 NOFILE 软/硬限制，并在日志中打印结果。
- 日志降噪：常见瞬时网络错误（Broken pipe、Connection reset、Timeout 等）会降级为 INFO。

## 限制与路线图

- 暂不支持 SOCKS5 的 UDP Associate；如需可后续扩展。



