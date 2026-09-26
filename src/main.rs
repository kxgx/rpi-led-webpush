//! Push video from a browser to a HUB75 LED matrix panel.
//!
//! Zero third-party dependencies: the HTTP server, the WebSocket framing, SHA-1 and base64 are
//! all implemented here, and the LED matrix is driven through the C API of
//! `rpi-rgb-led-matrix` with hand-written FFI declarations.
//!
//! A browser opens the built-in page, picks a video file / camera / screen share, and the browser
//! decodes and scales it to the panel's logical resolution — only small frames (~6 KB for 64×32)
//! are sent over the WebSocket, so the device driving the panel stays nearly idle.
//!
//! ```text
//!   browser  --HTTP-->  GET /              sender page (embedded)
//!            --WS---->  /push             binary frames: [w u16][h u16][RGB...]
//!            --HTTP-->  GET /geo          logical resolution, e.g. "64 32"
//!            --WS---->  /ws               preview of what the panel shows (server → browser)
//!            --HTTP-->  GET /settings     device settings page
//!            --HTTP-->  GET|POST /api/config
//!            --HTTP-->  POST /api/restart
//! ```
//!
//! Settings persist to a `key=value` file (see `config.rs`). CLI flags override the file
//! for the current run; the web UI writes the file and applies hot settings immediately.

mod clock;
mod config;
mod ffi;
mod web;

use std::ffi::CString;
use std::fs;
use std::process::exit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: std::ffi::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn take_value(argv: &[String], i: &mut usize) -> Option<String> {
    *i += 1;
    argv.get(*i).cloned()
}

/// 把 CLI 覆盖到配置上（只覆盖显式给出的参数）。
fn apply_cli(c: &mut config::Config) {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--rows" => if let Some(v) = take_value(&argv, &mut i) { c.rows = v.parse().unwrap_or(c.rows) },
            "--cols" => if let Some(v) = take_value(&argv, &mut i) { c.cols = v.parse().unwrap_or(c.cols) },
            "--chain" => if let Some(v) = take_value(&argv, &mut i) { c.chain = v.parse().unwrap_or(c.chain) },
            "--parallel" => if let Some(v) = take_value(&argv, &mut i) { c.parallel = v.parse().unwrap_or(c.parallel) },
            "--brightness" => if let Some(v) = take_value(&argv, &mut i) { c.brightness = v.parse().unwrap_or(c.brightness) },
            "--web-port" => if let Some(v) = take_value(&argv, &mut i) { c.web_port = v.parse().unwrap_or(c.web_port) },
            "--mapping" => if let Some(v) = take_value(&argv, &mut i) { c.mapping = v },
            "--rgb-sequence" => if let Some(v) = take_value(&argv, &mut i) { c.rgb_sequence = v },
            "--no-idle" => c.idle = false,
            "--no-clock" => c.show_clock = false,
            "--clock-12h" => c.clock_24h = false,
            // 硬件驱动
            "--panel-type" | "--driver" => if let Some(v) = take_value(&argv, &mut i) { c.panel_type = v },
            "--gpio-slowdown" => if let Some(v) = take_value(&argv, &mut i) { c.gpio_slowdown = v.parse().unwrap_or(c.gpio_slowdown) },
            "--pwm-bits" => if let Some(v) = take_value(&argv, &mut i) { c.pwm_bits = v.parse().unwrap_or(c.pwm_bits) },
            "--pwm-lsb-ns" => if let Some(v) = take_value(&argv, &mut i) { c.pwm_lsb_ns = v.parse().unwrap_or(c.pwm_lsb_ns) },
            "--pwm-dither" => if let Some(v) = take_value(&argv, &mut i) { c.pwm_dither = v.parse().unwrap_or(c.pwm_dither) },
            "--scan-mode" => if let Some(v) = take_value(&argv, &mut i) { c.scan_mode = v.parse().unwrap_or(c.scan_mode) },
            "--row-addr-type" => if let Some(v) = take_value(&argv, &mut i) { c.row_address_type = v.parse().unwrap_or(c.row_address_type) },
            "--multiplexing" => if let Some(v) = take_value(&argv, &mut i) { c.multiplexing = v.parse().unwrap_or(c.multiplexing) },
            "--no-hardware-pulse" => c.no_hardware_pulse = true,
            "--inverse" => c.inverse_colors = true,
            "--pixel-mapper" => if let Some(v) = take_value(&argv, &mut i) { c.pixel_mapper = v },
            "--limit-refresh" => if let Some(v) = take_value(&argv, &mut i) { c.limit_refresh_hz = v.parse().unwrap_or(c.limit_refresh_hz) },
            "--no-busy-waiting" => c.no_busy_waiting = true,
            "--rp1-rio" => c.rp1_pio = 0,
            "--help" | "-h" => {
                println!("usage: rpi-led-webpush [options]");
                println!("  panel:  --rows --cols --chain --parallel --brightness --no-idle");
                println!("          --no-clock --clock-12h");
                println!("  wiring: --mapping NAME --rgb-sequence RGB");
                println!("  web:    --web-port 8080");
                println!("  driver: --panel-type TYPE   (FM6126A / FM6127, empty = generic)");
                println!("          --gpio-slowdown 0..4 --pwm-bits 1..11 --pwm-lsb-ns NS");
                println!("          --pwm-dither 0..2 --scan-mode 0|1 --row-addr-type 0..4");
                println!("          --multiplexing N --no-hardware-pulse --inverse");
                println!("          --pixel-mapper STR --limit-refresh HZ --no-busy-waiting");
                println!("          --rp1-rio   (Pi 5: use RIO instead of PIO backend)");
                println!();
                println!("Settings are also stored in a config file (LED_CONFIG or ./rpi-led-webpush.conf)");
                println!("and can be edited at http://<device>:PORT/settings");
                exit(0);
            }
            other => {
                eprintln!("unknown argument: {other} (try --help)");
                exit(2);
            }
        }
        i += 1;
    }
    c.normalize();
}

/// Refuse to start if another process may already be driving the panel: the RP1 PIO block has only
/// four state machines and concurrent drivers corrupt each other's state.
fn guard() {
    let me = std::process::id();
    let mut others: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if pid == me {
                continue;
            }
            let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline")) else { continue };
            let cmd = String::from_utf8_lossy(&raw).replace('\0', " ");
            let cmd = cmd.trim().to_string();
            if cmd.is_empty() || cmd.contains("--dump") || cmd.contains("--help") {
                continue;
            }
            let exe = cmd.split_whitespace().next().unwrap_or("");
            let exe_base = exe.rsplit('/').next().unwrap_or(exe);
            // Only treat executables whose name looks like an LED matrix driver as conflicts.
            // Matching any cmdline containing "led-" would false-positive on grep/journalctl/vim.
            if !is_led_driver(exe_base) {
                continue;
            }
            others.push((pid.to_string(), cmd.chars().take(110).collect()));
        }
    }
    if others.is_empty() {
        return;
    }
    eprintln!("! another process may already be driving the panel:");
    for (pid, cmd) in &others {
        eprintln!("    pid {pid:<7} {cmd}");
    }
    eprintln!("  concurrent drivers corrupt each other (and the PIO state machines are shared).");
    if std::env::var("LED_FORCE").as_deref() == Ok("1") {
        eprintln!("  LED_FORCE=1 set — continuing anyway.");
        return;
    }
    eprintln!("  stop it first, or set LED_FORCE=1 to override.");
    exit(2);
}

/// Executable basenames that indicate another LED-matrix driver process.
fn is_led_driver(exe_base: &str) -> bool {
    let n = exe_base.to_ascii_lowercase();
    // this binary, led-stats, and the upstream demo/test binaries
    n == "rpi-led-webpush"
        || n.starts_with("led-")
        || n.starts_with("led_")
        || n.ends_with("-led")
        || n.contains("rgb-led-matrix")
        || n.contains("led-matrix")
        || n == "clockwise"
}

fn push_frame(canvas: *mut ffi::LedCanvas, pw: i32, ph: i32, sw: usize, sh: usize, rgb: &[u8]) {
    if pw <= 0 || ph <= 0 || sw == 0 || sh == 0 {
        return;
    }
    let Some(need) = sw.checked_mul(sh).and_then(|n| n.checked_mul(3)) else {
        return;
    };
    if rgb.len() < need {
        return;
    }
    // nearest-neighbour, in case the sender used a different size than the panel
    let mut buf = vec![ffi::Color::default(); (pw * ph) as usize];
    for y in 0..ph as usize {
        let sy = (y * sh / ph as usize).min(sh - 1);
        for x in 0..pw as usize {
            let sx = (x * sw / pw as usize).min(sw - 1);
            let i = (sy * sw + sx) * 3;
            buf[y * pw as usize + x] = ffi::Color { r: rgb[i], g: rgb[i + 1], b: rgb[i + 2] };
        }
    }
    unsafe { ffi::led_canvas_set_pixels(canvas, 0, 0, pw, ph, buf.as_ptr()) };
}

fn main() {
    let shared_cfg = config::open();
    let mut cfg = shared_cfg.get();
    apply_cli(&mut cfg);
    // CLI 覆盖并入内存配置，设置页显示的就是当前生效值；仅网页保存时写盘
    shared_cfg.set_memory(cfg.clone());
    let args = cfg.clone();
    guard();
    unsafe {
        ffi::register_signal(ffi::SIGINT, on_signal);
        ffi::register_signal(ffi::SIGTERM, on_signal);
    }

    let mapping = CString::new(args.mapping.clone()).unwrap();
    let seq = CString::new(args.rgb_sequence.clone()).unwrap();
    // panel_type / pixel_mapper 允许为空 → 传 NULL，库走默认
    let panel_type = CString::new(args.panel_type.clone()).unwrap();
    let pixel_mapper = CString::new(args.pixel_mapper.clone()).unwrap();
    let mut opts: ffi::RGBLedMatrixOptions = unsafe { std::mem::zeroed() };
    opts.hardware_mapping = mapping.as_ptr();
    opts.rows = args.rows;
    opts.cols = args.cols;
    opts.chain_length = args.chain;
    opts.parallel = args.parallel;
    opts.brightness = args.brightness;
    opts.pwm_bits = args.pwm_bits;
    opts.pwm_lsb_nanoseconds = args.pwm_lsb_ns;
    opts.pwm_dither_bits = args.pwm_dither;
    opts.scan_mode = args.scan_mode;
    opts.row_address_type = args.row_address_type;
    opts.multiplexing = args.multiplexing;
    opts.disable_hardware_pulsing = args.no_hardware_pulse;
    opts.inverse_colors = args.inverse_colors;
    opts.limit_refresh_rate_hz = args.limit_refresh_hz;
    opts.disable_busy_waiting = args.no_busy_waiting;
    opts.led_rgb_sequence = seq.as_ptr();
    opts.panel_type = if args.panel_type.is_empty() {
        std::ptr::null()
    } else {
        panel_type.as_ptr()
    };
    opts.pixel_mapper_config = if args.pixel_mapper.is_empty() {
        std::ptr::null()
    } else {
        pixel_mapper.as_ptr()
    };

    let mut rt_opts: ffi::RGBLedRuntimeOptions = unsafe { std::mem::zeroed() };
    // RT_OPT_COPY_IF_SET：0 = 沿用默认。drop_privileges 必须显式 -1，否则会 setuid 到
    // daemon，配置文件写不进 /root。daemon 保持 0（前台 + 允许刷新线程），不要用 -1。
    rt_opts.drop_privileges = -1;
    rt_opts.gpio_slowdown = args.gpio_slowdown;
    rt_opts.rp1_pio = args.rp1_pio; // 1 = Pi 5 PIO (low CPU), 0 = RIO

    let matrix = unsafe { ffi::led_matrix_create_from_options_and_rt_options(&mut opts, &mut rt_opts) };
    if matrix.is_null() {
        eprintln!("could not initialise the panel (mapping / power / permissions?)");
        exit(1);
    }
    let mut canvas = unsafe { ffi::led_matrix_create_offscreen_canvas(matrix) };
    if canvas.is_null() {
        eprintln!("could not create a canvas");
        unsafe { ffi::led_matrix_delete(matrix) };
        exit(1);
    }

    let (mut lw, mut lh) = (0i32, 0i32);
    unsafe { ffi::led_canvas_get_size(canvas, &mut lw, &mut lh) };

    let stop = Arc::new(AtomicBool::new(false));
    let mirror = web::new_mirror(lw as usize, lh as usize);
    let stream = web::new_stream();
    let ctl = web::new_runtime(args.brightness, args.idle);
    ctl.show_clock.store(args.show_clock, Ordering::SeqCst);
    ctl.clock_24h.store(args.clock_24h, Ordering::SeqCst);
    ctl.lang_en.store(args.lang == "en", Ordering::SeqCst);
    if args.web_port > 0 {
        web::spawn_web_server(
            args.web_port,
            Arc::clone(&mirror),
            Arc::clone(&stream),
            (lw as usize, lh as usize),
            Arc::clone(&stop),
            shared_cfg.clone(),
            Arc::clone(&ctl),
        );
        println!("sender page : http://<device>:{}/", args.web_port);
        println!("panel preview: http://<device>:{}/view", args.web_port);
        println!("settings page: http://<device>:{}/settings", args.web_port);
    }
    println!(
        "panel {}x{} (chain {}, parallel {}) mapping={} rgb-sequence={} brightness={}%",
        args.cols, args.rows, args.chain, args.parallel, args.mapping, args.rgb_sequence,
        args.brightness
    );
    println!("config file: {}", shared_cfg.path().display());
    println!("waiting for a browser to push video… Ctrl-C to stop");

    let mut phase: f32 = 0.0;
    let mut was_streaming = false;
    let mut last_brightness = args.brightness;
    let mut clock_buf: Vec<ffi::Color> = vec![ffi::Color::default(); (lw * lh) as usize];
    while !STOP.load(Ordering::SeqCst) {
        if ctl.restart.load(Ordering::SeqCst) {
            println!("restart requested from web UI");
            break;
        }
        // 网页改亮度后热更新，无需重开面板
        let want = ctl.brightness.load(Ordering::SeqCst).clamp(1, 100);
        if want != last_brightness {
            unsafe { ffi::led_matrix_set_brightness(matrix, want as u8) };
            last_brightness = want;
            println!("brightness -> {want}%");
        }
        if let Some((sw, sh, rgb)) = web::stream_active(&stream) {
            push_frame(canvas, lw, lh, sw, sh, &rgb);
            canvas = unsafe { ffi::led_matrix_swap_on_vsync(matrix, canvas) };
            if let Ok(mut m) = mirror.lock() {
                m.rgb.clear();
                m.rgb.extend_from_slice(&rgb);
                m.w = sw;
                m.h = sh;
                m.seq += 1;
            }
            if !was_streaming {
                was_streaming = true;
                println!("streaming {}x{} -> panel {}x{}", sw, sh, lw, lh);
            }
            thread::sleep(Duration::from_millis(15));
            continue;
        }
        if was_streaming {
            was_streaming = false;
            println!("stream stopped");
        }
        // idle：默认时钟/日期；可关掉后退回呼吸或黑屏
        if ctl.show_clock.load(Ordering::SeqCst) {
            let t = clock::now_local();
            let use24h = ctl.clock_24h.load(Ordering::SeqCst);
            let lang = if ctl.lang_en.load(Ordering::SeqCst) { "en" } else { "zh" };
            if clock_buf.len() != (lw * lh) as usize {
                clock_buf = vec![ffi::Color::default(); (lw * lh) as usize];
            }
            clock::draw_clock(&mut clock_buf, lw as usize, lh as usize, &t, use24h, lang);
            clock::blit(canvas, lw, lh, &clock_buf);
            canvas = unsafe { ffi::led_matrix_swap_on_vsync(matrix, canvas) };
            if let Ok(mut m) = mirror.lock() {
                if m.w != lw as usize || m.h != lh as usize || m.rgb.len() != (lw * lh * 3) as usize {
                    m.w = lw as usize;
                    m.h = lh as usize;
                    m.rgb.resize((lw * lh * 3) as usize, 0);
                }
                for (i, px) in clock_buf.iter().enumerate() {
                    let o = i * 3;
                    m.rgb[o] = px.r;
                    m.rgb[o + 1] = px.g;
                    m.rgb[o + 2] = px.b;
                }
                m.seq += 1;
                m.page = "clock".to_string();
            }
        } else if ctl.idle.load(Ordering::SeqCst) {
            phase = (phase + 0.04) % std::f32::consts::TAU;
            let v = (6.0 + 6.0 * (phase.sin() + 1.0)) as u8;
            unsafe { ffi::led_canvas_fill(canvas, 0, 0, v) };
            canvas = unsafe { ffi::led_matrix_swap_on_vsync(matrix, canvas) };
            // keep /view preview in sync with what the panel is actually showing
            if let Ok(mut m) = mirror.lock() {
                if m.w != lw as usize || m.h != lh as usize || m.rgb.len() != (lw * lh * 3) as usize {
                    m.w = lw as usize;
                    m.h = lh as usize;
                    m.rgb.resize((lw * lh * 3) as usize, 0);
                }
                for px in m.rgb.chunks_exact_mut(3) {
                    px[0] = 0;
                    px[1] = 0;
                    px[2] = v;
                }
                m.seq += 1;
                m.page.clear();
            }
        } else {
            // 空闲且关闭了呼吸/时钟：保持黑屏，但仍刷新 mirror 以免预览漂在旧帧上
            unsafe { ffi::led_canvas_fill(canvas, 0, 0, 0) };
            canvas = unsafe { ffi::led_matrix_swap_on_vsync(matrix, canvas) };
        }
        thread::sleep(Duration::from_millis(40));
    }

    stop.store(true, Ordering::SeqCst);
    unsafe {
        ffi::led_canvas_clear(canvas);
        let _ = ffi::led_matrix_swap_on_vsync(matrix, canvas);
        ffi::led_matrix_delete(matrix);
    }
    println!("\ncleared and stopped.");
}
