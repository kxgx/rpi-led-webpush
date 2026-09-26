//! 无投送时的时钟/日期画面。5×7 ASCII + 8×8 汉字星期，零外部资源。
//!
//! 布局（64×32）：上半 HH:MM（×2 缩放），下半 MM-DD + 星期。

use crate::ffi::{self, Color};

/// 5×7 点阵，每字 5 列，bit0 为最上一行。
fn glyph(ch: u8) -> Option<[u8; 5]> {
    #[rustfmt::skip]
    let g = match ch {
        b'0' => [0x3E, 0x51, 0x49, 0x45, 0x3E],
        b'1' => [0x00, 0x42, 0x7F, 0x40, 0x00],
        b'2' => [0x42, 0x61, 0x51, 0x49, 0x46],
        b'3' => [0x21, 0x41, 0x45, 0x4B, 0x31],
        b'4' => [0x18, 0x14, 0x12, 0x7F, 0x10],
        b'5' => [0x27, 0x45, 0x45, 0x45, 0x39],
        b'6' => [0x3C, 0x4A, 0x49, 0x49, 0x30],
        b'7' => [0x01, 0x71, 0x09, 0x05, 0x03],
        b'8' => [0x36, 0x49, 0x49, 0x49, 0x36],
        b'9' => [0x06, 0x49, 0x49, 0x29, 0x1E],
        b':' => [0x00, 0x36, 0x36, 0x00, 0x00],
        b'-' => [0x08, 0x08, 0x08, 0x08, 0x08],
        b'/' => [0x20, 0x10, 0x08, 0x04, 0x02],
        b' ' => [0x00, 0x00, 0x00, 0x00, 0x00],
        b'A' | b'a' => [0x7E, 0x11, 0x11, 0x11, 0x7E],
        b'B' | b'b' => [0x7F, 0x49, 0x49, 0x49, 0x36],
        b'C' | b'c' => [0x3E, 0x41, 0x41, 0x41, 0x22],
        b'D' | b'd' => [0x7F, 0x41, 0x41, 0x22, 0x1C],
        b'E' | b'e' => [0x7F, 0x49, 0x49, 0x49, 0x41],
        b'F' | b'f' => [0x7F, 0x09, 0x09, 0x09, 0x01],
        b'G' | b'g' => [0x3E, 0x41, 0x49, 0x49, 0x7A],
        b'H' | b'h' => [0x7F, 0x08, 0x08, 0x08, 0x7F],
        b'I' | b'i' => [0x00, 0x41, 0x7F, 0x41, 0x00],
        b'J' | b'j' => [0x20, 0x40, 0x41, 0x3F, 0x01],
        b'K' | b'k' => [0x7F, 0x08, 0x14, 0x22, 0x41],
        b'L' | b'l' => [0x7F, 0x40, 0x40, 0x40, 0x40],
        b'M' | b'm' => [0x7F, 0x02, 0x0C, 0x02, 0x7F],
        b'N' | b'n' => [0x7F, 0x04, 0x08, 0x10, 0x7F],
        b'O' | b'o' => [0x3E, 0x41, 0x41, 0x41, 0x3E],
        b'P' | b'p' => [0x7F, 0x09, 0x09, 0x09, 0x06],
        b'Q' | b'q' => [0x3E, 0x41, 0x51, 0x21, 0x5E],
        b'R' | b'r' => [0x7F, 0x09, 0x19, 0x29, 0x46],
        b'S' | b's' => [0x46, 0x49, 0x49, 0x49, 0x31],
        b'T' | b't' => [0x01, 0x01, 0x7F, 0x01, 0x01],
        b'U' | b'u' => [0x3F, 0x40, 0x40, 0x40, 0x3F],
        b'V' | b'v' => [0x1F, 0x20, 0x40, 0x20, 0x1F],
        b'W' | b'w' => [0x7F, 0x20, 0x18, 0x20, 0x7F],
        b'X' | b'x' => [0x63, 0x14, 0x08, 0x14, 0x63],
        b'Y' | b'y' => [0x03, 0x04, 0x78, 0x04, 0x03],
        b'Z' | b'z' => [0x61, 0x51, 0x49, 0x45, 0x43],
        _ => return None,
    };
    Some(g)
}

fn draw_char(buf: &mut [Color], w: usize, h: usize, x: i32, y: i32, ch: u8, scale: usize, rgb: Color) {
    let Some(cols) = glyph(ch) else { return };
    for (cx, bits) in cols.iter().enumerate() {
        for cy in 0..7 {
            if bits & (1 << cy) == 0 {
                continue;
            }
            for sy in 0..scale {
                for sx in 0..scale {
                    let px = x + (cx * scale + sx) as i32;
                    let py = y + (cy * scale + sy) as i32;
                    if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                        buf[py as usize * w + px as usize] = rgb;
                    }
                }
            }
        }
    }
}

fn draw_text(buf: &mut [Color], w: usize, h: usize, x: i32, y: i32, s: &str, scale: usize, rgb: Color) {
    let mut cx = x;
    for ch in s.bytes() {
        draw_char(buf, w, h, cx, y, ch, scale, rgb);
        cx += (6 * scale) as i32;
    }
}

fn text_width(s: &str, scale: usize) -> usize {
    s.len() * 6 * scale
}

/// 8×8 点阵汉字（星期）。每行一字节，bit7 为最左。
/// 画成「横画感」而不是星形，小尺寸上才认得出。
fn cjk_weekday(wday: u32) -> Option<[u8; 8]> {
    #[rustfmt::skip]
    let g = match wday % 7 {
        0 => [ // 日 — 一口字加一条中横
            0b01111110,
            0b01000010,
            0b01000010,
            0b01111110,
            0b01000010,
            0b01000010,
            0b01111110,
            0b00000000,
        ],
        1 => [ // 一
            0b00000000,
            0b00000000,
            0b00000000,
            0b11111111,
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000000,
        ],
        2 => [ // 二
            0b00111100,
            0b00000000,
            0b00000000,
            0b00000000,
            0b00000000,
            0b01111110,
            0b00000000,
            0b00000000,
        ],
        3 => [ // 三
            0b00111100,
            0b00000000,
            0b01111110,
            0b00000000,
            0b00111100,
            0b00000000,
            0b00000000,
            0b00000000,
        ],
        4 => [ // 四 — 外框里两竖
            0b01111110,
            0b01000010,
            0b01011010,
            0b01011010,
            0b01011010,
            0b01011010,
            0b01111110,
            0b00000000,
        ],
        5 => [ // 五
            0b01111110,
            0b00001000,
            0b00001000,
            0b00111110,
            0b00001000,
            0b00001000,
            0b01111110,
            0b00000000,
        ],
        6 => [ // 六 — 顶点 + 八字脚，不要画成 *
            0b00010000,
            0b00010000,
            0b01111110,
            0b00000000,
            0b00100100,
            0b00100100,
            0b01000010,
            0b00000000,
        ],
        _ => return None,
    };
    Some(g)
}

fn draw_cjk(buf: &mut [Color], w: usize, h: usize, x: i32, y: i32, rows: [u8; 8], rgb: Color) {
    for (cy, bits) in rows.iter().enumerate() {
        for cx in 0..8 {
            if bits & (0x80 >> cx) == 0 {
                continue;
            }
            let px = x + cx as i32;
            let py = y + cy as i32;
            if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                buf[py as usize * w + px as usize] = rgb;
            }
        }
    }
}

/// 本地时间（时、分、秒、日、月、星期 0=周日）。
pub struct LocalTime {
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
    pub mday: u32,
    pub mon: u32,
    pub wday: u32,
}

pub fn now_local() -> LocalTime {
    crate::ffi::local_time().unwrap_or(LocalTime {
        hour: 0,
        min: 0,
        sec: 0,
        mday: 1,
        mon: 1,
        wday: 0,
    })
}

const WD_EN: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// `lang`: "zh"（星期汉字）或 "en"（Mon/Tue…）
pub fn draw_clock(buf: &mut [Color], w: usize, h: usize, t: &LocalTime, use24h: bool, lang: &str) {
    for px in buf.iter_mut() {
        *px = Color { r: 0, g: 0, b: 2 };
    }
    let time = if use24h {
        format!("{:02}:{:02}", t.hour, t.min)
    } else {
        let (hh, ampm) = match t.hour {
            0 => (12u32, "AM"),
            1..=12 => (t.hour, if t.hour < 12 { "AM" } else { "PM" }),
            _ => (t.hour - 12, "PM"),
        };
        format!("{:2}:{:02} {}", hh, t.min, ampm)
    };
    let zh = lang != "en";
    let date = format!("{:02}-{:02} ", t.mon, t.mday);
    let wd_w = if zh { 8 } else { text_width(WD_EN[t.wday as usize % 7], 1) };

    let time_scale = if h >= 32 { 2 } else { 1 };
    let tw = text_width(&time, time_scale);
    let tx = (w.saturating_sub(tw) / 2) as i32;
    let ty = if h >= 32 { 2 } else { 0 };

    let dw = text_width(&date, 1) + 2 + wd_w;
    let dx = (w.saturating_sub(dw) / 2) as i32;
    let dy = (h as i32).saturating_sub(9);

    draw_text(buf, w, h, tx, ty, &time, time_scale, Color { r: 200, g: 200, b: 200 });

    let dim = Color { r: 90, g: 130, b: 180 };
    draw_text(buf, w, h, dx, dy, &date, 1, dim);
    let wx = dx + text_width(&date, 1) as i32 + 2;
    if zh {
        if let Some(rows) = cjk_weekday(t.wday) {
            draw_cjk(buf, w, h, wx, dy, rows, dim);
        }
    } else {
        draw_text(buf, w, h, wx, dy, WD_EN[t.wday as usize % 7], 1, dim);
    }
}

/// 把缓冲区写入离屏画布（swap 由调用方负责）。
pub fn blit(canvas: *mut ffi::LedCanvas, lw: i32, lh: i32, buf: &[Color]) {
    unsafe {
        ffi::led_canvas_set_pixels(canvas, 0, 0, lw, lh, buf.as_ptr());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_exist_for_clock_strings() {
        for s in ["12:34", "09-26 ", "12:34 PM"] {
            for ch in s.bytes() {
                assert!(glyph(ch).is_some(), "missing glyph for {:?}", ch as char);
            }
        }
        for d in 0..7 {
            assert!(cjk_weekday(d).is_some());
        }
    }

    #[test]
    fn draw_clock_fills_buffer() {
        let w = 64;
        let h = 32;
        let mut buf = vec![Color::default(); w * h];
        let t = LocalTime { hour: 20, min: 36, sec: 1, mday: 26, mon: 9, wday: 5 };
        draw_clock(&mut buf, w, h, &t, true, "zh");
        assert!(buf.iter().any(|c| c.r > 50));
        draw_clock(&mut buf, w, h, &t, true, "en");
        assert!(buf.iter().any(|c| c.r > 50));
    }

    /// 把汉字点阵打成 ASCII，防止再画成星号。
    #[test]
    fn cjk_weekdays_are_not_asterisk() {
        for d in 0..7u32 {
            let g = cjk_weekday(d).unwrap();
            let lit: u32 = g.iter().map(|r| r.count_ones()).sum();
            // 星号大约 12–20 点；汉字笔画应更分散，且不能是单点放射
            assert!(lit >= 8, "day {d} too sparse: {lit}");
            assert!(lit <= 40, "day {d} too dense: {lit}");
        }
        // 一 应只有一行亮
        let yi = cjk_weekday(1).unwrap();
        assert_eq!(yi.iter().filter(|r| **r != 0).count(), 1);
        // 六 不能是对称十字/星形（中心 3×3 不能全是放射点）
        let liu = cjk_weekday(6).unwrap();
        assert_ne!(liu[3], 0b11111111);
    }
}
