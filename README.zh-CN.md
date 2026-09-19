# rpi-led-webpush — 用浏览器把视频推送到 HUB75 LED 点阵屏

用手机或电脑打开一个网页，选择**视频文件**、**摄像头**或**共享屏幕**，画面就会实时显示在
HUB75 RGB LED 点阵屏上。

**解码和缩放都在浏览器里完成**，只把面板分辨率大小的小帧（64×32 时每帧约 6 KB）通过
WebSocket 发过来，所以驱动面板的设备几乎不占 CPU。

## 特点

* **零第三方依赖** —— `cargo tree` 只有本包自身。HTTP 服务、WebSocket 帧、SHA-1、base64
  都是手写的（见 `src/web.rs`），面板通过手写的 FFI 调用
  [rpi-rgb-led-matrix](https://github.com/hzeller/rpi-rgb-led-matrix) 的 C API。
  不引入任何 crates.io 依赖，因此可 `cargo build --offline` 离线构建，也没有供应链投毒面。
* **前端内嵌在二进制里** —— 投送页与预览页都在程序内部，无外部文件。
* **自适应面板** —— 页面从 `/geo` 读取面板的逻辑分辨率，链屏/并联屏无需改动前端。
* **帧率跟随源** —— 由 `requestVideoFrameCallback` 驱动，60fps 的视频就发 60fps，
  10fps 就发 10fps，不是固定节流。
* **背压丢帧** —— 链路跟不上时主动丢帧而不是堆积，慢网下是"降帧"而不是"延迟越滚越大"。
* **桌面端可后台运行** —— 会申请屏幕常亮（Wake Lock），并有两层兜底（静音 AudioContext
  节拍器 + 定时器）让最小化后仍持续发送。手机浏览器会挂起后台页面，这是平台限制，
  网页无法绕过。

## 路由

| 路由 | 方法 | 用途 |
|---|---|---|
| `/` | GET | 投送页（内嵌）：视频文件 / 摄像头 / 共享屏幕，可选适应方式 |
| `/view` | GET | 实时预览面板此刻显示的内容 |
| `/geo` | GET | 面板逻辑分辨率，如 `64 32` |
| `/push` | WebSocket | 投送方向：二进制帧 `[宽 u16][高 u16][RGB…]`（小端） |
| `/ws` | WebSocket | 预览方向：同样的帧格式，服务端 → 浏览器 |

## 依赖

* 树莓派 + HUB75 面板（Pi 5 上程序自动选择 RP1 PIO 后端）
* [rpi-rgb-led-matrix](https://github.com/hzeller/rpi-rgb-led-matrix)，需先编译（`make -C lib`）
* Rust 工具链（edition 2024）

## 编译

```bash
# 指向你的 rpi-rgb-led-matrix 目录或安装前缀（默认 /usr/local）
RGB_MATRIX_DIR=/path/to/rpi-rgb-led-matrix cargo build --release --offline
```

## 运行

```bash
sudo ./rpi-led-webpush                            # 64x32，RGB 顺序，网页服务在 8080
sudo ./rpi-led-webpush --rows 64 --cols 64        # 换面板尺寸
sudo ./rpi-led-webpush --chain 2                  # 两块串联（逻辑分辨率 128x32）
sudo ./rpi-led-webpush --rgb-sequence RBG         # 面板绿蓝对调时
sudo ./rpi-led-webpush --mapping adafruit-hat-pwm # 常见转接板的引脚定义
sudo ./rpi-led-webpush --web-port 0               # 关闭网页服务
```

需要 root 运行（要访问 `/dev/mem` 和 `/dev/pio0`）。

| 参数 | 默认 | 说明 |
|---|---|---|
| `--rows` / `--cols` | 32 / 64 | 面板尺寸 |
| `--chain` / `--parallel` | 1 / 1 | 串联 / 并联面板数 |
| `--brightness` | 60 | 1..100 |
| `--mapping` | `regular` | GPIO 映射（`regular`、`adafruit-hat`、`adafruit-hat-pwm`、`classic` 等） |
| `--rgb-sequence` | `RGB` | 面板的颜色通道顺序 |
| `--web-port` | 8080 | `0` 表示关闭网页服务 |
| `--no-idle` | — | 空闲时保持黑屏（默认是缓慢呼吸的暗蓝） |

## HTTPS（摄像头与屏幕共享需要）

浏览器只在**安全上下文**下开放摄像头和屏幕采集接口。用普通 HTTP 打开页面**仍然可以投送
视频文件**；要用摄像头或共享屏幕，需要在前面加一层 TLS —— `contrib/rpi-led-webpush-tls.service`
给出了用 `socat` + 自签证书的两行配置，浏览器里接受一次证书警告即可。

## 注意事项

* **同一时刻只能有一个程序驱动面板**（RP1 PIO 只有 4 个状态机，多进程会互相破坏）。程序会
  检测可能的冲突并拒绝启动；设 `LED_FORCE=1` 可强制运行。
* 网页界面**没有任何鉴权** —— 能访问到端口的人都可以投送内容，仅在可信网络中使用。

## 许可

GPL-2.0-or-later —— 因为链接了同为 GPL-2.0 的 rpi-rgb-led-matrix。
