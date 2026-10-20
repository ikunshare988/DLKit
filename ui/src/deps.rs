//! 依赖补齐：检测 aria2 / yt-dlp / ffmpeg 是否就位，缺的可以一键下载安装。
//!
//! 只依赖系统自带的 curl（下载）和 PowerShell（解压 zip），
//! 不需要用户装任何开发环境。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::engine;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Downloading,
    Extracting,
    Done,
    Error,
}

pub struct State {
    pub phase: Phase,
    pub key: String,
    pub progress: f32,
    pub message: String,
}

impl Default for State {
    fn default() -> Self {
        State {
            phase: Phase::Idle,
            key: String::new(),
            progress: 0.0,
            message: String::new(),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Kind {
    /// 直接下载成目标 exe
    Exe,
    /// 解压 zip，把同名 exe 复制到指定目录
    Zip {
        dest: &'static str,
        exes: &'static [&'static str],
    },
}

pub struct Dep {
    pub key: &'static str,
    pub name: &'static str,
    pub note: &'static str,
    /// 相对程序目录的检查路径
    pub check: &'static str,
    pub url: &'static str,
    /// 大约多少 MB，仅用于展示
    pub size_mb: u32,
    pub kind: Kind,
}

pub const DEPS: &[Dep] = &[
    Dep {
        key: "aria2",
        name: "aria2",
        note: "BT / 磁力 / aria2 兜底必需",
        check: r"bin\aria2\aria2c.exe",
        url: "https://github.com/aria2/aria2/releases/download/release-1.37.0/aria2-1.37.0-win-64bit-build1.zip",
        size_mb: 3,
        kind: Kind::Zip {
            dest: r"bin\aria2",
            exes: &["aria2c.exe"],
        },
    },
    Dep {
        key: "yt-dlp",
        name: "yt-dlp",
        note: "B 站 / YouTube 等视频站解析",
        check: r"bin\yt-dlp\yt-dlp.exe",
        url: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe",
        size_mb: 20,
        kind: Kind::Exe,
    },
    Dep {
        key: "ffmpeg",
        name: "ffmpeg",
        note: "视频音轨合并（yt-dlp 下载视频时需要）",
        check: r"bin\ffmpeg\bin\ffmpeg.exe",
        url: "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip",
        size_mb: 90,
        kind: Kind::Zip {
            dest: r"bin\ffmpeg\bin",
            exes: &["ffmpeg.exe", "ffprobe.exe"],
        },
    },
];

pub fn installed(root: &Path, dep: &Dep) -> bool {
    root.join(dep.check).exists()
}

pub struct Installer {
    st: Arc<Mutex<State>>,
}

impl Installer {
    pub fn new() -> Installer {
        Installer {
            st: Arc::new(Mutex::new(State::default())),
        }
    }

    pub fn state(&self) -> State {
        self.st
            .lock()
            .map(|s| State {
                phase: s.phase,
                key: s.key.clone(),
                progress: s.progress,
                message: s.message.clone(),
            })
            .unwrap_or_default()
    }

    pub fn start(&self, root: PathBuf, dep: &'static Dep) {
        {
            let mut s = match self.st.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            if matches!(s.phase, Phase::Downloading | Phase::Extracting) {
                return;
            }
            s.phase = Phase::Downloading;
            s.key = dep.key.to_string();
            s.progress = 0.0;
            s.message = format!("正在下载 {}…", dep.name);
        }
        let st = self.st.clone();
        thread::spawn(move || {
            let tmp_dir = std::env::temp_dir().join("dlkit-deps");
            let _ = fs::create_dir_all(&tmp_dir);
            let is_zip = matches!(dep.kind, Kind::Zip { .. });
            let tmp = tmp_dir.join(if is_zip {
                format!("{}.zip", dep.key)
            } else {
                format!("{}.exe", dep.key)
            });
            let _ = fs::remove_file(&tmp);

            let total = head_size(dep.url);
            let mut ok = false;
            for url in [dep.url.to_string(), format!("https://ghfast.top/{}", dep.url)] {
                if fetch(&url, &tmp, total, &st) {
                    ok = true;
                    break;
                }
            }
            if !ok {
                let _ = fs::remove_file(&tmp);
                set_err(&st, format!("{} 下载失败，请检查网络后重试", dep.name));
                return;
            }

            let result = match dep.kind {
                Kind::Exe => {
                    let dest = root.join(dep.check);
                    if let Some(p) = dest.parent() {
                        let _ = fs::create_dir_all(p);
                    }
                    fs::copy(&tmp, &dest)
                        .map(|_| ())
                        .map_err(|e| format!("写入 {} 失败：{}", dest.display(), e))
                }
                Kind::Zip { dest, exes } => {
                    if let Ok(mut s) = st.lock() {
                        s.phase = Phase::Extracting;
                        s.message = format!("正在解压 {}…", dep.name);
                        s.progress = 1.0;
                    }
                    extract(&tmp, &root, dest, exes)
                }
            };
            let _ = fs::remove_file(&tmp);

            match result {
                Ok(()) => {
                    if let Ok(mut s) = st.lock() {
                        s.phase = Phase::Done;
                        s.progress = 1.0;
                        s.message = format!("{} 已安装", dep.name);
                    }
                }
                Err(e) => set_err(&st, e),
            }
        });
    }
}

fn set_err(st: &Arc<Mutex<State>>, msg: String) {
    if let Ok(mut s) = st.lock() {
        s.phase = Phase::Error;
        s.message = msg;
    }
}

/// 取远端文件大小（失败也不影响下载，只是进度条不准）
fn head_size(url: &str) -> u64 {
    let args: Vec<String> = vec![
        "-sSI".into(),
        "-L".into(),
        "--max-time".into(),
        "20".into(),
        url.into(),
    ];
    let out = engine::curl_exe(&args).map(|(o, _, _)| o).unwrap_or_default();
    let mut size = 0u64;
    for line in out.lines() {
        let low = line.to_ascii_lowercase();
        if let Some(v) = low.strip_prefix("content-length:") {
            size = v.trim().parse().unwrap_or(size);
        }
    }
    size
}

fn fetch(url: &str, dest: &Path, total: u64, st: &Arc<Mutex<State>>) -> bool {
    let dest_s = dest.to_string_lossy().to_string();
    let mut child = match engine::curl_command()
        .args([
            "-L",
            "--fail",
            "--retry",
            "2",
            "--connect-timeout",
            "15",
            "-o",
            dest_s.as_str(),
            url,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    loop {
        thread::sleep(Duration::from_millis(250));
        if let Ok(meta) = fs::metadata(dest) {
            if let Ok(mut s) = st.lock() {
                if total > 0 {
                    s.progress = (meta.len() as f32 / total as f32).clamp(0.0, 1.0);
                    s.message = format!(
                        "正在下载… {:.0}%（{} / {}）",
                        s.progress * 100.0,
                        human(meta.len()),
                        human(total)
                    );
                } else {
                    s.message = format!("正在下载… {}", human(meta.len()));
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => return false,
        }
    }
}

fn extract(zip: &Path, root: &Path, dest_rel: &str, exes: &[&str]) -> Result<(), String> {
    let out = std::env::temp_dir().join(format!("dlkit-deps-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out);
    let _ = fs::create_dir_all(&out);
    let ps = format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        esc(&zip.to_string_lossy()),
        esc(&out.to_string_lossy())
    );
    let status = engine::new_cmd("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("无法启动解压进程：{}", e))?;
    if !status.success() {
        let _ = fs::remove_dir_all(&out);
        return Err("解压失败：压缩包可能已损坏".into());
    }
    let dest = root.join(dest_rel);
    fs::create_dir_all(&dest).map_err(|e| format!("创建目录失败：{}", e))?;
    for name in exes {
        let found = find_file(&out, name, 0).ok_or_else(|| format!("压缩包里没有 {} ", name))?;
        let target = dest.join(name);
        fs::copy(&found, &target)
            .map_err(|e| format!("复制 {} 失败：{}", name, e))?;
    }
    let _ = fs::remove_dir_all(&out);
    Ok(())
}

fn find_file(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    if depth > 5 {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    let mut dirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            dirs.push(p);
        } else if p.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case(name)).unwrap_or(false) {
            return Some(p);
        }
    }
    for d in dirs {
        if let Some(p) = find_file(&d, name, depth + 1) {
            return Some(p);
        }
    }
    None
}

fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

fn human(n: u64) -> String {
    const U: [&str; 3] = ["KB", "MB", "GB"];
    let mut v = n as f64 / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < 2 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", v, U[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实网络联调：cargo test --release real_install -- --ignored
    #[test]
    #[ignore]
    fn real_install_aria2() {
        let root = std::env::temp_dir().join(format!("dlkit-dep-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let dep = &DEPS[0];
        assert_eq!(dep.key, "aria2");
        let ins = Installer::new();
        ins.start(root.clone(), dep);
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        loop {
            let s = ins.state();
            if s.phase == Phase::Done {
                break;
            }
            if s.phase == Phase::Error {
                panic!("安装失败：{}", s.message);
            }
            assert!(std::time::Instant::now() < deadline, "安装超时：{}", s.message);
            thread::sleep(Duration::from_millis(300));
        }
        let exe = root.join(dep.check);
        assert!(exe.exists(), "没有生成 {}", exe.display());
        let out = std::process::Command::new(&exe)
            .arg("--version")
            .output()
            .expect("aria2c 无法运行");
        assert!(out.status.success());
        let _ = fs::remove_dir_all(&root);
    }
}
