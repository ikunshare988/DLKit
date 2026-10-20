//! 自动更新：检查 GitHub Releases → 下载 → SHA256 校验 → 重启替换。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::engine;

const REPO: &str = "ikunshare988/DLKit";
const API: &str = "https://api.github.com/repos/ikunshare988/DLKit/releases/latest";
const ASSET: &str = "DLKit.exe";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// 空闲（没有可用更新，或用户点了「稍后」）
    Idle,
    Checking,
    Available,
    Downloading,
    Ready,
    Error,
}

#[derive(Clone)]
pub struct State {
    pub phase: Phase,
    pub latest: String,
    pub notes: String,
    pub url: String,
    pub sha_url: String,
    pub size: u64,
    pub progress: f32,
    pub message: String,
}

impl Default for State {
    fn default() -> Self {
        State {
            phase: Phase::Idle,
            latest: String::new(),
            notes: String::new(),
            url: String::new(),
            sha_url: String::new(),
            size: 0,
            progress: 0.0,
            message: String::new(),
        }
    }
}

pub struct Updater {
    st: Arc<Mutex<State>>,
    dir: PathBuf,
}

impl Updater {
    pub fn new() -> Updater {
        let dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(engine::find_root);
        Updater {
            st: Arc::new(Mutex::new(State::default())),
            dir,
        }
    }

    pub fn state(&self) -> State {
        self.st.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn current() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// 用户点「稍后」：收起横幅，本次运行不再提示
    pub fn dismiss(&self) {
        if let Ok(mut s) = self.st.lock() {
            s.phase = Phase::Idle;
            s.message.clear();
        }
    }

    /// 后台检查最新版本（同一时间只跑一个检查）
    pub fn check(&self) {
        {
            let mut s = match self.st.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            if matches!(s.phase, Phase::Checking | Phase::Downloading | Phase::Ready) {
                return;
            }
            s.phase = Phase::Checking;
            s.message = "正在检查更新…".into();
        }
        let st = self.st.clone();
        std::thread::spawn(move || {
            let res = latest_release();
            let mut s = match st.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            match res {
                Ok(rel) => {
                    if is_newer(&rel.tag, Updater::current()) {
                        s.latest = rel.tag.trim_start_matches('v').to_string();
                        s.notes = rel.notes;
                        s.url = rel.url;
                        s.sha_url = rel.sha_url;
                        s.size = rel.size;
                        s.phase = Phase::Available;
                        s.message = format!("发现新版本 v{}", s.latest);
                    } else {
                        s.phase = Phase::Idle;
                        s.message = format!("已是最新版本 v{}", Updater::current());
                    }
                }
                Err(e) => {
                    s.phase = Phase::Error;
                    s.message = e;
                }
            }
        });
    }

    /// 下载新版本到程序同目录的 DLKit.update.exe
    pub fn download(&self) {
        let (url, sha_url, size) = {
            let mut s = match self.st.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            if !matches!(s.phase, Phase::Available | Phase::Error) || s.url.is_empty() {
                return;
            }
            s.phase = Phase::Downloading;
            s.progress = 0.0;
            s.message = "正在下载新版本…".into();
            (s.url.clone(), s.sha_url.clone(), s.size)
        };

        let st = self.st.clone();
        let dest = self.dest();
        std::thread::spawn(move || {
            if let Err(e) = fetch(&url, &dest, size, &st) {
                let _ = std::fs::remove_file(&dest);
                if let Ok(mut s) = st.lock() {
                    s.phase = Phase::Error;
                    s.message = e;
                }
                return;
            }
            // 校验：与官方 SHA256.txt 对不上就不装
            match verify(&dest, &sha_url) {
                Ok(_) => {
                    if let Ok(mut s) = st.lock() {
                        s.phase = Phase::Ready;
                        s.progress = 1.0;
                        s.message = "新版本已下载，重启后完成更新".into();
                    }
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&dest);
                    if let Ok(mut s) = st.lock() {
                        s.phase = Phase::Error;
                        s.message = e;
                    }
                }
            }
        });
    }

    fn dest(&self) -> PathBuf {
        self.dir.join("DLKit.update.exe")
    }

    /// 写一个等待当前进程退出的替换脚本并启动它；调用方随后应立即退出
    pub fn install(&self) -> Result<(), String> {
        let dest = self.dest();
        if !dest.exists() {
            return Err("更新包不存在，请重新下载".into());
        }
        let exe = std::env::current_exe().map_err(|e| format!("定位程序失败：{}", e))?;
        let name = exe
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "DLKit.exe".into());
        let cur = name.replace('%', "%%");
        let dir = exe.parent().ok_or("定位程序目录失败")?.to_path_buf();

        let script = format!(
            "@echo off\r\n\
             cd /d \"%~dp0\"\r\n\
             for /l %%i in (1,1,60) do (\r\n\
             \x20 move /y \"DLKit.update.exe\" \"{cur}\" >nul 2>&1 && goto run\r\n\
             \x20 ping -n 2 127.0.0.1 >nul\r\n\
             )\r\n\
             exit /b\r\n\
             :run\r\n\
             start \"\" \"{cur}\"\r\n\
             ping -n 3 127.0.0.1 >nul\r\n\
             start /b cmd /c del \"%~f0\" >nul 2>&1\r\n\
             exit /b\r\n"
        );
        let path = dir.join("dlkit-update.cmd");
        std::fs::write(&path, script).map_err(|e| format!("写入更新脚本失败：{}", e))?;

        let path_s = path.to_string_lossy().to_string();
        engine::new_cmd("cmd")
            .args(["/c", path_s.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("启动更新脚本失败：{}", e))?;
        Ok(())
    }
}

struct Release {
    tag: String,
    notes: String,
    url: String,
    sha_url: String,
    size: u64,
}

fn curl_text(args: &[String]) -> Result<String, String> {
    match engine::curl_exe(args) {
        Ok((out, _err, code)) if code == 0 => Ok(out),
        Ok((_, err, code)) => {
            let msg = err.lines().last().unwrap_or("").trim().to_string();
            Err(format!(
                "网络请求失败（curl {}）{}",
                code,
                if msg.is_empty() { String::new() } else { format!("：{}", msg) }
            ))
        }
        Err(e) => Err(e),
    }
}

fn latest_release() -> Result<Release, String> {
    // 首选 API：能拿到更新说明和准确的资源地址
    let args: Vec<String> = vec![
        "-sS".into(),
        "-L".into(),
        "--max-time".into(),
        "12".into(),
        "-H".into(),
        "Accept: application/vnd.github+json".into(),
        API.into(),
    ];
    if let Ok(body) = curl_text(&args) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
            let tag = v["tag_name"].as_str().unwrap_or("").to_string();
            if !tag.is_empty() {
                let mut rel = Release {
                    tag: tag.clone(),
                    notes: v["body"].as_str().unwrap_or("").to_string(),
                    url: String::new(),
                    sha_url: String::new(),
                    size: 0,
                };
                if let Some(list) = v["assets"].as_array() {
                    for a in list {
                        let name = a["name"].as_str().unwrap_or("");
                        let url = a["browser_download_url"].as_str().unwrap_or("").to_string();
                        if name.eq_ignore_ascii_case(ASSET) {
                            rel.size = a["size"].as_u64().unwrap_or(0);
                            rel.url = url;
                        } else if name.eq_ignore_ascii_case("SHA256.txt") {
                            rel.sha_url = url;
                        }
                    }
                }
                if rel.url.is_empty() {
                    fill_asset_urls(&mut rel);
                }
                return Ok(rel);
            }
        }
    }
    // 兜底：api.github.com 常被墙，但 github.com 本身通常可达
    let args: Vec<String> = vec![
        "-sS".into(),
        "-L".into(),
        "-o".into(),
        if cfg!(windows) { "NUL" } else { "/dev/null" }.into(),
        "-w".into(),
        "%{url_effective}".into(),
        "--max-time".into(),
        "12".into(),
        format!("https://github.com/{}/releases/latest", REPO),
    ];
    let final_url = curl_text(&args)?;
    let tag = final_url
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    if !tag.starts_with('v') {
        return Err("无法获取版本信息，请检查网络后重试".into());
    }
    let mut rel = Release {
        tag,
        notes: String::new(),
        url: String::new(),
        sha_url: String::new(),
        size: 0,
    };
    fill_asset_urls(&mut rel);
    Ok(rel)
}

fn fill_asset_urls(rel: &mut Release) {
    let base = format!(
        "https://github.com/{}/releases/download/{}",
        REPO, rel.tag
    );
    rel.url = format!("{}/{}", base, ASSET);
    rel.sha_url = format!("{}/SHA256.txt", base);
}

fn is_newer(latest: &str, current: &str) -> bool {
    fn nums(s: &str) -> Vec<u64> {
        s.trim_start_matches('v')
            .split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse().ok())
            .collect()
    }
    let a = nums(latest);
    let b = nums(current);
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 下载到 dest，同时按文件大小回报进度
fn fetch(url: &str, dest: &Path, total: u64, st: &Arc<Mutex<State>>) -> Result<(), String> {
    let dest_s = dest.to_string_lossy().to_string();
    let mut child = engine::curl_command()
        .args([
            "-L",
            "--fail",
            "--retry",
            "3",
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
        .map_err(|e| format!("无法启动 curl：{}", e))?;

    loop {
        std::thread::sleep(Duration::from_millis(250));
        if let Ok(meta) = std::fs::metadata(dest) {
            if let Ok(mut s) = st.lock() {
                s.size = total.max(meta.len());
                if total > 0 {
                    s.progress = (meta.len() as f32 / total as f32).clamp(0.0, 1.0);
                    s.message = format!(
                        "正在下载新版本… {:.0}%（{} / {}）",
                        s.progress * 100.0,
                        human(meta.len()),
                        human(total)
                    );
                } else {
                    s.message = format!("正在下载新版本… {}", human(meta.len()));
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                return Err(format!("下载失败（curl 退出码 {}）", status.code().unwrap_or(-1)));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("下载中断：{}", e)),
        }
    }
}

/// 用官方 SHA256.txt 校验下载文件
fn verify(dest: &Path, sha_url: &str) -> Result<String, String> {
    if sha_url.is_empty() {
        return Ok(String::new());
    }
    let args: Vec<String> = vec![
        "-sS".into(),
        "-L".into(),
        "--max-time".into(),
        "15".into(),
        sha_url.into(),
    ];
    let body = curl_text(&args).map_err(|e| format!("获取校验值失败：{}", e))?;
    let want = body
        .split_whitespace()
        .find(|w| w.len() == 64 && w.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|w| w.to_ascii_lowercase())
        .ok_or("校验文件格式异常")?;
    let got = engine::sha256_of(dest).ok_or("无法读取下载文件")?;
    if got != want {
        return Err(format!(
            "SHA256 校验失败（期望 {}…，实际 {}…）",
            &want[..8],
            &got[..8]
        ));
    }
    Ok(got)
}

fn human(n: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 3 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} B", n)
    } else {
        format!("{:.1} {}", v, U[i])
    }
}

/// 在浏览器里打开 Release 页面
pub fn open_release_page() {
    let url = format!("https://github.com/{}/releases/latest", REPO);
    let _ = engine::new_cmd("cmd")
        .args(["/c", "start", "", &url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
    }

    #[test]
    fn builds_asset_urls() {
        let mut r = Release {
            tag: "v1.2.3".into(),
            notes: String::new(),
            url: String::new(),
            sha_url: String::new(),
            size: 0,
        };
        fill_asset_urls(&mut r);
        assert!(r.url.ends_with("/releases/download/v1.2.3/DLKit.exe"));
        assert!(r.sha_url.ends_with("/releases/download/v1.2.3/SHA256.txt"));
    }

    /// 真实网络联调：cargo test --release real_release -- --ignored
    #[test]
    #[ignore]
    fn real_release_download_and_verify() {
        let dir = std::env::temp_dir().join("dlkit-update-selftest");
        let _ = std::fs::create_dir_all(&dir);
        let dest = dir.join("DLKit.update.exe");
        let rel = latest_release().expect("获取 release 信息失败");
        println!("latest = {}", rel.tag);
        let st = Arc::new(Mutex::new(State::default()));
        fetch(&rel.url, &dest, rel.size, &st).expect("下载失败");
        verify(&dest, &rel.sha_url).expect("校验失败");
        let len = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        println!("downloaded {} bytes, sha ok", len);
        let _ = std::fs::remove_file(&dest);
    }
}
