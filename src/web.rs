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
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::config::{ApplyOutcome, SharedConfig};

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

/// 主循环与网页设置之间共享的运行时控制。
pub struct RuntimeCtl {
    /// 目标亮度 1..100；主循环据此调用 led_matrix_set_brightness
    pub brightness: AtomicI32,
    pub idle: AtomicBool,
    pub restart: AtomicBool,
    /// 无投送时显示时钟
    pub show_clock: AtomicBool,
    /// 时钟 24 小时制
    pub clock_24h: AtomicBool,
    /// 界面语言：0=zh，1=en
    pub lang_en: AtomicBool,
}

pub type SharedRuntime = Arc<RuntimeCtl>;

pub fn new_runtime(brightness: i32, idle: bool) -> SharedRuntime {
    Arc::new(RuntimeCtl {
        brightness: AtomicI32::new(brightness),
        idle: AtomicBool::new(idle),
        restart: AtomicBool::new(false),
        show_clock: AtomicBool::new(true),
        clock_24h: AtomicBool::new(true),
        lang_en: AtomicBool::new(false),
    })
}

/// 启动网页服务（在后台线程里 accept）
pub fn spawn_web_server(port: u16, mirror: SharedMirror, stream: SharedStream,
                        geometry: (usize, usize), stop: Arc<AtomicBool>,
                        cfg: SharedConfig, rt: SharedRuntime) {
    thread::spawn(move || {
        let listener = match TcpListener::bind(("0.0.0.0", port)) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("网页服务无法监听 0.0.0.0:{port} —— {e}");
                return;
            }
        };
        println!("设置页面: http://<本机IP>:{port}/settings");
        for conn in listener.incoming() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(conn) = conn else { continue };
            let m = Arc::clone(&mirror);
            let st = Arc::clone(&stream);
            let cf = cfg.clone();
            let rt = Arc::clone(&rt);
            thread::spawn(move || {
                let _ = handle_client(conn, m, st, geometry, cf, rt);
            });
        }
    });
}

fn handle_client(mut s: TcpStream, mirror: SharedMirror, stream: SharedStream,
                 geometry: (usize, usize), cfg: SharedConfig, rt: SharedRuntime)
                 -> std::io::Result<()> {
    let peer = s.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    s.set_nodelay(true).ok();
    // 避免对端挂起不发数据时线程永久阻塞（无鉴权端口，恶意/异常客户端都可能来）
    s.set_read_timeout(Some(std::time::Duration::from_secs(120))).ok();
    s.set_write_timeout(Some(std::time::Duration::from_secs(30))).ok();
    let mut reader = BufReader::new(s.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.trim().is_empty() {
        let _ = write_http(&mut s, "400 Bad Request", "text/plain", b"bad request");
        return Ok(());
    }

    let mut ws_key: Option<String> = None;
    let mut content_length: usize = 0;
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
            let (k, v) = (k.trim(), v.trim());
            if k.eq_ignore_ascii_case("Sec-WebSocket-Key") {
                ws_key = Some(v.to_string());
            } else if k.eq_ignore_ascii_case("Content-Length") {
                content_length = v.parse().unwrap_or(0).min(64 * 1024);
            }
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_ascii_uppercase();
    let target = parts.next().unwrap_or("/").to_string();
    eprintln!("[http] {peer} {method} {target}");
    let (path_only, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    // 读请求体（用于 POST /api/config）
    let mut body = Vec::new();
    if content_length > 0 && method != "GET" {
        body.resize(content_length, 0);
        if reader.read_exact(&mut body).is_err() {
            body.clear();
        }
    }

    // BufReader 可能已经预读了 WebSocket 首帧；必须把缓冲区里剩余字节一并交给 WS 处理，
    // 否则那些字节会被丢掉，握手后的第一条帧永远读不齐。
    let leftover = {
        let pending = reader.buffer().to_vec();
        reader.consume(pending.len());
        pending
    };

    match path_only.as_str() {
        "/ws" => match ws_key {
            Some(key) => ws_stream(s, &key, mirror, leftover),
            None => write_http(&mut s, "400 Bad Request", "text/plain", b"websocket upgrade required"),
        },
        "/push" => match ws_key {
            Some(key) => ws_receive(s, &key, stream, leftover),
            None => write_http(&mut s, "400 Bad Request", "text/plain", b"websocket upgrade required"),
        },
        "/geo" => {
            let body = format!("{} {}", geometry.0, geometry.1);
            write_http(&mut s, "200 OK", "text/plain", body.as_bytes())
        }
        "/api/config" => {
            if method == "GET" {
                let json = config_json(&cfg, &rt, &stream, geometry);
                write_http(&mut s, "200 OK", "application/json; charset=utf-8", json.as_bytes())
            } else {
                api_config_set(&mut s, &cfg, &rt, &query, &body)
            }
        }
        "/api/restart" => {
            rt.restart.store(true, Ordering::SeqCst);
            write_http(&mut s, "200 OK", "application/json; charset=utf-8",
                       br#"{"ok":true,"restarting":true}"#)
        }
        // 实时亮度：只动 brightness，不碰其他设置，不触发 restart
        "/api/brightness" => {
            if method == "GET" {
                let v = rt.brightness.load(Ordering::Relaxed);
                let body = format!(r#"{{"brightness":{v}}}"#);
                write_http(&mut s, "200 OK", "application/json; charset=utf-8", body.as_bytes())
            } else {
                api_brightness_set(&mut s, &cfg, &rt, &query, &body)
            }
        }
        "/settings" => write_http(&mut s, "200 OK", "text/html; charset=utf-8",
                                  html_lang(SETTINGS_HTML, &rt).as_bytes()),
        "/view" => write_http(&mut s, "200 OK", "text/html; charset=utf-8",
                              html_lang(INDEX_HTML, &rt).as_bytes()),
        _ => write_http(&mut s, "200 OK", "text/html; charset=utf-8",
                        html_lang(SENDER_HTML, &rt).as_bytes()),
    }
}

/// 把页面里的 __LANG__ 换成 zh/en（全局中英切换）。
fn html_lang(html: &str, rt: &SharedRuntime) -> String {
    let lang = if rt.lang_en.load(Ordering::Relaxed) { "en" } else { "zh" };
    html.replace("__LANG__", lang)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SharedConfig;
    use std::io::Read;
    use std::net::TcpStream;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    fn get(port: u16, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").as_bytes())
            .unwrap();
        let mut buf = String::new();
        s.read_to_string(&mut buf).unwrap();
        buf
    }

    fn post(port: u16, path: &str, body: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(format!(
            "POST {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\
             Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ).as_bytes()).unwrap();
        let mut buf = String::new();
        s.read_to_string(&mut buf).unwrap();
        buf
    }

    #[test]
    fn http_config_roundtrip_and_pages() {
        let dir = std::env::temp_dir().join(format!("lpwp-http-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path: PathBuf = dir.join("c.conf");
        let cfg = SharedConfig::open_at(path.clone());
        // pick a free port
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let mirror = new_mirror(64, 32);
        let stream = new_stream();
        let stop = Arc::new(AtomicBool::new(false));
        let rt = new_runtime(60, true);
        spawn_web_server(port, mirror, stream, (64, 32), Arc::clone(&stop), cfg.clone(), Arc::clone(&rt));
        std::thread::sleep(std::time::Duration::from_millis(150));

        let settings = get(port, "/settings");
        assert!(settings.contains("200 OK"), "settings: {settings}");
        assert!(settings.contains("LED 面板设置"));

        let geo = get(port, "/geo");
        assert!(geo.contains("64 32"));

        let cfg_get = get(port, "/api/config");
        assert!(cfg_get.contains("\"rows\":32"), "cfg: {cfg_get}");
        assert!(cfg_get.contains("\"brightness\":60"));

        let upd = post(port, "/api/config", "brightness=75&idle=0&cols=128");
        assert!(upd.contains("\"ok\":true"), "upd: {upd}");
        assert!(upd.contains("restart_required\":true"), "cols change needs restart: {upd}");
        assert_eq!(rt.brightness.load(Ordering::SeqCst), 75);
        assert!(!rt.idle.load(Ordering::SeqCst));

        let upd2 = post(port, "/api/config", "brightness=50");
        assert!(upd2.contains("restart_required\":false"), "hot-only: {upd2}");

        // 硬件驱动参数：改 panel_type / gpio_slowdown 应要求重启，并写入配置
        let drv = post(port, "/api/config",
                       "panel_type=FM6126A&gpio_slowdown=3&pwm_bits=8&inverse_colors=1");
        assert!(drv.contains("restart_required\":true"), "driver change: {drv}");
        // 写盘必须真的落盘，不能只改内存
        let on_disk = std::fs::read_to_string(&path).expect("config file must exist after POST");
        assert!(on_disk.contains("panel_type=FM6126A"), "disk: {on_disk}");
        assert!(on_disk.contains("gpio_slowdown=3"));
        let after = get(port, "/api/config");
        assert!(after.contains("\"panel_type\":\"FM6126A\""), "got: {after}");
        assert!(after.contains("\"gpio_slowdown\":3"));
        assert!(after.contains("\"pwm_bits\":8"));
        assert!(after.contains("\"inverse_colors\":true"));

        let restart = post(port, "/api/restart", "");
        assert!(restart.contains("\"ok\":true"));
        assert!(rt.restart.load(Ordering::SeqCst));
    }

    #[test]
    fn api_brightness_live() {
        let dir = std::env::temp_dir().join(format!("lpwp-br-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("c.conf");
        let cfg = SharedConfig::open_at(path.clone());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let stop = Arc::new(AtomicBool::new(false));
        let rt = new_runtime(60, true);
        spawn_web_server(port, new_mirror(64, 32), new_stream(), (64, 32),
                         Arc::clone(&stop), cfg.clone(), Arc::clone(&rt));
        std::thread::sleep(std::time::Duration::from_millis(150));

        let r = post(port, "/api/brightness", "brightness=27");
        assert!(r.contains("\"brightness\":27"), "{r}");
        assert_eq!(rt.brightness.load(Ordering::SeqCst), 27);
        let disk = std::fs::read_to_string(&path).unwrap();
        assert!(disk.contains("brightness=27"), "{disk}");
        // 越界钳制
        let r = post(port, "/api/brightness", "brightness=200");
        assert!(r.contains("\"brightness\":100"), "{r}");
        stop.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 把 `a=b&c=d` 解析成键值对（urlencoded）。
fn parse_kv(s: &str) -> Vec<(String, String)> {
    s.split('&')
        .filter(|p| !p.is_empty())
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((urldecode(k), urldecode(v)))
        })
        .collect()
}

fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

fn config_json(cfg: &SharedConfig, rt: &SharedRuntime, stream: &SharedStream,
               geometry: (usize, usize)) -> String {
    let c = cfg.get();
    let streaming = stream_active(stream).is_some();
    format!(
        r#"{{"settings":{{"rows":{rows},"cols":{cols},"chain":{chain},"parallel":{parallel},
        "brightness":{brightness},"mapping":"{mapping}","rgb_sequence":"{rgb_sequence}",
        "web_port":{web_port},"idle":{idle},
        "show_clock":{show_clock},"clock_24h":{clock_24h},"lang":"{lang}",
        "panel_type":"{panel_type}","gpio_slowdown":{gpio_slowdown},
        "pwm_bits":{pwm_bits},"pwm_lsb_ns":{pwm_lsb_ns},"pwm_dither":{pwm_dither},
        "scan_mode":{scan_mode},"row_address_type":{row_addr},"multiplexing":{mux},
        "no_hardware_pulse":{no_pulse},"inverse_colors":{inv},
        "pixel_mapper":"{pixmap}","limit_refresh_hz":{lim},
        "no_busy_waiting":{nobusy},"rp1_pio":{rp1}}},
        "runtime":{{"panel_width":{gw},"panel_height":{gh},"streaming":{streaming},
        "brightness_now":{bnow},"config_path":"{cpath}"}}}}"#,
        rows = c.rows,
        cols = c.cols,
        chain = c.chain,
        parallel = c.parallel,
        brightness = c.brightness,
        mapping = json_escape(&c.mapping),
        rgb_sequence = json_escape(&c.rgb_sequence),
        web_port = c.web_port,
        idle = c.idle,
        show_clock = c.show_clock,
        clock_24h = c.clock_24h,
        lang = json_escape(&c.lang),
        panel_type = json_escape(&c.panel_type),
        gpio_slowdown = c.gpio_slowdown,
        pwm_bits = c.pwm_bits,
        pwm_lsb_ns = c.pwm_lsb_ns,
        pwm_dither = c.pwm_dither,
        scan_mode = c.scan_mode,
        row_addr = c.row_address_type,
        mux = c.multiplexing,
        no_pulse = c.no_hardware_pulse,
        inv = c.inverse_colors,
        pixmap = json_escape(&c.pixel_mapper),
        lim = c.limit_refresh_hz,
        nobusy = c.no_busy_waiting,
        rp1 = c.rp1_pio,
        gw = geometry.0,
        gh = geometry.1,
        streaming = streaming,
        bnow = rt.brightness.load(Ordering::Relaxed),
        cpath = json_escape(&cfg.path().display().to_string()),
    )
}

fn api_brightness_set(s: &mut TcpStream, cfg: &SharedConfig, rt: &SharedRuntime,
                      query: &str, body: &[u8]) -> std::io::Result<()> {
    let mut kvs = parse_kv(query);
    kvs.extend(parse_kv(&String::from_utf8_lossy(body)));
    let mut v: Option<i32> = None;
    for (k, val) in kvs {
        if k == "brightness" || k == "value" {
            if let Ok(n) = val.parse::<i32>() {
                v = Some(n.clamp(1, 100));
            }
        }
    }
    let Some(val) = v else {
        return write_http(s, "400 Bad Request", "application/json; charset=utf-8",
                          br#"{"ok":false,"error":"missing brightness"}"#);
    };
    // 立刻生效（主循环下一轮 led_matrix_set_brightness）
    rt.brightness.store(val, Ordering::SeqCst);
    // 并写入配置，重启后保持
    let mut c = cfg.get();
    c.brightness = val;
    match cfg.update(c) {
        ApplyOutcome::SaveFailed(e) => {
            let msg = format!(r#"{{"ok":true,"brightness":{val},"save_error":"{}"}}"#, json_escape(&e));
            return write_http(s, "200 OK", "application/json; charset=utf-8", msg.as_bytes());
        }
        _ => {}
    }
    let msg = format!(r#"{{"ok":true,"brightness":{val}}}"#);
    write_http(s, "200 OK", "application/json; charset=utf-8", msg.as_bytes())
}

fn api_config_set(s: &mut TcpStream, cfg: &SharedConfig, rt: &SharedRuntime,
                  query: &str, body: &[u8]) -> std::io::Result<()> {
    let mut kvs = parse_kv(query);
    kvs.extend(parse_kv(&String::from_utf8_lossy(body)));
    let mut c = cfg.get();
    for (k, v) in kvs {
        match k.as_str() {
            "rows" => if let Ok(n) = v.parse() { c.rows = n },
            "cols" => if let Ok(n) = v.parse() { c.cols = n },
            "chain" => if let Ok(n) = v.parse() { c.chain = n },
            "parallel" => if let Ok(n) = v.parse() { c.parallel = n },
            "brightness" => if let Ok(n) = v.parse::<i32>() { c.brightness = n.clamp(1, 100) },
            "mapping" => if !v.is_empty() { c.mapping = v },
            "rgb_sequence" => if !v.is_empty() { c.rgb_sequence = v },
            "web_port" => if let Ok(n) = v.parse() { c.web_port = n },
            "idle" => c.idle = matches!(v.as_str(), "1" | "true" | "yes" | "on"),
            "show_clock" => c.show_clock = matches!(v.as_str(), "1" | "true" | "yes" | "on"),
            "clock_24h" => c.clock_24h = matches!(v.as_str(), "1" | "true" | "yes" | "on"),
            "lang" => {
                c.lang = match v.to_ascii_lowercase().as_str() {
                    "en" | "english" => "en".to_string(),
                    _ => "zh".to_string(),
                }
            }
            // 硬件驱动
            "panel_type" | "driver" => c.panel_type = v,
            "gpio_slowdown" => if let Ok(n) = v.parse() { c.gpio_slowdown = n },
            "pwm_bits" => if let Ok(n) = v.parse() { c.pwm_bits = n },
            "pwm_lsb_ns" | "pwm_lsb_nanoseconds" => if let Ok(n) = v.parse() { c.pwm_lsb_ns = n },
            "pwm_dither" | "pwm_dither_bits" => if let Ok(n) = v.parse() { c.pwm_dither = n },
            "scan_mode" => if let Ok(n) = v.parse() { c.scan_mode = n },
            "row_address_type" => if let Ok(n) = v.parse() { c.row_address_type = n },
            "multiplexing" => if let Ok(n) = v.parse() { c.multiplexing = n },
            "no_hardware_pulse" => c.no_hardware_pulse = matches!(v.as_str(), "1" | "true" | "yes" | "on"),
            "inverse_colors" => c.inverse_colors = matches!(v.as_str(), "1" | "true" | "yes" | "on"),
            "pixel_mapper" | "pixel_mapper_config" => c.pixel_mapper = v,
            "limit_refresh_hz" | "limit_refresh_rate_hz" => if let Ok(n) = v.parse() { c.limit_refresh_hz = n },
            "no_busy_waiting" | "disable_busy_waiting" => {
                c.no_busy_waiting = matches!(v.as_str(), "1" | "true" | "yes" | "on")
            }
            "rp1_pio" => if let Ok(n) = v.parse::<i32>() { c.rp1_pio = if n != 0 { 1 } else { 0 } },
            _ => {}
        }
    }
    c.normalize();
    let outcome = cfg.update(c.clone());
    match outcome {
        ApplyOutcome::SaveFailed(err) => {
            let msg = format!(
                r#"{{"ok":false,"error":"save failed: {}"}}"#,
                json_escape(&err)
            );
            return write_http(s, "500 Internal Server Error", "application/json; charset=utf-8",
                              msg.as_bytes());
        }
        ApplyOutcome::NeedsRestart => {
            // 运行时可热更的字段仍然立刻生效
            rt.brightness.store(c.brightness, Ordering::SeqCst);
            rt.idle.store(c.idle, Ordering::SeqCst);
            rt.show_clock.store(c.show_clock, Ordering::SeqCst);
            rt.clock_24h.store(c.clock_24h, Ordering::SeqCst);
            rt.lang_en.store(c.lang == "en", Ordering::SeqCst);
            write_http(s, "200 OK", "application/json; charset=utf-8",
                       r#"{"ok":true,"restart_required":true,"hint":"hardware/geometry settings changed — POST /api/restart"}"#.as_bytes())
        }
        ApplyOutcome::Applied => {
            rt.brightness.store(c.brightness, Ordering::SeqCst);
            rt.idle.store(c.idle, Ordering::SeqCst);
            rt.show_clock.store(c.show_clock, Ordering::SeqCst);
            rt.clock_24h.store(c.clock_24h, Ordering::SeqCst);
            rt.lang_en.store(c.lang == "en", Ordering::SeqCst);
            write_http(s, "200 OK", "application/json; charset=utf-8",
                       br#"{"ok":true,"restart_required":false}"#)
        }
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

/// 最近还在投送吗（默认 5 秒内算活跃）
pub fn stream_active(stream: &SharedStream) -> Option<(usize, usize, Vec<u8>)> {
    let s = stream.lock().ok()?;
    let at = s.at?;
    if at.elapsed().as_secs_f32() > 5.0 || s.rgb.is_empty() {
        return None;
    }
    Some((s.w, s.h, s.rgb.clone()))
}

/// 接收投送端（浏览器）发来的帧；客户端帧带掩码，需要解掩码。
/// `leftover` 是 HTTP 握手解析时 BufReader 多读出来的字节（通常是首帧开头）。
fn ws_receive(s: TcpStream, key: &str, stream: SharedStream, leftover: Vec<u8>)
              -> std::io::Result<()> {
    let mut s = s;
    let accept = base64(&sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()));
    s.write_all(format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\r\n"
    ).as_bytes())?;
    s.flush()?;

    let mut fr = FrameReader { sock: s, pending: leftover, pos: 0 };

    const MAX_FRAME: usize = 4 * 1024 * 1024;
    loop {
        let mut h = [0u8; 2];
        if fr.read_exact(&mut h).is_err() {
            return Ok(());
        }
        let opcode = h[0] & 0x0f;
        let masked = h[1] & 0x80 != 0;
        let mut len = (h[1] & 0x7f) as u64;
        if len == 126 {
            let mut b = [0u8; 2];
            if fr.read_exact(&mut b).is_err() {
                return Ok(());
            }
            len = u16::from_be_bytes(b) as u64;
        } else if len == 127 {
            let mut b = [0u8; 8];
            if fr.read_exact(&mut b).is_err() {
                return Ok(());
            }
            len = u64::from_be_bytes(b);
        }
        // `len as usize` can silently truncate a huge u64 on 32-bit targets and then
        // desync the stream; reject anything that cannot fit in usize / MAX_FRAME.
        let Ok(len_usize) = usize::try_from(len) else {
            return Ok(());
        };
        if len_usize > MAX_FRAME {
            return Ok(()); // 异常大帧，断开
        }
        let mut mask = [0u8; 4];
        if masked && fr.read_exact(&mut mask).is_err() {
            return Ok(());
        }
        let mut data = vec![0u8; len_usize];
        if fr.read_exact(&mut data).is_err() {
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
                    // checked: a crafted w*h*3 must not wrap and then slip past the length check
                    if let Some(need) = w.checked_mul(h).and_then(|n| n.checked_mul(3)) {
                        if w > 0 && h > 0 {
                            if let Some(total) = need.checked_add(4) {
                                if data.len() >= total {
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
                    }
                }
            }
            0x8 => return Ok(()),          // close
            0x9 => {
                // ping：必须回 pong，否则部分浏览器/代理会断开
                let mut pong = Vec::with_capacity(2 + data.len());
                pong.push(0x8a); // FIN + pong
                if data.len() < 126 {
                    pong.push(data.len() as u8);
                } else if data.len() < 65536 {
                    pong.push(126);
                    pong.extend_from_slice(&(data.len() as u16).to_be_bytes());
                } else {
                    pong.push(127);
                    pong.extend_from_slice(&(data.len() as u64).to_be_bytes());
                }
                pong.extend_from_slice(&data);
                if fr.sock.write_all(&pong).is_err() {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

/// 先读握手剩余缓冲，再读 socket，避免 BufReader 预读丢字节。
struct FrameReader {
    sock: TcpStream,
    pending: Vec<u8>,
    pos: usize,
}

impl FrameReader {
    fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        let mut filled = 0;
        while filled < buf.len() {
            if self.pos < self.pending.len() {
                let n = (self.pending.len() - self.pos).min(buf.len() - filled);
                buf[filled..filled + n]
                    .copy_from_slice(&self.pending[self.pos..self.pos + n]);
                self.pos += n;
                filled += n;
            } else {
                self.sock.read_exact(&mut buf[filled..])?;
                return Ok(());
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- WebSocket

fn ws_stream(mut s: TcpStream, key: &str, mirror: SharedMirror, _leftover: Vec<u8>) -> std::io::Result<()> {
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

const SETTINGS_HTML: &str = r#"<!doctype html>
<html lang="__LANG__">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title data-en="LED panel settings">LED 面板设置</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body { margin:0; min-height:100vh; background:#0b0d10; color:#c9d1d9;
         font:14px/1.5 system-ui,-apple-system,"Noto Sans CJK SC",sans-serif;
         padding:20px 16px 48px; }
  .wrap { max-width:720px; margin:0 auto; }
  header { display:flex; align-items:baseline; justify-content:space-between;
           gap:12px; flex-wrap:wrap; margin-bottom:18px; }
  h1 { font-size:18px; font-weight:600; margin:0; letter-spacing:.02em; }
  h1 .sub { font-weight:400; color:#6e7681; font-size:13px; margin-left:8px; }
  nav a { color:#58a6ff; text-decoration:none; margin-left:14px; font-size:13px; }
  nav a:hover { text-decoration:underline; }
  section { background:#0e1116; border:1px solid #21262d; border-radius:12px;
            padding:16px 18px; margin-bottom:14px; }
  section h2 { margin:0 0 12px; font-size:12px; font-weight:600; color:#8b949e;
               text-transform:uppercase; letter-spacing:.08em; }
  .grid { display:grid; grid-template-columns:repeat(auto-fill,minmax(160px,1fr)); gap:12px 14px; }
  label { display:flex; flex-direction:column; gap:5px; font-size:12px; color:#8b949e; }
  label span.tag { font-size:11px; color:#6e7681; }
  input, select { background:#161b22; color:#e6edf3; border:1px solid #30363d;
                  border-radius:8px; padding:8px 10px; font:inherit; width:100%; }
  input:focus, select:focus { outline:none; border-color:#58a6ff; }
  .check { flex-direction:row; align-items:center; gap:8px; padding-top:22px; }
  .check input { width:auto; }
  .row { display:flex; gap:10px; flex-wrap:wrap; align-items:center; margin-top:16px; }
  button { background:#21262d; color:#c9d1d9; border:1px solid #30363d; border-radius:8px;
           padding:8px 16px; cursor:pointer; font:inherit; }
  button:hover { background:#30363d; }
  button.primary { background:#238636; border-color:#2ea043; color:#fff; }
  button.primary:hover { background:#2ea043; }
  button.danger { background:#21262d; border-color:#f85149; color:#f85149; }
  button.danger:hover { background:#3d1418; }
  .status { font-size:13px; color:#8b949e; min-height:1.2em; }
  .status.ok { color:#3fb950; }
  .status.warn { color:#d29922; }
  .status.err { color:#f85149; }
  .meta { font-size:12px; color:#6e7681; line-height:1.7; }
  .meta b { color:#8b949e; font-weight:500; }
  .pill { display:inline-block; padding:1px 8px; border-radius:99px; font-size:11px;
          border:1px solid #30363d; color:#8b949e; }
  .pill.on { border-color:#238636; color:#3fb950; }
</style>
</head>
<body>
<div class="wrap">
  <header>
    <h1><span data-en="LED panel settings">LED 面板设置</span><span class="sub">rpi-led-webpush</span></h1>
    <nav>
      <a href="/" data-en="Send">投送</a>
      <a href="/view" data-en="Preview">预览</a>
      <a href="/settings" data-en="Settings">设置</a>
    </nav>
  </header>

  <section>
    <h2 data-en="Panel">面板</h2>
    <div class="grid">
      <label>行 rows<span class="tag">面板高度 / 1 块</span>
        <input id="rows" type="number" min="1" max="512"></label>
      <label>列 cols<span class="tag">面板宽度 / 1 块</span>
        <input id="cols" type="number" min="1" max="512"></label>
      <label>串联 chain<span class="tag">左右拼接</span>
        <input id="chain" type="number" min="1" max="32"></label>
      <label>并联 parallel<span class="tag">上下拼接</span>
        <input id="parallel" type="number" min="1" max="8"></label>
      <label>亮度 %<span class="tag" data-en="applies live">拖动即时生效</span>
        <input id="brightness" type="range" min="1" max="100" step="1" style="padding:0">
        <div style="display:flex;justify-content:space-between;font-size:11px;color:#6e7681">
          <span>1</span><span id="brightness_val">—</span><span>100</span>
        </div>
      </label>
      <label class="check"><input id="idle" type="checkbox"> <span data-en="Idle breathing">空闲呼吸</span></label>
      <label class="check"><input id="show_clock" type="checkbox"> <span data-en="Clock when idle">无投送时显示时钟</span></label>
      <label class="check"><input id="clock_24h" type="checkbox"> <span data-en="24-hour clock">24 小时制</span></label>
      <label>语言 Language
        <select id="lang">
          <option value="zh">中文</option>
          <option value="en">English</option>
        </select></label>
    </div>
    <div class="hint" style="color:#6e7681;font-size:12px;margin-top:8px"
         data-en="Clock shows on the panel while nothing is streaming. Turn off to fall back to idle breathing.">
      时钟在没有浏览器推流时显示于面板；关闭后回到空闲呼吸（或黑屏）。
    </div>
  </section>

  <section>
    <h2 data-en="Hardware">硬件</h2>
    <div class="grid">
      <label>GPIO 映射 mapping
        <select id="mapping">
          <option>regular</option>
          <option>adafruit-hat</option>
          <option>adafruit-hat-pwm</option>
          <option>classic</option>
          <option>compute-module</option>
          <option>regular-pi1</option>
        </select></label>
      <label>RGB 顺序 rgb-sequence<span class="tag">如绿蓝对调填 RBG</span>
        <input id="rgb_sequence" type="text" maxlength="3"></label>
      <label>网页端口 web-port<span class="tag">0 = 关闭网页</span>
        <input id="web_port" type="number" min="0" max="65535"></label>
    </div>
  </section>

  <section>
    <h2 data-en="Driver / init">驱动芯片 / 初始化</h2>
    <div class="grid">
      <label>面板类型 panel-type<span class="tag">特殊 IC 需要上电序列</span>
        <select id="panel_type">
          <option value="">通用（无初始化）</option>
          <option value="FM6126A">FM6126A</option>
          <option value="FM6127">FM6127</option>
        </select></label>
      <label>GPIO 减速 gpio-slowdown<span class="tag">0–4，花屏/丢色加大</span>
        <input id="gpio_slowdown" type="number" min="0" max="4"></label>
      <label>PWM 位深 pwm-bits<span class="tag">1–11，越低越省 CPU</span>
        <input id="pwm_bits" type="number" min="1" max="11"></label>
      <label>PWM LSB (ns)<span class="tag">0–200，鬼影可加大</span>
        <input id="pwm_lsb_ns" type="number" min="0" max="200"></label>
      <label>PWM 抖动 pwm-dither<span class="tag">0–2</span>
        <input id="pwm_dither" type="number" min="0" max="2"></label>
      <label>扫描 scan-mode
        <select id="scan_mode">
          <option value="0">progressive</option>
          <option value="1">interlaced</option>
        </select></label>
      <label>行地址 row-address-type<span class="tag">0–4，64×64 常用 1</span>
        <input id="row_address_type" type="number" min="0" max="4"></label>
      <label>复用 multiplexing<span class="tag">0=直驱，2=1:8 checker</span>
        <input id="multiplexing" type="number" min="0" max="16"></label>
      <label>限刷 limit-refresh-hz<span class="tag">0 = 不限</span>
        <input id="limit_refresh_hz" type="number" min="0" max="240"></label>
      <label>Pi 5 后端
        <select id="rp1_pio">
          <option value="1">RP1 PIO（省 CPU）</option>
          <option value="0">RP1 RIO</option>
        </select></label>
      <label>像素映射 pixel-mapper<span class="tag">如 Rotate:90</span>
        <input id="pixel_mapper" type="text" placeholder="留空 = 无"></label>
      <label class="check"><input id="no_hardware_pulse" type="checkbox"> 禁用硬件脉冲</label>
      <label class="check"><input id="inverse_colors" type="checkbox"> 反色</label>
      <label class="check"><input id="no_busy_waiting" type="checkbox"> 限刷时 sleep</label>
    </div>
    <div class="hint" style="color:#6e7681;font-size:12px;margin-top:10px">
      本组参数在面板初始化时读取，保存后需「重启设备」才生效。
    </div>
  </section>

  <section>
    <h2 data-en="Status">状态</h2>
    <div class="meta" id="meta">加载中…</div>
  </section>

  <div class="row">
    <button class="primary" id="save" data-en="Save">保存设置</button>
    <button id="reload" data-en="Reload">重新读取</button>
    <button class="danger" id="restart" data-en="Restart device">重启设备</button>
    <span class="status" id="st"></span>
  </div>
</div>
<script>
const L = '__LANG__';
const $ = (id) => document.getElementById(id);
if (L === 'en') {
  document.querySelectorAll('[data-en]').forEach(el => { el.textContent = el.getAttribute('data-en'); });
}
const FIELDS = [
  'rows','cols','chain','parallel','brightness','mapping','rgb_sequence','web_port','lang',
  'panel_type','gpio_slowdown','pwm_bits','pwm_lsb_ns','pwm_dither','scan_mode',
  'row_address_type','multiplexing','limit_refresh_hz','rp1_pio','pixel_mapper'
];
const CHECKS = ['idle','show_clock','clock_24h','no_hardware_pulse','inverse_colors','no_busy_waiting'];

function setStatus(msg, cls) {
  const el = $('st');
  el.textContent = msg;
  el.className = 'status' + (cls ? ' ' + cls : '');
}

function fillForm(s) {
  for (const f of FIELDS) {
    const el = $(f);
    if (!el) continue;
    if (f === 'brightness') {
      el.value = s.brightness;
      $('brightness_val').textContent = s.brightness + '%';
      continue;
    }
    el.value = (s[f] === undefined || s[f] === null) ? '' : s[f];
  }
  for (const f of CHECKS) {
    const el = $(f);
    if (el) el.checked = !!s[f];
  }
}

// 亮度滑杆：拖动即 POST /api/brightness（节流），松手再存一次保证落盘
let brightTimer = 0;
function pushBrightness(v, save) {
  const url = save
    ? '/api/brightness?brightness=' + v
    : '/api/brightness?brightness=' + v + '&live=1';
  fetch(url, { method: 'POST' }).catch(() => {});
}
function onBrightnessInput(e) {
  const v = e.target.value;
  $('brightness_val').textContent = v + '%';
  clearTimeout(brightTimer);
  brightTimer = setTimeout(() => pushBrightness(v, false), 80);
}
function onBrightnessChange(e) {
  clearTimeout(brightTimer);
  pushBrightness(e.target.value, true);
}
$('brightness').addEventListener('input', onBrightnessInput);
$('brightness').addEventListener('change', onBrightnessChange);

async function load() {
  try {
    const r = await fetch('/api/config');
    const j = await r.json();
    fillForm(j.settings);
    const rt = j.runtime;
    $('meta').innerHTML =
      '<div>面板逻辑分辨率 <b>' + rt.panel_width + ' × ' + rt.panel_height + '</b>' +
      ' <span class="pill' + (rt.streaming ? ' on' : '') + '">' +
      (rt.streaming ? '推流中' : '空闲') + '</span></div>' +
      '<div>当前亮度 <b>' + rt.brightness_now + '%</b></div>' +
      '<div>配置文件 <b>' + rt.config_path + '</b></div>';
  } catch (e) {
    setStatus('读取失败：' + e, 'err');
  }
}

async function save() {
  const p = new URLSearchParams();
  for (const f of FIELDS) p.set(f, $(f).value);
  for (const f of CHECKS) p.set(f, $(f).checked ? '1' : '0');
  setStatus('保存中…');
  try {
    const r = await fetch('/api/config', {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: p.toString()
    });
    const j = await r.json();
    if (j.restart_required) {
      setStatus(L==='en' ? 'Saved. Hardware/geometry changed — restart the device.'
                         : '已保存。硬件/几何参数已改，需重启设备生效。', 'warn');
    } else {
      setStatus(L==='en' ? 'Saved and applied.' : '已保存并生效。', 'ok');
    }
    // 语言变更后刷新，让 data-en 立即套用
    if (L !== $('lang').value) {
      setTimeout(() => location.reload(), 400);
    } else {
      load();
    }
  } catch (e) {
    setStatus((L==='en'?'Save failed: ':'保存失败：') + e, 'err');
  }
}

async function restart() {
  if (!confirm(L==='en' ? 'Restart rpi-led-webpush? Streaming will drop.' : '确认重启 rpi-led-webpush？推流会中断。')) return;
  try {
    await fetch('/api/restart', { method: 'POST' });
    setStatus('已请求重启，稍候…', 'warn');
    setTimeout(() => location.reload(), 3000);
  } catch (e) {
    setStatus('重启失败：' + e, 'err');
  }
}

$('save').onclick = save;
$('reload').onclick = load;
$('restart').onclick = restart;
load();
</script>
</body>
</html>
"#;

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="__LANG__">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title data-en="LED panel preview">LED 面板实时预览</title>
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
    <label style="color:#6e7681;font-size:12px;display:flex;align-items:center;gap:6px">
      <span data-en="Bright">亮度</span>
      <input id="bright" type="range" min="1" max="100" value="60" style="width:110px;vertical-align:middle">
      <span id="bright_v">60%</span>
    </label>
    <button id="fs" data-en="Fullscreen">全屏</button>
    <a href="/settings" style="color:#58a6ff;text-decoration:none;margin-left:8px" data-en="Settings">设置</a>
    <a href="/" style="color:#58a6ff;text-decoration:none;margin-left:12px" data-en="Send">投送</a>
  </div>
  <div class="hint" data-en="What you see is what the panel is showing right now">画面即 LED 面板正在显示的内容，随窗口自适应缩放</div>
<script>
const L = '__LANG__';
if (L === 'en') {
  document.documentElement.lang = 'en';
  document.querySelectorAll('[data-en]').forEach(el => { el.textContent = el.getAttribute('data-en'); });
}
const c = document.getElementById('c');
const ctx = c.getContext('2d', { alpha: false });
let img = null, frames = 0, t0 = performance.now(), skipped = 0;

function setGeo(w, h) {
  c.width = w; c.height = h;
  document.documentElement.style.setProperty('--ar', (w / h).toFixed(4));
  document.getElementById('geo').textContent = w + ' × ' + h;
  img = ctx.createImageData(w, h);
}
setGeo(c.width || 64, c.height || 32);

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
    if (!img || c.width !== w || c.height !== h) setGeo(w, h);
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

// 实时亮度
const bright = document.getElementById('bright');
const brightV = document.getElementById('bright_v');
let brightTimer = 0;
function pushBright(v, save) {
  fetch('/api/brightness?brightness=' + v, { method: 'POST' }).catch(() => {});
}
bright.addEventListener('input', (e) => {
  brightV.textContent = e.target.value + '%';
  clearTimeout(brightTimer);
  brightTimer = setTimeout(() => pushBright(e.target.value), 80);
});
bright.addEventListener('change', (e) => {
  clearTimeout(brightTimer);
  pushBright(e.target.value, true);
});
fetch('/api/brightness').then(r => r.json()).then(j => {
  if (j.brightness) {
    bright.value = j.brightness;
    brightV.textContent = j.brightness + '%';
  }
}).catch(() => {});
</script>
</body>
</html>
"#;

// ---------------------------------------------------------------- 投送页（浏览器 → 面板）

const SENDER_HTML: &str = r#"<!doctype html>
<html lang="__LANG__">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title data-en="Send video to LED panel">投送视频到 LED 面板</title>
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
    <label style="color:#6e7681;font-size:12px;display:flex;align-items:center;gap:6px">
      <span data-en="Bright">亮度</span>
      <input id="bright" type="range" min="1" max="100" value="60" style="width:110px">
      <span id="bright_v">60%</span>
    </label>
    <a href="/view" style="color:#58a6ff;text-decoration:none" data-en="Preview">预览</a>
    <a href="/settings" style="color:#58a6ff;text-decoration:none" data-en="Settings">设置</a>
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
  <div class="hint" data-en="While streaming we request a screen Wake Lock. In a background tab rVFC/timers are throttled, so a Worker + silent-audio ticker keeps sending (capped ~20fps). Keep the tab visible if it still stutters.<br>Preview is the panel's logical resolution and follows chained/parallel layout. Decode and scale all happen in your browser.">
    投送期间会申请「屏幕常亮」以免息屏中断。切入后台后 rVFC/定时器会被浏览器限流，<br>
    本页用 Worker + 静音音频节拍继续抓帧发送（约 20fps 封顶）；若仍不流畅，请保持页面在前台。<br>
    预览即面板上正在显示的画面（画布就是面板的逻辑分辨率，随链屏/并联屏自动变化）。
    解码和缩放都在你的浏览器里完成，树莓派只接收小尺寸帧，CPU 占用极低。</div>
  <video id="v" muted loop playsinline></video>
<script>
const L = '__LANG__';
if (L === 'en') {
  document.documentElement.lang = 'en';
  document.querySelectorAll('[data-en]').forEach(el => { el.textContent = el.getAttribute('data-en'); });
}
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
  } else if (playing) {
    // 切到后台：rAF / rVFC / 普通定时器都会被限流，立刻用保活通道补一帧
    if (audioCtx && audioCtx.state === 'suspended') audioCtx.resume();
    pumpFrame(true);
  }
});

// 发送一帧（两条驱动路径共用）
let lastSendAt = 0;
function pumpFrame(force) {
  if (!playing || !ws || ws.readyState !== 1) return false;
  const now = performance.now();
  // 后台音频/Worker 节拍比视频帧率高，封顶约 20fps，避免同一帧重复推爆链路
  if (!force && now - lastSendAt < 50) return false;
  if (ws.bufferedAmount > 64 * 1024) { skipped++; return false; }
  if (!sendOneFrame()) return false;
  lastSendAt = now;
  return true;
}
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
  // 前台：rVFC 是主驱动；后台 rVFC 可能仍低频触发，直接发即可
  if (playing && sendOneFrame()) srcFrames++;
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

// 后台是否需要保活通道主动发帧：rVFC 已停，或整页被隐藏。
function backgroundDrive() {
  return playing && (document.hidden || performance.now() - lastVideoFrameAt > 400);
}

// 兜底一：主线程定时器。后台会被限流到约 1Hz，只作最后防线。
setInterval(() => {
  if (backgroundDrive()) pumpFrame(false);
}, 100);

// 兜底二：Worker 定时器。后台限流比主线程轻，是丢帧的主要补手。
let bgWorker = null;
function startWorkerTicker() {
  if (bgWorker) return;
  try {
    const src = `let t=null;onmessage=e=>{if(e.data==='run'){clearInterval(t);t=setInterval(()=>postMessage(1),50)}else{clearInterval(t)}}`;
    bgWorker = new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })));
    bgWorker.onmessage = () => { if (backgroundDrive()) pumpFrame(false); };
    bgWorker.postMessage('run');
  } catch (e) { bgWorker = null; }
}

// 兜底三：AudioContext 节拍器。音频回调在后台通常不被限流，是最靠谱的一路。
// 注意：gain 不能是 0，部分浏览器会把「完全静音」的图收掉、不再回调；用 1e-12 的
// 不可闻振荡器保活。buffer 取 1024（约 43Hz），够驱动 20fps 的封顶。
let audioCtx = null;
function startTicker() {
  startWorkerTicker();
  if (audioCtx) {
    if (audioCtx.state === 'suspended') audioCtx.resume();
    return;
  }
  try {
    const AC = window.AudioContext || window.webkitAudioContext;
    audioCtx = new AC();
    const osc = audioCtx.createOscillator();
    osc.frequency.value = 440;
    const gain = audioCtx.createGain();
    gain.gain.value = 1e-12;                   // 听不见，但管线保持 active
    const proc = audioCtx.createScriptProcessor(1024, 1, 1);
    proc.onaudioprocess = () => { if (backgroundDrive()) pumpFrame(false); };
    osc.connect(gain); gain.connect(proc); proc.connect(audioCtx.destination);
    osc.start();
  } catch (e) { audioCtx = null; }
}

function stopStream() {
  if (v.srcObject) {
    for (const t of v.srcObject.getTracks()) t.stop();
    v.srcObject = null;
  }
  if (v.src && v.src.startsWith('blob:')) {
    URL.revokeObjectURL(v.src);
    v.src = '';
  }
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
  stopStream();
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
    stopStream();
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
    stopStream();
    v.srcObject = s;
    s.getVideoTracks()[0].addEventListener('ended', () => {
      playing = false;
      document.getElementById('st').textContent = '屏幕共享已结束';
      document.getElementById('play').textContent = '继续';
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

// 实时亮度
const bright = document.getElementById('bright');
const brightV = document.getElementById('bright_v');
let brightTimer = 0;
function pushBright(v) {
  fetch('/api/brightness?brightness=' + v, { method: 'POST' }).catch(() => {});
}
bright.addEventListener('input', (e) => {
  brightV.textContent = e.target.value + '%';
  clearTimeout(brightTimer);
  brightTimer = setTimeout(() => pushBright(e.target.value), 80);
});
bright.addEventListener('change', (e) => {
  clearTimeout(brightTimer);
  pushBright(e.target.value);
});
fetch('/api/brightness').then(r => r.json()).then(j => {
  if (j.brightness) {
    bright.value = j.brightness;
    brightV.textContent = j.brightness + '%';
  }
}).catch(() => {});
</script>
</body>
</html>
"#;
