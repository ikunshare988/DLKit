//! 下载完成后的安全检查（增强版）。
//!
//! 三层：文件名/文件头启发式 → Authenticode 签名 → Windows Defender 单文件扫描。
//! 原则：只报告，不自动删除；隔离是把文件移到 quarantine 目录并留下记录，可人工恢复。

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum State {
    Clean,
    Suspicious,
    Threat,
    Error,
    Skipped,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub state: State,
    /// 实际跑过的检查，例如「启发式 + 签名 + Defender + 云端」
    pub engine: String,
    pub findings: Vec<String>,
    pub at: u64,
    /// 用户点了「保留」，不再提示
    pub accepted: bool,
    /// 云端查询用的 SHA256（只有开启云端时才计算）
    #[serde(default)]
    pub sha256: Option<String>,
}

/// `config/security.json` 里的云端信誉配置（增强版）
#[derive(Clone, Default, Deserialize)]
pub struct CloudCfg {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_min_detections")]
    pub min_detections: u32,
}

fn default_min_detections() -> u32 {
    1
}

#[derive(Deserialize, Default)]
struct SecurityRaw {
    #[serde(default)]
    cloud: CloudCfg,
}

pub fn load_cloud(root: &Path) -> CloudCfg {
    let path = root.join("config").join("security.json");
    let Ok(text) = fs::read_to_string(path) else {
        return CloudCfg::default();
    };
    serde_json::from_str::<SecurityRaw>(text.trim_start_matches('\u{feff}'))
        .map(|r| r.cloud)
        .unwrap_or_default()
}

pub fn save_cloud(root: &Path, cfg: &CloudCfg) -> Result<(), String> {
    let dir = root.join("config");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let v = serde_json::json!({
        "cloud": {
            "enabled": cfg.enabled,
            "api_key": cfg.api_key,
            "min_detections": cfg.min_detections,
        }
    });
    fs::write(dir.join("security.json"), v.to_string()).map_err(|e| e.to_string())
}

impl Report {
    pub fn summary(&self) -> String {
        match self.state {
            State::Clean => "未发现威胁".into(),
            State::Threat => "发现风险，建议隔离".into(),
            State::Suspicious => "有可疑特征，请确认来源".into(),
            State::Error => "检查未能完成".into(),
            State::Skipped => "已跳过检查".into(),
        }
    }

    pub fn alarming(&self) -> bool {
        !self.accepted && matches!(self.state, State::Threat | State::Suspicious)
    }
}

const EXEC_EXT: &[&str] = &[
    "exe", "com", "scr", "pif", "bat", "cmd", "ps1", "vbs", "vbe", "js", "jse", "wsf", "wsh",
    "msi", "msp", "jar", "apk", "dll", "sys", "cpl", "hta", "reg",
];
const ARCHIVE_EXT: &[&str] = &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "img"];
const DOC_EXT: &[&str] = &["doc", "docx", "xls", "xlsx", "ppt", "pptx", "docm", "xlsm", "pptm", "pdf", "rtf"];
const DECOY_EXT: &[&str] = &[
    "pdf", "jpg", "jpeg", "png", "gif", "bmp", "txt", "doc", "docx", "xls", "xlsx", "ppt",
    "pptx", "mp3", "mp4", "avi", "mkv", "zip", "rar",
];

fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// 只对可能带来执行风险的类型做检查，视频/图片/音频直接跳过
pub fn eligible(path: &Path) -> bool {
    let e = ext_of(path);
    EXEC_EXT.contains(&e.as_str()) || ARCHIVE_EXT.contains(&e.as_str()) || DOC_EXT.contains(&e.as_str())
}

pub fn scan(path: &Path, use_defender: bool, max_defender_gb: f64, cloud: &CloudCfg) -> Report {
    let now = now_secs();
    if !eligible(path) {
        return Report {
            state: State::Skipped,
            engine: "类型跳过".into(),
            findings: vec![format!("{} 不是可执行文件或压缩包", ext_of(path))],
            at: now,
            accepted: false,
            sha256: None,
        };
    }

    let mut engines = vec!["启发式".to_string()];
    let (mut findings, threat, suspicious) = heuristics(path);

    let mut defender_threat = false;
    let mut defender_note = String::new();
    let mut defender_failed = false;
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if use_defender {
        let limit = (max_defender_gb.max(0.0) * 1024.0 * 1024.0 * 1024.0) as u64;
        if limit > 0 && size > limit {
            defender_note = format!("文件超过 {} GB，跳过 Defender", max_defender_gb);
        } else {
            match defender_scan(path) {
                Ok(None) => engines.push("Defender".into()),
                Ok(Some(msg)) => {
                    engines.push("Defender".into());
                    defender_threat = true;
                    findings.push(msg);
                }
                Err(e) => {
                    defender_failed = true;
                    defender_note = e;
                }
            }
        }
    }

    let mut all = findings;
    if !defender_note.is_empty() {
        all.push(defender_note);
    }

    // 云端信誉：只发 SHA256，不上传文件
    let mut hash = None;
    let mut cloud_threat = false;
    if cloud.enabled && !cloud.api_key.trim().is_empty() {
        if let Some(h) = crate::engine::sha256_of(path) {
            hash = Some(h.clone());
            match cloud_lookup(&h, cloud.api_key.trim()) {
                Ok(CloudVerdict::Clean(total)) => {
                    engines.push("云端".into());
                    all.push(format!("云端信誉：0/{} 个引擎标记为恶意", total));
                }
                Ok(CloudVerdict::Malicious(hits, total)) => {
                    engines.push("云端".into());
                    cloud_threat = true;
                    all.push(format!("云端信誉：{}/{} 个引擎标记为恶意", hits, total));
                }
                Ok(CloudVerdict::Unknown) => {
                    engines.push("云端".into());
                    all.push("云端没有该文件的记录（新样本常见，不代表安全）".into());
                }
                Err(e) => all.push(format!("云端查询失败：{}", e)),
            }
        }
    }

    let state = if defender_threat || threat || cloud_threat {
        State::Threat
    } else if suspicious {
        State::Suspicious
    } else if defender_failed {
        // 系统引擎没跑成，不能对用户说「未发现威胁」
        State::Error
    } else {
        State::Clean
    };
    Report {
        state,
        engine: engines.join(" + "),
        findings: all,
        at: now,
        accepted: false,
        sha256: hash,
    }
}

enum CloudVerdict {
    Clean(u64),
    Malicious(u64, u64),
    Unknown,
}

/// VirusTotal v3：只按 SHA256 查询，命中 404 表示没有记录
fn cloud_lookup(hash: &str, key: &str) -> Result<CloudVerdict, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(format!("https://www.virustotal.com/api/v3/files/{}", hash))
        .header("x-apikey", key)
        .send()
        .map_err(|e| format!("网络错误：{}", e))?;
    match resp.status().as_u16() {
        200 => {
            let v: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
            let s = &v["data"]["attributes"]["last_analysis_stats"];
            let mal = s["malicious"].as_u64().unwrap_or(0)
                + s["suspicious"].as_u64().unwrap_or(0);
            let clean = s["harmless"].as_u64().unwrap_or(0)
                + s["undetected"].as_u64().unwrap_or(0);
            let total = mal + clean;
            if mal > 0 {
                Ok(CloudVerdict::Malicious(mal, total))
            } else {
                Ok(CloudVerdict::Clean(total))
            }
        }
        404 => Ok(CloudVerdict::Unknown),
        401 | 403 => Err("API key 无效或无权限".into()),
        429 => Err("达到查询频率上限，请稍后再试".into()),
        c => Err(format!("HTTP {}", c)),
    }
}

/// 返回 (发现列表, 是否命中威胁, 是否有可疑特征)
fn heuristics(path: &Path) -> (Vec<String>, bool, bool) {
    let mut findings = Vec::new();
    let mut threat = false;
    let mut suspicious = false;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let ext = ext_of(path);

    // 从右到左覆盖字符（U+202E）：常用来把 .exe 伪装成 .pdf
    if name.contains('\u{202e}') {
        threat = true;
        findings.push("文件名包含从右到左覆盖字符，常用于伪装扩展名".into());
    }

    // 双扩展名：report.pdf.exe
    let lower = name.to_ascii_lowercase();
    if EXEC_EXT.contains(&ext.as_str()) {
        if let Some((stem, _)) = lower.rsplit_once('.') {
            if let Some((_, inner)) = stem.rsplit_once('.') {
                if DECOY_EXT.contains(&inner) {
                    suspicious = true;
                    findings.push(format!("双扩展名伪装：.{} 结尾却带 .{} 扩展名", inner, ext));
                }
            }
        }
    }

    // 文件头与扩展名是否相符
    let head = read_head(path, 4096);
    let is_pe = looks_like_pe(&head);
    let is_zip = head.starts_with(b"PK\x03\x04");
    let is_rar = head.starts_with(b"Rar!");
    let is_7z = head.starts_with(b"7z\xBC\xAF\x27\x1C");
    let is_pdf = head.starts_with(b"%PDF");
    let is_script = matches!(ext.as_str(), "bat" | "cmd" | "ps1" | "vbs" | "vbe" | "js" | "jse" | "wsf" | "wsh" | "hta" | "reg");

    if EXEC_EXT.contains(&ext.as_str()) && !is_pe && !is_script {
        suspicious = true;
        findings.push(format!("扩展名是 .{}，但文件内容不是可执行程序", ext));
    }
    if is_pe && !EXEC_EXT.contains(&ext.as_str()) {
        suspicious = true;
        findings.push(format!("扩展名是 .{}，内容却是 Windows 可执行程序", ext));
    }
    if is_pdf && ext != "pdf" {
        suspicious = true;
        findings.push(format!("扩展名是 .{}，内容却是 PDF", ext));
    }
    if ARCHIVE_EXT.contains(&ext.as_str()) {
        let compressed = is_zip
            || is_rar
            || is_7z
            || head.starts_with(&[0x1F, 0x8B])          // gzip
            || head.starts_with(b"BZh")                  // bzip2
            || head.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) // xz
            || head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]); // zstd
        if !compressed && ext != "tar" {
            suspicious = true;
            findings.push("扩展名是压缩包，但文件头不匹配".into());
        }
    }

    // 可执行文件的数字签名
    if is_pe {
        match signature(path) {
            Sig::Unsigned => {
                suspicious = true;
                findings.push("没有内嵌数字签名（系统目录里的程序可能用目录签名）".into());
            }
            Sig::Untrusted => {
                suspicious = true;
                findings.push("数字签名无法验证（可能被修改或证书不受信任）".into());
            }
            Sig::Trusted => {}
        }
    }

    // 宏文档
    if matches!(ext.as_str(), "docm" | "xlsm" | "pptm") {
        suspicious = true;
        findings.push("文档包含宏，打开前请确认来源".into());
    }

    (findings, threat, suspicious)
}

fn read_head(path: &Path, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    match fs::File::open(path) {
        Ok(mut f) => {
            let mut read = 0;
            while read < n {
                match f.read(&mut buf[read..]) {
                    Ok(0) => break,
                    Ok(k) => read += k,
                    Err(_) => break,
                }
            }
            buf.truncate(read);
            buf
        }
        Err(_) => Vec::new(),
    }
}

fn looks_like_pe(b: &[u8]) -> bool {
    if b.len() < 0x40 || &b[0..2] != b"MZ" {
        return false;
    }
    let off = u32::from_le_bytes([b[0x3c], b[0x3d], b[0x3e], b[0x3f]]) as usize;
    off + 4 <= b.len() && &b[off..off + 4] == b"PE\0\0"
}

// ---------------- Authenticode ----------------

enum Sig {
    Trusted,
    Unsigned,
    Untrusted,
}

#[cfg(windows)]
fn signature(path: &Path) -> Sig {
    use std::os::windows::ffi::OsStrExt;

    const WTD_UI_NONE: u32 = 2;
    const WTD_REVOKE_NONE: u32 = 0;
    const WTD_CHOICE_FILE: u32 = 1;
    const WTD_STATEACTION_IGNORE: u32 = 0;
    const TRUST_E_NOSIGNATURE: i32 = 0x800B_0100u32 as i32;

    #[repr(C)]
    struct Guid {
        d1: u32,
        d2: u16,
        d3: u16,
        d4: [u8; 8],
    }
    #[repr(C)]
    struct FileInfo {
        cb_struct: u32,
        file_path: *const u16,
        h_file: isize,
        known_subject: *mut core::ffi::c_void,
    }
    #[repr(C)]
    struct TrustData {
        cb_struct: u32,
        policy_cb: *mut core::ffi::c_void,
        sip_client: *mut core::ffi::c_void,
        ui_choice: u32,
        revoke: u32,
        union_choice: u32,
        file: *mut FileInfo,
        state_action: u32,
        state_data: isize,
        prov_flags: u32,
        ui_context: u32,
        signature_settings: *mut core::ffi::c_void,
    }
    extern "system" {
        fn WinVerifyTrust(hwnd: isize, action: *mut Guid, data: *mut core::ffi::c_void) -> i32;
    }

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut file = FileInfo {
        cb_struct: std::mem::size_of::<FileInfo>() as u32,
        file_path: wide.as_ptr(),
        h_file: 0,
        known_subject: std::ptr::null_mut(),
    };
    let mut data = TrustData {
        cb_struct: std::mem::size_of::<TrustData>() as u32,
        policy_cb: std::ptr::null_mut(),
        sip_client: std::ptr::null_mut(),
        ui_choice: WTD_UI_NONE,
        revoke: WTD_REVOKE_NONE,
        union_choice: WTD_CHOICE_FILE,
        file: &mut file,
        state_action: WTD_STATEACTION_IGNORE,
        state_data: 0,
        prov_flags: 0,
        ui_context: 0,
        signature_settings: std::ptr::null_mut(),
    };
    let mut action = Guid {
        d1: 0x00AA_C56B,
        d2: 0xCD44,
        d3: 0x11D0,
        d4: [0x8C, 0xC2, 0x00, 0xC0, 0x4F, 0xC2, 0x95, 0xEE],
    };
    let rc = unsafe { WinVerifyTrust(0, &mut action, &mut data as *mut _ as *mut core::ffi::c_void) };
    if std::env::var_os("DLKIT_DEBUG_SIG").is_some() {
        eprintln!("WinVerifyTrust rc=0x{:08X} ({})", rc as u32, path.display());
    }
    if rc == 0 {
        Sig::Trusted
    } else if rc == TRUST_E_NOSIGNATURE {
        Sig::Unsigned
    } else {
        Sig::Untrusted
    }
}

#[cfg(not(windows))]
fn signature(_path: &Path) -> Sig {
    Sig::Unsigned
}

// ---------------- Windows Defender ----------------

fn defender_exe() -> Option<PathBuf> {
    if let Ok(pd) = std::env::var("ProgramData") {
        let base = PathBuf::from(pd).join("Microsoft\\Windows Defender\\Platform");
        if let Ok(rd) = fs::read_dir(&base) {
            let mut best: Option<(SystemTime, PathBuf)> = None;
            for e in rd.flatten() {
                let exe = e.path().join("MpCmdRun.exe");
                if !exe.exists() {
                    continue;
                }
                let t = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH);
                if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
                    best = Some((t, exe));
                }
            }
            if let Some((_, p)) = best {
                return Some(p);
            }
        }
    }
    let alt = PathBuf::from("C:\\Program Files\\Windows Defender\\MpCmdRun.exe");
    if alt.exists() {
        return Some(alt);
    }
    None
}

/// Ok(None)=干净，Ok(Some(msg))=发现威胁，Err=无法扫描
fn defender_scan(path: &Path) -> Result<Option<String>, String> {
    let exe = defender_exe().ok_or_else(|| "未找到 Windows Defender，已跳过".to_string())?;
    // 输出写到临时文件：既能拿到判定依据，又不会弹控制台
    let log_path = std::env::temp_dir().join(format!(
        "dlkit-defender-{}.log",
        std::process::id()
    ));
    let log = fs::File::create(&log_path).map_err(|e| format!("创建日志失败：{}", e))?;
    let log_err = log.try_clone().map_err(|e| format!("创建日志失败：{}", e))?;
    #[allow(unused_mut)]
    let mut cmd = Command::new(&exe);
    cmd.args(["-Scan", "-ScanType", "3", "-File"])
        .arg(path)
        // 只检测不处置：绝不替用户删除文件
        .arg("-DisableRemediation")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW | IDLE_PRIORITY_CLASS
        cmd.creation_flags(0x0800_0000 | 0x0000_0040);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::remove_file(&log_path);
            return Err(format!("Defender 启动失败：{}", e));
        }
    };
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let code = st.code().unwrap_or(-1);
                let text = fs::read_to_string(&log_path).unwrap_or_default();
                let _ = fs::remove_file(&log_path);
                let low = text.to_lowercase();
                // Defender 被禁用时同样返回 2，必须看输出才能区分
                if low.contains("0x80004005") || low.contains("[failed]") || low.contains("failed with") {
                    return Err("Defender 无法扫描（服务被禁用或被其他安全软件接管）".into());
                }
                return match code {
                    0 => Ok(None),
                    2 => Ok(Some("Windows Defender 报告该文件存在威胁".into())),
                    c => Err(format!("Defender 返回退出码 {}", c)),
                };
            }
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = fs::remove_file(&log_path);
                    return Err("Defender 扫描超时（180 秒），已跳过".into());
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                let _ = fs::remove_file(&log_path);
                return Err(format!("Defender 扫描失败：{}", e));
            }
        }
    }
}

// ---------------- 沙箱 / 隔离 ----------------

pub fn quarantine_dir(root: &Path) -> PathBuf {
    root.join("quarantine")
}

/// 沙箱暂存目录：和下载目录同盘，放行时用 rename，不复制大文件
pub fn sandbox_dir(download_dir: &Path) -> PathBuf {
    download_dir.join(".dlkit-sandbox")
}

/// 把检查通过的文件从沙箱放行到下载目录，重名时加 (1)
pub fn release(from: &Path, dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| format!("创建下载目录失败：{}", e))?;
    let name = from
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file.bin");
    let mut dest = dir.join(name);
    if dest.exists() {
        let (stem, ext) = match name.rsplit_once('.') {
            Some((a, b)) if !a.is_empty() => (a.to_string(), format!(".{}", b)),
            _ => (name.to_string(), String::new()),
        };
        for i in 1..1000 {
            let cand = dir.join(format!("{} ({}){}", stem, i, ext));
            if !cand.exists() {
                dest = cand;
                break;
            }
        }
    }
    match fs::rename(from, &dest) {
        Ok(_) => Ok(dest),
        Err(_) => {
            fs::copy(from, &dest).map_err(|e| format!("复制到下载目录失败：{}", e))?;
            fs::remove_file(from).map_err(|e| format!("移除沙箱文件失败：{}", e))?;
            Ok(dest)
        }
    }
}

/// 把文件移进 quarantine，并写一份来源记录；失败时返回错误，不会删除原文件
pub fn quarantine(root: &Path, path: &Path, reason: &str) -> Result<PathBuf, String> {
    let dir = quarantine_dir(root);
    fs::create_dir_all(&dir).map_err(|e| format!("创建隔离目录失败：{}", e))?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file.bin");
    let stamp = now_secs();
    let mut dest = dir.join(format!("{}-{}", stamp, name));
    let mut n = 1;
    while dest.exists() {
        dest = dir.join(format!("{}-{}-{}", stamp, n, name));
        n += 1;
    }
    match fs::rename(path, &dest) {
        Ok(_) => {}
        Err(_) => {
            fs::copy(path, &dest).map_err(|e| format!("复制到隔离目录失败：{}", e))?;
            fs::remove_file(path).map_err(|e| format!("删除原文件失败：{}", e))?;
        }
    }
    let meta = serde_json::json!({
        "original": path.to_string_lossy(),
        "quarantined_at": stamp,
        "reason": reason,
    });
    let _ = fs::write(dest.with_extension("json"), meta.to_string());
    Ok(dest)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dlkit-av-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir.join(name)
    }

    #[test]
    fn only_risky_types_are_checked() {
        assert!(eligible(Path::new("a.exe")));
        assert!(eligible(Path::new("a.zip")));
        assert!(eligible(Path::new("a.docm")));
        assert!(!eligible(Path::new("a.mp4")));
        assert!(!eligible(Path::new("a.jpg")));
    }

    #[test]
    fn rlo_filename_is_a_threat() {
        let p = tmp("\u{202e}fdp.exe");
        fs::write(&p, b"not really an exe").unwrap();
        let (_, threat, _) = heuristics(&p);
        assert!(threat);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn double_extension_is_suspicious() {
        let p = tmp("invoice.pdf.exe");
        fs::write(&p, b"plain text").unwrap();
        let (findings, _, suspicious) = heuristics(&p);
        assert!(suspicious);
        assert!(findings.iter().any(|f| f.contains("双扩展名")));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn pe_without_signature_is_suspicious() {
        let p = tmp("fake.exe");
        let mut data = vec![0u8; 0x200];
        data[0..2].copy_from_slice(b"MZ");
        data[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        data[0x80..0x84].copy_from_slice(b"PE\0\0");
        fs::write(&p, &data).unwrap();
        let (findings, _, suspicious) = heuristics(&p);
        assert!(suspicious);
        assert!(findings.iter().any(|f| f.contains("数字签名")));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn release_moves_out_with_suffix() {
        let root = std::env::temp_dir().join(format!("dlkit-av-rel-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let sbx = sandbox_dir(&root);
        fs::create_dir_all(&sbx).unwrap();
        fs::write(root.join("a.bin"), b"old").unwrap();
        fs::write(sbx.join("a.bin"), b"new").unwrap();
        let dest = release(&sbx.join("a.bin"), &root).unwrap();
        assert_eq!(dest.file_name().unwrap(), "a (1).bin");
        assert!(!sbx.join("a.bin").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn quarantine_moves_file_and_keeps_record() {
        let root = std::env::temp_dir().join(format!("dlkit-av-q-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let src = root.join("bad.exe");
        fs::write(&src, b"x").unwrap();
        let dest = quarantine(&root, &src, "test").unwrap();
        assert!(!src.exists());
        assert!(dest.exists());
        assert!(dest.with_extension("json").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
