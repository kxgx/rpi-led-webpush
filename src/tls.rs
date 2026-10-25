//! 内置 HTTPS 开关：程序本身不实现 TLS（保持零第三方依赖），而是用系统的
//! `socat` 做 TLS 终止，把 `https://<device>:PORT` 反代到本机
//! `127.0.0.1:<web_port>` 上的内嵌 HTTP 服务。
//!
//! 证书：`https_cert` / `https_key` 留空时用配置文件同目录下的
//! `tls/cert.pem`、`tls/key.pem`；文件不存在时用 `openssl` 生成一张自签名证书
//! （浏览器首次访问需要手动信任一次）。
//!
//! 前置条件：`socat` 与 `openssl`（Debian: `sudo apt install socat openssl`）。
//! 设置里改完需要重启进程才会重新搭这个前端。

use std::path::{Path, PathBuf};

use crate::config::Config;

/// 解析好的 HTTPS 前端参数（纯数据，便于单测）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub https_port: u16,
    pub http_port: u16,
    pub cert: PathBuf,
    pub key: PathBuf,
    /// 证书/私钥允许写在哪个目录下（强制白名单，防止网页把路径改成任意文件）
    pub tls_dir: PathBuf,
}

/// 由配置计算 HTTPS 前端参数；只做校验和路径计算，不碰文件系统。
pub fn plan(cfg: &Config, config_path: &Path) -> Result<Plan, String> {
    if !cfg.https {
        return Err("HTTPS 未启用".to_string());
    }
    if cfg.web_port == 0 {
        return Err("HTTPS 需要内嵌 HTTP 服务做后端（web_port > 0）".to_string());
    }
    if cfg.https_port == cfg.web_port {
        return Err(format!(
            "HTTPS 端口不能和网页端口相同（都是 {}）",
            cfg.https_port
        ));
    }
    let dir = config_dir(config_path);
    let tls_dir = dir.join("tls");
    Ok(Plan {
        https_port: cfg.https_port,
        http_port: cfg.web_port,
        cert: resolve_https_path(&cfg.https_cert, &tls_dir, "cert.pem")?,
        key: resolve_https_path(&cfg.https_key, &tls_dir, "key.pem")?,
        tls_dir,
    })
}

fn config_dir(config_path: &Path) -> PathBuf {
    match config_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// 词法规范化路径（不访问文件系统）。
fn lexical_normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if out.file_name().is_some() {
                    out.pop();
                }
            }
            Component::Normal(s) => out.push(s),
        }
    }
    out
}

fn absolute_lexical(p: &Path) -> PathBuf {
    if p.is_absolute() {
        lexical_normalize(p)
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        lexical_normalize(&cwd.join(p))
    }
}

/// `target` 是否位于 `base` 目录内（尽量 canonicalize 解析符号链接后再比）。
fn path_within(target: &Path, base: &Path) -> bool {
    let t = absolute_lexical(target);
    let b = absolute_lexical(base);
    if t.file_name().is_none() {
        return false;
    }
    if t.starts_with(&b) {
        return true;
    }
    // 配置目录本身可能是符号链接（如 /etc → /private/etc）：再按真实路径比一次
    let b_real = std::fs::canonicalize(&b).unwrap_or_else(|_| b.clone());
    let t_parent = t.parent().unwrap_or(Path::new(""));
    let t_parent_real =
        std::fs::canonicalize(t_parent).unwrap_or_else(|_| t_parent.to_path_buf());
    t_parent_real == b_real || t_parent_real.starts_with(&b_real)
}

/// 证书/私钥路径解析：空 → `<配置目录>/tls/<name>`；其余必须落在 tls 目录内。
/// 拒绝逗号/控制字符（会破坏 socat 的地址参数）和目录逃逸。
fn resolve_https_path(value: &str, tls_dir: &Path, default_name: &str) -> Result<PathBuf, String> {
    let raw = value.trim();
    if raw.is_empty() {
        return Ok(tls_dir.join(default_name));
    }
    if raw.contains(',') || raw.chars().any(|c| c.is_control()) {
        return Err("证书/私钥路径不能包含逗号或控制字符".to_string());
    }
    let p = PathBuf::from(raw);
    let abs = if p.is_absolute() {
        p
    } else {
        tls_dir.join(&p)
    };
    let normalized = absolute_lexical(&abs);
    if !path_within(&normalized, tls_dir) {
        return Err(format!(
            "证书/私钥路径必须位于 {} 目录内（当前：{}）",
            tls_dir.display(),
            normalized.display()
        ));
    }
    Ok(normalized)
}

/// `socat` 的参数：TLS 监听 + 转发到本机 HTTP 端口。
pub fn socat_args(p: &Plan) -> Vec<String> {
    vec![
        format!(
            "OPENSSL-LISTEN:{},cert={},key={},verify=0,reuseaddr,fork",
            p.https_port,
            p.cert.display(),
            p.key.display()
        ),
        format!("TCP:127.0.0.1:{}", p.http_port),
    ]
}

/// `openssl req ...` 的参数（生成自签名证书，825 天有效——缩短私钥泄露的影响期）。
pub fn openssl_args(p: &Plan) -> Vec<String> {
    vec![
        "req".to_string(),
        "-x509".to_string(),
        "-newkey".to_string(),
        "rsa:2048".to_string(),
        "-nodes".to_string(),
        "-days".to_string(),
        "825".to_string(),
        "-keyout".to_string(),
        p.key.display().to_string(),
        "-out".to_string(),
        p.cert.display().to_string(),
        "-subj".to_string(),
        "/CN=rpi-led-webpush".to_string(),
    ]
}

/// 证书 SAN：本机 hostname（及 .local 名）+ 可探测到的局域网 IP。
/// 没有 SAN 时现代浏览器即使信任该证书也会报名称不匹配。
#[cfg(unix)]
fn san_args() -> Vec<String> {
    let mut names: Vec<String> = vec!["localhost".to_string()];
    if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
        let h = h.trim();
        let ok = !h.is_empty()
            && h.len() <= 63
            && h.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
        if ok {
            names.push(h.to_string());
            if !h.contains('.') {
                names.push(format!("{h}.local"));
            }
        }
    }
    let mut san = format!(
        "subjectAltName={}",
        names
            .iter()
            .map(|n| format!("DNS:{n}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    if let Some(ip) = local_ip() {
        san.push_str(&format!(",IP:{ip}"));
    }
    vec!["-addext".to_string(), san]
}

/// 探测本机对外 IP（UDP connect 不实际发包，只看本地路由选出的地址）。
#[cfg(unix)]
fn local_ip() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    if ip.is_unspecified() || ip.is_loopback() {
        None
    } else {
        Some(ip)
    }
}

// ---------------------------------------------------------------- Linux 进程

#[cfg(unix)]
const PR_SET_PDEATHSIG: i32 = 1;
#[cfg(unix)]
const SIGTERM: i32 = 15;

#[cfg(unix)]
unsafe extern "C" {
    fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
}

/// 运行中的 TLS 前端（socat 子进程）。Drop 时结束它。
#[cfg(unix)]
pub struct TlsFrontEnd {
    child: std::process::Child,
}

#[cfg(unix)]
impl TlsFrontEnd {
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
impl Drop for TlsFrontEnd {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 设置文件权限（unix）。
#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

/// 确保证书存在；不存在则用 openssl 生成一张自签名证书。
///
/// 安全约束：`plan()` 已限制路径必须位于 `<配置目录>/tls/`；这里再检查目标是
/// 普通文件（拒绝符号链接/块设备/字符设备），避免被指向 `/etc/passwd`、
/// `/dev/mmcblk0` 之类的位置。
#[cfg(unix)]
fn ensure_cert(p: &Plan) -> Result<(), String> {
    for f in [&p.cert, &p.key] {
        match std::fs::symlink_metadata(f) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "证书/私钥路径是符号链接，拒绝写入：{}",
                    f.display()
                ));
            }
            Ok(meta) if !meta.is_file() => {
                return Err(format!(
                    "证书/私钥路径不是普通文件，拒绝写入：{}",
                    f.display()
                ));
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    if p.cert.is_file() && p.key.is_file() {
        chmod(&p.key, 0o600);
        return Ok(());
    }
    for f in [&p.cert, &p.key] {
        if let Some(dir) = f.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("无法创建证书目录 {}：{e}", dir.display()))?;
        }
    }
    let _ = std::fs::create_dir_all(&p.tls_dir);
    chmod(&p.tls_dir, 0o700);
    println!(
        "https: 证书不存在，正在生成自签名证书 {}（浏览器首次访问需手动信任）",
        p.cert.display()
    );
    let mut args = openssl_args(p);
    args.extend(san_args());
    let out = std::process::Command::new("openssl")
        .args(args)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "找不到 openssl，请先安装：sudo apt install openssl".to_string()
            } else {
                format!("调用 openssl 失败：{e}")
            }
        })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("openssl 生成证书失败：{}", err.trim()));
    }
    chmod(&p.key, 0o600);
    Ok(())
}

/// 启动 `socat` 做 TLS 终止（内部先确保证书存在）。
#[cfg(unix)]
pub fn start(p: &Plan) -> Result<TlsFrontEnd, String> {
    ensure_cert(p)?;
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new("socat");
    cmd.args(socat_args(p));
    cmd.stdin(std::process::Stdio::null());
    // 父进程不管怎么退出（正常退出 / 崩溃 / 被 kill），socat 都跟着退出，
    // 避免它变成孤儿进程一直占着 HTTPS 端口、让下次启动绑不上。
    unsafe {
        cmd.pre_exec(|| {
            let _ = prctl(PR_SET_PDEATHSIG, SIGTERM as u64, 0, 0, 0);
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "找不到 socat，请先安装：sudo apt install socat".to_string()
        } else {
            format!("启动 socat 失败：{e}")
        }
    })?;
    // socat 绑定失败（端口被占 / 证书读不了）会立刻退出：稍等一下确认它还活着，
    // 否则设置页会误报“运行中”。
    std::thread::sleep(std::time::Duration::from_millis(250));
    if let Ok(Some(status)) = child.try_wait() {
        return Err(format!(
            "socat 启动后立即退出（{status}）——HTTPS 端口可能被占用，或证书/私钥无法读取"
        ));
    }
    Ok(TlsFrontEnd { child })
}

// 非 Linux 占位：本程序只在树莓派（Linux）上跑；保留这个桩是为了在别的平台上
// 也能做 `cargo test` / 类型检查（socat 方案只适用于 Linux）。
#[cfg(not(unix))]
pub struct TlsFrontEnd;

#[cfg(not(unix))]
pub fn start(_p: &Plan) -> Result<TlsFrontEnd, String> {
    Err("内置 HTTPS 仅支持 Linux".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_HTTPS_PORT;

    fn https_cfg() -> Config {
        let mut c = Config::default();
        c.https = true;
        c
    }

    #[test]
    fn plan_uses_web_port_and_default_cert_paths() {
        let c = https_cfg();
        let p = plan(&c, Path::new("/etc/rpi-led-webpush/config")).unwrap();
        assert_eq!(p.https_port, DEFAULT_HTTPS_PORT);
        assert_eq!(p.http_port, c.web_port);
        assert_eq!(p.tls_dir, PathBuf::from("/etc/rpi-led-webpush/tls"));
        assert_eq!(p.cert, PathBuf::from("/etc/rpi-led-webpush/tls/cert.pem"));
        assert_eq!(p.key, PathBuf::from("/etc/rpi-led-webpush/tls/key.pem"));
    }

    #[test]
    fn plan_honours_explicit_ports_and_cert_paths() {
        // 用临时目录拼路径，避免 Windows / Linux 绝对路径语义差异
        let dir = std::env::temp_dir().join(format!("lpwp-plan-explicit-{}", std::process::id()));
        let cfg_path = dir.join("config");
        let mut c = https_cfg();
        c.https_port = 443;
        c.web_port = 9000;
        c.https_cert = dir.join("tls").join("panel.pem").to_string_lossy().into_owned();
        c.https_key = "panel.key".into(); // 相对路径 → tls 目录
        let p = plan(&c, &cfg_path).unwrap();
        assert_eq!(p.https_port, 443);
        assert_eq!(p.http_port, 9000);
        assert_eq!(p.cert, dir.join("tls").join("panel.pem"));
        assert_eq!(p.key, dir.join("tls").join("panel.key"));
    }

    #[test]
    fn plan_rejects_invalid_combinations() {
        let dir = std::env::temp_dir().join(format!("lpwp-plan-reject-{}", std::process::id()));
        let cfg_path = dir.join("config");
        let mut c = https_cfg();
        c.https = false;
        assert!(plan(&c, &cfg_path).is_err(), "未启用时不应启动前端");

        c.https = true;
        c.web_port = 0;
        assert!(plan(&c, &cfg_path).is_err(), "web_port=0 没有后端");

        c.web_port = 8080;
        c.https_port = 8080;
        assert!(plan(&c, &cfg_path).is_err(), "两个端口相同时应拒绝");

        // 证书/私钥路径白名单：逃出 tls 目录、含逗号都要拒绝
        c.https_port = 443;
        c.https_cert = dir.join("outside.pem").to_string_lossy().into_owned();
        assert!(plan(&c, &cfg_path).is_err(), "绝对路径逃逸必须拒绝");
        c.https_cert = "../outside.pem".into();
        assert!(plan(&c, &cfg_path).is_err(), "相对路径逃逸必须拒绝");
        c.https_cert = String::new();
        c.https_key = "a,b.pem".into();
        assert!(plan(&c, &cfg_path).is_err(), "逗号路径必须拒绝");
    }

    #[test]
    fn socat_and_openssl_command_lines() {
        let c = https_cfg();
        let p = plan(&c, Path::new("/etc/rpi-led-webpush/config")).unwrap();

        // 用 plan 里解析出来的路径拼期望值，避免 Windows / Linux 的路径分隔符差异
        assert_eq!(
            socat_args(&p),
            vec![
                format!(
                    "OPENSSL-LISTEN:8443,cert={},key={},verify=0,reuseaddr,fork",
                    p.cert.display(),
                    p.key.display()
                ),
                "TCP:127.0.0.1:8080".to_string(),
            ]
        );
        let listen = &socat_args(&p)[0];
        assert!(listen.starts_with("OPENSSL-LISTEN:8443,"));
        assert!(listen.ends_with(",verify=0,reuseaddr,fork"));

        let a = openssl_args(&p);
        assert_eq!(a[0], "req");
        assert_eq!(a[1], "-x509");
        assert!(a.contains(&"/CN=rpi-led-webpush".to_string()));
        assert!(a.contains(&"rsa:2048".to_string()));
        assert!(a.contains(&"825".to_string()), "有效期应缩短到 825 天: {a:?}");
        assert!(a.iter().any(|s| s.ends_with("cert.pem")));
        assert!(a.iter().any(|s| s.ends_with("key.pem")));
    }
}
