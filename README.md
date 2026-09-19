# rpi-led-webpush — push video from a browser to a HUB75 LED matrix

Open a web page on your phone or PC, pick a **video file**, your **camera**, or a **screen share**,
and it plays on a HUB75 RGB LED matrix panel in real time.

The heavy lifting happens **in the browser**: it decodes and scales the source to the panel's
logical resolution and sends only tiny frames (6 KB each for 64×32) over a WebSocket, so the
device driving the panel stays almost idle.

* **Zero third-party dependencies** — `cargo tree` shows only this package. The HTTP server,
  WebSocket framing, SHA-1 and base64 are implemented in `src/web.rs`; the panel is driven through
  hand-written FFI declarations against
  [rpi-rgb-led-matrix](https://github.com/hzeller/rpi-rgb-led-matrix)'s C API. Nothing comes from
  crates.io, so `cargo build --offline` works and there is no supply-chain surface.
* **Self-contained front-end** — the sender and preview pages are embedded in the binary.
* **Adapts to the panel** — the page reads the logical resolution from the device (`/geo`), so
  chained and parallel panels are handled without touching the front-end.

## Routes

| Route | Method | Purpose |
|---|---|---|
| `/` | GET | sender page (embedded): video file / camera / screen share, fit modes |
| `/view` | GET | live preview of what the panel is displaying right now |
| `/geo` | GET | logical resolution, e.g. `64 32` |
| `/push` | WebSocket | sender → device: binary frames `[w u16][h u16][RGB…]` (little-endian) |
| `/ws` | WebSocket | device → browser: the same frame format, for the preview |

## Behaviour worth knowing

* **Frame rate follows the source.** Sending is driven by `requestVideoFrameCallback`, so a 60 fps
  video is sent at 60 fps and a 10 fps one at 10 fps — no fixed throttle.
* **Back-pressure.** If the link cannot keep up, frames are dropped instead of queued, so a slow
  network degrades gracefully instead of building up latency. The page shows `src → sent` rates and
  a dropped-frame counter.
* **Runs in the background (desktop).** A screen Wake Lock is requested to prevent the display from
  sleeping, and two fallbacks (a silent `AudioContext` ticker and a timer) keep frames flowing when
  the browser deprioritises a minimised tab. Mobile browsers suspend background pages entirely —
  that is a platform limit, not something a web page can work around.
* **Idle state.** While nothing is being pushed, the panel shows a slow dim-blue breathing pattern
  (disable with `--no-idle`).

## Requirements

* A Raspberry Pi with a HUB75 panel (on Pi 5 the program selects the RP1 PIO backend itself)
* [rpi-rgb-led-matrix](https://github.com/hzeller/rpi-rgb-led-matrix), built (`make -C lib`)
* Rust toolchain (edition 2024)

## Build

```bash
# point at your rpi-rgb-led-matrix checkout / install prefix (default: /usr/local)
RGB_MATRIX_DIR=/path/to/rpi-rgb-led-matrix cargo build --release --offline
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
| `--no-idle` | — | keep the panel dark while idle |

## HTTPS (needed for camera and screen capture)

Browsers only expose the camera and screen-capture APIs in a **secure context**. Opening the page
over plain HTTP still works for **video files**. To use the camera or screen sharing, put a TLS
front-end in front of it — `contrib/rpi-led-webpush-tls.service` shows the two-line `socat` setup with a
self-signed certificate; accept the certificate warning once in the browser.

## Notes

* Only **one process may drive the panel at a time** (the RP1 PIO block has four state machines and
  concurrent drivers corrupt each other). The program detects likely competitors and refuses to
  start; set `LED_FORCE=1` to override.
* The web UI has **no authentication** — anyone who can reach the port can push content. Use it on
  a trusted network only.

## Licence

GPL-2.0-or-later — it links against rpi-rgb-led-matrix, which is GPL-2.0.
