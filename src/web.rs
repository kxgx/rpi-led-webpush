//! 内嵌网页服务器：HTTP + WebSocket + 前端页面全部编进二进制，无外部文件。
//!
//! 设计参考 https://github.com/Xelckis/qr-server 的「自包含」思路——一个二进制搞定，
//! 不依赖任何第三方 crate（HTTP、WebSocket、SHA-1、base64 都是手写的）。
//!
//! - `GET /`   → 内嵌的响应式页面（canvas 按窗口自适应缩放，像素风格放大）
//! - `GET /ws` → WebSocket，持续推送面板当前画面（二进制帧）
//!
//! 帧格式（小端）：[宽 u16][高 u16][序号 u32][RGB 数据 w*h*3]
//! 页面据此设置 canvas 尺寸，所以链屏/并联屏（屏幕组合）会自动适配。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

pub struct Mirror {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
    pub seq: u64,
    /// 当前显示的页面名（如 clock / address），供网页显示
    pub page: String,
}

pub type SharedMirror = Arc<Mutex<Mirror>>;

pub fn new_mirror(w: usize, h: usize) -> SharedMirror {
    Arc::new(Mutex::new(Mirror {
        w,
        h,
        rgb: vec![0; w * h * 3],
        seq: 0,
        page: String::new(),
    }))
}

/// 启动网页服务（在后台线程里 accept）
pub fn spawn_web_server(port: u16, mirror: SharedMirror, stream: SharedStream,
                        geometry: (usize, usize), stop: Arc<AtomicBool>) {
    thread::spawn(move || {
        let listener = match TcpListener::bind(("0.0.0.0", port)) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("网页服务无法监听 0.0.0.0:{port} —— {e}");
                return;
            }
        };
        println!("网页实时预览: http://<本机IP>:{port}/");
        for conn in listener.incoming() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(conn) = conn else { continue };
            let m = Arc::clone(&mirror);
            let st = Arc::clone(&stream);
            thread::spawn(move || {
                let _ = handle_client(conn, m, st, geometry);
            });
        }
    });
}

fn handle_client(mut s: TcpStream, mirror: SharedMirror, stream: SharedStream,
                 geometry: (usize, usize)) -> std::io::Result<()> {
    s.set_nodelay(true).ok();
    let mut reader = BufReader::new(s.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let mut ws_key: Option<String> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let t = line.trim_end();
        if t.is_empty() {
            break;
        }
        if let Some((k, v)) = t.split_once(':') {
            if k.eq_ignore_ascii_case("Sec-WebSocket-Key") {
                ws_key = Some(v.trim().to_string());
            }
        }
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("/").to_string();
    let path = path.split('?').next().unwrap_or("/").to_string();
    match path.as_str() {
        "/ws" => match ws_key {
            Some(key) => ws_stream(s, &key, mirror),
            None => write_http(&mut s, "400 Bad Request", "text/plain", b"websocket upgrade required"),
        },
        "/push" => match ws_key {
            Some(key) => ws_receive(s, &key, stream),
            None => write_http(&mut s, "400 Bad Request", "text/plain", b"websocket upgrade required"),
        },
        "/geo" => {
            let body = format!("{} {}", geometry.0, geometry.1);
            write_http(&mut s, "200 OK", "text/plain", body.as_bytes())
        }
        "/view" => write_http(&mut s, "200 OK", "text/html; charset=utf-8", INDEX_HTML.as_bytes()),
        _ => write_http(&mut s, "200 OK", "text/html; charset=utf-8", SENDER_HTML.as_bytes()),
    }
}

fn write_http(
    s: &mut TcpStream,
    status: &str,
    ctype: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(header.as_bytes())?;
    s.write_all(body)?;
    s.flush()
}


// ---------------------------------------------------------------- 投送（浏览器 → 面板）

/// 浏览器投送上来的当前帧
#[derive(Default)]
pub struct StreamBuffer {
    pub rgb: Vec<u8>,
    pub w: usize,
    pub h: usize,
    pub seq: u64,
    /// 最后一次收到帧的时间（用于判断投送是否已停止）
    pub at: Option<std::time::Instant>,
}

pub type SharedStream = Arc<Mutex<StreamBuffer>>;

pub fn new_stream() -> SharedStream {
    Arc::new(Mutex::new(StreamBuffer::default()))
}

/// 最近还在投送吗（默认 2 秒内算活跃）
pub fn stream_active(stream: &SharedStream) -> Option<(usize, usize, Vec<u8>)> {
    let s = stream.lock().ok()?;
    let at = s.at?;
    if at.elapsed().as_secs_f32() > 5.0 || s.rgb.is_empty() {
        return None;
    }
    Some((s.w, s.h, s.rgb.clone()))
}

/// 接收投送端（浏览器）发来的帧；客户端帧带掩码，需要解掩码
fn ws_receive(mut s: TcpStream, key: &str, stream: SharedStream) -> std::io::Result<()> {
    let accept = base64(&sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()));
    s.write_all(format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\r\n"
    ).as_bytes())?;
    s.flush()?;

    const MAX_FRAME: usize = 4 * 1024 * 1024;
    loop {
        let mut h = [0u8; 2];
        if s.read_exact(&mut h).is_err() {
            return Ok(());
        }
        let opcode = h[0] & 0x0f;
        let masked = h[1] & 0x80 != 0;
        let mut len = (h[1] & 0x7f) as u64;
        if len == 126 {
            let mut b = [0u8; 2];
            s.read_exact(&mut b)?;
            len = u16::from_be_bytes(b) as u64;
        } else if len == 127 {
            let mut b = [0u8; 8];
            s.read_exact(&mut b)?;
            len = u64::from_be_bytes(b);
        }
        if len as usize > MAX_FRAME {
            return Ok(()); // 异常大帧，断开
        }
        let mut mask = [0u8; 4];
        if masked {
            s.read_exact(&mut mask)?;
        }
        let mut data = vec![0u8; len as usize];
        if s.read_exact(&mut data).is_err() {
            return Ok(());
        }
        if masked {
            for (i, b) in data.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        match opcode {
            0x2 => {
                // 二进制帧：[宽 u16][高 u16][RGB...]
                if data.len() >= 4 {
                    let w = u16::from_le_bytes([data[0], data[1]]) as usize;
                    let h = u16::from_le_bytes([data[2], data[3]]) as usize;
                    let need = w * h * 3;
                    if w > 0 && h > 0 && data.len() >= 4 + need {
                        if let Ok(mut st) = stream.lock() {
                            st.rgb.clear();
                            st.rgb.extend_from_slice(&data[4..4 + need]);
                            st.w = w;
                            st.h = h;
                            st.seq += 1;
                            st.at = Some(std::time::Instant::now());
                        }
                    }
                }
            }
            0x8 => return Ok(()),          // close
            0x9 => { /* ping：忽略（浏览器很少发） */ }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- WebSocket

fn ws_stream(mut s: TcpStream, key: &str, mirror: SharedMirror) -> std::io::Result<()> {
    let accept = base64(&sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()));
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    s.write_all(resp.as_bytes())?;
    s.flush()?;

    let mut last_seq = u64::MAX;
    loop {
        let (w, h, seq, data) = {
            let m = match mirror.lock() {
                Ok(m) => m,
                Err(_) => return Ok(()),
            };
            if m.seq == last_seq {
                drop(m);
                thread::sleep(std::time::Duration::from_millis(30));
                continue;
            }
            (m.w, m.h, m.seq, m.rgb.clone())
        };
        last_seq = seq;
        let mut payload = Vec::with_capacity(8 + data.len());
        payload.extend_from_slice(&(w as u16).to_le_bytes());
        payload.extend_from_slice(&(h as u16).to_le_bytes());
        payload.extend_from_slice(&(seq as u32).to_le_bytes());
        payload.extend_from_slice(&data);
        if ws_send_binary(&mut s, &payload).is_err() {
            return Ok(()); // 客户端断开
        }
        thread::sleep(std::time::Duration::from_millis(40)); // ≈25fps 上限
    }
}

fn ws_send_binary(s: &mut TcpStream, payload: &[u8]) -> std::io::Result<()> {
    let mut hdr = Vec::with_capacity(10);
    hdr.push(0x82); // FIN + binary
    let len = payload.len();
    if len < 126 {
        hdr.push(len as u8);
    } else if len < 65536 {
        hdr.push(126);
        hdr.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        hdr.push(127);
        hdr.extend_from_slice(&(len as u64).to_be_bytes());
    }
    s.write_all(&hdr)?;
    s.write_all(payload)?;
    s.flush()
}

// ---------------------------------------------------------------- SHA-1 / base64

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

// ---------------------------------------------------------------- 内嵌前端页面

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>LED 面板实时预览</title>
<style>
  :root { color-scheme: dark; --ar: 2; }
  * { box-sizing: border-box; }
  body { margin:0; min-height:100vh; background:#0b0d10; color:#c9d1d9;
         font:14px/1.5 system-ui,-apple-system,"Noto Sans CJK SC",sans-serif;
         display:flex; flex-direction:column; align-items:center; justify-content:center;
         gap:14px; padding:16px; }
  .panel { background:#000; border:1px solid #2a2f36; border-radius:10px; padding:8px;
           box-shadow:0 10px 40px #000a; max-width:100%; }
  canvas { display:block; image-rendering: pixelated; image-rendering: crisp-edges;
           width: min(94vw, calc(86vh * var(--ar))); height:auto; max-width:100%;
           cursor:zoom-in; }
  .meta { display:flex; gap:16px; align-items:center; flex-wrap:wrap; justify-content:center;
          font-variant-numeric: tabular-nums; }
  .dot { width:9px; height:9px; border-radius:50%; background:#e5534b; display:inline-block;
         margin-right:6px; vertical-align:middle; }
  .dot.on { background:#3fb950; }
  .hint { color:#6e7681; font-size:12px; text-align:center; }
  button { background:#21262d; color:#c9d1d9; border:1px solid #30363d; border-radius:6px;
           padding:4px 12px; cursor:pointer; font:inherit; }
  button:hover { background:#30363d; }
</style>
</head>
<body>
  <div class="panel"><canvas id="c" width="64" height="32"></canvas></div>
  <div class="meta">
    <span><span id="dot" class="dot"></span><span id="st">连接中…</span></span>
    <span id="geo">—</span>
    <span id="pg"></span>
    <span id="fps">—</span>
    <button id="fs">全屏</button>
  </div>
  <div class="hint">画面即 LED 面板正在显示的内容，随窗口自适应缩放</div>
<script>
const c = document.getElementById('c');
const ctx = c.getContext('2d', { alpha: false });
let img = null, frames = 0, t0 = performance.now();

function setGeo(w, h) {
  c.width = w; c.height = h;
  document.documentElement.style.setProperty('--ar', (w / h).toFixed(4));
  document.getElementById('geo').textContent = w + ' × ' + h;
  img = ctx.createImageData(w, h);
}

function connect() {
  const ws = new WebSocket((location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws');
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => {
    document.getElementById('dot').classList.add('on');
    document.getElementById('st').textContent = '已连接';
  };
  ws.onclose = () => {
    document.getElementById('dot').classList.remove('on');
    document.getElementById('st').textContent = '已断开，重连中…';
    setTimeout(connect, 1000);
  };
  ws.onerror = () => ws.close();
  ws.onmessage = (ev) => {
    const dv = new DataView(ev.data);
    const w = dv.getUint16(0, true), h = dv.getUint16(2, true);
    if (c.width !== w || c.height !== h) setGeo(w, h);
    const src = new Uint8Array(ev.data, 8);
    const d = img.data;
    for (let i = 0, j = 0; i < src.length; i += 3, j += 4) {
      d[j] = src[i]; d[j + 1] = src[i + 1]; d[j + 2] = src[i + 2]; d[j + 3] = 255;
    }
    ctx.putImageData(img, 0, 0);
    frames++;
    const now = performance.now();
    if (now - t0 > 1000) {
      document.getElementById('fps').textContent =
      frames + ' fps' + (skipped ? '（丢 ' + skipped + '）' : '');
    skipped = 0;
      frames = 0; t0 = now;
    }
  };
}
connect();

function toggleFs() {
  if (document.fullscreenElement) document.exitFullscreen();
  else document.documentElement.requestFullscreen();
}
document.getElementById('fs').onclick = toggleFs;
c.onclick = toggleFs;
</script>
</body>
</html>
"#;

// ---------------------------------------------------------------- 投送页（浏览器 → 面板）

const SENDER_HTML: &str = r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>投送视频到 LED 面板</title>
<style>
  :root { color-scheme: dark; --ar: 2; }
  * { box-sizing: border-box; }
  body { margin:0; min-height:100vh; background:#0b0d10; color:#c9d1d9;
         font:14px/1.5 system-ui,-apple-system,"Noto Sans CJK SC",sans-serif;
         display:flex; flex-direction:column; align-items:center; justify-content:center;
         gap:14px; padding:16px; }
  .panel { background:#000; border:1px solid #2a2f36; border-radius:10px; padding:8px;
           box-shadow:0 10px 40px #000a; max-width:100%; }
  canvas { display:block; image-rendering: pixelated; image-rendering: crisp-edges;
           width: min(94vw, calc(64vh * var(--ar))); height:auto; max-width:100%; }
  .row { display:flex; gap:10px; align-items:center; flex-wrap:wrap; justify-content:center; }
  .dot { width:9px; height:9px; border-radius:50%; background:#e5534b; display:inline-block;
         margin-right:6px; vertical-align:middle; }
  .dot.on { background:#3fb950; }
  .hint { color:#6e7681; font-size:12px; text-align:center; max-width:620px; }
  button, select, label.btn { background:#21262d; color:#c9d1d9; border:1px solid #30363d;
           border-radius:6px; padding:6px 12px; cursor:pointer; font:inherit; }
  button:hover, label.btn:hover { background:#30363d; }
  input[type=file] { display:none; }
  video { display:none; }
</style>
</head>
<body>
  <div class="panel"><canvas id="cv" width="64" height="32"></canvas></div>
  <div class="row">
    <span><span id="dot" class="dot"></span><span id="st">连接中…</span></span>
    <span id="geo">—</span>
    <span id="fps">—</span>
  </div>
  <div class="row">
    <label class="btn">选择视频文件<input type="file" id="file" accept="video/*"></label>
    <button id="cam">用摄像头</button>
    <button id="screen">共享屏幕</button>
    <select id="fit">
      <option value="contain">完整显示（留黑边）</option>
      <option value="cover" selected>裁切铺满</option>
      <option value="stretch">拉伸铺满（变形）</option>
    </select>
    <button id="play">暂停</button>
  </div>
  <div class="hint">投送期间会申请「屏幕常亮」以免息屏中断（浏览器不允许网页在真正切到后台后继续使用摄像头，<br>
    所以请保持本页面在前台；息屏或切走会暂停投送，回来会自动恢复）。<br>
    预览即面板上正在显示的画面（画布就是面板的逻辑分辨率，随链屏/并联屏自动变化）。
    解码和缩放都在你的浏览器里完成，树莓派只接收小尺寸帧，CPU 占用极低。</div>
  <video id="v" muted loop playsinline></video>
<script>
const cv = document.getElementById('cv');
const v = document.getElementById('v');
let ctx = null, geo = { w: 64, h: 32 }, ws = null;
let frames = 0, t0 = performance.now(), lastSend = 0;
let playing = true, fitMode = 'cover';

function setGeo(w, h) {
  geo = { w, h };
  cv.width = w; cv.height = h;
  document.documentElement.style.setProperty('--ar', (w / h).toFixed(4));
  document.getElementById('geo').textContent = w + ' × ' + h;
  ctx = cv.getContext('2d', { willReadFrequently: true });
}

function connectPush() {
  ws = new WebSocket((location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/push');
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => { document.getElementById('dot').classList.add('on');
                      document.getElementById('st').textContent = '投送中'; };
  ws.onclose = () => { document.getElementById('dot').classList.remove('on');
                       document.getElementById('st').textContent = '已断开，重连中…';
                       setTimeout(connectPush, 1000); };
  ws.onerror = () => ws.close();
}

function drawFit() {
  const vw = v.videoWidth, vh = v.videoHeight;
  if (!vw || !vh) return false;
  const dw = geo.w, dh = geo.h;
  ctx.fillStyle = '#000';
  ctx.fillRect(0, 0, dw, dh);
  if (fitMode === 'stretch') {
    ctx.drawImage(v, 0, 0, dw, dh);
  } else {
    const s = fitMode === 'cover' ? Math.max(dw / vw, dh / vh) : Math.min(dw / vw, dh / vh);
    const w = vw * s, h = vh * s;
    ctx.drawImage(v, (dw - w) / 2, (dh - h) / 2, w, h);
  }
  return true;
}

let skipped = 0, lastProgress = 0, lastProgressAt = performance.now();

// 屏幕常亮：手机投送中断最常见的原因是屏幕自动熄灭（随后浏览器挂起页面）。
// Wake Lock 能在页面处于前台时阻止息屏；切到后台被释放后，回到前台会自动重新申请。
let wakeLock = null;
async function keepAwake() {
  if (!('wakeLock' in navigator)) return false;
  try {
    wakeLock = await navigator.wakeLock.request('screen');
    wakeLock.addEventListener('release', () => { wakeLock = null; });
    return true;
  } catch (e) { return false; }
}
document.addEventListener('visibilitychange', () => {
  if (!document.hidden) {
    if (playing) keepAwake();                    // 回到前台：重新申请常亮
    if (audioCtx && audioCtx.state === 'suspended') audioCtx.resume();
    lastVideoFrameAt = performance.now();        // 让兜底逻辑立刻恢复正常
    lastProgressAt = performance.now();
  }
});

// 发送一帧（两条驱动路径共用）
function sendOneFrame() {
  if (!playing || !ws || ws.readyState !== 1) return false;
  if (ws.bufferedAmount > 64 * 1024) { skipped++; return false; }
  if (!drawFit()) return false;
  const d = ctx.getImageData(0, 0, geo.w, geo.h).data;
  const buf = new Uint8Array(4 + geo.w * geo.h * 3);
  buf[0] = geo.w & 255; buf[1] = geo.w >> 8; buf[2] = geo.h & 255; buf[3] = geo.h >> 8;
  for (let i = 0, j = 4; i < d.length; i += 4, j += 3) {
    buf[j] = d[i]; buf[j + 1] = d[i + 1]; buf[j + 2] = d[i + 2];
  }
  ws.send(buf);
  frames++;
  return true;
}

// 首选驱动方式：requestVideoFrameCallback —— 每解码出一帧视频回调一次，
// 因此发送速率与视频自身帧率一致（60fps 就发 60、10fps 就发 10）。
let frameCbActive = false, srcFrames = 0, srcT0 = performance.now(), srcFps = 0;
let lastVideoFrameAt = 0;   // 最近一次视频帧回调的时间

function onVideoFrame() {
  if (frameCbActive) v.requestVideoFrameCallback(onVideoFrame);
  const now = performance.now();
  lastVideoFrameAt = now;
  if (playing && sendOneFrame()) srcFrames++;   // 后台不主动停发（真后台由平台限制，靠背压防堆积）
  if (now - srcT0 > 1000) {
    srcFps = srcFrames * 1000 / (now - srcT0);
    srcFrames = 0; srcT0 = now;
    const t = performance.now();
    if (t - t0 > 1000) {
      document.getElementById('fps').textContent =
        '源 ' + srcFps.toFixed(0) + 'fps → 发 ' + frames + 'fps' + (skipped ? '（丢 ' + skipped + '）' : '');
      frames = 0; t0 = t; skipped = 0;
    }
  }
}

// 兜底：若视频帧回调超过 500ms 没触发（典型情况：窗口最小化后浏览器降低了解码优先级），
// 就用定时器继续发当前画面，避免服务端 2 秒收不到帧而切回状态页。
setInterval(() => {
  if (!playing) return;
  if (performance.now() - lastVideoFrameAt > 500) sendOneFrame();
}, 100);

// 兜底二：静音 AudioContext。浏览器对后台定时器有限流，但音频回调不受影响，
// 用它当节拍器可以显著提高最小化/后台时的发送率（约 12fps）。
let audioCtx = null;
function startTicker() {
  if (audioCtx) { audioCtx.resume && audioCtx.resume(); return; }
  try {
    audioCtx = new (window.AudioContext || window.webkitAudioContext)();
    const src = audioCtx.createConstantSource();
    const gain = audioCtx.createGain();
    gain.gain.value = 0;                    // 完全静音，不会出声
    const proc = audioCtx.createScriptProcessor(4096, 1, 1);
    proc.onaudioprocess = () => { if (playing) sendOneFrame(); };
    src.connect(gain); gain.connect(proc); proc.connect(audioCtx.destination);
    src.start();
  } catch (e) { audioCtx = null; }
}

function startFrameCallback() {
  if (typeof v.requestVideoFrameCallback === 'function' && !frameCbActive) {
    frameCbActive = true;
    v.requestVideoFrameCallback(onVideoFrame);
  }
}

function tick() {
  requestAnimationFrame(tick);
  const now = performance.now();
  if (frameCbActive) return;                // 已由视频帧回调驱动（速率与视频一致）
  if (!playing || !ws || ws.readyState !== 1) return;
  if (now - lastSend < 40) return;          // 约 25fps 上限
  // 背压：发得比网络快时不要堆积，宁可丢帧 —— 否则延迟越滚越大，表现为「卡住」
  if (ws.bufferedAmount > 64 * 1024) { skipped++; return; }
  // 看门狗：视频/摄像头若不再推进（currentTime 不动），重新 play 一次
  const t = v.currentTime;
  if (t !== lastProgress) { lastProgress = t; lastProgressAt = now; }
  else if (now - lastProgressAt > 3000) { lastProgressAt = now; v.play().catch(() => {}); }
  if (!drawFit()) return;
  lastSend = now;
  const d = ctx.getImageData(0, 0, geo.w, geo.h).data;
  const buf = new Uint8Array(4 + geo.w * geo.h * 3);
  buf[0] = geo.w & 255; buf[1] = geo.w >> 8;
  buf[2] = geo.h & 255; buf[3] = geo.h >> 8;
  for (let i = 0, j = 4; i < d.length; i += 4, j += 3) {
    buf[j] = d[i]; buf[j + 1] = d[i + 1]; buf[j + 2] = d[i + 2];
  }
  ws.send(buf);
  frames++;
  if (now - t0 > 1000) {
    document.getElementById('fps').textContent = frames + ' fps';
    frames = 0; t0 = now;
  }
}

document.getElementById('file').onchange = (e) => {
  const f = e.target.files[0];
  if (!f) return;
  v.srcObject = null;
  v.src = URL.createObjectURL(f);
  v.play();
  playing = true;
  startFrameCallback();
  startTicker();
  keepAwake().then(ok => { if (ok) document.getElementById('st').textContent = '投送中（屏幕常亮）'; });
  document.getElementById('play').textContent = '暂停';
};
document.getElementById('cam').onclick = async () => {
  if (!window.isSecureContext) {
    alert('浏览器只允许安全上下文（HTTPS 或 localhost）访问摄像头。\n' +
          '用本机打开可用 localhost；从别的设备访问需要 HTTPS。\n' +
          '也可以先用「选择视频文件」投送，它不需要任何权限。');
    return;
  }
  try {
    // 面板只有 64×32，没必要让手机解 1080p —— 低分辨率 + 低帧率能显著降低手机侧负载
    const s = await navigator.mediaDevices.getUserMedia({
      video: { width: { ideal: 640 }, height: { ideal: 360 },
               frameRate: { ideal: 15, max: 20 } }
    });
    v.srcObject = s;
    await v.play();
    playing = true;
    startFrameCallback();
    startTicker();
    keepAwake().then(ok => { if (ok) document.getElementById('st').textContent = '投送中（屏幕常亮）'; });
    document.getElementById('play').textContent = '暂停';
  } catch (err) { alert('无法打开摄像头: ' + err); }
};
document.getElementById('screen').onclick = async () => {
  if (!window.isSecureContext) {
    alert('屏幕共享需要安全上下文（HTTPS 或 localhost）。');
    return;
  }
  try {
    // 桌面浏览器即使标签页切到后台，屏幕采集也会继续 —— 适合长时间投送
    const s = await navigator.mediaDevices.getDisplayMedia({
      video: { frameRate: { ideal: 15, max: 30 } }, audio: false
    });
    v.srcObject = s;
    s.getVideoTracks()[0].addEventListener('ended', () => {
      document.getElementById('st').textContent = '屏幕共享已结束';
    });
    await v.play();
    playing = true;
    startFrameCallback();
    startTicker();
    keepAwake();
    document.getElementById('st').textContent = '投送中（屏幕共享）';
  } catch (err) { /* 用户取消 */ }
};
document.getElementById('fit').onchange = (e) => { fitMode = e.target.value; };
document.getElementById('play').onclick = (e) => {
  playing = !playing;
  if (playing) v.play(); else v.pause();
  e.target.textContent = playing ? '暂停' : '继续';
};

fetch('/geo').then(r => r.text()).then(t => {
  const p = t.trim().split(/\s+/).map(Number);
  if (p.length >= 2 && p[0] > 0) setGeo(p[0], p[1]);
}).catch(() => setGeo(64, 32)).finally(() => { connectPush(); tick(); });
</script>
</body>
</html>
"#;
