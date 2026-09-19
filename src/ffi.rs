//! 与 librgbmatrix 的 FFI 绑定。
//!
//! 全部 `unsafe` 都集中在本模块，字段顺序严格照抄 `include/led-matrix-c.h`，
//! 改动前务必对照头文件（字段顺序错了会内存错乱）。
//!
//! 关于零值语义：C API 源码里用 `OPT_COPY_IF_SET` / `RT_OPT_COPY_IF_SET`
//! 宏（`if (opts->o) default.o = opts->o`）复制字段，**非零才算显式设置**，
//! 零则用 C++ 侧的默认值。所以选项结构体用 `mem::zeroed()` 初始化是安全且
//! 符合预期的——想改的字段显式赋值，其余保持零即取默认。

#![allow(non_snake_case)]

use std::ffi::{c_char, c_int, c_void};

pub const SIGINT: c_int = 2;
pub const SIGTERM: c_int = 15;

/// `struct RGBLedMatrixOptions`（include/led-matrix-c.h:48）
#[repr(C)]
pub struct RGBLedMatrixOptions {
    pub hardware_mapping: *const c_char,
    pub rows: c_int,
    pub cols: c_int,
    pub chain_length: c_int,
    pub parallel: c_int,
    pub pwm_bits: c_int,
    pub pwm_lsb_nanoseconds: c_int,
    pub pwm_dither_bits: c_int,
    pub brightness: c_int,
    pub scan_mode: c_int,
    pub row_address_type: c_int,
    pub multiplexing: c_int,
    pub disable_hardware_pulsing: bool,
    pub show_refresh_rate: bool,
    pub inverse_colors: bool,
    pub led_rgb_sequence: *const c_char,
    pub pixel_mapper_config: *const c_char,
    pub panel_type: *const c_char,
    pub limit_refresh_rate_hz: c_int,
    pub disable_busy_waiting: bool,
}

/// `struct RGBLedRuntimeOptions`（include/led-matrix-c.h:167）
#[repr(C)]
pub struct RGBLedRuntimeOptions {
    pub gpio_slowdown: c_int,
    pub rp1_pio: c_int,
    pub daemon: c_int,
    pub drop_privileges: c_int,
    pub do_gpio_init: bool,
    pub drop_priv_user: *const c_char,
    pub drop_priv_group: *const c_char,
}

/// `struct Color`（led-matrix-c.h）
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

pub enum RGBLedMatrix {}
pub enum LedCanvas {}

// 链接仓库里编译好的 librgbmatrix（静态库，搜索路径由 .cargo/config.toml 提供）。
#[link(name = "rgbmatrix", kind = "static")]
unsafe extern "C" {
    // ---- 矩阵与画布 ----
    pub fn led_matrix_create_from_options_and_rt_options(
        opts: *mut RGBLedMatrixOptions,
        rt_opts: *mut RGBLedRuntimeOptions,
    ) -> *mut RGBLedMatrix;
    pub fn led_matrix_delete(matrix: *mut RGBLedMatrix);
    pub fn led_matrix_create_offscreen_canvas(matrix: *mut RGBLedMatrix) -> *mut LedCanvas;
    pub fn led_matrix_swap_on_vsync(
        matrix: *mut RGBLedMatrix,
        canvas: *mut LedCanvas,
    ) -> *mut LedCanvas;

    pub fn led_canvas_set_pixel(canvas: *mut LedCanvas, x: c_int, y: c_int, r: u8, g: u8, b: u8);
    pub fn led_canvas_fill(canvas: *mut LedCanvas, r: u8, g: u8, b: u8);
    pub fn led_canvas_clear(canvas: *mut LedCanvas);
    pub fn led_canvas_get_size(canvas: *mut LedCanvas, width: *mut c_int, height: *mut c_int);
    pub fn led_canvas_set_pixels(canvas: *mut LedCanvas, x: c_int, y: c_int, width: c_int, height: c_int, colors: *const Color);


    // ---- libc（只声明 signal，不引入 libc crate）----
    fn signal(signum: c_int, handler: extern "C" fn(c_int)) -> usize;
}

// librgbmatrix 是 C++ 写的，需要 C++ 运行时支撑
#[link(name = "stdc++")]
unsafe extern "C" {}

/// 注册信号处理函数（用于 Ctrl-C / systemctl stop 时优雅退出）。
///
/// # Safety
/// handler 必须是 async-signal-safe 的（只做原子写，不能分配内存或加锁）。
pub unsafe fn register_signal(signum: c_int, handler: extern "C" fn(c_int)) {
    unsafe { signal(signum, handler) };
}

