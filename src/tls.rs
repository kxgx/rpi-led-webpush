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
    Ok(Plan {
        https_port: cfg.https_port,
        http_port: cfg.web_port,
        cert: resolve_path(&cfg.https_cert, &dir, "cert.pem"),
        key: resolve_path(&cfg.https_key, &dir, "key.pem"),
    })
}

fn config_dir(config_path: &Path) -> PathBuf {
    match config_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// 空值 → `<配置目录>/tls/<name>`；相对路径按配置目录解析，绝对路径原样使用。
fn resolve_path(value: &str, dir: &Path, file_name: &str) -> PathBuf {
    let value = value.trim();
    if value.is_empty() {
        return dir.join("tls").join(file_name);
    }
    let p = PathBuf::from(value);
    if p.is_absolute() {
        p
    } else {
        dir.join(p)
    }
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

/// `openssl req ...` 的参数（生成自签名证书，3650 天有效）。
pub fn openssl_args(p: &Plan) -> Vec<String> {
    vec![
        "req".to_string(),
        "-x509".to_string(),
        "-newkey".to_string(),
        "rsa:2048".to_string(),
        "-nodes".to_string(),
        "-days".to_string(),
        "3650".to_string(),
        "-keyout".to_string(),
        p.key.display().to_string(),
        "-out".to_string(),
        p.cert.display().to_string(),
        "-subj".to_string(),
        "/CN=rpi-led-webpush".to_string(),
    ]
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

/// 确保证书存在；不存在则用 openssl 生成一张自签名证书。
#[cfg(unix)]
fn ensure_cert(p: &Plan) -> Result<(), String> {
    if p.cert.is_file() && p.key.is_file() {
        return Ok(());
    }
    for f in [&p.cert, &p.key] {
        if let Some(dir) = f.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("无法创建证书目录 {}：{e}", dir.display()))?;
        }
    }
    println!(
        "https: 证书不存在，正在生成自签名证书 {}（浏览器首次访问需手动信任）",
        p.cert.display()
    );
    let out = std::process::Command::new("openssl")
        .args(openssl_args(p))
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
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p.key, std::fs::Permissions::from_mode(0o600));
    }
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
        assert_eq!(p.cert, PathBuf::from("/etc/rpi-led-webpush/tls/cert.pem"));
        assert_eq!(p.key, PathBuf::from("/etc/rpi-led-webpush/tls/key.pem"));
    }

    #[test]
    fn plan_honours_explicit_ports_and_cert_paths() {
        let mut c = https_cfg();
        c.https_port = 443;
        c.web_port = 9000;
        c.https_cert = "/certs/panel.pem".into();
        c.https_key = "keys/panel.key".into(); // 相对路径 → 配置文件目录
        let p = plan(&c, Path::new("/data/cfg/config")).unwrap();
        assert_eq!(p.https_port, 443);
        assert_eq!(p.http_port, 9000);
        assert_eq!(p.cert, PathBuf::from("/certs/panel.pem"));
        assert_eq!(p.key, PathBuf::from("/data/cfg/keys/panel.key"));
    }

    #[test]
    fn plan_rejects_invalid_combinations() {
        let mut c = https_cfg();
        c.https = false;
        assert!(plan(&c, Path::new("/x/config")).is_err(), "未启用时不应启动前端");

        c.https = true;
        c.web_port = 0;
        assert!(plan(&c, Path::new("/x/config")).is_err(), "web_port=0 没有后端");

        c.web_port = 8080;
        c.https_port = 8080;
        assert!(plan(&c, Path::new("/x/config")).is_err(), "两个端口相同时应拒绝");
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
        assert_eq!(a[a.len() - 1], "/CN=rpi-led-webpush");
        assert!(a.contains(&"rsa:2048".to_string()));
        assert!(a.iter().any(|s| s.ends_with("cert.pem")));
        assert!(a.iter().any(|s| s.ends_with("key.pem")));
    }
}
