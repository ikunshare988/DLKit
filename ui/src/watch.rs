//! 链接跟踪：定时检查直链有没有更新，有更新时弹 Windows 通知。
//!
//! 只通知，不自动下载——用户看到提示后自己在「跟踪」页点下载。
//! 检查用条件请求（If-None-Match / If-Modified-Since），命中 304 时几乎不耗流量。

use crate::engine;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 默认轮询间隔（分钟）。用户要求默认 2 分钟。
pub const DEFAULT_INTERVAL_MIN: u32 = 2;
/// 下限，避免误操作把间隔调到 0 而对站点连发请求
const MIN_INTERVAL_MIN: u32 = 1;
/// 上限
const MAX_INTERVAL_MIN: u32 = 24 * 60;
/// 失败后的最长退避
const MAX_BACKOFF_MIN: u32 = 60;
/// 主循环空闲时的等待粒度
const TICK: Duration = Duration::from_millis(500);

pub const STATE_OK: &str = "ok";
pub const STATE_UPDATED: &str = "updated";
pub const STATE_FAILED: &str = "failed";
pub const STATE_GONE: &str = "gone";

/// 一次探测得到的指纹。理论上 ETag 最可靠，长度和最终地址次之。
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Fingerprint {
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub content_length: Option<u64>,
    #[serde(default)]
    pub final_url: String,
    #[serde(default)]
    pub filename: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WatchItem {
    pub id: String,
    pub url: String,
    pub name: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_interval")]
    pub interval_min: u32,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub last: Option<Fingerprint>,
    /// 弱信号（只有 Last-Modified / 文件名变化）先记在这里，下次仍不同才确认
    #[serde(default)]
    pub pending: Option<Fingerprint>,
    #[serde(default = "default_state")]
    pub state: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub fail_count: u32,
    #[serde(default)]
    pub last_checked: u64,
    #[serde(default)]
    pub next_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    /// 已经通知过的指纹，避免同一版本反复弹
    #[serde(default)]
    pub last_notified: Option<String>,
}

fn default_interval() -> u32 {
    DEFAULT_INTERVAL_MIN
}

fn default_state() -> String {
    STATE_OK.to_string()
}

#[derive(Serialize, Deserialize, Default)]
struct Store {
    #[serde(default)]
    items: Vec<WatchItem>,
}

pub enum Cmd {
    Add {
        url: String,
        name: String,
        headers: BTreeMap<String, String>,
    },
    Remove(String),
    Toggle(String),
    CheckNow(String),
    CheckAll,
}

/// 界面侧句柄：读快照 + 下发命令，真正的检查在后台线程里跑
pub struct Watcher {
    items: Arc<Mutex<Vec<WatchItem>>>,
    tx: Sender<Cmd>,
}

impl Watcher {
    pub fn new(root: &Path) -> Watcher {
        let path = root.join("config").join("watches.json");
        let mut items = load(&path);
        // 启动时先全部扫一遍（符合「每次打开软件扫描一次」）
        for it in &mut items {
            it.next_at = 0;
        }
        let shared = Arc::new(Mutex::new(items.clone()));
        let (tx, rx) = channel();
        let shared2 = shared.clone();
        std::thread::spawn(move || run(rx, shared2, path, items));
        Watcher { items: shared, tx }
    }

    pub fn snapshot(&self) -> Vec<WatchItem> {
        self.items
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    pub fn count_updated(&self) -> usize {
        self.items
            .lock()
            .map(|v| v.iter().filter(|i| i.state == STATE_UPDATED).count())
            .unwrap_or(0)
    }

    pub fn add(&self, url: String, name: String, headers: BTreeMap<String, String>) {
        let _ = self.tx.send(Cmd::Add { url, name, headers });
    }

    pub fn remove(&self, id: &str) {
        let _ = self.tx.send(Cmd::Remove(id.to_string()));
    }

    pub fn toggle(&self, id: &str) {
        let _ = self.tx.send(Cmd::Toggle(id.to_string()));
    }

    pub fn check_now(&self, id: &str) {
        let _ = self.tx.send(Cmd::CheckNow(id.to_string()));
    }

    pub fn check_all(&self) {
        let _ = self.tx.send(Cmd::CheckAll);
    }
}

fn run(
    rx: Receiver<Cmd>,
    shared: Arc<Mutex<Vec<WatchItem>>>,
    path: PathBuf,
    mut items: Vec<WatchItem>,
) {
    publish(&shared, &items);
    loop {
        // 先把积压的命令处理掉
        while let Ok(cmd) = rx.try_recv() {
            apply(&mut items, cmd);
            publish(&shared, &items);
            save(&path, &items);
        }
        let now = now_secs();
        let due = items
            .iter()
            .position(|i| !i.paused && i.next_at <= now);
        if let Some(idx) = due {
            let note = check(&mut items[idx]);
            let interval = interval_secs(&items[idx]);
            items[idx].next_at = now_secs() + interval;
            publish(&shared, &items);
            save(&path, &items);
            if let Some((title, body)) = note {
                toast(&title, &body);
            }
            continue;
        }
        // 没事可做就等命令或下一轮
        match rx.recv_timeout(TICK) {
            Ok(cmd) => {
                apply(&mut items, cmd);
                publish(&shared, &items);
                save(&path, &items);
            }
            Err(_) => {}
        }
    }
}

fn apply(items: &mut Vec<WatchItem>, cmd: Cmd) {
    match cmd {
        Cmd::Add { url, name, headers } => {
            if items.iter().any(|i| i.url == url) {
                return;
            }
            let id = next_id(items);
            let name = if name.trim().is_empty() {
                filename_of(&url)
            } else {
                name
            };
            items.push(WatchItem {
                id,
                url,
                name,
                headers,
                interval_min: DEFAULT_INTERVAL_MIN,
                paused: false,
                last: None,
                pending: None,
                state: STATE_OK.to_string(),
                message: "等待首次检查".into(),
                fail_count: 0,
                last_checked: 0,
                next_at: 0,
                updated_at: 0,
                last_notified: None,
            });
        }
        Cmd::Remove(id) => items.retain(|i| i.id != id),
        Cmd::Toggle(id) => {
            if let Some(it) = items.iter_mut().find(|i| i.id == id) {
                it.paused = !it.paused;
                if !it.paused {
                    it.next_at = 0;
                    it.message = "已恢复检查".into();
                } else {
                    it.message = "已暂停".into();
                }
            }
        }
        Cmd::CheckNow(id) => {
            if let Some(it) = items.iter_mut().find(|i| i.id == id) {
                it.next_at = 0;
            }
        }
        Cmd::CheckAll => {
            for it in items.iter_mut() {
                if !it.paused {
                    it.next_at = 0;
                }
            }
        }
    }
}

fn next_id(items: &[WatchItem]) -> String {
    let mut n = items.len() + 1;
    loop {
        let id = format!("w{}", n);
        if !items.iter().any(|i| i.id == id) {
            return id;
        }
        n += 1;
    }
}

/// 检查一条链接；返回需要弹出的通知文案
fn check(item: &mut WatchItem) -> Option<(String, String)> {
    item.last_checked = now_secs();

    let mut headers = item.headers.clone();
    if let Some(last) = &item.last {
        if let Some(etag) = &last.etag {
            headers.insert("If-None-Match".into(), etag.clone());
        }
        if let Some(lm) = &last.last_modified {
            headers.insert("If-Modified-Since".into(), lm.clone());
        }
    }

    let info = match engine::head_info(&item.url, &headers) {
        Ok(i) => i,
        Err(e) => {
            item.fail_count += 1;
            item.state = STATE_FAILED.to_string();
            item.message = match e {
                engine::EngErr::Fatal(m) | engine::EngErr::Soft(m) => m,
            };
            return None;
        }
    };

    if info.status == 304 {
        item.fail_count = 0;
        if item.state != STATE_UPDATED {
            item.state = STATE_OK.to_string();
        }
        item.message = "未变化".into();
        return None;
    }

    if info.status == 404 || info.status == 410 {
        item.fail_count = 0;
        item.state = STATE_GONE.to_string();
        item.message = format!("链接已失效（HTTP {}）", info.status);
        return None;
    }

    if info.status >= 400 || info.status == 0 {
        item.fail_count += 1;
        item.state = STATE_FAILED.to_string();
        item.message = if info.status == 0 {
            "检查失败：没有拿到响应".to_string()
        } else {
            format!("检查失败（HTTP {}）", info.status)
        };
        return None;
    }

    item.fail_count = 0;
    let new = Fingerprint {
        etag: info.etag.clone(),
        last_modified: info.last_modified.clone(),
        content_length: info.content_length,
        final_url: info.url.clone(),
        filename: info.filename.clone(),
    };
    if item.name.is_empty() || item.name == "下载.bin" {
        item.name = info.filename.clone();
    }

    let old = item.last.clone();
    match classify(old.as_ref(), &new) {
        Change::First => {
            item.message = format!("已建立基线（{}）", size_text(new.content_length));
            item.last = Some(new);
            item.state = STATE_OK.to_string();
            None
        }
        Change::Same => {
            item.message = "未变化".into();
            item.pending = None;
            item.last = Some(new);
            item.state = STATE_OK.to_string();
            None
        }
        Change::Updated => {
            let before = old.clone().unwrap_or_default();
            let key = fingerprint_key(&new);
            item.last = Some(new.clone());
            item.pending = None;
            item.state = STATE_UPDATED.to_string();
            item.updated_at = now_secs();
            item.message = change_text(&before, &new);
            if item.last_notified.as_deref() == Some(key.as_str()) {
                return None;
            }
            item.last_notified = Some(key);
            Some((
                "DLKit · 链接有更新".to_string(),
                format!("{}  {}", item.name, item.message),
            ))
        }
        Change::Maybe => {
            // 弱信号：同一次变化出现两次才认，避免 CDN 抖动误报
            let confirmed = item
                .pending
                .as_ref()
                .map(|p| weak_key(p) == weak_key(&new))
                .unwrap_or(false);
            if confirmed {
                let before = old.clone().unwrap_or_default();
                let key = fingerprint_key(&new);
                item.last = Some(new.clone());
                item.pending = None;
                item.state = STATE_UPDATED.to_string();
                item.updated_at = now_secs();
                item.message = change_text(&before, &new);
                if item.last_notified.as_deref() == Some(key.as_str()) {
                    return None;
                }
                item.last_notified = Some(key);
                Some((
                    "DLKit · 链接有更新".to_string(),
                    format!("{}  {}", item.name, item.message),
                ))
            } else {
                item.pending = Some(new);
                item.state = STATE_OK.to_string();
                item.message = "可能有变化，等待下次确认".into();
                None
            }
        }
    }
}

enum Change {
    /// 第一次检查，只建基线
    First,
    Same,
    /// 强信号（ETag / 长度 / 最终地址）变化
    Updated,
    /// 弱信号（Last-Modified / 文件名）变化，需要二次确认
    Maybe,
}

fn classify(old: Option<&Fingerprint>, new: &Fingerprint) -> Change {
    let old = match old {
        Some(o) => o,
        None => return Change::First,
    };
    // ETag 是服务端给的强验证器：两边都有时以它为准
    if let (Some(a), Some(b)) = (&old.etag, &new.etag) {
        return if a == b { Change::Same } else { Change::Updated };
    }
    if let (Some(a), Some(b)) = (old.content_length, new.content_length) {
        if a != b {
            return Change::Updated;
        }
    }
    let pa = url_key(&old.final_url);
    let pb = url_key(&new.final_url);
    if !pa.is_empty() && !pb.is_empty() && pa != pb {
        return Change::Updated;
    }
    if let (Some(a), Some(b)) = (&old.last_modified, &new.last_modified) {
        if a != b {
            return Change::Maybe;
        }
    }
    if !old.filename.is_empty() && !new.filename.is_empty() && old.filename != new.filename {
        return Change::Maybe;
    }
    Change::Same
}

/// 比较用的地址：去掉 query（预签名链接每次都不一样）和片段
fn url_key(u: &str) -> String {
    let no_frag = u.split('#').next().unwrap_or(u);
    let no_query = no_frag.split('?').next().unwrap_or(no_frag);
    no_query.trim_end_matches('/').to_lowercase()
}

fn weak_key(f: &Fingerprint) -> String {
    format!(
        "{}|{}|{}",
        url_key(&f.final_url),
        f.content_length.unwrap_or(0),
        f.filename
    )
}

fn fingerprint_key(f: &Fingerprint) -> String {
    if let Some(e) = &f.etag {
        return format!("etag:{}", e);
    }
    format!(
        "{}|{}|{}|{}",
        url_key(&f.final_url),
        f.content_length.unwrap_or(0),
        f.filename,
        f.last_modified.clone().unwrap_or_default()
    )
}

fn change_text(old: &Fingerprint, new: &Fingerprint) -> String {
    if let (Some(a), Some(b)) = (version_of(&old.final_url), version_of(&new.final_url)) {
        if a != b {
            return format!("{} → {}", a, b);
        }
    }
    match (old.content_length, new.content_length) {
        (Some(a), Some(b)) if a != b => {
            format!("{} → {}", size_text(Some(a)), size_text(Some(b)))
        }
        _ => "文件已更新".to_string(),
    }
}

/// 从最终地址里找形如 v1.2.3 / 1.2.3 的版本段，只用于通知文案
fn version_of(u: &str) -> Option<String> {
    let path = u.split('?').next()?;
    path.split('/').rev().find_map(|seg| {
        let seg = seg.trim();
        if seg.is_empty() || seg.len() > 32 {
            return None;
        }
        let core = seg.strip_prefix('v').unwrap_or(seg);
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() >= 2
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        {
            Some(seg.to_string())
        } else {
            None
        }
    })
}

fn size_text(v: Option<u64>) -> String {
    match v {
        Some(n) if n > 0 => {
            let mut x = n as f64;
            let mut i = 0;
            const U: [&str; 4] = ["B", "KB", "MB", "GB"];
            while x >= 1024.0 && i + 1 < U.len() {
                x /= 1024.0;
                i += 1;
            }
            if i == 0 {
                format!("{} B", n)
            } else {
                format!("{:.1} {}", x, U[i])
            }
        }
        _ => "大小未知".to_string(),
    }
}

fn interval_secs(item: &WatchItem) -> u64 {
    // 已失效的链接降到 1 小时一次，别对着 404 每两分钟敲一遍
    if item.state == STATE_GONE {
        return 3600;
    }
    let base = item.interval_min.clamp(MIN_INTERVAL_MIN, MAX_INTERVAL_MIN) as u64;
    // 失败退避：连续失败时逐步拉长，最多 60 分钟
    let backoff = if item.fail_count > 0 {
        (1u64 << item.fail_count.min(5)).min(MAX_BACKOFF_MIN as u64)
    } else {
        1
    };
    let secs = base.saturating_mul(60).saturating_mul(backoff);
    // ±10% 抖动，避免多个条目同一秒集中请求
    let jitter = (secs as f64 * 0.1) as u64;
    if jitter == 0 {
        secs
    } else {
        secs - jitter / 2 + (now_secs() % jitter.max(1))
    }
}

fn filename_of(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or("").trim();
    if name.is_empty() {
        url.to_string()
    } else {
        name.to_string()
    }
}

/// 相对时间文案，给界面用
pub fn ago(secs: u64) -> String {
    if secs == 0 {
        return "从未".into();
    }
    let d = now_secs().saturating_sub(secs);
    if d < 60 {
        "刚刚".into()
    } else if d < 3600 {
        format!("{} 分钟前", d / 60)
    } else if d < 86400 {
        format!("{} 小时前", d / 3600)
    } else {
        format!("{} 天前", d / 86400)
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn publish(shared: &Arc<Mutex<Vec<WatchItem>>>, items: &[WatchItem]) {
    if let Ok(mut v) = shared.lock() {
        *v = items.to_vec();
    }
}

fn load(path: &Path) -> Vec<WatchItem> {
    std::fs::read_to_string(path)
        .ok()
        // 记事本等编辑器可能写入 BOM，去掉后再解析
        .map(|t| t.trim_start_matches('\u{feff}').to_string())
        .and_then(|t| serde_json::from_str::<Store>(&t).ok())
        .map(|s| s.items)
        .unwrap_or_default()
}

fn save(path: &Path, items: &[WatchItem]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let store = Store {
        items: items.to_vec(),
    };
    let text = match serde_json::to_string_pretty(&store) {
        Ok(t) => t,
        Err(_) => return,
    };
    // 先写临时文件再改名，避免断电留下半个 JSON
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn toast(title: &str, body: &str) {
    #[cfg(windows)]
    {
        use tauri_winrt_notification::Toast;
        // 未做安装包，没有注册自己的 AppUserModelID；
        // 借用 PowerShell 的（系统已注册）保证通知能弹出来。
        let _ = Toast::new(Toast::POWERSHELL_APP_ID)
            .title(title)
            .text1(body)
            .show();
    }
    #[cfg(not(windows))]
    {
        let _ = (title, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(etag: Option<&str>, len: Option<u64>, url: &str) -> Fingerprint {
        let key = url_key(url);
        Fingerprint {
            etag: etag.map(|s| s.to_string()),
            last_modified: None,
            content_length: len,
            final_url: url.to_string(),
            filename: key.rsplit('/').next().unwrap_or("").to_string(),
        }
    }

    #[test]
    fn first_check_builds_baseline() {
        assert!(matches!(classify(None, &fp(Some("a"), Some(1), "x")), Change::First));
    }

    #[test]
    fn etag_wins_over_length() {
        let old = fp(Some("\"a\""), Some(10), "u");
        let same = fp(Some("\"a\""), Some(10), "u");
        assert!(matches!(classify(Some(&old), &same), Change::Same));
        let changed = fp(Some("\"b\""), Some(10), "u");
        assert!(matches!(classify(Some(&old), &changed), Change::Updated));
    }

    #[test]
    fn length_change_is_update() {
        let old = fp(None, Some(10), "u");
        let new = fp(None, Some(11), "u");
        assert!(matches!(classify(Some(&old), &new), Change::Updated));
    }

    #[test]
    fn signed_query_is_ignored() {
        let old = fp(None, Some(10), "https://x/file.zip?token=aaa");
        let new = fp(None, Some(10), "https://x/file.zip?token=bbb");
        assert!(matches!(classify(Some(&old), &new), Change::Same));
    }

    #[test]
    fn version_bump_in_path_is_update() {
        let old = fp(None, Some(10), "https://x/download/v1.2.3/app.zip");
        let new = fp(None, Some(10), "https://x/download/v1.2.4/app.zip");
        assert!(matches!(classify(Some(&old), &new), Change::Updated));
        assert_eq!(version_of("https://x/download/v1.2.4/app.zip").as_deref(), Some("v1.2.4"));
    }

    #[test]
    fn last_modified_needs_confirmation() {
        let mut old = fp(None, Some(10), "u");
        old.last_modified = Some("a".into());
        let mut new = fp(None, Some(10), "u");
        new.last_modified = Some("b".into());
        assert!(matches!(classify(Some(&old), &new), Change::Maybe));
    }
}
