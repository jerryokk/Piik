# 部署 Piik Server

[English](./self-hosting.md) · 简体中文 · [文档导航](../guide/README.zh-CN.md)

Piik Server 将网页、房间管理和可选的媒体转发打包在**一个 Windows 程序** `piik-server.exe` 中。
解压后即可运行，房间数据保存在 SQLite 中。

默认的 P2P 配置由服务器负责房间管理、信令和 STUN，画面与声音在参与者之间传输。
启用 SFU 后，服务器还会承担媒体转发的带宽和处理开销。配置较低的服务器可以先用默认设置，
按预计的房间数和观众数观察 CPU、内存与网络占用，再决定是否开启 SFU。
降低画面分辨率主要减轻参与者设备的采集和编码负担。

## 先在本机试用

向提供 Piik 程序包的人获取
`piik-<revision>-windows-amd64-runtime.zip`（Windows x64）。解压后，在该目录打开 PowerShell 并运行：

```powershell
./piik-server.exe
```

打开 `http://localhost:8787` 即可试用。需要让朋友通过互联网访问时，
继续完成下方配置。如果只想在自己的电脑上临时开房间，可直接使用 [Piik App](../guide/getting-started.zh-CN.md#用-piik-app-分享)。

## 无域名的内网部署

家用服务器或虚拟局域网可以直接使用 Piik 提供的 HTTPS，无需另装反向代理。
在 `piik-server.exe` 旁创建 `.env`，填写以下配置，将所有 `192.168.1.10` 换成服务器在该网络中的 IP：

```dotenv
PIIK_ENV=production
LISTEN_HOST=0.0.0.0
PORT=8787
PUBLIC_BASE_URL=https://192.168.1.10:8787
TLS_SELF_SIGNED=true
STUN_URLS=stun:192.168.1.10:3478
```

在该目录先运行 `./piik-server.exe --check-config`，检查通过后运行 `./piik-server.exe`。
在该网络放行 TCP 8787 和 UDP 3478，然后用各台设备打开上述 **HTTPS** 地址。
不需要公网 IP 或域名。

浏览器会提示证书不受信任。请先核对证书的 SHA-256 指纹，与服务端启动时的
**HTTPS certificate** 信息一致后再继续，见[本地证书确认说明](../guide/troubleshooting.zh-CN.md#本地-https-证书提示)。
临时证书只保存在内存中，重启 Piik 后会更换，需要重新确认。
开麦、摄像头和远控仍需浏览器支持相关接口，并完成各自的正常授权。

如果已有自己的证书，关闭 `TLS_SELF_SIGNED`，同时设置 `TLS_CERT_FILE` 和 `TLS_KEY_FILE`，
指向 PEM 格式的证书和私钥文件。证书需覆盖 `PUBLIC_BASE_URL` 中的域名或 IP；
`--check-config` 会检查文件配对、地址与有效期，替换文件后需重启 Piik。
Piik 不负责申请或续期证书；长期对外服务的站点建议使用能自动管理证书的 HTTPS 代理，见下文。
网页端口被占用时，先停止占用服务，或同时修改 `PORT` 与 `PUBLIC_BASE_URL` 中的端口，
再重新发送邀请。

## 对外提供服务

准备一台 Windows 服务器，以及一个指向服务器公网 IP 的域名。
下文以 `share.example.com` 为例，请替换成自己的域名。

### 1. 配置并启动 Piik

在 `piik-server.exe` 旁新建 `.env` 文件：

```dotenv
PIIK_ENV=production
LISTEN_HOST=127.0.0.1
PUBLIC_BASE_URL=https://share.example.com
STUN_URLS=stun:share.example.com:3478
MAX_VIEWERS_PER_ROOM=20
SITE_ACCESS_PASSWORD=
```

使用普通用户，在该目录的 PowerShell 中执行：

```powershell
./piik-server.exe --check-config
./piik-server.exe
```

程序会自动读取 `.env`，并将房间数据保存在工作目录的 `rooms.sqlite` 中。
`SITE_ACCESS_PASSWORD` 留空时，进入站点无须口令；
如需设置口令，填写你想使用的口令即可。房间邀请与加入权限仍由房主管理。

`PUBLIC_BASE_URL` 应与浏览器访问的网址一致，包括 HTTPS 和非默认端口。
只使用这个地址时，`ALLOWED_ORIGINS` 不设置或留空即可；已有的非空值会覆盖此默认值，
旧地址未更新可能导致[创建房间返回 403](../guide/troubleshooting.zh-CN.md#创建房间返回-403)。

`MAX_VIEWERS_PER_ROOM` 可设置每房观众上限，不含房主，支持 `1..20`，修改后重启生效。
人数越多，对网络和转发资源的需求也可能增加。默认值及 App 房间的区别见
[人数限制](../standards/configuration.md#room-capacity)。

公开站点可选设 `ROOM_EMPTY_TIMEOUT_SECONDS=3600`，重启后，无人在线的房间会在空置一小时后
回收；房间号用满时，会提前回收最早创建的空房间，仍有人在线的房间不受影响。
被回收的房间需要重新创建并发送邀请。不设置或设为 `0` 则保持默认的长期保留行为，
详见[房间保留策略](../standards/configuration.md#room-retention)。

### 2. 配置 HTTPS

使用 HTTPS 反向代理将请求转发到 `127.0.0.1:8787`，并启用 WebSocket 支持。
如果代理设置了 `Permissions-Policy`，请使用 `camera=(self), microphone=(self), geolocation=()`。
摄像头和麦克风的空白允许列表会阻止浏览器采集，即使用户已授权；`self` 允许本站请求访问。

### 3. 放行端口并检查

在 Windows 防火墙和云平台安全组中放行 **TCP 80/443**（HTTPS）和 **UDP 3478**（STUN）。
TCP 8787 仅供本机反向代理访问。STUN 域名需要直接解析到服务器，不能只经过 CDN 的 HTTP 代理。
可选服务的端口见[完整端口表](../standards/configuration.md#public-and-private-ports)。
内置 SFU 的媒体流复用 `SFU_UDP_PORT` 指定的单个 UDP 端口。

打开 `https://share.example.com/healthz`，应返回 `{"status":"ok"}`。
随后打开站点，分享一个画面，并用另一台设备加入验证。
上述基础配置使用 P2P 传输，参与者之间需要可用的 UDP 通路。

## 可选：启用媒体兜底

在 `.env` 中添加 `SFU_UDP_PORT=7882`，放行 UDP 7882，再重启 Piik，即可启用自动 SFU 兜底。
如果服务器处于 NAT 后方，还需将 `SFU_PUBLIC_IP` 设置为外部可达的公网 IPv4 地址或 DDNS 域名。
域名应直接指向该 IPv4，关闭 CDN 代理。Piik 启动时解析域名，之后每分钟更新一次。
DDNS 更新后，新媒体连接使用最新地址，无需重启服务。若地址变化打断了当前分享，房主重新开播，观众重新进入房间。
公网和内部映射的 UDP 端口都应与 `SFU_UDP_PORT` 一致。
这项功能由同一个服务端程序提供。
服务器需要为实际经由 SFU 观看的观众承担转发负载和出口带宽。
房主需要在开始分享前关闭 **隐私模式**，才会允许使用这条线路。
直连和 SFU 媒体都需要可用的 UDP 通路。

如需所有房间都通过服务器转发，再设置 `SFU_ONLY=true` 并重启。
分享设置中会显示 **服务器转发**。请为每位观众预留服务器带宽；
服务器线路失败时，此模式不会改走 P2P。

## 长期运行与更新

可使用已有的服务管理工具或任务计划程序运行 `piik-server.exe`；Piik 不会安装 Windows 服务。
更新时保留 `.env` 和 `rooms.sqlite`：停止服务、备份房间数据、替换新版程序，再启动并检查健康状态和房间访问。
涉及数据格式变化的版本，请先阅读发布说明。

[全部配置与端口](../standards/configuration.md) · [维护者发版工具](release.md)
