# rpi-led-webpush — push video from a browser to a HUB75 LED matrix

[中文说明](README.zh-CN.md) · Open a web page on your phone or PC, pick a **video file**, your **camera**, or a **screen share**,
and it plays on a HUB75 RGB LED matrix panel in real time.

The heavy lifting happens **in the browser**: it decodes and scales the source to the panel's
logical resolution and sends only tiny frames (6 KB each for 64×32) over a WebSocket, so the
device driving the panel stays almost idle.

* **Zero third-party dependencies** — `cargo tree` shows only this package. The HTTP server,
  WebSocket framing, SHA-1 and base64 are implemented in `src/web.rs`. The HUB75 driver
  ([rpi-rgb-led-matrix](https://github.com/hzeller/rpi-rgb-led-matrix), GPL-2.0) is vendored
  under `third_party/` and compiled by `build.rs`, so a plain `cargo build` is enough —
  no `RGB_MATRIX_DIR`, no preinstalled `librgbmatrix`.
* **Self-contained front-end** — the sender, preview and settings pages are embedded in the binary.
* **Adapts to the panel** — the page reads the logical resolution from the device (`/geo`), so
  chained and parallel panels are handled without touching the front-end.

## Routes

| Route | Method | Purpose |
|---|---|---|
| `/` | GET | sender page (embedded): video file / camera / screen share, fit modes |
| `/view` | GET | live preview of what the panel is displaying right now |
| `/settings` | GET | device settings page (panel size, brightness, driver, language, …) |
| `/geo` | GET | logical resolution, e.g. `64 32` |
| `/api/config` | GET | current settings + runtime info (JSON) |
| `/api/config` | POST | update settings (`application/x-www-form-urlencoded`); persisted |
| `/api/brightness` | GET | current brightness |
| `/api/brightness` | POST | set brightness (`?brightness=1..100`); applied immediately |
| `/api/restart` | POST | stop cleanly so the supervisor can restart (new geometry etc.) |
| `/push` | WebSocket | sender → device: binary frames `[w u16][h u16][RGB…]` (little-endian) |
| `/ws` | WebSocket | device → browser: same RGB payload plus a `u32` sequence number, for the preview |

## Settings & persistence

Settings live in a plain `key=value` file (no format library):

* `LED_CONFIG` if set, else `/etc/rpi-led-webpush/config`, else `./rpi-led-webpush.conf`
* CLI flags override the file **for that run only**; the web UI edits the file.
* Saving from the web UI is **incremental**: only the keys that actually changed are
  rewritten — comments, unknown keys and key order in your file are preserved.
* **Hot-applied** (no restart): `brightness`, `idle`, `show_clock`, `clock_24h`, `lang`.
* **Need restart** (`POST /api/restart`): geometry (`rows`/`cols`/`chain`/`parallel`),
  wiring (`mapping`/`rgb_sequence`), `web_port`, the HTTPS settings, and all **hardware driver**
  options below.

While nothing is streaming the panel shows a **clock + date** by default (`show_clock=1`,
`clock_24h`, `lang=zh|en` weekday). Turn the clock off to fall back to the dim-blue idle
breathing (`idle=1`) or a dark panel (`idle=0`).

### Hardware driver options

These map onto `rpi-rgb-led-matrix`'s init parameters (same as `--led-*` flags) and take effect
at panel bring-up:

| Key | CLI | Notes |
|---|---|---|
| `panel_type` | `--panel-type` / `--driver` | `FM6126A` / `FM6127` (empty = generic) |
| `gpio_slowdown` | `--gpio-slowdown` | 0..4 |
| `pwm_bits` | `--pwm-bits` | 1..11 |
| `pwm_lsb_ns` | `--pwm-lsb-ns` | nanoseconds |
| `pwm_dither` | `--pwm-dither` | 0..2 |
| `scan_mode` | `--scan-mode` | 0 progressive / 1 interlaced |
| `row_address_type` | `--row-addr-type` | 0..4 |
| `multiplexing` | `--multiplexing` | 0 direct, 2 = 1:8 checker |
| `no_hardware_pulse` | `--no-hardware-pulse` | OE not on GPIO 18 |
| `inverse_colors` | `--inverse` | |
| `pixel_mapper` | `--pixel-mapper` | e.g. `Rotate:90` |
| `limit_refresh_hz` | `--limit-refresh` | 0 = unlimited |
| `no_busy_waiting` | `--no-busy-waiting` | |
| `rp1_pio` | `--rp1-rio` to disable | Pi 5: 1=PIO (default), 0=RIO |

When running under systemd, prefer `ExecStart` without CLI flags (see `contrib/`) so the
web-saved config is what restarts with.

## Behaviour worth knowing

* **Frame rate follows the source.** Sending is driven by `requestVideoFrameCallback`, so a 60 fps
  video is sent at 60 fps and a 10 fps one at 10 fps — no fixed throttle.
* **Back-pressure.** If the link cannot keep up, frames are dropped instead of queued, so a slow
  network degrades gracefully instead of building up latency. The page shows `src → sent` rates and
  a dropped-frame counter.
* **Runs in the background (desktop).** A screen Wake Lock keeps the display awake, and three
  keep-alives (Worker timer, silent AudioContext ticker, and a main-thread timer) keep sending
  when the tab is throttled (capped at ~20 fps). Mobile browsers suspend background pages
  entirely — that is a platform limit, not something a web page can work around.
* **Idle state.** Clock + date by default; optional dim-blue breathing or black.

## Requirements

* A Raspberry Pi with a HUB75 panel (on Pi 5 the program selects the RP1 PIO backend itself)
* A C/C++ toolchain (`g++`/`gcc`/`ar`) — used by `build.rs` to compile the vendored driver
* Rust toolchain (edition 2024)

## Build

```bash
cargo build --release
# or produce a .deb
./deploy/build-deb.sh dist
```

## Run

```bash
sudo ./rpi-led-webpush                                  # 64x32, RGB order, web UI on :8080
sudo ./rpi-led-webpush --rows 64 --cols 64              # different panel
sudo ./rpi-led-webpush --chain 2                        # two panels daisy-chained (128x32 logical)
sudo ./rpi-led-webpush --rgb-sequence RBG               # if your panel has green/blue swapped
sudo ./rpi-led-webpush --mapping adafruit-hat-pwm       # GPIO mapping of a common adapter board
sudo ./rpi-led-webpush --web-port 0                     # no web UI (panel stays idle)
```

Run it as root: it needs `/dev/mem` and `/dev/pio0`.

| Option | Default | Description |
|---|---|---|
| `--rows` / `--cols` | 32 / 64 | panel size |
| `--chain` / `--parallel` | 1 / 1 | daisy-chained / parallel panels |
| `--brightness` | 60 | 1..100 |
| `--mapping` | `regular` | GPIO mapping (`regular`, `adafruit-hat`, `adafruit-hat-pwm`, `classic`, …) |
| `--rgb-sequence` | `RGB` | channel order of your panel |
| `--web-port` | 8080 | `0` disables the web UI |
| `--web-host` | auto | bind address; empty = `127.0.0.1` when HTTPS is on, else `0.0.0.0` |
| `--https` / `--no-https` | off | built-in HTTPS front-end (socat TLS termination) |
| `--https-port` | 8443 | HTTPS listen port (`web_port` stays the internal HTTP port) |
| `--https-cert` / `--https-key` | auto | PEM cert/key paths; empty = `<config dir>/tls/*` (self-signed, generated on first use) |
| `--no-idle` | — | keep the panel dark while idle (no breathing) |
| `--no-clock` | — | do not show clock/date while idle |
| `--clock-12h` | — | 12-hour clock instead of 24-hour |

## HTTPS (needed for camera and screen capture)

Browsers only expose the camera and screen-capture APIs in a **secure context**. Opening the page
over plain HTTP still works for **video files**.

Enable the built-in TLS front-end in **Settings → HTTPS** (or `https=1`): the program runs the
system `socat` as a TLS terminator in front of its local HTTP port, and generates a self-signed
certificate with `openssl` into `<config dir>/tls/` on first use. Then open
`https://<device>:8443/` and trust the certificate once per browser.

* requires `socat` + `openssl` — `sudo apt install socat openssl` (the `.deb` recommends both)
* `https_port` defaults to 8443 (443 works too); `web_port` remains the internal HTTP port
* with HTTPS on, the internal HTTP server binds `127.0.0.1` by default and is only reachable
  through socat. Set `web_host=0.0.0.0` only if you really need plain-HTTP access from your LAN
* `https_cert` / `https_key` may only point inside `<config dir>/tls/` (prevents the web API from
  writing certificates over arbitrary files); leave them empty to use the auto-generated cert
* changing any HTTPS setting needs a restart (`POST /api/restart` or the settings button)
* if you prefer a systemd-managed TLS front-end on 443 with your own certificate, the external
  `contrib/rpi-led-webpush-tls.service` setup still works

## Notes

* Only **one process may drive the panel at a time** (the RP1 PIO block has four state machines and
  concurrent drivers corrupt each other). The program detects likely competitors and refuses to
  start; set `LED_FORCE=1` to override.
* The web UI has **no authentication** — anyone who can reach the port can push content. Use it on
  a trusted network only. Built-in hardening: state-changing requests require `POST` plus the
  `X-Led-Webpush: 1` header (CSRF), WebSocket upgrades are rejected when the `Origin` host differs
  from `Host`, request/header/connection sizes are capped, and the cert/key paths are confined to
  `<config dir>/tls/`.

## Licence

GPL-2.0-or-later — it links against rpi-rgb-led-matrix, which is GPL-2.0.
