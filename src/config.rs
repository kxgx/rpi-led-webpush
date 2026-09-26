//! 持久化配置：`key=value` 纯文本，零依赖可解析。
//!
//! 优先级：CLI 参数 > 配置文件 > 内置默认值。
//! 保存时写回打开时用的路径（默认 `./rpi-led-webpush.conf`）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const DEFAULT_CONFIG_NAME: &str = "rpi-led-webpush.conf";

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub rows: i32,
    pub cols: i32,
    pub chain: i32,
    pub parallel: i32,
    pub brightness: i32,
    pub mapping: String,
    pub rgb_sequence: String,
    pub web_port: u16,
    pub idle: bool,
    // ---- 硬件驱动（rpi-rgb-led-matrix 初始化参数）----
    /// 驱动芯片 / 面板类型：空 = 通用；FM6126A / FM6127 需要上电初始化序列
    pub panel_type: String,
    /// GPIO 时序减速 0..4（信号完整性差时加大）
    pub gpio_slowdown: i32,
    /// PWM 位深 1..11，越低刷新率越高
    pub pwm_bits: i32,
    /// 最低有效位的导通时间（ns），影响鬼影/刷新率
    pub pwm_lsb_ns: i32,
    /// 时间抖动位数
    pub pwm_dither: i32,
    /// 扫描方式 0=progressive 1=interlaced
    pub scan_mode: i32,
    /// 行地址类型：0=直驱；1=A/B（部分 64×64）；2/3=更复杂的行寻址
    pub row_address_type: i32,
    /// 复用方式：0=直驱 1=stripe 2=checker(1:8) …
    pub multiplexing: i32,
    /// 关闭 OE 硬件脉冲（GPIO18 未接 OE 时有用）
    pub no_hardware_pulse: bool,
    /// 反色面板
    pub inverse_colors: bool,
    /// 像素映射器配置，如 "U-mapper;Rotate:90"
    pub pixel_mapper: String,
    /// 限制面板刷新率 Hz；0 = 不限制
    pub limit_refresh_hz: i32,
    /// 刷新限速时 sleep 而不是 busy-wait（省 CPU，时序略松）
    pub no_busy_waiting: bool,
    /// Pi 5 后端：1=RP1 PIO（低 CPU）0=RP1 RIO
    pub rp1_pio: i32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            rows: 32,
            cols: 64,
            chain: 1,
            parallel: 1,
            brightness: 60,
            mapping: "regular".to_string(),
            rgb_sequence: "RGB".to_string(),
            web_port: 8080,
            idle: true,
            panel_type: String::new(),
            gpio_slowdown: 1,
            pwm_bits: 11,
            pwm_lsb_ns: 0,
            pwm_dither: 0,
            scan_mode: 0,
            row_address_type: 0,
            multiplexing: 0,
            no_hardware_pulse: false,
            inverse_colors: false,
            pixel_mapper: String::new(),
            limit_refresh_hz: 0,
            no_busy_waiting: false,
            rp1_pio: 1,
        }
    }
}

impl Config {
    fn parse_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return;
        }
        let Some((k, v)) = line.split_once('=') else {
            return;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "rows" => self.rows = v.parse().unwrap_or(self.rows),
            "cols" => self.cols = v.parse().unwrap_or(self.cols),
            "chain" => self.chain = v.parse().unwrap_or(self.chain),
            "parallel" => self.parallel = v.parse().unwrap_or(self.parallel),
            "brightness" => self.brightness = v.parse().unwrap_or(self.brightness),
            "mapping" => self.mapping = v.to_string(),
            "rgb_sequence" => self.rgb_sequence = v.to_string(),
            "web_port" => self.web_port = v.parse().unwrap_or(self.web_port),
            "idle" => self.idle = matches!(v, "1" | "true" | "yes" | "on"),
            // 硬件驱动
            "panel_type" | "driver" => self.panel_type = v.to_string(),
            "gpio_slowdown" => self.gpio_slowdown = v.parse().unwrap_or(self.gpio_slowdown),
            "pwm_bits" => self.pwm_bits = v.parse().unwrap_or(self.pwm_bits),
            "pwm_lsb_ns" | "pwm_lsb_nanoseconds" => self.pwm_lsb_ns = v.parse().unwrap_or(self.pwm_lsb_ns),
            "pwm_dither" | "pwm_dither_bits" => self.pwm_dither = v.parse().unwrap_or(self.pwm_dither),
            "scan_mode" => self.scan_mode = v.parse().unwrap_or(self.scan_mode),
            "row_address_type" => self.row_address_type = v.parse().unwrap_or(self.row_address_type),
            "multiplexing" => self.multiplexing = v.parse().unwrap_or(self.multiplexing),
            "no_hardware_pulse" => self.no_hardware_pulse = matches!(v, "1" | "true" | "yes" | "on"),
            "inverse_colors" => self.inverse_colors = matches!(v, "1" | "true" | "yes" | "on"),
            "pixel_mapper" | "pixel_mapper_config" => self.pixel_mapper = v.to_string(),
            "limit_refresh_hz" | "limit_refresh_rate_hz" => {
                self.limit_refresh_hz = v.parse().unwrap_or(self.limit_refresh_hz)
            }
            "no_busy_waiting" | "disable_busy_waiting" => {
                self.no_busy_waiting = matches!(v, "1" | "true" | "yes" | "on")
            }
            "rp1_pio" => self.rp1_pio = v.parse().unwrap_or(self.rp1_pio),
            _ => {}
        }
    }

    pub fn normalize(&mut self) {
        self.rows = self.rows.clamp(1, 512);
        self.cols = self.cols.clamp(1, 512);
        self.chain = self.chain.clamp(1, 32);
        self.parallel = self.parallel.clamp(1, 8);
        self.brightness = self.brightness.clamp(1, 100);
        if self.mapping.is_empty() {
            self.mapping = "regular".to_string();
        }
        if self.rgb_sequence.is_empty() {
            self.rgb_sequence = "RGB".to_string();
        }
        self.gpio_slowdown = self.gpio_slowdown.clamp(0, 4);
        self.pwm_bits = self.pwm_bits.clamp(1, 11);
        self.pwm_lsb_ns = self.pwm_lsb_ns.clamp(0, 200);
        self.pwm_dither = self.pwm_dither.clamp(0, 2);
        self.scan_mode = self.scan_mode.clamp(0, 1);
        self.row_address_type = self.row_address_type.clamp(0, 4);
        self.multiplexing = self.multiplexing.clamp(0, 16);
        self.limit_refresh_hz = self.limit_refresh_hz.clamp(0, 240);
        self.rp1_pio = if self.rp1_pio != 0 { 1 } else { 0 };
        // panel_type 统一成库认识的大小写风格
        self.panel_type = match self.panel_type.trim().to_ascii_lowercase().as_str() {
            "" => String::new(),
            "fm6126" | "fm6126a" => "FM6126A".to_string(),
            "fm6126b" => "FM6126B".to_string(),
            "fm6127" => "FM6127".to_string(),
            other => other.to_ascii_uppercase(),
        };
    }

    /// 这些字段改动后必须重开面板（库的初始化选项）。
    pub fn hardware_eq(&self, other: &Config) -> bool {
        self.rows == other.rows
            && self.cols == other.cols
            && self.chain == other.chain
            && self.parallel == other.parallel
            && self.mapping == other.mapping
            && self.rgb_sequence == other.rgb_sequence
            && self.web_port == other.web_port
            && self.panel_type == other.panel_type
            && self.gpio_slowdown == other.gpio_slowdown
            && self.pwm_bits == other.pwm_bits
            && self.pwm_lsb_ns == other.pwm_lsb_ns
            && self.pwm_dither == other.pwm_dither
            && self.scan_mode == other.scan_mode
            && self.row_address_type == other.row_address_type
            && self.multiplexing == other.multiplexing
            && self.no_hardware_pulse == other.no_hardware_pulse
            && self.inverse_colors == other.inverse_colors
            && self.pixel_mapper == other.pixel_mapper
            && self.limit_refresh_hz == other.limit_refresh_hz
            && self.no_busy_waiting == other.no_busy_waiting
            && self.rp1_pio == other.rp1_pio
    }
}

/// 运行时可改、无需重开面板的字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// 全部生效
    Applied,
    /// 存盘成功，但几何/映射等需重启进程
    NeedsRestart,
    /// 写盘失败（内存已更新，但没保存）
    SaveFailed(String),
}

#[derive(Clone)]
pub struct SharedConfig {
    inner: Arc<Mutex<Config>>,
    path: PathBuf,
}

impl SharedConfig {
    /// 用给定路径打开（测试或自定义位置）。
    pub fn open_at(path: PathBuf) -> SharedConfig {
        let cfg = load(&path);
        SharedConfig {
            inner: Arc::new(Mutex::new(cfg)),
            path,
        }
    }

    pub fn get(&self) -> Config {
        self.inner.lock().map(|c| c.clone()).unwrap_or_default()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 覆盖配置并写盘。写失败会返回 `SaveFailed`，不会假装成功。
    pub fn update(&self, patch: Config) -> ApplyOutcome {
        let (old, mut new) = {
            let Ok(mut g) = self.inner.lock() else {
                return ApplyOutcome::SaveFailed("config lock poisoned".into());
            };
            let old = g.clone();
            *g = patch.clone();
            g.normalize();
            (old, g.clone())
        };
        new.normalize();
        if let Err(e) = write_config(&self.path, &new) {
            return ApplyOutcome::SaveFailed(format!("{}: {e}", self.path.display()));
        }
        if !old.hardware_eq(&new) {
            ApplyOutcome::NeedsRestart
        } else {
            ApplyOutcome::Applied
        }
    }

    /// 仅更新内存中的配置（用于启动时并入 CLI 覆盖），不写盘。
    pub fn set_memory(&self, c: Config) {
        if let Ok(mut g) = self.inner.lock() {
            *g = c;
            g.normalize();
        }
    }
}

/// 原子写入：先写同目录临时文件再 rename，避免半截文件。
fn write_config(path: &Path, c: &Config) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => std::env::current_dir()?,
    };
    fs::create_dir_all(&dir)?;
    let body = format!(
        "# rpi-led-webpush configuration\n\
         rows={}\ncols={}\nchain={}\nparallel={}\n\
         brightness={}\nmapping={}\nrgb_sequence={}\n\
         web_port={}\nidle={}\n\
         \n# ---- hardware driver ----\n\
         panel_type={}\n\
         gpio_slowdown={}\n\
         pwm_bits={}\n\
         pwm_lsb_ns={}\n\
         pwm_dither={}\n\
         scan_mode={}\n\
         row_address_type={}\n\
         multiplexing={}\n\
         no_hardware_pulse={}\n\
         inverse_colors={}\n\
         pixel_mapper={}\n\
         limit_refresh_hz={}\n\
         no_busy_waiting={}\n\
         rp1_pio={}\n",
        c.rows,
        c.cols,
        c.chain,
        c.parallel,
        c.brightness,
        c.mapping,
        c.rgb_sequence,
        c.web_port,
        if c.idle { 1 } else { 0 },
        c.panel_type,
        c.gpio_slowdown,
        c.pwm_bits,
        c.pwm_lsb_ns,
        c.pwm_dither,
        c.scan_mode,
        c.row_address_type,
        c.multiplexing,
        if c.no_hardware_pulse { 1 } else { 0 },
        if c.inverse_colors { 1 } else { 0 },
        c.pixel_mapper,
        c.limit_refresh_hz,
        if c.no_busy_waiting { 1 } else { 0 },
        c.rp1_pio,
    );
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("cfg")
    ));
    fs::write(&tmp, &body)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// 解析配置路径：`LED_CONFIG` → `/etc/rpi-led-webpush/config` → `./rpi-led-webpush.conf`
/// 返回绝对路径，避免工作目录变化后写到意外位置。
pub fn config_path() -> PathBuf {
    let p = if let Ok(p) = std::env::var("LED_CONFIG") {
        if !p.is_empty() {
            PathBuf::from(p)
        } else {
            PathBuf::from(DEFAULT_CONFIG_NAME)
        }
    } else {
        let etc = PathBuf::from("/etc/rpi-led-webpush/config");
        if etc.is_file() {
            etc
        } else {
            PathBuf::from(DEFAULT_CONFIG_NAME)
        }
    };
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    }
}

pub fn load(path: &Path) -> Config {
    let mut c = Config::default();
    if let Ok(text) = fs::read_to_string(path) {
        for line in text.lines() {
            c.parse_line(line);
        }
    }
    c.normalize();
    c
}

pub fn open() -> SharedConfig {
    SharedConfig::open_at(config_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_normalize() {
        let mut c = Config::default();
        c.parse_line("rows=64");
        c.parse_line("# comment");
        c.parse_line("idle=0");
        c.parse_line("mapping=adafruit-hat-pwm");
        c.parse_line("garbage line without equals");
        c.parse_line("brightness=999");
        c.normalize();
        assert_eq!(c.rows, 64);
        assert!(!c.idle);
        assert_eq!(c.mapping, "adafruit-hat-pwm");
        assert_eq!(c.brightness, 100);
    }

    #[test]
    fn roundtrip_file() {
        let dir = std::env::temp_dir().join(format!("lpwp-test-rt-{}", std::process::id()));
        let path = dir.join("c.conf");
        let mut c = Config::default();
        c.brightness = 42;
        c.rgb_sequence = "RBG".into();
        c.idle = false;
        write_config(&path, &c).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded, c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_detects_restart() {
        let dir = std::env::temp_dir().join(format!("lpwp-test-up-{}", std::process::id()));
        let path = dir.join("c.conf");
        write_config(&path, &Config::default()).unwrap();
        let sc = SharedConfig::open_at(path.clone());
        let mut only_hot = Config::default();
        only_hot.brightness = 80;
        assert_eq!(sc.update(only_hot), ApplyOutcome::Applied);
        let mut geo = Config::default();
        geo.cols = 128;
        assert_eq!(sc.update(geo), ApplyOutcome::NeedsRestart);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
