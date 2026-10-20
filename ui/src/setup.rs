//! 首次运行的环境准备：默认目录、配置初始化、崩溃日志。
//!
//! 目标是「解压后双击就能用」：不要求用户先复制示例配置、手填 rpc-secret。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 同一进程内连续生成 secret 时，光靠时间戳可能撞在一起，加个计数器
static SECRET_SEQ: AtomicU64 = AtomicU64::new(0x2545_f491_4f6c_dd1d);

/// 单文件发布时的默认 aria2 配置：路径都相对程序目录
const DEFAULT_ARIA2_CONF: &str = r#"# DLKit 自动生成，可按需修改
dir=downloads
split=16
max-connection-per-server=16
min-split-size=1M
file-allocation=none
input-file=config/aria2.session
save-session=config/aria2.session
log=logs/aria2.log
check-certificate=true
enable-rpc=true
rpc-listen-all=false
rpc-listen-port=6800
rpc-secret=__SECRET__
force-save=false
"#;

pub struct Layout {
    /// 程序目录是否可写（放在 Program Files 之类的位置时会是 false）
    pub writable: bool,
    /// 本次是否自动生成了 aria2.conf
    pub created_conf: bool,
}

/// 默认下载目录：优先用系统的「下载」文件夹，找不到再退回程序目录下
pub fn default_download_dir(root: &Path) -> PathBuf {
    if let Ok(home) = std::env::var("USERPROFILE") {
        let p = PathBuf::from(home).join("Downloads");
        if p.is_dir() {
            return p;
        }
    }
    root.join("downloads")
}

/// 启动时补齐运行所需的目录与配置
pub fn ensure_layout(root: &Path) -> Layout {
    let config = root.join("config");
    let logs = root.join("logs");
    let _ = fs::create_dir_all(&config);
    let _ = fs::create_dir_all(&logs);
    let writable = can_write(root);

    let mut created_conf = false;
    let conf = config.join("aria2.conf");
    if !conf.exists() {
        // 有示例就用示例（保留用户自己的模板），没有就写一份内置默认配置。
        // 单文件发布时程序目录里只有 exe，不能指望用户先复制示例。
        let example = config.join("aria2.conf.example");
        let text = fs::read_to_string(&example)
            .unwrap_or_else(|_| DEFAULT_ARIA2_CONF.replace("__SECRET__", "change-me-to-a-random-string"));
        let text = text.replace("change-me-to-a-random-string", &random_secret());
        if fs::write(&conf, text).is_ok() {
            created_conf = true;
        }
    }

    let session = config.join("aria2.session");
    if !session.exists() {
        let _ = fs::write(&session, "");
    }

    Layout {
        writable,
        created_conf,
    }
}

pub fn can_write(dir: &Path) -> bool {
    let probe = dir.join(".dlkit-write-test");
    match fs::write(&probe, b"ok") {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 生成 24 位十六进制随机串，做 aria2 的 rpc-secret
fn random_secret() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut x = nanos
        ^ ((std::process::id() as u128) << 77)
        ^ ((SECRET_SEQ.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed) as u128) << 32);
    let mut out = String::with_capacity(24);
    for _ in 0..24 {
        out.push(char::from_digit(((x >> 60) & 0xf) as u32, 16).unwrap_or('0'));
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
    }
    out
}

/// 崩溃日志：任何 panic 都写一份到 logs，窗口消失后还能查
pub fn install_panic_hook() {
    let root = crate::engine::find_root();
    let dir = root.join("logs");
    std::panic::set_hook(Box::new(move |info| {
        let _ = fs::create_dir_all(&dir);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "未知位置".into());
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "未知错误".into()
        };
        let text = format!(
            "DLKit v{}\n时间: {}\n位置: {}\n信息: {}\n",
            env!("CARGO_PKG_VERSION"),
            ts,
            loc,
            msg
        );
        let path = dir.join(format!("crash-{}.log", ts));
        let _ = fs::write(&path, &text);
        // 保留默认输出，调试运行时还能在终端看到
        eprintln!("{}", text);
    }));
}

/// 找到最近一次崩溃日志（已确认过的会被改名成 .seen）
pub fn last_crash(root: &Path) -> Option<PathBuf> {
    let dir = root.join("logs");
    let mut best: Option<(SystemTime, PathBuf)> = None;
    let rd = fs::read_dir(&dir).ok()?;
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if !name.starts_with("crash-") || !name.ends_with(".log") {
            continue;
        }
        let t = e.metadata().and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH);
        if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
            best = Some((t, p));
        }
    }
    best.map(|(_, p)| p)
}

/// 用户点过「知道了」后把日志改名，避免每次启动都提示
pub fn mark_crash_seen(path: &Path) {
    let seen = path.with_extension("log.seen");
    let _ = fs::rename(path, seen);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_hex_and_unique() {
        let a = random_secret();
        let b = random_secret();
        assert_eq!(a.len(), 24);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn layout_creates_dirs_and_conf() {
        let dir = std::env::temp_dir().join(format!("dlkit-layout-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("config")).unwrap();
        fs::write(
            dir.join("config").join("aria2.conf.example"),
            "rpc-secret=change-me-to-a-random-string\n",
        )
        .unwrap();
        let l = ensure_layout(&dir);
        assert!(l.writable);
        assert!(l.created_conf);
        let conf = fs::read_to_string(dir.join("config").join("aria2.conf")).unwrap();
        assert!(!conf.contains("change-me-to-a-random-string"));
        assert!(dir.join("config").join("aria2.session").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
