//! 浏览器接管：本机回环桥 + Chrome/Edge native messaging host。
//!
//! 浏览器扩展不直接下载文件，只把链接、来源页交给正在运行的 DLKit。
//! 所有通信都在 127.0.0.1 上，用随机 token 校验，不开放外网端口。

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const HOST_NAME: &str = "com.dlkit.bridge";
/// 由 browser-extension/manifest.json 里的 key 固定下来，不能随意改
pub const EXTENSION_ID: &str = "ffbjgmkdclbopflkmjbmbhmaaonhlakf";

const MAX_BODY: usize = 256 * 1024;
const MAX_ITEMS: usize = 200;
const MAX_URL: usize = 8 * 1024;

#[derive(Clone, Debug)]
pub struct Item {
    pub url: String,
    pub referer: String,
    pub filename: String,
}

#[derive(Debug)]
pub enum Msg {
    Download(Vec<Item>),
    Scan(String),
    Show,
    /// 扩展加载成功后的握手
    Hello,
}

#[derive(Clone, Serialize, Deserialize)]
struct State {
    port: u16,
    token: String,
    pid: u32,
}

#[derive(Deserialize)]
struct Req {
    #[serde(default)]
    token: String,
    action: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    origin: String,
    #[serde(default)]
    items: Vec<RawItem>,
}

#[derive(Deserialize)]
struct RawItem {
    url: String,
    #[serde(default)]
    referer: String,
    #[serde(default)]
    filename: String,
}

pub struct Bridge {
    pub port: u16,
    token: String,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        // 只删自己的，避免把新实例的记录删掉
        if let Some(s) = read_state() {
            if s.token == self.token {
                let _ = std::fs::remove_file(state_path());
            }
        }
    }
}

/// 启动本地桥，返回监听端口；失败时返回 None（浏览器接管不可用，但主程序照常）。
pub fn start(tx: Sender<Msg>) -> Option<Bridge> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    let token = random_token();
    if !write_state(&State {
        port,
        token: token.clone(),
        pid: std::process::id(),
    }) {
        return None;
    }
    let own = token.clone();
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut c) = conn else { continue };
            let _ = c.set_read_timeout(Some(Duration::from_secs(3)));
            let _ = c.set_write_timeout(Some(Duration::from_secs(3)));
            let line = match read_line(&mut c) {
                Some(l) => l,
                None => continue,
            };
            let resp = handle_line(&line, &own, &tx);
            let _ = c.write_all(format!("{}\n", resp).as_bytes());
        }
    });
    Some(Bridge { port, token })
}

fn handle_line(line: &str, token: &str, tx: &Sender<Msg>) -> String {
    let req: Req = match serde_json::from_str(line.trim()) {
        Ok(r) => r,
        Err(_) => return err("请求格式错误"),
    };
    if req.token != token {
        return err("令牌无效");
    }
    match req.action.as_str() {
        "ping" => ok("在线"),
        "hello" => {
            if !req.origin.is_empty()
                && !req
                    .origin
                    .starts_with(&format!("chrome-extension://{}/", EXTENSION_ID))
            {
                return err("来源不是 DLKit 扩展");
            }
            mark_extension(&req.origin);
            let _ = tx.send(Msg::Hello);
            ok("已连接")
        }
        "show" => {
            let _ = tx.send(Msg::Show);
            ok("已显示")
        }
        "scan" => {
            if !valid_url(&req.url) {
                return err("只支持 http/https 链接");
            }
            let _ = tx.send(Msg::Scan(req.url.clone()));
            ok("已开始解析")
        }
        "download" => {
            let items: Vec<Item> = req
                .items
                .into_iter()
                .filter(|i| valid_url(&i.url))
                .take(MAX_ITEMS)
                .map(|i| Item {
                    url: i.url,
                    referer: i.referer,
                    filename: i.filename,
                })
                .collect();
            if items.is_empty() {
                return err("没有可用的下载地址");
            }
            let n = items.len();
            let _ = tx.send(Msg::Download(items));
            ok(&format!("已接收 {} 个任务", n))
        }
        _ => err("未知操作"),
    }
}

fn ok(msg: &str) -> String {
    serde_json::json!({ "ok": true, "message": msg }).to_string()
}

fn err(msg: &str) -> String {
    serde_json::json!({ "ok": false, "message": msg }).to_string()
}

fn valid_url(url: &str) -> bool {
    let s = url.trim();
    (s.starts_with("http://") || s.starts_with("https://"))
        && s.len() <= MAX_URL
        && !s.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn app_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("DLKit")
}

pub fn state_path() -> PathBuf {
    app_dir().join("bridge.json")
}

fn extension_state_path() -> PathBuf {
    app_dir().join("extension.json")
}

/// 最近一次扩展握手的时间（Unix 秒），没有握过手就是 None
pub fn extension_seen() -> Option<u64> {
    let text = std::fs::read_to_string(extension_state_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("at").and_then(|x| x.as_u64())
}

fn mark_extension(origin: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let body = serde_json::json!({ "at": now, "origin": origin });
    let _ = std::fs::create_dir_all(app_dir());
    let _ = std::fs::write(extension_state_path(), body.to_string());
}

fn read_state() -> Option<State> {
    let text = std::fs::read_to_string(state_path()).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_state(s: &State) -> bool {
    let dir = app_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let tmp = dir.join("bridge.json.tmp");
    let body = match serde_json::to_string(s) {
        Ok(v) => v,
        Err(_) => return false,
    };
    if std::fs::write(&tmp, body).is_err() {
        return false;
    }
    std::fs::rename(&tmp, state_path()).is_ok()
}

fn random_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for _ in 0..2 {
        let mut h = RandomState::new().build_hasher();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        h.write_u32(std::process::id());
        h.write_u128(now);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

fn connect(port: u16) -> Option<TcpStream> {
    let addr = format!("127.0.0.1:{}", port);
    TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(400)).ok()
}

/// 读一行 JSON（最多 256KB）
fn read_line(c: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match c.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > MAX_BODY {
                    return None;
                }
                if buf.contains(&b'\n') {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if buf.is_empty() {
        return None;
    }
    let s = String::from_utf8_lossy(&buf).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 已有实例在运行时，把窗口叫到前台并退出新实例。
pub fn notify_existing() -> bool {
    let Some(s) = read_state() else { return false };
    let Some(mut c) = connect(s.port) else { return false };
    let req = serde_json::json!({ "v": 1, "token": s.token, "action": "show" }).to_string();
    if c.write_all(format!("{}\n", req).as_bytes()).is_err() {
        return false;
    }
    read_line(&mut c).is_some()
}

/// Chrome 的 native messaging：4 字节小端长度 + JSON。
/// 每条消息转发给主程序；主程序没开就先拉起，再重试。
pub fn native_host_main() -> i32 {
    // Chrome/Edge 会把扩展来源作为参数传进来（不带 --native-host），只接受我们自己的扩展
    let origin = std::env::args()
        .skip(1)
        .find(|a| a.starts_with("chrome-extension://") || a.starts_with("edge-extension://"))
        .unwrap_or_default();
    host_log(&format!("start origin={}", origin));
    if !origin.is_empty()
        && origin != format!("chrome-extension://{}/", EXTENSION_ID)
        && origin != format!("edge-extension://{}/", EXTENSION_ID)
    {
        host_log("reject origin");
        return 2;
    }
    let mut handled = false;
    while let Some(mut v) = read_frame() {
        handled = true;
        let action = v
            .get("action")
            .and_then(|a| a.as_str())
            .unwrap_or("?")
            .to_string();
        if let Some(o) = v.as_object_mut() {
            o.insert("origin".into(), serde_json::Value::String(origin.clone()));
        }
        let resp = forward(&v);
        host_log(&format!(
            "action={} ok={}",
            action,
            resp.get("ok").and_then(|x| x.as_bool()).unwrap_or(false)
        ));
        write_frame(&resp);
    }
    host_log(if handled { "exit ok" } else { "exit no frame" });
    if handled {
        0
    } else {
        1
    }
}

/// native host 的排查日志（只记动作和结果，不记链接内容）
fn host_log(msg: &str) {
    use std::sync::OnceLock;
    static START: OnceLock<u64> = OnceLock::new();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let start = *START.get_or_init(|| now);
    let path = app_dir().join("native-host.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 128 * 1024 {
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "[+{:>4}s] {}", now.saturating_sub(start), msg);
    }
}

fn read_frame() -> Option<serde_json::Value> {
    let mut len = [0u8; 4];
    std::io::stdin().read_exact(&mut len).ok()?;
    let n = u32::from_le_bytes(len) as usize;
    if n == 0 || n > 1024 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; n];
    std::io::stdin().read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

fn write_frame(v: &serde_json::Value) {
    let body = match serde_json::to_vec(v) {
        Ok(b) => b,
        Err(_) => serde_json::to_vec(&err("序列化失败")).unwrap_or_default(),
    };
    let mut out = std::io::stdout();
    let _ = out.write_all(&(body.len() as u32).to_le_bytes());
    let _ = out.write_all(&body);
    let _ = out.flush();
}

fn forward(value: &serde_json::Value) -> serde_json::Value {
    let mut req = value.clone();
    let wake = value.get("wake").and_then(|v| v.as_bool()).unwrap_or(true);
    if let Some(o) = req.as_object_mut() {
        o.insert("v".into(), serde_json::json!(1));
    }
    for attempt in 0..28 {
        if let Some(s) = read_state() {
            if let Some(mut c) = connect(s.port) {
                if let Some(o) = req.as_object_mut() {
                    o.insert("token".into(), serde_json::json!(s.token));
                }
                if c.write_all(format!("{}\n", req).as_bytes()).is_ok() {
                    if let Some(line) = read_line(&mut c) {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                            return v;
                        }
                    }
                }
            }
        }
        if attempt == 0 {
            // 握手消息（wake=false）不负责拉起程序，避免打开浏览器就弹出 DLKit
            if !wake {
                return serde_json::json!({ "ok": false, "message": "DLKit 未运行" });
            }
            spawn_app();
        }
        thread::sleep(Duration::from_millis(250));
    }
    serde_json::json!({ "ok": false, "message": "DLKit 没有在运行，请先打开软件" })
}

fn spawn_app() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut c = std::process::Command::new(exe);
    c.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let _ = c.spawn();
}

// ---------------- 安装 / 状态 ----------------

fn reg_keys() -> [(&'static str, String); 4] {
    [
        (
            "Chrome",
            format!("HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\{}", HOST_NAME),
        ),
        (
            "Edge",
            format!("HKCU\\Software\\Microsoft\\Edge\\NativeMessagingHosts\\{}", HOST_NAME),
        ),
        (
            "Brave",
            format!(
                "HKCU\\Software\\BraveSoftware\\Brave-Browser\\NativeMessagingHosts\\{}",
                HOST_NAME
            ),
        ),
        (
            "Chromium",
            format!("HKCU\\Software\\Chromium\\NativeMessagingHosts\\{}", HOST_NAME),
        ),
    ]
}

fn reg(args: &[&str]) -> bool {
    let mut c = std::process::Command::new("reg.exe");
    c.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.status().map(|s| s.success()).unwrap_or(false)
}

fn host_manifest_path() -> PathBuf {
    app_dir().join("native-host").join(format!("{}.json", HOST_NAME))
}

/// 扩展目录：放在用户看得见的地方。
/// AppData 是隐藏目录，浏览器「加载已解压的扩展」的选择框里经常找不到，
/// 这也是之前“按文档加载不进去”的主要原因。
pub fn extension_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let base = home
        .as_ref()
        .map(|h| {
            let docs = h.join("Documents");
            if docs.is_dir() {
                docs
            } else {
                h.clone()
            }
        })
        .unwrap_or_else(app_dir);
    base.join("DLKit").join("browser-extension")
}

/// 旧版本写扩展的位置。已经加载过旧目录的浏览器继续读那里，
/// 升级后也把内容同步过去，避免扩展代码过期。
fn legacy_extension_dir() -> PathBuf {
    app_dir().join("extension")
}

fn extension_files() -> [(&'static str, &'static str); 4] {
    [
        (
            "manifest.json",
            include_str!("../../browser-extension/manifest.json"),
        ),
        (
            "background.js",
            include_str!("../../browser-extension/background.js"),
        ),
        (
            "options.html",
            include_str!("../../browser-extension/options.html"),
        ),
        (
            "options.js",
            include_str!("../../browser-extension/options.js"),
        ),
    ]
}

/// 把扩展文件写到本机目录，供“加载已解压的扩展”使用
pub fn write_extension_files() -> Result<(), String> {
    let dir = extension_dir();
    write_files_to(&dir)?;
    let legacy = legacy_extension_dir();
    if legacy.is_dir() {
        let _ = write_files_to(&legacy);
    }
    Ok(())
}

fn write_files_to(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建扩展目录失败：{}", e))?;
    for (name, body) in extension_files() {
        let path = dir.join(name);
        let same = std::fs::read_to_string(&path)
            .map(|s| s == body)
            .unwrap_or(false);
        if !same {
            std::fs::write(&path, body).map_err(|e| format!("写入 {} 失败：{}", name, e))?;
        }
    }
    Ok(())
}

/// 扩展文件是否齐全（4 个文件都在，而且 manifest 是合法 JSON）
pub fn extension_ok() -> bool {
    let dir = extension_dir();
    if extension_files()
        .iter()
        .any(|(name, _)| !dir.join(name).is_file())
    {
        return false;
    }
    std::fs::read_to_string(dir.join("manifest.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some()
}

/// 注册 native messaging host（HKCU，不需要管理员）
pub fn install_native_host(exe: &Path) -> Result<String, String> {
    std::fs::create_dir_all(app_dir().join("native-host"))
        .map_err(|e| format!("创建目录失败：{}", e))?;
    let manifest = host_manifest_path();
    let body = serde_json::json!({
        "name": HOST_NAME,
        "description": "DLKit 浏览器接管",
        "path": exe.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{}/", EXTENSION_ID)],
    });
    let text = serde_json::to_string_pretty(&body).map_err(|e| e.to_string())?;
    std::fs::write(&manifest, text).map_err(|e| format!("写入清单失败：{}", e))?;
    write_extension_files()?;

    let path = manifest.to_string_lossy().to_string();
    let mut ok = Vec::new();
    for (name, key) in reg_keys() {
        if reg(&["add", &key, "/ve", "/t", "REG_SZ", "/d", &path, "/f"]) {
            ok.push(name);
        }
    }
    if ok.is_empty() {
        Err("写入注册表失败，可能被安全软件拦截".into())
    } else {
        Ok(format!("接管已注册（{}）", ok.join(" / ")))
    }
}

pub fn uninstall_native_host() {
    for (_, key) in reg_keys() {
        let _ = reg(&["delete", &key, "/f"]);
    }
    let _ = std::fs::remove_file(host_manifest_path());
}

fn registered_names() -> Vec<&'static str> {
    reg_keys()
        .into_iter()
        .filter(|(_, key)| reg(&["query", key, "/ve"]))
        .map(|(name, _)| name)
        .collect()
}

/// 给设置页显示的一句话状态。只说明“接管注册好了”，不代表扩展已加载。
pub fn host_status() -> String {
    let names = registered_names();
    if !host_manifest_path().exists() || names.is_empty() {
        "接管未注册".into()
    } else {
        format!("接管已注册（{}）", names.join(" / "))
    }
}

/// 浏览器是否曾经调用到本程序（native host 被拉起就会写日志）
pub fn browser_called() -> bool {
    app_dir().join("native-host.log").is_file()
}

/// native host 日志的最后几条，用于判断浏览器到底有没有连上来
pub fn host_log_tail(n: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(app_dir().join("native-host.log")) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect();
    if lines.len() > n {
        lines.drain(0..lines.len() - n);
    }
    lines
}

/// 自检用的快照：注册表、清单、扩展文件、浏览器调用记录
pub struct Diagnostics {
    pub registered: Vec<&'static str>,
    pub manifest: PathBuf,
    pub target: String,
    pub target_ok: bool,
    pub ext_dir: PathBuf,
    pub ext_ok: bool,
    pub called: bool,
    pub tail: Vec<String>,
    pub seen: Option<u64>,
}

pub fn diagnose() -> Diagnostics {
    let manifest = host_manifest_path();
    let target = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("path")
                .and_then(|p| p.as_str())
                .map(|p| p.to_string())
        })
        .unwrap_or_default();
    Diagnostics {
        registered: registered_names(),
        manifest,
        target_ok: !target.is_empty() && Path::new(&target).is_file(),
        target,
        ext_dir: extension_dir(),
        ext_ok: extension_ok(),
        called: browser_called(),
        tail: host_log_tail(4),
        seen: extension_seen(),
    }
}

/// 已安装的 Chromium 系浏览器（显示名、可执行文件）
pub fn installed_browsers() -> Vec<(&'static str, PathBuf)> {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default();
    let groups: [(&'static str, Vec<PathBuf>); 4] = [
        (
            "Edge",
            vec![
                PathBuf::from(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"),
                PathBuf::from(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"),
                local.join(r"Microsoft\Edge\Application\msedge.exe"),
            ],
        ),
        (
            "Chrome",
            vec![
                PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
                PathBuf::from(r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"),
                local.join(r"Google\Chrome\Application\chrome.exe"),
            ],
        ),
        (
            "Brave",
            vec![
                PathBuf::from(
                    r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
                ),
                PathBuf::from(
                    r"C:\Program Files (x86)\BraveSoftware\Brave-Browser\Application\brave.exe",
                ),
                local.join(r"BraveSoftware\Brave-Browser\Application\brave.exe"),
            ],
        ),
        (
            "Chromium",
            vec![
                PathBuf::from(r"C:\Program Files\Chromium\Application\chrome.exe"),
                PathBuf::from(r"C:\Program Files (x86)\Chromium\Application\chrome.exe"),
                local.join(r"Chromium\Application\chrome.exe"),
            ],
        ),
    ];
    let mut out = Vec::new();
    for (name, paths) in groups {
        if let Some(p) = paths.into_iter().find(|p| p.exists()) {
            out.push((name, p));
        }
    }
    out
}

fn extensions_url(name: &str) -> &'static str {
    if name == "Edge" {
        "edge://extensions/"
    } else {
        "chrome://extensions/"
    }
}

/// 直接打开某个浏览器的扩展管理页（已经在运行就新开一个标签）
pub fn open_extensions_page(name: &str) -> Result<(), String> {
    let exe = installed_browsers()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, p)| p)
        .ok_or_else(|| format!("没有找到 {}", name))?;
    std::process::Command::new(exe)
        .arg(extensions_url(name))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("打开扩展页失败：{}", e))
}

/// 打开扩展目录（顺便把最新文件写进去）
pub fn open_extension_dir() -> Result<(), String> {
    write_extension_files()?;
    crate::open_path(&extension_dir())
}

// ---------- 直接用命令行把扩展挂进浏览器启动 ----------
//
// Edge/Chrome 都认 `--load-extension=目录`：带这个参数启动时，浏览器会直接加载该扩展，
// 不需要开开发者模式，也不需要手动「加载已解压的扩展」，绕开了「外来插件装不进 Edge」。
// 唯一的限制是浏览器已经在运行时，Chromium 会把参数丢掉（只把地址转给已有实例），
// 所以调用前必须先确认浏览器没有在运行。

fn exe_name(exe: &Path) -> String {
    exe.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn browser_exe(name: &str) -> Option<PathBuf> {
    installed_browsers()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, p)| p)
}

/// 浏览器是否正在运行
pub fn is_running(name: &str) -> bool {
    let Some(exe) = browser_exe(name) else {
        return false;
    };
    let image = exe_name(&exe);
    let mut c = std::process::Command::new("tasklist");
    c.args(["/FI", &format!("IMAGENAME eq {}", image), "/NH"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    match c.output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .to_lowercase()
            .contains(&image.to_lowercase()),
        Err(_) => false,
    }
}

/// 结束浏览器的所有进程（会关掉用户当前所有标签页，必须由用户确认后再调用）
pub fn kill(name: &str) -> bool {
    let Some(exe) = browser_exe(name) else {
        return false;
    };
    let mut c = std::process::Command::new("taskkill");
    c.args(["/IM", &exe_name(&exe), "/F"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    match c.status() {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

/// 用带扩展的方式启动浏览器。要求浏览器当前没有在运行。
pub fn launch_with_extension(name: &str) -> Result<String, String> {
    let exe = browser_exe(name).ok_or_else(|| format!("没有找到 {}", name))?;
    write_extension_files()?;
    if is_running(name) {
        return Err(format!("{} 正在运行，参数会被丢掉", name));
    }
    let dir = extension_dir();
    std::process::Command::new(exe)
        .arg(format!("--load-extension={}", dir.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .spawn()
        .map_err(|e| format!("启动 {} 失败：{}", name, e))?;
    Ok(format!("已用带扩展的方式启动 {}", name))
}

/// 浏览器接管的开关状态 + 安装/卸载入口
pub fn apply(enabled: bool, exe: Option<&Path>) -> String {
    if enabled {
        match exe {
            Some(p) => match install_native_host(p) {
                Ok(s) => s,
                Err(e) => format!("安装失败：{}", e),
            },
            None => "拿不到程序路径".into(),
        }
    } else {
        uninstall_native_host();
        "已关闭".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn req(v: serde_json::Value) -> String {
        v.to_string()
    }

    #[test]
    fn rejects_bad_token() {
        let (tx, _rx) = mpsc::channel();
        let line = req(serde_json::json!({
            "token": "bad", "action": "ping"
        }));
        let r = handle_line(&line, "good", &tx);
        assert!(r.contains("\"ok\":false"));
    }

    #[test]
    fn download_reaches_ui() {
        let (tx, rx) = mpsc::channel();
        let line = req(serde_json::json!({
            "token": "t", "action": "download",
            "items": [{ "url": "https://example.com/a.zip", "referer": "https://example.com/" }]
        }));
        let r = handle_line(&line, "t", &tx);
        assert!(r.contains("\"ok\":true"));
        match rx.try_recv().expect("应收到任务") {
            Msg::Download(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].referer, "https://example.com/");
            }
            _ => panic!("类型不对"),
        }
    }

    #[test]
    fn rejects_non_http() {
        let (tx, _rx) = mpsc::channel();
        let line = req(serde_json::json!({
            "token": "t", "action": "scan", "url": "file:///etc/passwd"
        }));
        let r = handle_line(&line, "t", &tx);
        assert!(r.contains("\"ok\":false"));
    }

    #[test]
    fn url_rules() {
        assert!(valid_url("https://example.com/a"));
        assert!(!valid_url("file:///c:/a"));
        assert!(!valid_url("https://example.com/a b"));
        assert!(!valid_url("javascript:alert(1)"));
    }
}
