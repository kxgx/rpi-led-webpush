//! 持久化配置：`key=value` 纯文本，零依赖可解析。
//!
//! 优先级：CLI 参数 > 配置文件 > 内置默认值。
//! 保存时写回打开时用的路径（默认 `./rpi-led-webpush.conf`）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const DEFAULT_CONFIG_NAME: &str = "rpi-led-webpush.conf";
/// 开启 HTTPS 但没显式填端口时用的默认端口。
pub const DEFAULT_HTTPS_PORT: u16 = 8443;

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
    /// 网页服务绑定地址；空 = 自动（HTTPS 开启时 127.0.0.1，否则 0.0.0.0）
    pub web_host: String,
    pub idle: bool,
    /// 无投送时显示时钟/日期（默认开）
    pub show_clock: bool,
    /// 时钟用 24 小时制（否则 12 小时制）
    pub clock_24h: bool,
    /// 界面与星期语言："zh" / "en"
    pub lang: String,
    // ---- HTTPS（程序本身不做 TLS，交给系统 socat 做 TLS 终止）----
    /// 启用内置 HTTPS 前端（socat TLS 终止 → 反代到 127.0.0.1:web_port）
    pub https: bool,
    /// HTTPS 监听端口（web_port 仍要可用，作为本机后端）
    pub https_port: u16,
    /// 证书 PEM 路径；留空 = <配置文件目录>/tls/cert.pem
    pub https_cert: String,
    /// 私钥 PEM 路径；留空 = <配置文件目录>/tls/key.pem
    pub https_key: String,
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
            web_host: String::new(),
            idle: true,
            show_clock: true,
            clock_24h: true,
            lang: "zh".to_string(),
            https: false,
            https_port: DEFAULT_HTTPS_PORT,
            https_cert: String::new(),
            https_key: String::new(),
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
            "web_host" => self.web_host = v.to_string(),
            "idle" => self.idle = matches!(v, "1" | "true" | "yes" | "on"),
            "show_clock" => self.show_clock = matches!(v, "1" | "true" | "yes" | "on"),
            "clock_24h" => self.clock_24h = matches!(v, "1" | "true" | "yes" | "on"),
            "lang" => {
                self.lang = match v.to_ascii_lowercase().as_str() {
                    "en" | "english" => "en".to_string(),
                    _ => "zh".to_string(),
                }
            }
            // HTTPS（socat TLS 前端）
            "https" => self.https = matches!(v, "1" | "true" | "yes" | "on"),
            "https_port" => self.https_port = v.parse().unwrap_or(self.https_port),
            "https_cert" => self.https_cert = v.to_string(),
            "https_key" => self.https_key = v.to_string(),
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
        if self.lang != "en" {
            self.lang = "zh".to_string();
        }
        // HTTPS 端口 0 没意义（开关用 https=1），回落到默认端口
        if self.https_port == 0 {
            self.https_port = DEFAULT_HTTPS_PORT;
        }
        self.https_cert = self.https_cert.trim().to_string();
        self.https_key = self.https_key.trim().to_string();
        // 绑定地址：只接受简单主机名/IP，空白或含空白/控制字符时回落到自动
        self.web_host = self.web_host.trim().to_string();
        if self.web_host.chars().any(|c| c.is_whitespace() || c.is_control()) {
            self.web_host.clear();
        }
        // panel_type 统一成库认识的大小写风格
        self.panel_type = match self.panel_type.trim().to_ascii_lowercase().as_str() {
            "" => String::new(),
            "fm6126" | "fm6126a" => "FM6126A".to_string(),
            "fm6126b" => "FM6126B".to_string(),
            "fm6127" => "FM6127".to_string(),
            other => other.to_ascii_uppercase(),
        };
    }

    /// 这些字段改动后必须重启进程（面板初始化选项、网络监听）。
    pub fn restart_eq(&self, other: &Config) -> bool {
        self.rows == other.rows
            && self.cols == other.cols
            && self.chain == other.chain
            && self.parallel == other.parallel
            && self.mapping == other.mapping
            && self.rgb_sequence == other.rgb_sequence
            && self.web_port == other.web_port
            && self.web_host == other.web_host
            && self.https == other.https
            && self.https_port == other.https_port
            && self.https_cert == other.https_cert
            && self.https_key == other.https_key
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
        // 增量写盘：只改 old→new 之间真正变化的键，保留用户文件的注释/未知键/顺序/写法
        if let Err(e) = write_config_patch(&self.path, &old, &new) {
            return ApplyOutcome::SaveFailed(format!("{}: {e}", self.path.display()));
        }
        if !old.restart_eq(&new) {
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
    let body = format!(
        "# rpi-led-webpush configuration\n\
         rows={}\ncols={}\nchain={}\nparallel={}\n\
         brightness={}\nmapping={}\nrgb_sequence={}\n\
         web_port={}\nweb_host={}\nidle={}\n\
         show_clock={}\nclock_24h={}\n\
         lang={}\n\
         \n# ---- HTTPS (socat TLS front-end; needs socat + openssl) ----\n\
         https={}\n\
         https_port={}\n\
         https_cert={}\n\
         https_key={}\n\
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
        c.web_host,
        if c.idle { 1 } else { 0 },
        if c.show_clock { 1 } else { 0 },
        if c.clock_24h { 1 } else { 0 },
        c.lang,
        if c.https { 1 } else { 0 },
        c.https_port,
        c.https_cert,
        c.https_key,
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
    atomic_write(path, &body)
}

impl Config {
    /// 规范化的 `key=value` 序列（顺序固定）。完整写入与增量 diff 都以它为准，
    /// 新增配置项时**务必同时加进这里**，否则增量写盘不会覆盖该键。
    fn pairs(&self) -> Vec<(&'static str, String)> {
        fn b(v: bool) -> &'static str {
            if v { "1" } else { "0" }
        }
        vec![
            ("rows", self.rows.to_string()),
            ("cols", self.cols.to_string()),
            ("chain", self.chain.to_string()),
            ("parallel", self.parallel.to_string()),
            ("brightness", self.brightness.to_string()),
            ("mapping", self.mapping.clone()),
            ("rgb_sequence", self.rgb_sequence.clone()),
            ("web_port", self.web_port.to_string()),
            ("web_host", self.web_host.clone()),
            ("idle", b(self.idle).to_string()),
            ("show_clock", b(self.show_clock).to_string()),
            ("clock_24h", b(self.clock_24h).to_string()),
            ("lang", self.lang.clone()),
            ("https", b(self.https).to_string()),
            ("https_port", self.https_port.to_string()),
            ("https_cert", self.https_cert.clone()),
            ("https_key", self.https_key.clone()),
            ("panel_type", self.panel_type.clone()),
            ("gpio_slowdown", self.gpio_slowdown.to_string()),
            ("pwm_bits", self.pwm_bits.to_string()),
            ("pwm_lsb_ns", self.pwm_lsb_ns.to_string()),
            ("pwm_dither", self.pwm_dither.to_string()),
            ("scan_mode", self.scan_mode.to_string()),
            ("row_address_type", self.row_address_type.to_string()),
            ("multiplexing", self.multiplexing.to_string()),
            ("no_hardware_pulse", b(self.no_hardware_pulse).to_string()),
            ("inverse_colors", b(self.inverse_colors).to_string()),
            ("pixel_mapper", self.pixel_mapper.clone()),
            ("limit_refresh_hz", self.limit_refresh_hz.to_string()),
            ("no_busy_waiting", b(self.no_busy_waiting).to_string()),
            ("rp1_pio", self.rp1_pio.to_string()),
        ]
    }
}

/// old → new 之间真正变化的键（按规范顺序，便于写文件）。
fn diff_pairs(old: &Config, new: &Config) -> Vec<(&'static str, String)> {
    new.pairs()
        .into_iter()
        .zip(old.pairs())
        .filter_map(|((k, v), (old_k, old_v))| {
            debug_assert_eq!(k, old_k);
            if v != old_v { Some((k, v)) } else { None }
        })
        .collect()
}

/// 从配置行里取出 key（跳过注释、空行、没有等号的行）。
fn line_key(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.starts_with('#') {
        return None;
    }
    let (k, _) = t.split_once('=')?;
    let k = k.trim();
    if k.is_empty() { None } else { Some(k) }
}

/// 行尾注释（`key=value  # comment` 里的 `# comment`），替换值时保留它。
fn line_comment(line: &str) -> Option<&str> {
    let eq = line.find('=')?;
    let hash = line.find('#')?;
    if hash > eq { Some(line[hash..].trim_end()) } else { None }
}

/// 增量写盘：只改 `old → new` 之间**真正变化**的键。
///
/// 文件里的注释、未知键、键顺序和用户写法都原样保留（例如用户写的 `idle=true`
/// 不会被改写成 `idle=1`，因为该值没变化）；没有变化时完全不碰文件。
/// 文件不存在（首次运行）时才用带分区注释的完整模板生成。
fn write_config_patch(path: &Path, old: &Config, new: &Config) -> std::io::Result<()> {
    let mut pending = diff_pairs(old, new);
    if pending.is_empty() {
        return Ok(());
    }
    if !path.is_file() {
        return write_config(path, new);
    }

    let content = fs::read_to_string(path)?;
    let eol = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = content
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    if lines.last().map(|s| s.is_empty()).unwrap_or(false) {
        lines.pop();
    }

    // 每个变化键替换文件里**最后一次**出现的行（load 的语义是后面的值覆盖前面的）；
    // 文件里没有的键先留着，最后统一追加。
    pending.retain(|(key, value)| {
        let Some(pos) = lines.iter().rposition(|l| line_key(l) == Some(*key)) else {
            return true;
        };
        // 保留该行原有的行尾注释，例如 `brightness=40  # 我调的`
        lines[pos] = match line_comment(&lines[pos]) {
            Some(comment) => format!("{key}={value}  {comment}"),
            None => format!("{key}={value}"),
        };
        false
    });

    if !pending.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("# added by rpi-led-webpush".to_string());
        for (key, value) in pending {
            lines.push(format!("{key}={value}"));
        }
    }

    let mut body = lines.join(eol);
    body.push_str(eol);
    atomic_write(path, &body)
}

/// 原子写入：先写同目录临时文件再 rename，避免半截文件；已有文件时沿用其权限。
fn atomic_write(path: &Path, body: &str) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => std::env::current_dir()?,
    };
    fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("cfg")
    ));
    fs::write(&tmp, body)?;
    if let Ok(meta) = fs::metadata(path) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    fs::rename(&tmp, path)
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
        c.parse_line("https=1");
        c.parse_line("https_port=443");
        c.parse_line("https_cert=/etc/ssl/led/cert.pem");
        c.parse_line("web_host=127.0.0.1");
        c.normalize();
        assert_eq!(c.rows, 64);
        assert!(!c.idle);
        assert_eq!(c.mapping, "adafruit-hat-pwm");
        assert_eq!(c.brightness, 100);
        assert!(c.https);
        assert_eq!(c.https_port, 443);
        assert_eq!(c.https_cert, "/etc/ssl/led/cert.pem");
        assert_eq!(c.web_host, "127.0.0.1");
    }

    #[test]
    fn https_fields_parse_and_normalize() {
        let mut c = Config::default();
        assert!(!c.https, "HTTPS 默认关闭");
        assert_eq!(c.https_port, DEFAULT_HTTPS_PORT);
        assert!(c.https_cert.is_empty());
        assert!(c.https_key.is_empty());

        c.parse_line("https=true");
        c.parse_line("https_port=0");
        c.parse_line("  https_cert = /certs/a.pem  ");
        c.parse_line("https_key=");
        c.normalize();
        assert!(c.https);
        assert_eq!(c.https_port, DEFAULT_HTTPS_PORT, "端口 0 应回落到默认值");
        assert_eq!(c.https_cert, "/certs/a.pem", "路径应去除首尾空白");
        assert!(c.https_key.is_empty());
    }

    #[test]
    fn roundtrip_file() {
        let dir = std::env::temp_dir().join(format!("lpwp-test-rt-{}", std::process::id()));
        let path = dir.join("c.conf");
        let mut c = Config::default();
        c.brightness = 42;
        c.rgb_sequence = "RBG".into();
        c.idle = false;
        c.https = true;
        c.https_port = 9443;
        c.https_cert = "/tmp/panel.crt".into();
        c.https_key = "/tmp/panel.key".into();
        c.web_host = "127.0.0.1".into();
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
        // HTTPS 相关字段也要落盘并触发重启（socat 前端在启动时搭）
        let mut tls = sc.get();
        tls.https = true;
        assert_eq!(sc.update(tls), ApplyOutcome::NeedsRestart);
        // 绑定地址变化也要重启（网络监听）
        let mut host = sc.get();
        host.web_host = "127.0.0.1".into();
        assert_eq!(sc.update(host), ApplyOutcome::NeedsRestart);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `pairs()` 是完整写入与增量 diff 的唯一数据源：每个键都必须出现在完整写入的文件里。
    #[test]
    fn canonical_pairs_match_file_writer() {
        let dir = std::env::temp_dir().join(format!("lpwp-test-pairs-{}", std::process::id()));
        let path = dir.join("c.conf");
        let mut c = Config::default();
        c.rows = 64;
        c.cols = 128;
        c.mapping = "adafruit-hat".into();
        c.brightness = 33;
        c.lang = "en".into();
        c.idle = false;
        c.https = true;
        c.https_port = 443;
        c.https_cert = "/c/cert.pem".into();
        c.panel_type = "FM6126A".into();
        c.pixel_mapper = "Rotate:90".into();
        write_config(&path, &c).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for (key, value) in c.pairs() {
            let want = format!("{key}={value}");
            assert!(
                text.lines().any(|l| l.trim() == want),
                "完整写入缺少键值 {want}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 增量更新：只改变化的键，保留注释、未知键、顺序、用户写法（不改写未变化的键）。
    #[test]
    fn incremental_update_preserves_user_file() {
        let dir = std::env::temp_dir().join(format!("lpwp-test-inc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.conf");
        let original = "# my custom comment\n\
                        rows=32\n\
                        cols=64\n\
                        # brightness comment\n\
                        brightness = 40  # 手动改过\n\
                        my_future_key=hello\n\
                        idle=true\n";
        std::fs::write(&path, original).unwrap();

        let sc = SharedConfig::open_at(path.clone());
        let mut c = sc.get();
        c.brightness = 80;
        assert_eq!(sc.update(c), ApplyOutcome::Applied);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my custom comment"), "注释被破坏: {text}");
        assert!(text.contains("# brightness comment"), "注释被破坏: {text}");
        assert!(text.contains("my_future_key=hello"), "未知键被删: {text}");
        assert!(text.contains("brightness=80"), "变化值没写回: {text}");
        assert!(!text.contains("brightness = 40"), "旧值残留: {text}");
        // 变化行的行尾注释也要保留
        assert!(text.contains("# 手动改过"), "行尾注释被删: {text}");
        // 未变化的键保持用户写法（不做规范化重写）
        assert!(text.contains("idle=true"), "未变化的键被改写: {text}");
        // 顺序保留：注释仍紧挨在 brightness 行上方
        let lines: Vec<&str> = text.lines().collect();
        let pos = lines.iter().position(|l| l.starts_with("brightness=")).unwrap();
        assert_eq!(lines[pos - 1], "# brightness comment", "行顺序被改: {text}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 文件里没有的新键会追加；再次保存（无变化）不会重复追加、也不动文件。
    #[test]
    fn incremental_update_appends_missing_keys_once() {
        let dir = std::env::temp_dir().join(format!("lpwp-test-append-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.conf");
        std::fs::write(&path, "rows=32\ncustom=1\n").unwrap();

        let sc = SharedConfig::open_at(path.clone());
        let mut c = sc.get();
        c.https = true;
        c.https_port = 9443;
        assert_eq!(sc.update(c), ApplyOutcome::NeedsRestart);

        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.starts_with("rows=32\ncustom=1\n"), "原有行被破坏: {first}");
        assert!(first.contains("# added by rpi-led-webpush"), "{first}");
        assert!(first.contains("https=1"), "{first}");
        assert!(first.contains("https_port=9443"), "{first}");

        // 无变化再保存：文件必须逐字节不变（不重复追加）
        let same = sc.get();
        assert_eq!(sc.update(same), ApplyOutcome::Applied);
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second, "无变化时不应重写文件");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 没有变化的保存完全不碰文件；CRLF 文件保持 CRLF。
    #[test]
    fn incremental_update_is_noop_and_keeps_line_endings() {
        let dir = std::env::temp_dir().join(format!("lpwp-test-noop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.conf");
        let original = "# keep me\r\nrows=32\r\nbrightness=50\r\n";
        std::fs::write(&path, original).unwrap();

        let sc = SharedConfig::open_at(path.clone());
        // 无变化保存
        assert_eq!(sc.update(sc.get()), ApplyOutcome::Applied);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        // 有变化时保持 CRLF
        let mut c = sc.get();
        c.brightness = 70;
        sc.update(c);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep me\r\n"));
        assert!(text.contains("brightness=70\r\n"));
        assert!(!text.replace("\r\n", "").contains('\n'), "出现混合行尾: {text:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
