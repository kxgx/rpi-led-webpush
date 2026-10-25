# AGENTS.md — 给 AI / 自动化代理的项目说明

> 在本仓库改代码前先读这份文件。它记录**项目状态、代码地图、各项约定、已知坑**，
> 以及**设置页的功能分类规范**。改配置项、设置页、时钟字模、HTTPS、打包发布时，
> 对应章节是硬性约定，不要随手打破。

---

## 1. 项目概览

**rpi-led-webpush**：用浏览器把视频 / 摄像头 / 屏幕共享推到 Raspberry Pi 的 HUB75
RGB LED 点阵屏。解码与缩放都在浏览器里完成，只通过 WebSocket 发送面板分辨率的小帧
（64×32 时约 6 KB/帧），驱动面板的设备几乎不占 CPU。

核心约束（**不要破坏**）：

- **零第三方 crate 依赖**：`cargo tree` 只有本包。HTTP 服务、WebSocket 帧、SHA-1、
  base64 全部手写在 `src/web.rs`。**不要为了省事加 rustls / tokio / serde 之类的依赖。**
  需要新能力时优先用系统工具（如 HTTPS 用 `socat`）或手写实现，并在 README 说明。
- **前端全部内嵌**：投送页 / 预览页 / 设置页是 `src/web.rs` 里的 raw string 常量，
  没有外部静态文件、没有构建前端步骤。
- **HUB75 驱动 vendored**：`third_party/rpi-rgb-led-matrix`（GPL-2.0）由 `build.rs`
  用系统 `g++/gcc/ar` 自动编译成静态库；不需要 `RGB_MATRIX_DIR`、不需要预装。
- Rust edition 2024；License GPL-2.0-or-later（因为静态链接 GPL 的驱动）。
- 目标平台只有 **Raspberry Pi / Linux**（需要 root、`/dev/mem`、`/dev/pio0`）。

---

## 2. 当前状态（维护本节，发版/大改后更新）

- 版本：`Cargo.toml` = **0.2.3**；最近 release commit `release: v0.2.3`，annotated tag `v0.2.3`。
- 已实现功能：
  - 投送：视频文件 / 摄像头 / 屏幕共享，适应方式可选；`/view` 实时预览面板画面；
  - 设置页：面板几何、亮度（热更新）、空闲行为、语言、硬件驱动参数、网络 / HTTPS、
    状态面板；
  - 空闲显示：时钟 + 日期 + 星期（中/英）、空闲呼吸、全黑；
  - **内置 HTTPS 开关**：用系统 `socat` 做 TLS 终止 + `openssl` 首启自签证书（见 §8），
    从 `v0.2.3` 开始提供；
  - deb 打包（arm64）+ tag 触发 CI 自动构建并创建 GitHub Release。
- 测试规模：**16 个单元测试**（clock 6 / config 4 / tls 4 / web 2），Linux 上 `cargo test` 全绿。
- 已知未修问题（见 §7）：12 小时制在 64 宽面板会裁字；逻辑宽度 < 46 px 的窄面板会裁掉星期。
- 未提交改动以 `git status` 为准；本文件若与代码不一致，**以代码为准并顺手修正本文件**。

---

## 3. 快速命令

```bash
# 构建（Linux/Pi；需要 g++/gcc/ar）
cargo build --release

# 运行（必须 root）
sudo ./target/release/rpi-led-webpush                          # 64x32，网页 :8080
sudo ./target/release/rpi-led-webpush --rows 64 --cols 64      # 换面板
sudo ./target/release/rpi-led-webpush --chain 2               # 串联两块 → 逻辑 128x32
sudo ./target/release/rpi-led-webpush --web-port 0            # 关闭网页服务
sudo ./target/release/rpi-led-webpush --https --https-port 8443
./target/release/rpi-led-webpush --help                        # 全部参数

# 测试
cargo test

# 打 .deb（本地或 CI）
deploy/build-deb.sh [输出目录]
```

Windows 开发机无法 `cargo build`，替代验证方法见 §9。

---

## 4. 代码地图

| 文件 | 职责 | 注意 |
|---|---|---|
| `src/main.rs` | CLI 解析、面板初始化（FFI options）、主循环（推流帧 → 面板；空闲时钟/呼吸/黑屏）、TLS 前端生命周期、信号处理、多驱动并发检测 `guard()` | 改硬件参数要同步 `ffi.rs` 结构体字段顺序（照抄 C 头文件） |
| `src/web.rs` | 手写 HTTP/WebSocket/SHA-1/base64；三个内嵌页面；路由与 API；`Mirror`（/view 数据源）、`Stream`（推流帧）、`RuntimeCtl`（热更新控制 + HTTPS 运行状态） | 页面是 raw string；`data-en` 做中英切换；表单 id 必须与 JS 的 `FIELDS`/`CHECKS` 对应 |
| `src/config.rs` | `key=value` 配置：默认值、解析、`normalize()`、原子写盘、`restart_eq()` 判定热改/重启、`SharedConfig` | 新增配置项要改**多处**（见 §5、§13） |
| `src/clock.rs` | 空闲时钟：5×7 ASCII + 8×8 中文星期字模、绘制、blit | 字模规范与结构测试见 §7 |
| `src/tls.rs` | 内置 HTTPS：参数校验/路径解析、socat/openssl 命令行、证书自动生成、socat 子进程管理 | 程序本身**不做 TLS**（零依赖约束） |
| `src/ffi.rs` | librgbmatrix + libc（time/signal）手写 FFI | 结构体字段顺序必须与 `third_party/.../include/led-matrix-c.h` 一致 |
| `build.rs` | 编译 vendored C/C++ → `librgbmatrix.a` | 依赖系统 `g++`/`gcc`/`ar`；源文件变更才重编 |
| `deploy/build-deb.sh` | 生成 .deb（systemd unit、conffile、postinst） | 版本号取自 `Cargo.toml` |
| `.github/workflows/deb.yml` | CI：push main/deb-packaging 或 tag `v*` → arm64 deb；tag 时创建 Release | tag 触发发布 |
| `contrib/` | systemd unit、外置 socat TLS unit、配置示例 | 配置示例要跟随新配置项更新 |

---

## 5. 配置系统（`src/config.rs`）

**路径解析**：`LED_CONFIG` 环境变量 → `/etc/rpi-led-webpush/config`（存在时）→
`./rpi-led-webpush.conf`（相对路径会转成绝对路径）。

**优先级**：CLI 参数 > 配置文件 > 内置默认值。CLI 只覆盖当次运行，**不写盘**；
网页 `/api/config` POST 会把当前内存配置写盘（原子写：同目录 `.tmp` + rename）。

**生效方式**：

- **热更新**（无需重启）：`brightness`、`idle`、`show_clock`、`clock_24h`、`lang`。
- **需重启**：其余全部——几何（`rows`/`cols`/`chain`/`parallel`）、接线
  （`mapping`/`rgb_sequence`）、`web_port`、全部 `https_*`、全部硬件驱动参数。
  判定逻辑在 `Config::restart_eq()`；`SharedConfig::update()` 返回
  `Applied` / `NeedsRestart` / `SaveFailed`。

**全部配置键**（默认值 / 生效方式 / 设置页分区）：

| key | 默认 | 生效 | 设置页分区 | 说明 |
|---|---|---|---|---|
| `rows` | 32 | 重启 | 面板 | 面板高度 / 1 块 |
| `cols` | 64 | 重启 | 面板 | 面板宽度 / 1 块 |
| `chain` | 1 | 重启 | 面板 | 左右串联块数 |
| `parallel` | 1 | 重启 | 面板 | 上下并联块数 |
| `brightness` | 60 | **热** | 面板 | 1..100，滑杆即时生效 |
| `idle` | 1 | **热** | 面板 | 空闲呼吸 |
| `show_clock` | 1 | **热** | 面板 | 无投送时显示时钟 |
| `clock_24h` | 1 | **热** | 面板 | 24 小时制 |
| `lang` | zh | **热** | 面板 | 面板星期 + 网页界面语言（`zh`/`en`） |
| `mapping` | regular | 重启 | 硬件 | GPIO 映射：`regular`/`adafruit-hat`/`adafruit-hat-pwm`/`classic`/`compute-module`/`regular-pi1` |
| `rgb_sequence` | RGB | 重启 | 硬件 | 颜色通道顺序，如 `RBG` |
| `web_port` | 8080 | 重启 | **网络 / HTTPS** | `0` = 关闭网页服务 |
| `https` | 0 | 重启 | 网络 / HTTPS | 启用内置 HTTPS 前端 |
| `https_port` | 8443 | 重启 | 网络 / HTTPS | HTTPS 监听端口 |
| `https_cert` | 空 | 重启 | 网络 / HTTPS | 证书 PEM 路径；空 = `<配置目录>/tls/cert.pem` |
| `https_key` | 空 | 重启 | 网络 / HTTPS | 私钥 PEM 路径；空 = `<配置目录>/tls/key.pem` |
| `panel_type` | 空 | 重启 | 驱动芯片 / 初始化 | `FM6126A` / `FM6127`；空 = 通用 |
| `gpio_slowdown` | 1 | 重启 | 驱动芯片 / 初始化 | 0..4 |
| `pwm_bits` | 11 | 重启 | 驱动芯片 / 初始化 | 1..11 |
| `pwm_lsb_ns` | 0 | 重启 | 驱动芯片 / 初始化 | 0..200 |
| `pwm_dither` | 0 | 重启 | 驱动芯片 / 初始化 | 0..2 |
| `scan_mode` | 0 | 重启 | 驱动芯片 / 初始化 | 0=progressive / 1=interlaced |
| `row_address_type` | 0 | 重启 | 驱动芯片 / 初始化 | 0..4 |
| `multiplexing` | 0 | 重启 | 驱动芯片 / 初始化 | 0=直驱 / 2=1:8 checker |
| `no_hardware_pulse` | 0 | 重启 | 驱动芯片 / 初始化 | OE 不在 GPIO18 时 |
| `inverse_colors` | 0 | 重启 | 驱动芯片 / 初始化 | 反色面板 |
| `pixel_mapper` | 空 | 重启 | 驱动芯片 / 初始化 | 如 `Rotate:90` |
| `limit_refresh_hz` | 0 | 重启 | 驱动芯片 / 初始化 | 0 = 不限 |
| `no_busy_waiting` | 0 | 重启 | 驱动芯片 / 初始化 | 限刷时 sleep |
| `rp1_pio` | 1 | 重启 | 驱动芯片 / 初始化 | Pi 5：1=PIO（省 CPU）/ 0=RIO |

**normalize 规则**（`Config::normalize`）：几何/亮度/PWM 等 clamp；`mapping`/
`rgb_sequence` 空值回落默认；`lang` 非 `en` 一律 `zh`；`https_port == 0` 回落 8443；
`https_cert`/`https_key` 去首尾空白；`panel_type` 归一化成库认识的大小写。
新增键时**必须同步**：结构体字段、`Default`、`parse_line`、`normalize`、
`write_config`、`restart_eq`、`config_json`、`api_config_set`、设置页、README、
`contrib/rpi-led-webpush.conf.example`。

---

## 6. 网页与 HTTP/WS（`src/web.rs`）

### 6.1 页面 / 路由分类

| 路由 | 方法 | 页面/功能 |
|---|---|---|
| `/` | GET | **投送页**（内嵌）：视频文件 / 摄像头 / 屏幕共享、适应方式、亮度滑杆 |
| `/view` | GET | **预览页**（内嵌）：通过 WebSocket 显示面板当前画面 |
| `/settings` | GET | **设置页**（内嵌）：设备设置（分区见 §6.2） |
| `/geo` | GET | 逻辑分辨率，纯文本 `64 32` |
| `/api/config` | GET / POST | 设置 + 运行时信息（JSON）/ 修改设置（表单编码，自动存盘） |
| `/api/brightness` | GET / POST | 读取亮度 / 设置亮度（`?brightness=1..100`，热更新并落盘） |
| `/api/restart` | POST | 干净退出，交给 supervisor 重启（改几何/HTTPS 后需要） |
| `/push` | WS | 浏览器 → 面板：`[宽 u16 LE][高 u16 LE][RGB...]` |
| `/ws` | WS | 面板 → 浏览器预览：`[宽 u16 LE][高 u16 LE][seq u32 LE][RGB...]` |

**网页没有任何鉴权**，只能在可信网络使用。浏览器只在安全上下文（HTTPS 或 localhost）
开放摄像头/屏幕采集；文件投送普通 HTTP 即可。

### 6.2 设置页功能分类（规范，★ 重要）

设置页固定 **5 个粗分区**，顺序与内容如下。**新增设置项必须归入对应分区，
不要新增细分区（如「显示与空闲」「接线」等）**，除非用户明确要求拆分。

| # | 分区（中 / data-en） | 包含 | 生效 |
|---|---|---|---|
| 1 | **面板 / Panel** | `rows` `cols` `chain` `parallel`、`brightness`、`idle`、`show_clock`、`clock_24h`、`lang` | 混合：几何需重启，亮度/空闲/时钟/语言热更新 |
| 2 | **硬件 / Hardware** | GPIO 映射 `mapping`、颜色顺序 `rgb_sequence` | 需重启 |
| 3 | **网络 / HTTPS / Network / HTTPS** | `web_port`、`https`、`https_port`、`https_cert`、`https_key` | 需重启 |
| 4 | **驱动芯片 / 初始化 / Driver / init** | `panel_type` `gpio_slowdown` `pwm_bits` `pwm_lsb_ns` `pwm_dither` `scan_mode` `row_address_type` `multiplexing` `limit_refresh_hz` `rp1_pio` `pixel_mapper` `no_hardware_pulse` `inverse_colors` `no_busy_waiting` | 需重启 |
| 5 | **状态 / Status** | 只读：面板逻辑分辨率、推流状态、当前亮度、HTTPS 运行状态/端口/错误、配置文件路径 | — |

归类口径：

- 「面板」= 面板几何 + 显示亮度 + 空闲显示行为 + 语言（业务含义是「面板显示相关」）；
- 「硬件」只放**接线**相关（GPIO 映射、颜色顺序）；
- **`web_port` 属于「网络 / HTTPS」，不要放回「硬件」**；
- 「驱动芯片 / 初始化」= 传给 rpi-rgb-led-matrix 的初始化参数；
- 「状态」只读；页面底部是 保存 / 重新读取 / 重启设备 三个按钮 + 状态提示。

设置页实现约定：

- `SETTINGS_HTML` 里的表单控件 id 必须同时出现在 JS 的 `FIELDS`（文本/数字）或
  `CHECKS`（勾选框）数组里，否则保存/回填不会生效；checkbox 默认就是 false。
- 保存走 `POST /api/config`（`application/x-www-form-urlencoded`）；返回
  `restart_required:true` 时页面提示「硬件/网络参数已改，需重启设备」。
- 亮度滑杆特殊：拖动走 `POST /api/brightness?brightness=N` 节流热更新，松手再存一次。
- 中英切换：元素写 `data-en="English"`，Server 端把 `__LANG__` 替换成 `zh`/`en`；
  页面加载后 JS 替换 `data-en` 文本（不写 data-en 的保持中文）。
- 页面样式统一暗色；canvas 预览用 `image-rendering: pixelated`。
- 运行时状态要显示到「状态」区：在 `RuntimeCtl` 加字段 → 写进 `config_json()` 的
  `runtime` 对象 → 设置页 JS 的 `load()` 里渲染。

---

## 7. 时钟与 LED 显示（`src/clock.rs`）

布局（默认 64×32）：上半 `HH:MM` 两倍缩放，下半日期 `MM-DD` + 8×8 星期汉字；
背景 `(0,0,2)`、时间 `(200,200,200)`、日期/星期 `(90,130,180)`；日期行 `dy = h-9`。

**8×8 星期字模硬性规范**：

- 每行 1 字节，**bit7 = 最左**、row0 = 最上，1 = 点亮；
- **第 8 行（row7）留空**，与 5×7 日期数字同基线（结构测试会检查）；
- `三`：中横最短、底横最长；`四`：外框 + 「儿」两笔分开（不能糊成实心中柱）；
  `五`：中横下是「竖 + 横折」两笔（不能画成「王」）；`六`：点 + 通栏横 + 八字脚；
- 改动字模必须保留/更新 `src/clock.rs` 的单测：`weekday_glyphs_have_correct_strokes`、
  `weekday_glyphs_share_baseline`、`every_weekday_renders_distinctly`；
- 画字模用仓库外的像素编辑器：`C:\Users\26959\文档\led-glyph-editor\index.html`
  （单文件网页，64×32 预览与 `draw_clock` 逐字节一致；导出 JSON / Rust 代码）。

**已知限制（未修，改之前先看这里）**：

- 12 小时制时间串 `" 8:36 PM"` 在 2 倍缩放时是 96 px > 64 px，64 宽面板会裁掉
  “PM”/首字符（只影响上半时间行，日期/星期行正常）。
- 逻辑宽度 < 46 px（例如 32×32 面板）时，星期字模的 x 坐标超出画布，会被整块裁掉；
  这是布局问题不是字模问题。
- 5×7 ASCII 是经典点阵字体，数字看起来略斜属正常。

**预览/镜像约定**：主循环所有空闲分支（时钟/呼吸/黑屏）都必须更新 `mirror`
（`m.rgb`、`m.w/h`、`m.seq += 1`、`m.page`），否则 `/view` 会停在旧帧。
`clock::blit()` 用 `led_canvas_set_pixels(canvas, 0, 0, lw, lh, buf)`，行主序 RGB。

---

## 8. 内置 HTTPS（`src/tls.rs`）

**为什么不用 rustls**：零依赖约束。程序用系统 `socat` 做 TLS 终止，把
`https://<device>:<https_port>` 反代到本机 `127.0.0.1:<web_port>` 的内嵌 HTTP 服务。

启动流程（`https=1` 时）：

1. `tls::plan()` 校验：`web_port > 0`、HTTPS 端口 ≠ web_port、解析证书/私钥路径
   （空 = `<配置目录>/tls/cert.pem|key.pem`；相对路径按配置目录解析）。
2. 证书不存在 → 用 `openssl req -x509 -newkey rsa:2048 -nodes -days 3650` 生成自签名
   证书（CN=rpi-led-webpush，私钥 chmod 600）。
3. spawn `socat OPENSSL-LISTEN:<port>,cert=…,key=…,verify=0,reuseaddr,fork TCP:127.0.0.1:<web_port>`。
4. 等 250 ms 用 `try_wait()` 确认 socat 没立即退出（端口占用/证书读不了会报错）。
5. 运行期间：`TlsFrontEnd` 在 Drop 里 kill + wait；子进程另设 `PR_SET_PDEATHSIG`，
   主进程崩溃/被 kill 时 socat 一起退出，避免孤儿进程占端口。
6. 启动失败只影响 HTTPS：打印原因、写入 `RuntimeCtl.https_note`，HTTP 服务继续跑。

配置/CLI：`https`、`https_port`、`https_cert`、`https_key`；
`--https`、`--no-https`、`--https-port`、`--https-cert`、`--https-key`。修改需重启。

依赖：`socat` + `openssl`（deb 里是 `Recommends`，apt 默认会装；
`dpkg -i` 需要自己 `apt install socat openssl`）。HTTPS 端口可以设 443（程序本来就是 root）。

已知取舍：启用 HTTPS 后普通 HTTP **仍然监听 0.0.0.0:web_port**（方便旧书签；
未强制收窄到 loopback）。外置 systemd 方案 `contrib/rpi-led-webpush-tls.service` 仍有效，
但**不要和内置 HTTPS 同时启用**（会抢端口/出现两个 TLS 前端）。

---

## 9. 构建、测试与 Windows 开发机注意事项

- **Linux/Pi**：`cargo build --release` 即可；`build.rs` 自动编 vendored 驱动。
- **测试**：`cargo test`（16 个）。`web.rs` 的测试会在 `127.0.0.1:0` 起真实 HTTP 服务，
  需要允许监听本地 TCP；`config.rs`/`web.rs` 测试会写系统临时目录。
- **Windows 开发机无法 `cargo build`**（`build.rs` 需要 `g++` 编 C++，且程序依赖 Linux
  的 `/dev/mem`、`/dev/pio0`、`prctl` 等）。可在 Windows 上做的验证：
  1. **整 crate 类型检查**（不链接）：`rustc --edition 2024 --crate-type bin --emit=metadata src/main.rs -o %TEMP%\m.rmeta`，exit 0 即通过；
  2. **模块单测**：把 `config.rs` / `tls.rs` / `clock.rs` 复制到临时目录，写
     `#[path="…"] mod …;` harness 后 `rustc --edition 2024 --test harness.rs`；
     `clock.rs` 需要 stub `ffi` 模块（`Color`、`LedCanvas`、`led_canvas_set_pixels`、
     `local_time<T>() -> Option<T>`）；
  3. **内嵌页面**：从 `web.rs` 提取 `SETTINGS_HTML` / `INDEX_HTML` / `SENDER_HTML`
     的 raw string 写临时 `.html`，用 `node --check` 校验其中 `<script>`，可用
     headless Chrome (`--headless=new --screenshot`) 截图看布局。
- **行尾**：仓库 `core.autocrlf=true`，工作区文件是 **CRLF**；新建文件也要写成 CRLF，
  否则 `git add` 会报 `LF will be replaced by CRLF` 警告。
- **rustfmt 未强制**：`clock.rs` 含 `#[rustfmt::skip]` 字模表，文件整体也不是 rustfmt
  干净状态；**不要整文件跑 rustfmt** 制造无关 diff。
- 面板硬件无法在非 Pi 上跑，不要在 Windows 上启动二进制。
- 修改 `src/ffi.rs` 的 C 结构体时，必须对照
  `third_party/rpi-rgb-led-matrix/include/led-matrix-c.h`（字段顺序错了会内存错乱）。

---

## 10. 硬件与运行约束

- **同一时间只能有一个程序驱动面板**：RP1 PIO 只有 4 个状态机，并发驱动会互相破坏。
  `guard()` 扫描 `/proc/*/cmdline` 检测疑似驱动进程并拒绝启动；`LED_FORCE=1` 可强制运行。
- 逻辑分辨率 = `cols × chain`、`rows × parallel`；页面从 `/geo` 读取，自适应。
- Pi 5 后端：`rp1_pio=1`（PIO，省 CPU）/ `0`（RIO）；`gpio_slowdown` 等调参见 README 表。
- 亮度热更新：`RuntimeCtl.brightness` → 主循环调 `led_matrix_set_brightness`，不重开面板。
- 信号：`SIGINT`/`SIGTERM` 置原子 STOP → 主循环退出 → 清屏、删矩阵、停 TLS 前端。
- `POST /api/restart` 只是干净退出，依赖 systemd `Restart=always` 拉起（unit 已配置）。

---

## 11. 打包与发布

- **deb**：`deploy/build-deb.sh`。包名 `rpi-led-webpush`，架构取 `dpkg --print-architecture`
  （CI 构建 arm64），装到 `/usr/bin`，配置 `/etc/rpi-led-webpush/config`（conffile），
  systemd unit 装到 `/lib/systemd/system`；`Depends: libc6, libstdc++6`，
  `Recommends: socat, openssl`。postinst 会提示非交互升级命令
  （`dpkg -i --force-confold` 保留配置）。
- **CI**（`.github/workflows/deb.yml`）：push `main`/`deb-packaging` 或 tag `v*` 触发；
  arm64 runner 构建 deb 上传 artifact；tag 构建成功后创建 GitHub Release
  （仓库是 `kxgx/rpi-led-webpush` 才执行 release job）；仅文档变更会跳过构建。
  版本唯一来源是 `Cargo.toml`（工作流不写死版本）：checks job 在 tag 构建时校验
  `vX.Y.Z` 与 `Cargo.toml` 版本一致，deb-arm64 job 构建后校验产物文件名包含该版本；
  发版时忘记改 `Cargo.toml` 会让 CI 直接失败，而不是静默打出错版本的 deb。
- **CI 工具版本**：action 用**浮动 major 标签**（当前 `actions/checkout@v7`、
  `actions/upload-artifact@v7`、`actions/download-artifact@v8`；
  `Swatinem/rust-cache@v2`、`softprops/action-gh-release@v3` 同样自动跟最新 2.x/3.x；
  `dtolnay/rust-toolchain@stable` 每次取最新 stable Rust）。升级 action major 前先看
  对应 release notes 的 breaking changes，并同步更新本节；runner 保持 LTS
  （`ubuntu-24.04` 的 x64 / arm64 版本，暂不追新）。
- **发布流程**：
  1. 改 `Cargo.toml` 版本号为 `x.y.z`；
  2. commit `release: vX.Y.Z`（body 简述本版改动）；
  3. annotated tag：`git tag -a vX.Y.Z -m "rpi-led-webpush vX.Y.Z" -m "- 中文要点…"`；
  4. `git push origin main` + `git push origin vX.Y.Z`。
- `Cargo.lock` 里的包版本从 0.1.0 起一直没随 release 更新（历史如此；deb 版本取
  `Cargo.toml`）。如需同步可顺手改，但别当成 bug。
- commit 风格：conventional commits（`feat:` / `fix:` / `docs:` / `ci:` / `release:`…），
  说明用中文/英文均可。

---

## 12. 安全与注意事项

- 网页/API/WS **完全没有鉴权**：只部署在可信内网；对公网暴露需自行加反代+鉴权。
- HTTPS 是**自签名证书**：浏览器首次访问需手动信任一次；摄像头/屏幕共享必须走 HTTPS
  （安全上下文要求）。
- 后台保活：桌面浏览器用 Wake Lock + Worker 定时器 + 静音音频三层保活；手机浏览器挂起
  后台页面是平台限制，网页无法绕过。
- 修改网络/HTTPS/几何/驱动参数后必须重启进程（设置页有提示和「重启设备」按钮）。
- 加依赖前先想清楚：零 crate 依赖是项目卖点；优先系统工具或手写实现。
- 改动涉及端口/协议/配置键时，同步更新 README.md、README.zh-CN.md、本文件、
  `contrib/rpi-led-webpush.conf.example`。

---

## 13. 常见任务 playbook

**加一个配置项**：
1. `src/config.rs`：结构体字段、`Default`、`parse_line`、`normalize`、`write_config`；
   需要重启则加进 `restart_eq()`；
2. `src/web.rs`：`config_json()` 的 `settings` 对象（+ 如要显示运行时状态则 `RuntimeCtl`
   与 `runtime` 对象）、`api_config_set()` 的匹配分支；
3. `src/main.rs`：CLI 参数（如需）+ 启动时把值 store 进 `RuntimeCtl`（热更新项）；
4. `SETTINGS_HTML`：按 §6.2 归入正确分区，控件 id 加进 JS `FIELDS`/`CHECKS`；
5. `README.md` / `README.zh-CN.md` / `contrib/rpi-led-webpush.conf.example`；
6. 补测试（config 解析/落盘、web 的 POST 行为）。

**加一条 HTTP 路由 / 新页面**：`handle_client()` 的 `match path_only` 加分枝；
页面做成 `const XXX_HTML: &str = r#"…"#;` 并用 `html_lang()` 替换 `__LANG__`；
如需中英切换用 `data-en`；不要引入外部静态资源。

**改时钟字模**：见 §7；用外部像素编辑器画 → 替换 `cjk_weekday()` → `cargo test` →
渲染 7 天画面确认。

**调整硬件初始化参数**：`src/main.rs` 里 `RGBLedMatrixOptions` 字段映射 + `config.rs`
配置项 + 设置页「驱动芯片 / 初始化」分区 + README 硬件表；注意 `ffi.rs` 字段顺序。

**加运行时状态显示**：`RuntimeCtl` 加字段 → `config_json()` 的 `runtime` 输出 →
设置页 `load()` 渲染到「状态」区。

---

## 14. 术语

- **HUB75**：常见 RGB LED 点阵屏接口；一块面板 64×32 是默认规格。
- **逻辑分辨率**：`cols×chain` 与 `rows×parallel` 得到的 Canvas 尺寸，页面据此缩放。
- **PIO / RIO**：Pi 5 的 RP1 两种 GPIO 驱动后端（PIO 省 CPU）。
- **热更新 / 需重启**：见 §5；设置页保存后是否要 `POST /api/restart`。
- **mirror / stream**：`Mirror` 是给 `/view` 的当前画面快照；`Stream` 是浏览器推来的帧。
