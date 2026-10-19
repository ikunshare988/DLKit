//! OST 下载器引擎：curl 分片 + aria2(BT/兜底) + yt-dlp(媒体) + OpenList(网盘直链)。

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::fs::FileExt;

pub const DEFAULT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                             (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const MIN_CHUNK: u64 = 512 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Http,
    Torrent,
    Media,
    Netdisk,
    OpenList,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Http => "直链",
            Kind::Torrent => "磁力/BT",
            Kind::Media => "媒体站",
            Kind::Netdisk => "网盘",
            Kind::OpenList => "OpenList",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

pub struct Task {
    pub url: String,
    pub name: String,
    pub kind: Kind,
    pub status: Status,
    pub message: String,
    pub done: u64,
    pub total: u64,
    pub speed: f64,
    /// 网络收包速度（MB/s），0 表示未测量
    pub net_speed: f64,
    /// 磁盘写入速度（MB/s），0 表示未测量
    pub disk_speed: f64,
    pub log: Vec<String>,
    pub cancel: Arc<AtomicBool>,
    pub output: Option<PathBuf>,
    pub started: Instant,
}

pub type Shared = Arc<Mutex<Task>>;

impl Task {
    pub fn new(url: String, kind: Kind, name: String) -> Task {
        Task {
            url,
            name,
            kind,
            status: Status::Queued,
            message: "排队中".into(),
            done: 0,
            total: 0,
            speed: 0.0,
            net_speed: 0.0,
            disk_speed: 0.0,
            log: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            output: None,
            started: Instant::now(),
        }
    }
}

/// 分块引擎的 I/O 统计：
/// 网络速率直接取实际吞吐（每秒落入文件的字节数）；
/// 磁盘速率 = 累计写入字节 / 累计写盘耗时（写盘阶段的真实吞吐）。
/// 若磁盘速率远高于网络速率，说明瓶颈在网络而不是磁盘。
#[derive(Default)]
struct IoStats {
    disk_bytes: AtomicU64,
    disk_ns: AtomicU64,
}

pub enum EngErr {
    Fatal(String),
    Soft(String),
}

pub fn find_root() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..6 {
            if let Some(p) = cur.clone() {
                if p.join("bin").join("aria2").join("aria2c.exe").exists() {
                    return p;
                }
                cur = p.parent().map(|q| q.to_path_buf());
            } else {
                break;
            }
        }
    }
    PathBuf::from(r"D:\DLKit")
}

// ---------------- 链接分类 ----------------
pub fn classify(url: &str) -> (Kind, &'static str) {
    let low = url.trim().to_lowercase();
    if low.starts_with("magnet:") || low.ends_with(".torrent") {
        return (Kind::Torrent, "磁力/种子");
    }
    if low.starts_with("http") {
        if openlist_path_from_url(url).is_some() {
            return (Kind::OpenList, "OpenList 链接");
        }
        let host = host_of(url).unwrap_or_default();
        const MEDIA: &[&str] = &[
            "youtube.com", "youtu.be", "bilibili.com", "b23.tv", "douyin.com", "iesdouyin.com",
            "weibo.com", "weibo.cn", "tiktok.com", "twitter.com", "x.com", "vimeo.com",
            "twitch.tv", "instagram.com", "facebook.com", "nicovideo.jp", "acfun.cn",
            "ixigua.com", "kuaishou.com", "huya.com", "douyu.com", "v.qq.com", "iqiyi.com",
            "youku.com", "mgtv.com", "music.163.com", "y.qq.com",
        ];
        const PAN: &[&str] = &[
            "pan.baidu.com", "yun.baidu.com", "aliyundrive.com", "alipan.com", "pan.quark.cn",
            "115.com", "115cdn.com", "cloud.189.cn", "drive.google.com", "onedrive.live.com",
            "1drv.ms", "pan.xunlei.com", "123pan.com", "lanzou", "caiyun.139.com",
            "mypikpak.com", "terabox.com", "jianguoyun.com",
        ];
        if PAN.iter().any(|h| host == *h || host.ends_with(&format!(".{}", h)) || host.contains(h)) {
            return (Kind::Netdisk, "网盘链接");
        }
        if MEDIA.iter().any(|h| host == *h || host.ends_with(&format!(".{}", h))) {
            return (Kind::Media, "媒体站");
        }
        return (Kind::Http, "普通直链");
    }
    (Kind::Http, "普通直链")
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    let host = rest.split('/').next()?;
    let host = host.split('@').last()?;
    Some(host.split(':').next().unwrap_or(host).to_lowercase())
}

fn url_path(url: &str) -> String {
    let rest = match url.split("://").nth(1) {
        Some(r) => r,
        None => return String::new(),
    };
    match rest.find('/') {
        Some(i) => rest[i..].to_string(),
        None => "/".into(),
    }
}

fn filename_from_url(url: &str) -> String {
    let p = url_path(url);
    let p = p.split('?').next().unwrap_or(&p);
    let name = p.rsplit('/').next().unwrap_or("").trim();
    if name.is_empty() {
        "download.bin".into()
    } else {
        percent_decode(name)
    }
}

fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    let t = cleaned.trim().trim_matches('.').to_string();
    if t.is_empty() {
        "download.bin".into()
    } else {
        t
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

// ---------------- 进程启动（隐藏控制台窗口） ----------------
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 统一入口：GUI 程序启动 curl / yt-dlp / powershell 时不弹黑窗
fn new_cmd<S: AsRef<std::ffi::OsStr>>(program: S) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// curl 统一加 --ssl-no-revoke：Windows Schannel 吊销服务器不可达时会直接握手失败
fn curl_command() -> Command {
    let mut c = new_cmd("curl");
    c.arg("--ssl-no-revoke");
    c
}

// ---------------- curl 封装 ----------------
fn curl(args: &[String]) -> Result<(String, String, i32), String> {
    let out = curl_command()
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("无法启动 curl：{}", e))?;
    Ok((
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    ))
}

/// 供页面解析模块复用（同样带 --ssl-no-revoke 与隐藏窗口）
pub(crate) fn curl_exe(args: &[String]) -> Result<(String, String, i32), String> {
    curl(args)
}

/// 与 curl_exe 相同，但保留原始字节（页面可能是 GBK）
pub(crate) fn curl_bytes(args: &[String]) -> Result<(Vec<u8>, String, i32), String> {
    let out = curl_command()
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("无法启动 curl：{}", e))?;
    Ok((
        out.stdout,
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    ))
}

pub(crate) fn url_host(url: &str) -> Option<String> {
    host_of(url)
}

pub(crate) fn decode_percent(s: &str) -> String {
    percent_decode(s)
}

/// 域名解析到回环地址时返回该域名（可能是 hosts 屏蔽，也可能是本地加速器如 Watt Toolkit）。
/// 是否真的不可用要由 probe 的实际响应判断，不能一律拒绝。
fn loopback_host(url: &str) -> Option<String> {
    use std::net::ToSocketAddrs;
    let host = host_of(url)?;
    if host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    let has_ipv6 = host.contains(':');
    let addrs = (host.as_str(), if has_ipv6 { 0 } else { 443 }).to_socket_addrs().ok()?;
    let ips: Vec<std::net::IpAddr> = addrs.map(|a| a.ip()).collect();
    if !ips.is_empty() && ips.iter().all(|ip| ip.is_loopback()) {
        Some(host)
    } else {
        None
    }
}

/// 从 curl 输出中取出 -w 写入的最终地址
fn split_eff(s: String) -> (String, Option<String>) {
    const MARK: &str = "__DLKIT_EFF__";
    match s.find(MARK) {
        Some(i) => {
            let url = s[i + MARK.len()..].trim().to_string();
            (s[..i].to_string(), (!url.is_empty()).then_some(url))
        }
        None => (s, None),
    }
}

fn header_args(headers: &BTreeMap<String, String>) -> Vec<String> {
    let mut v = Vec::new();
    for (k, val) in headers {
        v.push("-H".into());
        v.push(format!("{}: {}", k, val));
    }
    v
}

fn parse_response(text: &str) -> (Option<u16>, BTreeMap<String, String>) {
    let mut blocks: Vec<&str> = text.split("\r\n\r\n").filter(|b| !b.trim().is_empty()).collect();
    if blocks.is_empty() {
        blocks = text.split("\n\n").filter(|b| !b.trim().is_empty()).collect();
    }
    let block = match blocks.last() {
        Some(b) => *b,
        None => return (None, BTreeMap::new()),
    };
    let mut lines = block.lines();
    let status = lines.next().and_then(|l| {
        l.split_whitespace()
            .nth(1)
            .and_then(|c| c.parse::<u16>().ok())
    });
    let mut map = BTreeMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            map.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    (status, map)
}

fn status_text(code: u16) -> String {
    match code {
        400 => "请求无效（HTTP 400）".into(),
        401 => "需要登录或授权（HTTP 401），请填写 Cookie".into(),
        403 => "请求被拒绝（HTTP 403）：可能需要 Cookie/Referer，或该站禁止直接下载".into(),
        404 => "链接失效或文件不存在（HTTP 404）".into(),
        410 => "文件已被删除（HTTP 410）".into(),
        429 => "请求过于频繁被限流（HTTP 429）".into(),
        500 => "服务器内部错误（HTTP 500）".into(),
        502 => "网关错误（HTTP 502）".into(),
        503 => "服务暂不可用（HTTP 503）".into(),
        504 => "网关超时（HTTP 504）".into(),
        _ => format!("HTTP {}", code),
    }
}

// ---------------- 探测 ----------------
pub struct Probe {
    pub url: String,
    pub size: u64,
    pub ranges: bool,
    pub name: String,
    pub ua: Option<String>,
    /// 探测耗时（毫秒）
    pub probe_ms: u128,
}

pub fn probe(url: &str, headers: &BTreeMap<String, String>) -> Result<Probe, EngErr> {
    let t_begin = Instant::now();
    let loop_host = loopback_host(url);
    let mut args: Vec<String> = ["-sS", "-I", "-L", "--http1.1", "--connect-timeout", "15",
                                 "--max-time", "35", "-w", "\n__DLKIT_EFF__%{url_effective}"]
        .iter().map(|s| s.to_string()).collect();
    args.extend(header_args(headers));
    args.push(url.to_string());
    let (out, _err, _code) = curl(&args).map_err(EngErr::Soft)?;
    // 记下 302 之后的最终地址，分块时直接请求它，省掉每条连接各跟一次跳转
    let (out, eff) = split_eff(out);

    let (status, hdrs) = parse_response(&out);
    if let Some(code) = status {
        if matches!(code, 401 | 404 | 410) {
            return Err(EngErr::Fatal(status_text(code)));
        }
    }
    let mut size = hdrs
        .get("content-length")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let mut ranges = hdrs
        .get("accept-ranges")
        .map(|v| v.to_lowercase().starts_with("bytes"))
        .unwrap_or(false);
    let final_url = eff.unwrap_or_else(|| url.to_string());

    if size == 0 || !ranges {
        let mut a: Vec<String> = ["-sS", "-L", "--http1.1", "-o", "NUL", "-D", "-", "-r", "0-1",
                                  "--connect-timeout", "15", "--max-time", "35"]
            .iter().map(|s| s.to_string()).collect();
        a.extend(header_args(headers));
        a.push(url.to_string());
        let (o2, _e2, _c2) = curl(&a).map_err(EngErr::Soft)?;
        let (st2, h2) = parse_response(&o2);
        if let Some(code) = st2 {
            if code >= 400 {
                if code == 429 {
                    return Err(EngErr::Soft(status_text(code)));
                }
                return Err(EngErr::Fatal(status_text(code)));
            }
        }
        if let Some(cr) = h2.get("content-range") {
            if cr.starts_with("bytes") {
                if let Some(total) = cr.rsplit('/').next().and_then(|s| s.parse::<u64>().ok()) {
                    size = total;
                    ranges = true;
                }
            }
        } else if size == 0 {
            size = h2
                .get("content-length")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
        }
    }

    // 部分镜像只对下载器 UA 开放 Range
    let mut ua_override = None;
    if !ranges {
        let mut h2 = headers.clone();
        h2.insert("User-Agent".into(), "curl/8.5.0".into());
        let mut a: Vec<String> = ["-sS", "-L", "--http1.1", "-o", "NUL", "-D", "-", "-r", "0-1",
                                  "--connect-timeout", "15", "--max-time", "35"]
            .iter().map(|s| s.to_string()).collect();
        a.extend(header_args(&h2));
        a.push(url.to_string());
        if let Ok((o3, _e3, _c3)) = curl(&a) {
            let (_, h3) = parse_response(&o3);
            if h3
                .get("content-range")
                .map(|v| v.starts_with("bytes"))
                .unwrap_or(false)
            {
                ranges = true;
                ua_override = Some("curl/8.5.0".to_string());
            }
        }
    }

    if size == 0 {
        if let Some(host) = loop_host {
            return Err(EngErr::Fatal(format!(
                "域名 {} 被解析到本机回环地址（127.0.0.1），本地代理没有返回可下载内容。\n\
                 若是 hosts 屏蔽请删除对应行；若是 Watt Toolkit / Steam++ 等加速器请确认已启动并生效。",
                host
            )));
        }
    }

    let name = hdrs
        .get("content-disposition")
        .and_then(|cd| {
            cd.split(';').find_map(|seg| {
                let seg = seg.trim();
                if let Some(v) = seg.strip_prefix("filename=") {
                    Some(v.trim_matches('"').to_string())
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| filename_from_url(&final_url));

    // 注意：不做 CDN 多节点轮换。实测把连接强行分散到 4 个 A 记录后，
    // GitHub 发布包从 2.5s 退化到 5.3s——curl 自带的地址优选更可靠。
    let probe_ms = t_begin.elapsed().as_millis();

    Ok(Probe {
        url: final_url,
        size,
        ranges,
        name: sanitize_name(&name),
        ua: ua_override,
        probe_ms,
    })
}

// ---------------- 并发闸门 ----------------
struct Limiter {
    state: Mutex<(usize, usize)>,
    cv: Condvar,
}

impl Limiter {
    fn new(cap: usize) -> Limiter {
        Limiter {
            state: Mutex::new((cap.max(1), 0)),
            cv: Condvar::new(),
        }
    }
    fn acquire(&self) {
        let mut g = self.state.lock().unwrap();
        while g.1 >= g.0 {
            g = self.cv.wait(g).unwrap();
        }
        g.1 += 1;
    }
    fn release(&self) {
        let mut g = self.state.lock().unwrap();
        g.1 = g.1.saturating_sub(1);
        self.cv.notify_all();
    }
    fn shrink(&self, cap: usize) {
        let mut g = self.state.lock().unwrap();
        if cap < g.0 {
            g.0 = cap.max(1);
        }
        self.cv.notify_all();
    }
    /// 动态调整并发上限（可升可降），已持有的连接不受影响，只影响后续请求
    fn set_cap(&self, cap: usize) {
        let mut g = self.state.lock().unwrap();
        let cap = cap.max(1);
        if cap != g.0 {
            g.0 = cap;
            self.cv.notify_all();
        }
    }
    fn cap(&self) -> usize {
        self.state.lock().unwrap().0
    }
}

// ---------------- 进度工具 ----------------
fn set_progress(task: &Shared, done: u64, total: u64, speed: f64) {
    if let Ok(mut t) = task.lock() {
        if total > 0 {
            t.total = total;
        }
        t.done = done;
        t.speed = speed;
    }
}

/// 单独刷新网络/磁盘速度（disk = 0 表示该引擎不区分磁盘阶段）
fn set_io(task: &Shared, net: f64, disk: f64) {
    if let Ok(mut t) = task.lock() {
        t.net_speed = net;
        t.disk_speed = disk;
    }
}

fn log_line(task: &Shared, line: String) {
    if let Ok(mut t) = task.lock() {
        t.log.push(line);
        if t.log.len() > 500 {
            t.log.drain(0..100);
        }
    }
}

fn cancelled(task: &Shared) -> bool {
    task.lock()
        .map(|t| t.cancel.load(Ordering::SeqCst))
        .unwrap_or(true)
}

// ---------------- 高并发分片 ----------------
#[allow(clippy::too_many_arguments)]
fn fetch_chunk(
    idx: usize,
    s: u64,
    e: u64,
    part: &Path,
    src: &str,
    headers: &BTreeMap<String, String>,
    limiter: &Limiter,
    task: &Shared,
    fatal: &Mutex<Option<String>>,
) -> bool {
    let need = e - s + 1;
    let mut attempt = 0;
    while attempt < 6 {
        if cancelled(task) {
            return false;
        }
        attempt += 1;
        let mut have = 0u64;
        if let Ok(md) = fs::metadata(part) {
            have = md.len();
            if have > need {
                let _ = fs::remove_file(part);
                have = 0;
            } else if have == need {
                return true;
            }
        }
        let start = s + have;
        let tmp = part.with_extension(format!("part{:03}.tmp", idx));
        let _ = fs::remove_file(&tmp);
        let mut args: Vec<String> = vec![
            "-sS".into(), "-L".into(), "--http1.1".into(),
            "-r".into(), format!("{}-{}", start, e),
            "--connect-timeout".into(), "15".into(),
            "--retry".into(), "2".into(), "--retry-delay".into(), "1".into(),
            "--retry-all-errors".into(),
            "-o".into(), tmp.to_string_lossy().to_string(),
            "-w".into(), "%{http_code}".into(),
        ];
        args.extend(header_args(headers));
        args.push(src.to_string());

        limiter.acquire();
        let out = curl_command()
            .args(&args)
            .stdin(Stdio::null())
            .output();
        limiter.release();

        let (stdout, _stderr, code) = match out {
            Ok(o) => (
                String::from_utf8_lossy(&o.stdout).trim().to_string(),
                String::from_utf8_lossy(&o.stderr).to_string(),
                o.status.code().unwrap_or(-1),
            ),
            Err(_) => {
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };

        match stdout.as_str() {
            "404" | "410" => {
                *fatal.lock().unwrap() = Some(status_text(404));
                return false;
            }
            "401" => {
                *fatal.lock().unwrap() = Some(status_text(401));
                return false;
            }
            "403" => {
                if attempt >= 3 {
                    *fatal.lock().unwrap() = Some(status_text(403));
                    return false;
                }
                limiter.shrink(limiter.cap() / 2);
                thread::sleep(Duration::from_secs(attempt as u64 + 1));
                continue;
            }
            "429" => {
                limiter.shrink(limiter.cap() / 2);
                thread::sleep(Duration::from_secs((attempt as u64).min(8) + 1));
                continue;
            }
            c if c.starts_with('5') => {
                if attempt >= 5 {
                    *fatal.lock().unwrap() = Some(status_text(503));
                    return false;
                }
                thread::sleep(Duration::from_secs((attempt as u64).min(5)));
                continue;
            }
            _ => {}
        }

        let ok_http = code == 0 && (stdout == "200" || stdout == "206");
        if ok_http {
            let want = e - start + 1;
            if let Ok(md) = fs::metadata(&tmp) {
                if md.len() == want {
                    if have > 0 {
                        if let (Ok(mut dst), Ok(mut srcf)) =
                            (OpenOptions::new().append(true).open(part), File::open(&tmp))
                        {
                            let mut buf = vec![0u8; 1 << 20];
                            loop {
                                match srcf.read(&mut buf) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        if dst.write_all(&buf[..n]).is_err() {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                        }
                        let _ = fs::remove_file(&tmp);
                    } else {
                        let _ = fs::rename(&tmp, part);
                    }
                    if fs::metadata(part).map(|m| m.len()).unwrap_or(0) == need {
                        return true;
                    }
                } else {
                    let _ = fs::remove_file(&tmp);
                    if attempt >= 3 {
                        *fatal.lock().unwrap() =
                            Some("返回内容尺寸不符，可能被站点拦截并返回了验证页".into());
                        return false;
                    }
                }
            }
        } else {
            let _ = fs::remove_file(&tmp);
        }
        thread::sleep(Duration::from_secs((attempt as u64).min(6)));
    }
    false
}

pub fn run_turbo(
    task: Shared,
    url: &str,
    headers: &BTreeMap<String, String>,
    pr: &Probe,
    out_dir: &Path,
    conn: usize,
) -> Result<PathBuf, EngErr> {
    if pr.size == 0 || !pr.ranges {
        return Err(EngErr::Soft("目标不支持 Range，无法分片".into()));
    }
    let _ = fs::create_dir_all(out_dir);
    let out_path = out_dir.join(&pr.name);
    let size = pr.size;

    // 清理上次异常退出留下的临时响应头文件
    if let Ok(entries) = fs::read_dir(out_dir) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with(".dlkit-hdr-") {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    // 分块大小必须让每条连接传足够多的数据：
    // 实测同一文件（Cloudflare）32 连接 × 1MB/块 只有 14MB/s，
    // 32 连接 × 4.7MB/块 可达 45MB/s——小块会让每条新连接反复经历 TCP 慢启动。
    // 每连接约 1.5 块：既保持大块传输，又留出少量收尾窃取空间。
    let lanes = ((conn as u64) * 3 / 2).max(1);
    let block = ((size + lanes - 1) / lanes)
        .clamp(4 << 20, 32 << 20)
        .min(size.max(1));
    let total_blocks = ((size + block - 1) / block) as usize;
    let workers = conn.max(1).min(total_blocks).max(1);

    let state_path = out_dir.join(format!("{}.dlkit.json", pr.name));
    #[derive(serde::Serialize, serde::Deserialize, Default)]
    struct State {
        size: u64,
        block: u64,
        done: Vec<u64>,
    }

    let prev: State = fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str::<State>(&s).ok())
        .filter(|st| st.size == size && st.block == block)
        .unwrap_or(State { size, block, done: Vec::new() });

    // 一次性预分配目标文件，避免下载过程中反复扩展文件造成碎片
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&out_path)
        .map_err(|e| EngErr::Fatal(format!("无法创建目标文件：{}", e)))?;
    if file.metadata().map(|m| m.len()).unwrap_or(0) != size {
        file.set_len(size)
            .map_err(|e| EngErr::Fatal(format!("预分配磁盘空间失败：{}", e)))?;
    }
    let file = Arc::new(file);

    let done: Arc<Mutex<BTreeSet<u64>>> =
        Arc::new(Mutex::new(prev.done.iter().copied().collect()));
    let queue: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let d = done.lock().unwrap();
        let mut q = queue.lock().unwrap();
        let mut off = 0u64;
        while off < size {
            if !d.contains(&off) {
                q.push(off);
            }
            off += block;
        }
    }
    if queue.lock().unwrap().is_empty() {
        set_progress(&task, size, size, 0.0);
        let _ = fs::remove_file(&state_path);
        return Ok(out_path);
    }

    let base_done: u64 = done
        .lock()
        .unwrap()
        .iter()
        .map(|&o| (size - o).min(block))
        .sum();
    let counter = Arc::new(AtomicU64::new(base_done));
    let io = Arc::new(IoStats::default());
    let limiter = Arc::new(Limiter::new(workers));
    let fatal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cancel = task
        .lock()
        .map(|t| t.cancel.clone())
        .unwrap_or_else(|_| Arc::new(AtomicBool::new(false)));
    let headers = Arc::new(headers.clone());
    let url = Arc::new(url.to_string());
    let out_dir_arc = Arc::new(out_dir.to_path_buf());
    log_line(
        &task,
        format!(
            "探测 {:.2}s，{} 块 × {:.1} MB",
            pr.probe_ms as f64 / 1000.0,
            total_blocks,
            block as f64 / 1048576.0
        ),
    );

    let mut handles = Vec::new();
    for wid in 0..workers {
        let done = done.clone();
        let queue = queue.clone();
        let file = file.clone();
        let limiter = limiter.clone();
        let fatal = fatal.clone();
        let cancel = cancel.clone();
        let counter = counter.clone();
        let io_stats = io.clone();
        let headers = headers.clone();
        let url = url.clone();
        let out_dir_arc = out_dir_arc.clone();
        let task_log = task.clone();
        handles.push(thread::spawn(move || {
            let hdr = out_dir_arc.join(format!(".dlkit-hdr-{}.txt", wid));
            loop {
                if cancel.load(Ordering::SeqCst) || fatal.lock().unwrap().is_some() {
                    break;
                }
                let off = match queue.lock().unwrap().pop() {
                    Some(o) => o,
                    None => break,
                };
                let end = (off + block - 1).min(size - 1);
                let ok = fetch_block(
                    &url, &headers, &file, off, end, &hdr, &counter, &io_stats, &limiter, &cancel,
                    &fatal, &task_log,
                );
                if ok {
                    done.lock().unwrap().insert(off);
                } else if !cancel.load(Ordering::SeqCst) && fatal.lock().unwrap().is_none() {
                    // 该块没成功但未判死：放回队尾，换个时间点再抢
                    queue.lock().unwrap().push(off);
                    thread::sleep(Duration::from_millis(700));
                }
            }
            let _ = fs::remove_file(&hdr);
        }));
    }

    let mut last_done = base_done;
    let mut last_t = Instant::now();
    let mut speed = 0.0f64;
    let mut smooth_disk = 0.0f64;
    let mut snap = (0u64, 0u64); // disk_bytes, disk_ns
    let mut last_save = Instant::now();
    let mut best_speed = 0.0f64;
    let mut low_streak = 0u32;
    let mut last_tune = Instant::now() - Duration::from_secs(5);
    loop {
        let finished = handles.iter().all(|h| h.is_finished());
        let cur = counter.load(Ordering::Relaxed);
        let now = Instant::now();
        let dt = now.duration_since(last_t).as_secs_f64();
        if dt >= 0.3 {
            let inst = cur.saturating_sub(last_done) as f64 / 1048576.0 / dt;
            // 平滑：避免展示值抖动（用户感知到的"速度不稳"）
            speed = if speed == 0.0 { inst } else { speed * 0.65 + inst * 0.35 };
            set_progress(&task, cur, size, speed);

            let db = io.disk_bytes.load(Ordering::Relaxed);
            let dn = io.disk_ns.load(Ordering::Relaxed);
            let disk_inst = if dn > snap.1 && db >= snap.0 {
                (db - snap.0) as f64 / ((dn - snap.1) as f64 / 1e9) / 1048576.0
            } else {
                smooth_disk
            };
            smooth_disk = if smooth_disk == 0.0 {
                disk_inst
            } else {
                smooth_disk * 0.6 + disk_inst * 0.4
            };
            snap = (db, dn);
            // 网络速率取实际吞吐；磁盘速率取写盘阶段吞吐
            set_io(&task, speed, smooth_disk);

            // 自适应并发：吞吐明显低于本任务峰值并持续 4s，就降档（常见于站点按 IP 限流）
            if now.duration_since(last_tune) >= Duration::from_secs(2) {
                if speed > best_speed {
                    best_speed = speed;
                }
                if best_speed > 4.0 && speed < best_speed * 0.6 {
                    low_streak += 1;
                } else {
                    low_streak = 0;
                }
                let cap = limiter.cap();
                if low_streak >= 2 && cap > 4 {
                    let new_cap = (cap * 3 / 4).max(4);
                    limiter.set_cap(new_cap);
                    log_line(
                        &task,
                        format!(
                            "吞吐下降（{:.1} MB/s），并发 {} → {} 路",
                            speed, cap, new_cap
                        ),
                    );
                    low_streak = 0;
                    best_speed = speed.max(1.0);
                }
                last_tune = now;
            }

            last_done = cur;
            last_t = now;
        }
        if now.duration_since(last_save) >= Duration::from_secs(2) {
            save_block_state(&state_path, size, block, &done.lock().unwrap());
            last_save = now;
        }
        if finished || cancel.load(Ordering::SeqCst) || fatal.lock().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(150));
    }
    for h in handles {
        let _ = h.join();
    }
    save_block_state(&state_path, size, block, &done.lock().unwrap());

    if let Some(msg) = fatal.lock().unwrap().take() {
        return Err(EngErr::Fatal(msg));
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(EngErr::Soft("已取消，进度已保留".into()));
    }
    let finished_blocks = done.lock().unwrap().len();
    if finished_blocks < total_blocks {
        return Err(EngErr::Soft(format!(
            "已完成 {}/{} 块，进度已保留，重新运行可续传",
            finished_blocks, total_blocks
        )));
    }

    let _ = fs::remove_file(&state_path);
    set_progress(&task, size, size, 0.0);
    log_line(
        &task,
        format!("{} 块 × {:.1} MB，{} 路并发完成", total_blocks, block as f64 / 1048576.0, workers),
    );
    Ok(out_path)
}

fn save_block_state(path: &Path, size: u64, block: u64, done: &BTreeSet<u64>) {
    let v = serde_json::json!({
        "size": size,
        "block": block,
        "done": done.iter().copied().collect::<Vec<u64>>()
    });
    let _ = fs::write(path, v.to_string());
}

fn read_status(path: &Path) -> Option<u16> {
    let text = fs::read_to_string(path).ok()?;
    let mut last = None;
    for line in text.lines() {
        if line.starts_with("HTTP/") {
            if let Some(code) = line.split_whitespace().nth(1).and_then(|c| c.parse::<u16>().ok()) {
                last = Some(code);
            }
        }
    }
    last
}

/// 拉取单个分块：curl 流式输出 → 直接定位写入目标文件（零临时文件、零合并阶段）
#[allow(clippy::too_many_arguments)]
fn fetch_block(
    url: &str,
    headers: &BTreeMap<String, String>,
    file: &File,
    off: u64,
    end: u64,
    hdr_path: &Path,
    counter: &AtomicU64,
    io: &IoStats,
    limiter: &Limiter,
    cancel: &AtomicBool,
    fatal: &Mutex<Option<String>>,
    task: &Shared,
) -> bool {
    let want = end - off + 1;
    let mut attempt = 0u32;
    while attempt < 6 {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        attempt += 1;
        let _ = fs::remove_file(hdr_path);
        let mut args: Vec<String> = vec![
            "-sS".into(), "-L".into(), "--http1.1".into(),
            "--fail".into(),
            "--tcp-nodelay".into(),
            "-r".into(), format!("{}-{}", off, end),
            "--connect-timeout".into(), "15".into(),
            // 卡死/极慢的单条连接尽快放弃，交给重试换一条线路，
            // 否则一个卡在几十 KB/s 的分块会把整个任务拖到几十秒。
            "--speed-limit".into(), "102400".into(),
            "--speed-time".into(), "10".into(),
            "--retry".into(), "3".into(), "--retry-delay".into(), "1".into(),
            "--retry-all-errors".into(),
            "-D".into(), hdr_path.to_string_lossy().to_string(),
            "-o".into(), "-".into(),
        ];
        args.extend(header_args(headers));
        args.push(url.to_string());

        limiter.acquire();
        let mut child = match curl_command()
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => {
                limiter.release();
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };

        let mut written = 0u64;
        let mut io_err = false;
        if let Some(so) = child.stdout.take() {
            let mut reader = std::io::BufReader::with_capacity(1 << 20, so);
            let mut buf = vec![0u8; 1 << 20];
            loop {
                if cancel.load(Ordering::SeqCst) {
                    io_err = true;
                    break;
                }
                let read_res = reader.read(&mut buf);
                match read_res {
                    Ok(0) => break,
                    Ok(n) => {
                        let left = want - written;
                        let take = (n as u64).min(left) as usize;
                        if take > 0 {
                            let t_write = Instant::now();
                            let wres = file.seek_write(&buf[..take], off + written);
                            io.disk_ns
                                .fetch_add(t_write.elapsed().as_nanos() as u64, Ordering::Relaxed);
                            if wres.is_err() {
                                io_err = true;
                                break;
                            }
                            io.disk_bytes
                                .fetch_add(take as u64, Ordering::Relaxed);
                            written += take as u64;
                            counter.fetch_add(take as u64, Ordering::Relaxed);
                        }
                        if take < n {
                            // 服务器给的数据超过本块：截断并收工（此时数据已够）
                            let _ = child.kill();
                            break;
                        }
                    }
                    Err(_) => {
                        io_err = true;
                        break;
                    }
                }
            }
        }
        if io_err {
            let _ = child.kill();
        }
        let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
        limiter.release();
        if code == 2 {
            *fatal.lock().unwrap() =
                Some("curl 初始化失败（参数不被当前版本支持或系统资源不足）".into());
            return false;
        }

        let status = read_status(hdr_path);
        match status {
            Some(401) => {
                *fatal.lock().unwrap() = Some(status_text(401));
                return false;
            }
            Some(404) | Some(410) => {
                *fatal.lock().unwrap() = Some(status_text(404));
                return false;
            }
            Some(403) => {
                if attempt >= 4 {
                    *fatal.lock().unwrap() = Some(status_text(403));
                    return false;
                }
                let cap = (limiter.cap() / 2).max(1);
                limiter.shrink(cap);
                log_line(
                    task,
                    format!(
                        "分块 {}–{} 被拒绝(403)，并发降到 {}，第 {} 次重试",
                        off, end, cap, attempt
                    ),
                );
                thread::sleep(Duration::from_secs(attempt as u64 + 1));
                continue;
            }
            Some(429) => {
                let cap = (limiter.cap() / 2).max(1);
                limiter.shrink(cap);
                log_line(
                    task,
                    format!(
                        "分块 {}–{} 被限流(429)，并发降到 {}，第 {} 次重试",
                        off, end, cap, attempt
                    ),
                );
                thread::sleep(Duration::from_secs((attempt as u64).min(8) + 1));
                continue;
            }
            Some(c) if c >= 500 => {
                log_line(
                    task,
                    format!("分块 {}–{} 服务端错误(HTTP {})，{}s 后重试", off, end, c, attempt.min(5)),
                );
                thread::sleep(Duration::from_secs((attempt as u64).min(5)));
                continue;
            }
            Some(200) if want < file_len_hint(file) => {
                // 服务器忽略了 Range，返回整文件：不能当分片用
                *fatal.lock().unwrap() = Some("服务器忽略了 Range 请求，无法分片下载".into());
                return false;
            }
            _ => {}
        }

        // Range 请求只要字节数收齐就是完整数据，不因 curl 退出码/连接收尾方式判失败
        if written == want {
            return true;
        }
        if written > 0 {
            counter.fetch_sub(written, Ordering::Relaxed);
        }
        log_line(
            task,
            format!(
                "分块 {}–{} 中断（HTTP {}，curl={}，已收 {}/{} 字节），第 {} 次重试",
                off,
                end,
                status.map(|c| c.to_string()).unwrap_or_else(|| "无响应".into()),
                code,
                written,
                want,
                attempt
            ),
        );
        thread::sleep(Duration::from_secs((attempt as u64).min(6)));
    }
    if fatal.lock().unwrap().is_none() {
        *fatal.lock().unwrap() =
            Some("部分分块下载失败：网络不稳或站点限流，重新运行可续传".into());
    }
    false
}

fn file_len_hint(file: &File) -> u64 {
    file.metadata().map(|m| m.len()).unwrap_or(u64::MAX)
}

#[allow(dead_code)]
fn run_turbo_legacy(
    task: Shared,
    url: &str,
    headers: &BTreeMap<String, String>,
    pr: &Probe,
    out_dir: &Path,
    conn: usize,
) -> Result<PathBuf, EngErr> {
    if pr.size == 0 || !pr.ranges {
        return Err(EngErr::Soft("目标不支持 Range，无法分片".into()));
    }
    let _ = fs::create_dir_all(out_dir);
    let out_path = out_dir.join(&pr.name);
    let size = pr.size;
    let mut n = conn.max(1);
    if size / (n as u64) < MIN_CHUNK {
        n = ((size / MIN_CHUNK) as usize).max(1);
    }

    let state_path = out_dir.join(format!("{}.dlkit.json", pr.name));
    #[derive(serde::Serialize, Deserialize, Clone)]
    struct Chunk {
        s: u64,
        e: u64,
        done: bool,
    }
    #[derive(serde::Serialize, Deserialize)]
    struct State {
        size: u64,
        chunks: Vec<Chunk>,
    }

    let mut state: State = fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str::<State>(&s).ok())
        .filter(|st| st.size == size)
        .unwrap_or_else(|| {
            let step = size / n as u64;
            let mut chunks = Vec::new();
            let mut pos = 0u64;
            for i in 0..n {
                let end = if i == n - 1 { size - 1 } else { pos + step - 1 };
                chunks.push(Chunk { s: pos, e: end, done: false });
                pos = end + 1;
            }
            State { size, chunks }
        });

    let save_state = |st: &State| {
        if let Ok(txt) = serde_json::to_string(st) {
            let tmp = state_path.with_extension("json.tmp");
            let _ = fs::write(&tmp, txt);
            let _ = fs::rename(&tmp, &state_path);
        }
    };
    save_state(&state);

    let limiter = Arc::new(Limiter::new(conn));
    let fatal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cancel = task
        .lock()
        .map(|t| t.cancel.clone())
        .unwrap_or_else(|_| Arc::new(AtomicBool::new(false)));

    // 预先为每个块算出 part 路径（顺序固定）
    let parts: Vec<PathBuf> = (0..state.chunks.len())
        .map(|i| out_dir.join(format!("{}.part{:03}", pr.name, i)))
        .collect();

    let mut base_done = 0u64;
    for (i, c) in state.chunks.iter_mut().enumerate() {
        if c.done && parts[i].exists() {
            base_done += c.e - c.s + 1;
        } else {
            c.done = false;
        }
    }

    let mut handles = Vec::new();
    let headers_arc = Arc::new(headers.clone());
    let src_arc = Arc::new(vec![url.to_string()]);
    for i in 0..state.chunks.len() {
        if state.chunks[i].done {
            continue;
        }
        let s = state.chunks[i].s;
        let e = state.chunks[i].e;
        let part = parts[i].clone();
        let h = headers_arc.clone();
        let src = src_arc.clone();
        let lim = limiter.clone();
        let t = task.clone();
        let f = fatal.clone();
        let canc = cancel.clone();
        handles.push((i, thread::spawn(move || {
            if canc.load(Ordering::SeqCst) {
                return false;
            }
            let ok = fetch_chunk(i, s, e, &part, &src[0], &h, &lim, &t, &f);
            if canc.load(Ordering::SeqCst) {
                return false;
            }
            ok
        })));
    }

    // 监控进度 + 取消
    let mut last_done = base_done;
    let mut last_t = Instant::now();
    let mut speed = 0.0f64;
    loop {
        let finished = handles.iter().all(|(_, h)| h.is_finished());
        let mut done = base_done;
        for (i, h) in handles.iter() {
            if h.is_finished() {
                // 已完成的块，part 大小即完整
                if let Ok(md) = fs::metadata(&parts[*i]) {
                    done += md.len();
                }
            } else if let Ok(md) = fs::metadata(&parts[*i]) {
                let c = &state.chunks[*i];
                done += md.len().min(c.e - c.s + 1);
            }
        }
        let now = Instant::now();
        let dt = now.duration_since(last_t).as_secs_f64();
        if dt >= 0.3 {
            let inst = (done.saturating_sub(last_done)) as f64 / 1048576.0 / dt;
            speed = if speed == 0.0 { inst } else { speed * 0.6 + inst * 0.4 };
            set_progress(&task, done, size, speed);
            last_done = done;
            last_t = now;
        }
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        if fatal.lock().map(|f| f.is_some()).unwrap_or(false) {
            break;
        }
        if finished {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }

    let mut all_ok = !cancel.load(Ordering::SeqCst);
    for (i, h) in handles {
        let ok = h.join().unwrap_or(false);
        if ok {
            state.chunks[i].done = true;
        } else {
            all_ok = false;
        }
    }
    save_state(&state);

    if let Some(msg) = fatal.lock().unwrap().take() {
        return Err(EngErr::Fatal(msg));
    }
    if !all_ok {
        return Err(EngErr::Soft("部分分片未完成，已保留进度".into()));
    }

    // 合并
    let tmp_out = out_dir.join(format!("{}.merging", pr.name));
    {
        let mut dst = File::create(&tmp_out).map_err(|e| EngErr::Fatal(format!("无法写入文件：{}", e)))?;
        for i in 0..state.chunks.len() {
            let mut f = File::open(&parts[i])
                .map_err(|e| EngErr::Fatal(format!("分片缺失：{}", e)))?;
            let mut buf = vec![0u8; 4 << 20];
            loop {
                match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        dst.write_all(&buf[..n])
                            .map_err(|e| EngErr::Fatal(format!("合并失败：{}", e)))?;
                    }
                    Err(e) => return Err(EngErr::Fatal(format!("读取分片失败：{}", e))),
                }
            }
        }
    }
    fs::rename(&tmp_out, &out_path).map_err(|e| EngErr::Fatal(format!("写入失败：{}", e)))?;
    for p in parts.iter() {
        let _ = fs::remove_file(p);
    }
    let _ = fs::remove_file(&state_path);
    set_progress(&task, size, size, 0.0);
    log_line(&task, format!("并发 {} 路完成", limiter.cap()));
    Ok(out_path)
}

// ---------------- aria2 ----------------
/// 从 config/aria2.conf 读取 rpc-secret；读不到就用默认值
fn aria2_secret(root: &Path) -> String {
    let conf = root.join("config").join("aria2.conf");
    if let Ok(text) = fs::read_to_string(conf) {
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            if let Some(v) = line.strip_prefix("rpc-secret=") {
                let v = v.trim();
                if !v.is_empty() {
                    return v.to_string();
                }
            }
        }
    }
    "dlkit-local-token".to_string()
}

fn aria2_rpc(root: &Path, method: &str, params: Value) -> Result<Value, EngErr> {
    if !root.exists() {
        return Err(EngErr::Soft("未找到 DLKit 根目录".into()));
    }
    // aria2 开启 rpc-secret 后，params 第一位必须是 "token:<secret>"，否则返回 Unauthorized
    let mut list = vec![json!(format!("token:{}", aria2_secret(root)))];
    match params {
        Value::Array(items) => list.extend(items),
        Value::Null => {}
        other => list.push(other),
    }
    let payload = json!({"jsonrpc":"2.0","id":"dlkit","method":method,"params":list});
    let args: Vec<String> = vec![
        "-sS".into(), "-X".into(), "POST".into(),
        "-H".into(), "Content-Type: application/json".into(),
        "--data-binary".into(), payload.to_string(),
        "http://127.0.0.1:6800/jsonrpc".into(),
    ];
    let out = new_cmd("curl")
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| EngErr::Soft(format!("aria2 RPC 不可用：{}", e)))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let v: Value = serde_json::from_str(&text)
        .map_err(|_| EngErr::Soft("aria2 未运行，请先启动服务".into()))?;
    if let Some(err) = v.get("error") {
        return Err(EngErr::Soft(format!(
            "aria2 错误：{}",
            err.get("message").and_then(|m| m.as_str()).unwrap_or("未知")
        )));
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

fn friendly_aria2(msg: &str) -> String {
    let low = msg.to_lowercase();
    if low.contains("unauthorized") {
        "aria2 RPC 鉴权失败：请确认 config/aria2.conf 的 rpc-secret 与服务端一致".into()
    } else if low.contains("not found") {
        "链接失效或文件不存在（HTTP 404）".into()
    } else if msg.contains("403") {
        "请求被拒绝（HTTP 403）：可能需要 Cookie/Referer".into()
    } else if low.contains("timeout") {
        "连接超时：目标不可达或网络不稳定".into()
    } else {
        msg.to_string()
    }
}

fn run_aria2_uri(
    task: Shared,
    root: &Path,
    uri: &str,
    dir: &Path,
    headers: &BTreeMap<String, String>,
    conn: usize,
    is_file: bool,
) -> Result<PathBuf, EngErr> {
    let dir_s = dir.to_string_lossy().to_string();
    let mut opts = json!({
        "dir": dir_s,
        "split": conn.min(16).to_string(),
        "max-connection-per-server": "16",
        "min-split-size": "1M",
        "file-allocation": "none",
        "continue": "true"
    });
    if let Some(v) = headers.get("User-Agent") {
        opts["user-agent"] = json!(v);
    }
    if let Some(v) = headers.get("Referer") {
        opts["referer"] = json!(v);
    }
    if let Some(v) = headers.get("Cookie") {
        opts["header"] = json!(format!("Cookie: {}", v));
    }

    let gid = if is_file {
        let bytes = fs::read(uri).map_err(|e| EngErr::Fatal(format!("读取种子失败：{}", e)))?;
        let b64 = base64_encode(&bytes);
        aria2_rpc(root, "aria2.addTorrent", json!([b64, [], opts]))?
            .as_str().unwrap_or("").to_string()
    } else {
        aria2_rpc(root, "aria2.addUri", json!([[uri], opts]))?
            .as_str().unwrap_or("").to_string()
    };
    if gid.is_empty() {
        return Err(EngErr::Soft("aria2 拒绝任务".into()));
    }
    log_line(&task, format!("aria2 gid={}", gid));

    let cancel = task
        .lock()
        .map(|t| t.cancel.clone())
        .unwrap_or_else(|_| Arc::new(AtomicBool::new(false)));

    let mut last_t = Instant::now();
    let mut last_done = 0u64;
    let started = Instant::now();
    let mut last_progress = Instant::now();
    let mut seen_total = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = aria2_rpc(root, "aria2.forceRemove", json!([gid]));
            return Err(EngErr::Soft("已取消".into()));
        }
        let st = aria2_rpc(
            root,
            "aria2.tellStatus",
            json!([gid, ["status", "completedLength", "totalLength", "downloadSpeed", "files", "errorMessage"]]),
        )?;
        let status = st.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let done = st
            .get("completedLength")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let total = st
            .get("totalLength")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if total > 0 {
            seen_total = total;
        }
        if done > last_done {
            last_progress = Instant::now();
        }

        // 看门狗：始终拿不到文件长度 → 目标没有正常响应（常见于被 hosts 屏蔽到本机）
        if status == "active" && seen_total == 0 && started.elapsed() > Duration::from_secs(15) {
            let _ = aria2_rpc(root, "aria2.forceRemove", json!([gid]));
            return Err(EngErr::Fatal(
                "目标服务器始终没有返回文件大小，下载无法开始。\n\
                 常见原因：域名被 hosts 指向 127.0.0.1、被本机代理拦截，或该 CDN 拒绝当前请求。"
                    .into(),
            ));
        }
        // 看门狗：长时间零速度（已拿到长度但完全没有数据）
        if status == "active" && last_progress.elapsed() > Duration::from_secs(45) {
            let _ = aria2_rpc(root, "aria2.forceRemove", json!([gid]));
            return Err(EngErr::Fatal(
                "已连续 45 秒没有任何数据，已中止。请检查网络、代理或该链接是否被屏蔽。".into(),
            ));
        }

        let now = Instant::now();
        let dt = now.duration_since(last_t).as_secs_f64();
        let spd = if dt > 0.2 {
            let s = (done.saturating_sub(last_done)) as f64 / 1048576.0 / dt;
            last_t = now;
            last_done = done;
            s
        } else {
            0.0
        };
        set_progress(&task, done, total, spd);
        // aria2 在自身进程内完成写盘，无法拆分阶段，只上报网络速率
        set_io(&task, spd, 0.0);

        if status == "complete" {
            let path = st
                .get("files")
                .and_then(|f| f.get(0))
                .and_then(|f| f.get("path"))
                .and_then(|p| p.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| dir.to_path_buf());
            set_progress(&task, total.max(done), total.max(done), 0.0);
            return Ok(path);
        }
        if status == "error" || status == "removed" {
            let msg = st
                .get("errorMessage")
                .and_then(|m| m.as_str())
                .unwrap_or("未知错误");
            return Err(EngErr::Soft(friendly_aria2(msg)));
        }
        thread::sleep(Duration::from_millis(400));
    }
}

// ---------------- 单连接直下（不支持 Range 的目标） ----------------
fn run_single_stream(
    task: Shared,
    url: &str,
    name: &str,
    dir: &Path,
    headers: &BTreeMap<String, String>,
    expected: u64,
) -> Result<PathBuf, EngErr> {
    let _ = fs::create_dir_all(dir);
    let out_path = dir.join(sanitize_name(name));
    let _ = fs::remove_file(&out_path);
    if let Ok(mut t) = task.lock() {
        t.message = "单连接下载中…".into();
    }

    let mut args: Vec<String> = vec![
        "-sS".into(), "-L".into(), "--http1.1".into(),
        "--connect-timeout".into(), "15".into(),
        "--retry".into(), "3".into(), "--retry-delay".into(), "2".into(),
        "--retry-all-errors".into(),
        "-o".into(), out_path.to_string_lossy().to_string(),
        "-w".into(), "%{http_code}".into(),
    ];
    args.extend(header_args(headers));
    args.push(url.to_string());

    let mut child = curl_command()
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| EngErr::Fatal(format!("无法启动 curl：{}", e)))?;

    let cancel = task.lock().ok().map(|t| t.cancel.clone());
    let started = Instant::now();
    let mut last_progress = Instant::now();
    let mut last_size = 0u64;
    let mut last_t = Instant::now();

    loop {
        if cancel
            .as_ref()
            .map(|c| c.load(Ordering::SeqCst))
            .unwrap_or(false)
        {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&out_path);
            return Err(EngErr::Soft("已取消".into()));
        }
        let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
        if size > last_size {
            let dt = last_t.elapsed().as_secs_f64();
            if dt > 0.2 {
                let spd = (size - last_size) as f64 / 1048576.0 / dt;
                set_progress(&task, size, expected.max(size), spd);
                // 单连接直下由 curl 直接写文件，读写不可分离，两者按同一速率显示
                set_io(&task, spd, spd);
                last_size = size;
                last_t = Instant::now();
            }
            last_progress = Instant::now();
        } else if size == 0 && started.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&out_path);
            return Err(EngErr::Fatal(
                "20 秒内没有收到任何数据，已中止。\n\
                 常见原因：域名被 hosts 指向 127.0.0.1、代理拦截，或该 CDN 拒绝当前请求。"
                    .into(),
            ));
        } else if last_progress.elapsed() > Duration::from_secs(45) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&out_path);
            return Err(EngErr::Fatal(
                "下载连续 45 秒没有任何进展，已中止。请检查网络或代理。".into(),
            ));
        }

        match child.try_wait() {
            Ok(Some(st)) => {
                let mut code = String::new();
                if let Some(mut so) = child.stdout.take() {
                    let _ = so.read_to_string(&mut code);
                }
                let size = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
                if st.success() && matches!(code.trim(), "200" | "206") {
                    if expected > 0 && size != expected {
                        return Err(EngErr::Fatal(format!(
                            "文件不完整：实际 {} 字节，应为 {} 字节（可能被拦截返回了错误页）",
                            size, expected
                        )));
                    }
                    set_progress(&task, size, size.max(expected), 0.0);
                    return Ok(out_path);
                }
                return Err(EngErr::Soft(format!(
                    "单连接下载失败（curl 退出码 {}）",
                    st.code().unwrap_or(-1)
                )));
            }
            Ok(None) => thread::sleep(Duration::from_millis(250)),
            Err(e) => return Err(EngErr::Fatal(format!("下载异常：{}", e))),
        }
    }
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

// ---------------- yt-dlp ----------------
fn run_media(task: Shared, root: &Path, url: &str, dir: &Path, headers: &BTreeMap<String, String>, conn: usize) -> Result<PathBuf, EngErr> {
    let _ = fs::create_dir_all(dir);
    let ytdlp = root.join("bin").join("yt-dlp").join("yt-dlp.exe");
    let ffmpeg = root.join("bin").join("ffmpeg").join("bin");
    let mut cmd = new_cmd(&ytdlp);
    cmd.arg("--newline")
        .arg("--no-playlist")
        .arg("-P").arg(dir)
        .arg("--ffmpeg-location").arg(&ffmpeg)
        .arg("--no-mtime")
        .arg("--concurrent-fragments").arg(conn.min(16).max(2).to_string())
        .arg("--buffer-size").arg("16M")
        .arg("--http-chunk-size").arg("10M")
        .arg("--retries").arg("10")
        .arg("--fragment-retries").arg("10")
        .arg("--downloader").arg("aria2c")
        .arg("--downloader-args")
        .arg("aria2c:-x16 -s16 -k1M --file-allocation=none --console-log-level=warn --summary-interval=0");
    if let Some(v) = headers.get("Cookie") {
        cmd.arg("--add-header").arg(format!("Cookie: {}", v));
    }
    if let Some(v) = headers.get("Referer") {
        cmd.arg("--referer").arg(v);
    }
    if let Some(v) = headers.get("User-Agent") {
        cmd.arg("--user-agent").arg(v);
    }
    cmd.arg(url);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| EngErr::Fatal(format!("无法启动 yt-dlp：{}", e)))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let cancel = task
        .lock()
        .map(|t| t.cancel.clone())
        .unwrap_or_else(|_| Arc::new(AtomicBool::new(false)));

    let t2 = task.clone();
    let reader = thread::spawn(move || {
        use std::io::BufRead;
        let mut lines = Vec::new();
        if let Some(so) = stdout {
            for line in std::io::BufReader::new(so).lines().map_while(Result::ok) {
                lines.push(line);
            }
        }
        if let Some(se) = stderr {
            for line in std::io::BufReader::new(se).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    lines.push(line);
                }
            }
        }
        for l in lines {
            parse_ytdlp_line(&t2, &l);
        }
    });

    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EngErr::Soft("已取消".into()));
        }
        match child.try_wait() {
            Ok(Some(st)) => {
                let _ = reader.join();
                if !st.success() {
                    return Err(EngErr::Fatal("yt-dlp 下载失败（可能需要 Cookie 或该站不支持）".into()));
                }
                let mut t = task.lock().unwrap();
                if t.total > 0 {
                    t.done = t.total;
                }
                return Ok(dir.to_path_buf());
            }
            Ok(None) => thread::sleep(Duration::from_millis(300)),
            Err(e) => return Err(EngErr::Fatal(format!("yt-dlp 异常：{}", e))),
        }
    }
}

fn parse_ytdlp_line(task: &Shared, line: &str) {
    let mut t = match task.lock() {
        Ok(t) => t,
        Err(_) => return,
    };
    t.log.push(line.to_string());
    if t.log.len() > 500 {
        t.log.drain(0..100);
    }
    // [download]  12.3% of  123.45MiB at  1.23MiB/s ETA 00:12
    if let Some(p) = line.find('%') {
        let head = &line[..p];
        let num: String = head
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if let Ok(pct) = num.parse::<f64>() {
            if let Some(of) = line.find(" of ") {
                let rest = &line[of + 4..];
                let size_txt: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                let unit = rest[size_txt.len()..].trim_start();
                let mult = if unit.starts_with("GiB") {
                    1024.0 * 1024.0 * 1024.0
                } else if unit.starts_with("MiB") {
                    1024.0 * 1024.0
                } else if unit.starts_with("KiB") {
                    1024.0
                } else {
                    1.0
                };
                if let Ok(v) = size_txt.parse::<f64>() {
                    let total = (v * mult) as u64;
                    if total > 0 {
                        t.total = total;
                        t.done = ((total as f64) * pct / 100.0) as u64;
                    }
                }
            }
            if let Some(at) = line.find(" at ") {
                let spd_txt: String = line[at + 4..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                if let Ok(v) = spd_txt.parse::<f64>() {
                    let mb = if line[at..].contains("GiB/s") {
                        v * 1024.0
                    } else if line[at..].contains("KiB/s") {
                        v / 1024.0
                    } else {
                        v
                    };
                    t.speed = mb;
                    t.net_speed = mb;
                    t.disk_speed = mb;
                }
            }
        }
    }
}

// ---------------- OpenList ----------------
fn openlist_conf(root: &Path) -> (String, String, String) {
    let p = root.join("config").join("openlist.json");
    let text = fs::read_to_string(p).unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    (
        v.get("base").and_then(|x| x.as_str()).unwrap_or("http://127.0.0.1:5244").to_string(),
        v.get("username").and_then(|x| x.as_str()).unwrap_or("admin").to_string(),
        v.get("password").and_then(|x| x.as_str()).unwrap_or("DLKit@2026").to_string(),
    )
}

fn ol_post(base: &str, path: &str, body: Value, token: Option<&str>) -> Result<Value, EngErr> {
    let mut args: Vec<String> = vec![
        "-sS".into(), "-X".into(), "POST".into(),
        "-H".into(), "Content-Type: application/json".into(),
        "--data-binary".into(), body.to_string(),
        format!("{}{}", base, path),
    ];
    if let Some(t) = token {
        args.push("-H".into());
        args.push(format!("Authorization: {}", t));
    }
    let out = new_cmd("curl").args(&args).output()
        .map_err(|e| EngErr::Soft(format!("OpenList 请求失败：{}", e)))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let v: Value = serde_json::from_str(&text)
        .map_err(|_| EngErr::Soft("OpenList 未运行（请先启动服务）".into()))?;
    if v.get("code").and_then(|c| c.as_i64()).unwrap_or(0) != 200 {
        return Err(EngErr::Soft(format!(
            "OpenList：{}",
            v.get("message").and_then(|m| m.as_str()).unwrap_or("请求失败")
        )));
    }
    Ok(v.get("data").cloned().unwrap_or(Value::Null))
}

pub fn openlist_path_from_url(url: &str) -> Option<String> {
    let root = find_root();
    let (base, _, _) = openlist_conf(&root);
    let bh = host_of(&base)?;
    let uh = host_of(url)?;
    let bp = base.split("://").nth(1)?.split('/').next()?.split(':').nth(1).and_then(|p| p.parse::<u16>().ok()).unwrap_or(80);
    let up = url.split("://").nth(1)?.split('/').next()?.split(':').nth(1).and_then(|p| p.parse::<u16>().ok()).unwrap_or(80);
    if bh != uh || bp != up {
        return None;
    }
    let path = percent_decode(&url_path(url));
    let path = path.split('?').next().unwrap_or(&path).to_string();
    if let Some(rest) = path.strip_prefix("/d/") {
        Some(format!("/{}", rest))
    } else if let Some(rest) = path.strip_prefix("/p/") {
        Some(format!("/{}", rest))
    } else if path != "/" && !path.is_empty() {
        Some(path)
    } else {
        None
    }
}

fn run_openlist(task: Shared, root: &Path, url: &str, dir: &Path, conn: usize, user_ol: Option<String>) -> Result<PathBuf, EngErr> {
    let (base, user, pass) = openlist_conf(root);
    log_line(&task, format!("OpenList: {}", base));
    let data = ol_post(&base, "/api/auth/login", json!({"username": user, "password": pass}), None)?;
    let token = data.get("token").and_then(|t| t.as_str()).unwrap_or("").to_string();
    if token.is_empty() {
        return Err(EngErr::Fatal("OpenList 登录失败".into()));
    }
    let path = match user_ol.or_else(|| openlist_path_from_url(url)) {
        Some(p) => p,
        None => return Err(EngErr::Fatal("无法解析 OpenList 路径".into())),
    };
    log_line(&task, format!("解析路径 {}", path));
    let info = ol_post(&base, "/api/fs/get", json!({"path": path}), Some(&token))?;
    let raw = info.get("raw_url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if raw.is_empty() {
        return Err(EngErr::Fatal("OpenList 未返回直链".into()));
    }
    let raw = if raw.starts_with('/') { format!("{}{}", base, raw) } else { raw };
    let name = info.get("name").and_then(|v| v.as_str()).unwrap_or("download.bin").to_string();

    let headers = default_headers();
    let mut pr = probe(&raw, &headers)?;
    pr.name = sanitize_name(&name);
    if pr.ranges {
        run_turbo(task.clone(), &pr.url, &headers, &pr, dir, conn)
    } else {
        run_aria2_uri(task, root, &raw, dir, &headers, conn, false)
    }
}

pub fn default_headers() -> BTreeMap<String, String> {
    let mut h = BTreeMap::new();
    h.insert("User-Agent".into(), DEFAULT_UA.into());
    h
}

// ---------------- 任务调度 ----------------
pub fn spawn(root: PathBuf, task: Shared, dir: PathBuf, conn: usize, engine: String, ol_path: Option<String>) {
    spawn_ext(root, task, dir, conn, engine, ol_path, BTreeMap::new())
}

/// 与 spawn 相同，但允许附带页面解析得到的 Referer / Cookie 等请求头
pub fn spawn_ext(
    root: PathBuf,
    task: Shared,
    dir: PathBuf,
    conn: usize,
    engine: String,
    ol_path: Option<String>,
    extra: BTreeMap<String, String>,
) {
    thread::spawn(move || {
        if let Ok(mut t) = task.lock() {
            t.status = Status::Running;
            t.message = "正在识别…".into();
            t.started = Instant::now();
        }
        let (url, kind) = {
            let t = task.lock().unwrap();
            (t.url.clone(), t.kind)
        };
        let with_extra = |mut h: BTreeMap<String, String>| {
            for (k, v) in &extra {
                h.insert(k.clone(), v.clone());
            }
            h
        };

        let result: Result<PathBuf, EngErr> = match kind {
            Kind::Netdisk => Err(EngErr::Fatal(
                "网盘分享链接需要账号授权：请在 OpenList 挂载该网盘后，用「OpenList 路径」方式添加".into(),
            )),
            Kind::Torrent => {
                if let Ok(mut t) = task.lock() {
                    t.message = "aria2 BT 下载中…".into();
                }
                let is_file = url.to_lowercase().ends_with(".torrent");
                run_aria2_uri(task.clone(), &root, &url, &dir, &default_headers(), conn, is_file)
            }
            Kind::Media => {
                if let Ok(mut t) = task.lock() {
                    t.message = "yt-dlp 解析中…".into();
                }
                let headers = with_extra(default_headers());
                run_media(task.clone(), &root, &url, &dir, &headers, conn)
            }
            Kind::OpenList => run_openlist(task.clone(), &root, &url, &dir, conn, ol_path),
            Kind::Http => {
                let mut headers = with_extra(default_headers());
                let prefer_aria2 = engine == "aria2";
                if prefer_aria2 {
                    run_aria2_uri(task.clone(), &root, &url, &dir, &headers, conn, false)
                } else {
                    match probe(&url, &headers) {
                        Ok(pr) => {
                            if let Some(ua) = &pr.ua {
                                headers.insert("User-Agent".into(), ua.clone());
                                log_line(&task, "该站仅对下载器 UA 开放分片，已切换 UA".into());
                            }
                            if let Ok(mut t) = task.lock() {
                                t.total = pr.size;
                                if t.name.is_empty() || t.name == "解析中…" {
                                    t.name = pr.name.clone();
                                }
                                t.message = format!("{} 路分片下载中…", conn);
                            }
                            if pr.ranges {
                                match run_turbo(task.clone(), &pr.url, &headers, &pr, &dir, conn) {
                                    Ok(p) => Ok(p),
                                    Err(EngErr::Soft(m)) => {
                                        log_line(&task, format!("{} → 改用单连接直下", m));
                                        run_single_stream(
                                            task.clone(),
                                            &pr.url,
                                            &pr.name,
                                            &dir,
                                            &headers,
                                            pr.size,
                                        )
                                    }
                                    Err(e) => Err(e),
                                }
                            } else {
                                log_line(
                                    &task,
                                    "目标不支持 Range（无法分片），改用单连接直下".into(),
                                );
                                run_single_stream(
                                    task.clone(),
                                    &pr.url,
                                    &pr.name,
                                    &dir,
                                    &headers,
                                    pr.size,
                                )
                            }
                        }
                        Err(EngErr::Fatal(m)) => Err(EngErr::Fatal(m)),
                        Err(EngErr::Soft(m)) => {
                            log_line(&task, format!("{} → 改用单连接直下", m));
                            let name = filename_from_url(&url);
                            run_single_stream(task.clone(), &url, &name, &dir, &headers, 0)
                        }
                    }
                }
            }
        };

        if let Ok(mut t) = task.lock() {
            t.speed = 0.0;
            t.net_speed = 0.0;
            t.disk_speed = 0.0;
            match result {
                Ok(path) => {
                    t.status = Status::Done;
                    t.message = "已完成".into();
                    t.output = Some(path);
                    if t.total == 0 {
                        t.total = t.done;
                    }
                    t.done = t.total;
                }
                Err(EngErr::Fatal(m)) => {
                    t.status = Status::Failed;
                    t.message = m;
                }
                Err(EngErr::Soft(m)) => {
                    if t.cancel.load(Ordering::SeqCst) {
                        t.status = Status::Cancelled;
                        t.message = "已取消".into();
                    } else {
                        t.status = Status::Failed;
                        t.message = m;
                    }
                }
            }
        }
    });
}
