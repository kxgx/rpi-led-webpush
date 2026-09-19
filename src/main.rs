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
//!   browser  --HTTP-->  GET /            sender page (embedded)
//!            --WS---->  /push           binary frames: [w u16][h u16][RGB...]
//!            --HTTP-->  GET /geo        logical resolution, e.g. "64 32"
//!            --WS---->  /ws             preview of what the panel shows (server → browser)
//! ```

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

struct Args {
    rows: i32,
    cols: i32,
    chain: i32,
    parallel: i32,
    brightness: i32,
    mapping: String,
    rgb_sequence: String,
    web_port: u16,
    idle: bool,
}

fn take_value(argv: &[String], i: &mut usize) -> Option<String> {
    *i += 1;
    argv.get(*i).cloned()
}

fn parse_args() -> Args {
    let mut a = Args {
        rows: 32,
        cols: 64,
        chain: 1,
        parallel: 1,
        brightness: 60,
        mapping: "regular".to_string(),
        rgb_sequence: "RGB".to_string(),
        web_port: 8080,
        idle: true,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--rows" => if let Some(v) = take_value(&argv, &mut i) { a.rows = v.parse().unwrap_or(a.rows) },
            "--cols" => if let Some(v) = take_value(&argv, &mut i) { a.cols = v.parse().unwrap_or(a.cols) },
            "--chain" => if let Some(v) = take_value(&argv, &mut i) { a.chain = v.parse().unwrap_or(a.chain) },
            "--parallel" => if let Some(v) = take_value(&argv, &mut i) { a.parallel = v.parse().unwrap_or(a.parallel) },
            "--brightness" => if let Some(v) = take_value(&argv, &mut i) { a.brightness = v.parse().unwrap_or(a.brightness) },
            "--web-port" => if let Some(v) = take_value(&argv, &mut i) { a.web_port = v.parse().unwrap_or(a.web_port) },
            "--mapping" => if let Some(v) = take_value(&argv, &mut i) { a.mapping = v },
            "--rgb-sequence" => if let Some(v) = take_value(&argv, &mut i) { a.rgb_sequence = v },
            "--no-idle" => a.idle = false,
            "--help" | "-h" => {
                println!("usage: rpi-led-webpush [--rows 32] [--cols 64] [--chain 1] [--parallel 1]");
                println!("                [--brightness 60] [--web-port 8080] [--no-idle]");
                println!("                [--mapping NAME] [--rgb-sequence RGB]");
                exit(0);
            }
            other => {
                eprintln!("unknown argument: {other} (try --help)");
                exit(2);
            }
        }
        i += 1;
    }
    a
}

/// Refuse to start if another process may already be driving the panel: the RP1 PIO block has only
/// four state machines and concurrent drivers corrupt each other's state.
fn guard() {
    let me = std::process::id();
    let self_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_string_lossy().to_string()))
        .unwrap_or_default();
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
            if matches!(exe_base, "bash" | "sh" | "sudo" | "timeout" | "nohup" | "env" | "setsid") {
                continue;
            }
            let cwd = fs::read_link(format!("/proc/{pid}/cwd"))
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if cmd.contains("led-") || (!self_dir.is_empty() && cwd == self_dir) {
                others.push((pid.to_string(), cmd.chars().take(110).collect()));
            }
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

fn push_frame(canvas: *mut ffi::LedCanvas, pw: i32, ph: i32, sw: usize, sh: usize, rgb: &[u8]) {
    if pw <= 0 || ph <= 0 || sw == 0 || sh == 0 || rgb.len() < sw * sh * 3 {
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
    let args = parse_args();
    guard();
    unsafe {
        ffi::register_signal(ffi::SIGINT, on_signal);
        ffi::register_signal(ffi::SIGTERM, on_signal);
    }

    let mapping = CString::new(args.mapping.clone()).unwrap();
    let seq = CString::new(args.rgb_sequence.clone()).unwrap();
    let mut opts: ffi::RGBLedMatrixOptions = unsafe { std::mem::zeroed() };
    opts.hardware_mapping = mapping.as_ptr();
    opts.rows = args.rows;
    opts.cols = args.cols;
    opts.chain_length = args.chain;
    opts.parallel = args.parallel;
    opts.brightness = args.brightness;
    opts.pwm_bits = 11;
    opts.led_rgb_sequence = seq.as_ptr();

    let mut rt: ffi::RGBLedRuntimeOptions = unsafe { std::mem::zeroed() };
    rt.rp1_pio = 1; // Raspberry Pi 5: the PIO backend is the low-CPU one

    let matrix = unsafe { ffi::led_matrix_create_from_options_and_rt_options(&mut opts, &mut rt) };
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
    if args.web_port > 0 {
        web::spawn_web_server(
            args.web_port,
            Arc::clone(&mirror),
            Arc::clone(&stream),
            (lw as usize, lh as usize),
            Arc::clone(&stop),
        );
        println!("sender page : http://<device>:{}/", args.web_port);
        println!("panel preview: http://<device>:{}/view", args.web_port);
    }
    println!(
        "panel {}x{} (chain {}, parallel {}) mapping={} rgb-sequence={} brightness={}%",
        args.cols, args.rows, args.chain, args.parallel, args.mapping, args.rgb_sequence,
        args.brightness
    );
    println!("waiting for a browser to push video… Ctrl-C to stop");

    let mut phase: f32 = 0.0;
    let mut was_streaming = false;
    while !STOP.load(Ordering::SeqCst) {
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
        // idle: a slow dim-blue breathing so it is obvious the program is alive
        if args.idle {
            phase = (phase + 0.04) % std::f32::consts::TAU;
            let v = (6.0 + 6.0 * (phase.sin() + 1.0)) as u8;
            unsafe { ffi::led_canvas_fill(canvas, 0, 0, v) };
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
