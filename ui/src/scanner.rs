//! 下载页解析：抓取页面 → 提取链接 → 跟随重定向探测 → 输出可下载候选。
//!
//! 只做静态 HTML 提取（不执行 JS），覆盖常见下载站：
//! `<a href>`、`<video/audio/source/iframe src>`、`<meta og:video>`、`<link rel=enclosure>`，
//! 以及内联 JSON / `__NEXT_DATA__` 里的 URL。
//! 第 0 层是用户给的页面，第 1 层只跟进同源且看起来像下载页的链接。

use crate::engine;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const MAX_HTML: usize = 4 << 20;
const MAX_LEVEL1_PAGES: usize = 8;
const MAX_CANDIDATES: usize = 120;
const PROBE_WORKERS: usize = 8;
const SCAN_BUDGET: Duration = Duration::from_secs(45);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CandKind {
    File,
    Media,
}

#[derive(Clone)]
pub struct Candidate {
    pub title: String,
    pub page: String,
    /// 原始链接（不存最终地址：预签名链接会过期，下载时重新跟随 302）
    pub url: String,
    pub size: u64,
    pub kind: CandKind,
    pub type_label: String,
    pub note: String,
    pub checked: bool,
    /// 页面中的出现顺序，用于保持「最新版本在前」的直觉排序
    pub order: u32,
}

pub struct ScanState {
    pub running: bool,
    pub message: String,
    pub pages_done: usize,
    pub candidates: Vec<Candidate>,
    pub error: String,
    /// 抓页面时得到的 Cookie / Referer，下载时一并带上
    pub headers: BTreeMap<String, String>,
    pub cancel: Arc<AtomicBool>,
}

pub type ScanShared = Arc<Mutex<ScanState>>;

pub fn start(page_url: String) -> ScanShared {
    let shared: ScanShared = Arc::new(Mutex::new(ScanState {
        running: true,
        message: "正在打开页面…".into(),
        pages_done: 0,
        candidates: Vec::new(),
        error: String::new(),
        headers: BTreeMap::new(),
        cancel: Arc::new(AtomicBool::new(false)),
    }));
    let s = shared.clone();
    thread::spawn(move || run(s, page_url));
    shared
}

pub fn stop(shared: &ScanShared) {
    if let Ok(st) = shared.lock() {
        st.cancel.store(true, Ordering::SeqCst);
    }
}

fn set_msg(shared: &ScanShared, msg: impl Into<String>) {
    if let Ok(mut st) = shared.lock() {
        st.message = msg.into();
    }
}

fn cancelled(shared: &ScanShared) -> bool {
    shared
        .lock()
        .map(|st| st.cancel.load(Ordering::SeqCst))
        .unwrap_or(true)
}

fn run(shared: ScanShared, page_url: String) {
    let jar = std::env::temp_dir().join(format!("dlkit-scan-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&jar);
    let result = scan(&shared, &page_url, &jar);
    let cookie = read_cookie_jar(&jar);
    let _ = std::fs::remove_file(&jar);

    if let Ok(mut st) = shared.lock() {
        st.running = false;
        match result {
            Ok(mut list) => {
                list.sort_by_key(|c| (c.kind != CandKind::File, c.order));
                list.truncate(MAX_CANDIDATES);
                if list.is_empty() && st.error.is_empty() {
                    st.error = "页面里没有找到可直接下载的文件（可能是 JS 动态加载，或需要登录）".into();
                }
                st.candidates = list;
                st.message = format!("解析完成，共 {} 个可下载文件", st.candidates.len());
                let mut h = BTreeMap::new();
                h.insert("Referer".into(), page_url.clone());
                if !cookie.is_empty() {
                    h.insert("Cookie".into(), cookie);
                }
                st.headers = h;
            }
            Err(e) => {
                if !st.cancel.load(Ordering::SeqCst) {
                    st.error = e;
                }
                st.message = "解析中断".into();
            }
        }
    }
}

// ---------------- 抓页面 ----------------

struct Page {
    html: String,
    final_url: String,
    content_type: String,
}

fn fetch_page(url: &str, jar: &Path) -> Result<Page, String> {
    let args: Vec<String> = vec![
        "-sS".into(),
        "-L".into(),
        "--http1.1".into(),
        "--compressed".into(),
        "--connect-timeout".into(),
        "10".into(),
        "--max-time".into(),
        "20".into(),
        "--max-filesize".into(),
        MAX_HTML.to_string(),
        "-A".into(),
        UA.into(),
        "-H".into(),
        "Accept: text/html,application/xhtml+xml,application/json,*/*;q=0.8".into(),
        "-H".into(),
        "Accept-Language: zh-CN,zh;q=0.9,en;q=0.8".into(),
        "-c".into(),
        jar.to_string_lossy().to_string(),
        "-b".into(),
        jar.to_string_lossy().to_string(),
        "-w".into(),
        "\n__DLKIT_META__%{content_type}|%{url_effective}".into(),
        url.to_string(),
    ];
    let (raw_out, err, _code) = engine::curl_bytes(&args)?;
    const MARK: &[u8] = b"__DLKIT_META__";
    let (body_bytes, meta) = match raw_out.windows(MARK.len()).rposition(|w| w == MARK) {
        Some(i) => (
            &raw_out[..i],
            String::from_utf8_lossy(&raw_out[i + MARK.len()..])
                .trim()
                .to_string(),
        ),
        None => (&raw_out[..], String::new()),
    };
    if body_bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Err(if err.trim().is_empty() {
            format!("页面没有返回内容：{}", url)
        } else {
            err.trim().lines().next().unwrap_or("抓取失败").to_string()
        });
    }
    let mut it = meta.split('|');
    let content_type = it.next().unwrap_or("").trim().to_lowercase();
    let final_url = it.next().unwrap_or(url).trim().to_string();
    let html = decode_body(body_bytes, &content_type);
    Ok(Page {
        html,
        final_url,
        content_type,
    })
}

fn decode_body(bytes: &[u8], content_type: &str) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    if content_type.contains("gbk")
        || content_type.contains("gb2312")
        || content_type.contains("gb18030")
    {
        return decode_gbk(bytes);
    }
    String::from_utf8_lossy(bytes).to_string()
}

/// GBK 解码：调用系统码表转换，避免引入额外依赖。
#[cfg(windows)]
fn decode_gbk(bytes: &[u8]) -> String {
    extern "system" {
        fn MultiByteToWideChar(
            cp: u32,
            flags: u32,
            s: *const u8,
            cb: i32,
            out: *mut u16,
            cch: i32,
        ) -> i32;
        fn WideCharToMultiByte(
            cp: u32,
            flags: u32,
            s: *const u16,
            cch: i32,
            out: *mut u8,
            cb: i32,
            def: *const u8,
            used: *mut i32,
        ) -> i32;
    }
    const CP_GBK: u32 = 936;
    const CP_UTF8: u32 = 65001;
    unsafe {
        let n = MultiByteToWideChar(
            CP_GBK,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        );
        if n <= 0 {
            return String::from_utf8_lossy(bytes).to_string();
        }
        let mut wide = vec![0u16; n as usize];
        MultiByteToWideChar(
            CP_GBK,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            wide.as_mut_ptr(),
            n,
        );
        let m = WideCharToMultiByte(
            CP_UTF8,
            0,
            wide.as_ptr(),
            n,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null_mut(),
        );
        if m <= 0 {
            return String::from_utf8_lossy(bytes).to_string();
        }
        let mut utf8 = vec![0u8; m as usize];
        WideCharToMultiByte(
            CP_UTF8,
            0,
            wide.as_ptr(),
            n,
            utf8.as_mut_ptr(),
            m,
            std::ptr::null(),
            std::ptr::null_mut(),
        );
        String::from_utf8_lossy(&utf8).to_string()
    }
}

#[cfg(not(windows))]
fn decode_gbk(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn read_cookie_jar(path: &Path) -> String {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    let mut pairs: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') && !line.starts_with("#HttpOnly_") {
            continue;
        }
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() >= 7 {
            let name = cols[cols.len() - 2].trim();
            let value = cols[cols.len() - 1].trim();
            if !name.is_empty() {
                pairs.push(format!("{}={}", name, value));
            }
        }
    }
    pairs.join("; ")
}

// ---------------- 主流程 ----------------

struct Raw {
    url: String,
    title: String,
    page: String,
    order: u32,
}

fn scan(shared: &ScanShared, page_url: &str, jar: &Path) -> Result<Vec<Candidate>, String> {
    // 媒体站（B站/YouTube…）交给 yt-dlp，不必解析页面
    if engine::classify(page_url).0 == engine::Kind::Media {
        return Ok(vec![Candidate {
            title: format!("媒体链接 {}", engine::url_host(page_url).unwrap_or_default()),
            page: page_url.to_string(),
            url: page_url.to_string(),
            size: 0,
            kind: CandKind::Media,
            type_label: "视频".into(),
            note: "交给 yt-dlp 解析格式".into(),
            checked: true,
            order: 0,
        }]);
    }

    let host0 = engine::url_host(page_url).unwrap_or_default();
    let mut level1: Vec<String> = vec![page_url.to_string()];
    let mut seen_pages: HashSet<String> = HashSet::new();
    let mut raw: Vec<Raw> = Vec::new();
    let deadline = Instant::now() + SCAN_BUDGET;

    for depth in 0..2 {
        let mut next: Vec<String> = Vec::new();
        for url in std::mem::take(&mut level1) {
            if Instant::now() > deadline || cancelled(shared) {
                break;
            }
            let key = url.split('#').next().unwrap_or(&url).to_string();
            if !seen_pages.insert(key) {
                continue;
            }
            set_msg(shared, format!("正在解析第 {} 层页面：{}", depth, short_url(&url)));
            let page = match fetch_page(&url, jar) {
                Ok(p) => p,
                Err(e) => {
                    if depth == 0 {
                        return Err(e);
                    }
                    continue;
                }
            };
            if let Ok(mut st) = shared.lock() {
                st.pages_done += 1;
            }

            // 直接给出的是文件（不是 HTML / JSON 页面）
            let ct = &page.content_type;
            let is_doc = ct.is_empty()
                || ct.contains("html")
                || ct.contains("text/")
                || ct.contains("json")
                || ct.contains("xml");
            if !is_doc {
                let order = raw.len() as u32;
                raw.push(Raw {
                    url: page.final_url.clone(),
                    title: String::new(),
                    page: url.clone(),
                    order,
                });
                continue;
            }

            for link in extract_links(&page.html) {
                let abs = match join_url(&page.final_url, &link.href) {
                    Some(u) => u,
                    None => continue,
                };
                if !abs.starts_with("http") {
                    continue;
                }
                let ext = ext_of(&abs);
                if is_media_ext(&ext) || is_file_ext(&ext) || looks_download(&abs, &link.text) {
                    let order = raw.len() as u32;
                    raw.push(Raw {
                        url: abs,
                        title: link.text,
                        page: page.final_url.clone(),
                        order,
                    });
                } else if depth == 0
                    && next.len() < MAX_LEVEL1_PAGES
                    && is_page_like(&ext)
                    && engine::url_host(&abs).unwrap_or_default() == host0
                    && looks_download(&abs, &link.text)
                {
                    next.push(abs);
                }
            }
        }
        if depth == 0 {
            level1 = next;
            if level1.is_empty() {
                break;
            }
        }
    }

    if cancelled(shared) {
        return Err("已取消".into());
    }
    if raw.is_empty() {
        return Ok(Vec::new());
    }

    set_msg(shared, format!("发现 {} 个链接，正在探测文件信息…", raw.len()));
    Ok(probe_all(shared, raw, jar, page_url))
}

fn short_url(u: &str) -> String {
    let s = u.split(['?', '#']).next().unwrap_or(u);
    let s = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    if s.chars().count() > 60 {
        format!("{}…", s.chars().take(60).collect::<String>())
    } else {
        s.to_string()
    }
}

// ---------------- 探测 ----------------

struct HeadInfo {
    status: u16,
    final_url: String,
    ctype: String,
    len: u64,
    filename: Option<String>,
}

fn probe_all(shared: &ScanShared, raw: Vec<Raw>, jar: &Path, referer: &str) -> Vec<Candidate> {
    // 先按 URL 去重
    let mut seen = HashSet::new();
    let mut list: VecDeque<Raw> = VecDeque::new();
    for r in raw {
        let key = r.url.split('#').next().unwrap_or(&r.url).to_string();
        if seen.insert(key) {
            list.push_back(r);
        }
    }
    let total = list.len();
    let queue = Arc::new(Mutex::new(list));
    let out: Arc<Mutex<Vec<Candidate>>> = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(Mutex::new(0usize));
    let jar = jar.to_path_buf();
    let referer = referer.to_string();

    let workers = PROBE_WORKERS.min(total.max(1));
    let mut handles = Vec::new();
    for _ in 0..workers {
        let queue = queue.clone();
        let out = out.clone();
        let done = done.clone();
        let jar = jar.clone();
        let referer = referer.clone();
        let shared2 = shared.clone();
        handles.push(thread::spawn(move || {
            loop {
                if cancelled(&shared2) {
                    break;
                }
                let item = match queue.lock().unwrap().pop_front() {
                    Some(x) => x,
                    None => break,
                };
                if let Some(c) = probe_one(&item, &jar, &referer) {
                    out.lock().unwrap().push(c);
                }
                let mut d = done.lock().unwrap();
                *d += 1;
                let now = *d;
                drop(d);
                set_msg(&shared2, format!("探测文件信息 {}/{}", now, total));
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    let mut v = Arc::try_unwrap(out).map(|m| m.into_inner().unwrap()).unwrap_or_default();
    // 同一最终地址只保留一条（重定向后的多个入口会指到同一个文件）
    let mut by_url: BTreeMap<String, Candidate> = BTreeMap::new();
    for c in v.drain(..) {
        let key = c.url.clone();
        match by_url.get(&key) {
            Some(old) if old.size >= c.size => {}
            _ => {
                by_url.insert(key, c);
            }
        }
    }
    by_url.into_values().collect()
}

fn probe_one(item: &Raw, jar: &Path, referer: &str) -> Option<Candidate> {
    let info = head(item, jar, referer)?;
    if matches!(info.status, 404 | 410) {
        return None;
    }
    let ctype = info.ctype.to_lowercase();
    // 它是网页，不是文件
    if ctype.starts_with("text/html") || ctype.starts_with("application/xhtml") {
        return None;
    }
    let auth = matches!(info.status, 401 | 403);
    if !auth && info.len == 0 && ctype.is_empty() && info.filename.is_none() {
        return None;
    }

    let ext = ext_of(&item.url);
    let ext_name = ext_of(&info.final_url);
    // 优先用原始链接的扩展名：GitHub 的最终地址常是 /refs/tags/v1.19.3，
    // 会被误当成 ".3"；原始链接的 .zip / .tar.gz 才是用户看到的真实类型
    let label_ext = if !ext.is_empty() { &ext } else { &ext_name };
    let kind = if is_media_ext(&ext) || is_media_ext(&ext_name) {
        CandKind::Media
    } else {
        CandKind::File
    };

    let url_name = {
        let n = file_name_from_url(&info.final_url);
        if n.is_empty() {
            file_name_from_url(&item.url)
        } else {
            n
        }
    };
    let title = pick_title(&item.title, info.filename.as_deref(), &url_name);
    let url = item.url.clone();
    let note = if auth {
        format!("HTTP {}，可能需要登录或防盗链", info.status)
    } else if info.len == 0 {
        "大小未知".to_string()
    } else {
        String::new()
    };

    Some(Candidate {
        title,
        page: item.page.clone(),
        url,
        size: info.len,
        kind,
        type_label: type_label(&ctype, label_ext).to_string(),
        note,
        checked: true,
        order: item.order,
    })
}

fn head(item: &Raw, jar: &Path, referer: &str) -> Option<HeadInfo> {
    let a: Vec<String> = vec![
        "-sS".into(),
        "-I".into(),
        "-L".into(),
        "--http1.1".into(),
        "--connect-timeout".into(),
        "8".into(),
        "--max-time".into(),
        "15".into(),
        "-A".into(),
        UA.into(),
        "-H".into(),
        "Accept: */*".into(),
        "-e".into(),
        referer.to_string(),
        "-c".into(),
        jar.to_string_lossy().to_string(),
        "-b".into(),
        jar.to_string_lossy().to_string(),
        "-w".into(),
        "\n__DLKIT_META__%{http_code}|%{url_effective}".into(),
        item.url.clone(),
    ];
    let (out, _err, _code) = engine::curl_exe(&a).ok()?;
    let (head_text, meta) = match out.rsplit_once("__DLKIT_META__") {
        Some((h, m)) => (h.to_string(), m.trim().to_string()),
        None => (out, String::new()),
    };
    let mut mp = meta.split('|');
    let status = mp.next().and_then(|s| s.trim().parse::<u16>().ok()).unwrap_or(0);
    let final_url = mp.next().unwrap_or(&item.url).trim().to_string();

    let mut info = parse_head(&head_text, status, final_url);

    // HEAD 被拒或不给长度时，用 2 字节 Range 再探一次
    let need_probe = info.status >= 400 || info.len == 0 || info.ctype.is_empty();
    if need_probe {
        let a: Vec<String> = vec![
            "-sS".into(),
            "-L".into(),
            "--http1.1".into(),
            "--connect-timeout".into(),
            "8".into(),
            "--max-time".into(),
            "15".into(),
            "-A".into(),
            UA.into(),
            "-o".into(),
            "NUL".into(),
            "-D".into(),
            "-".into(),
            "-r".into(),
            "0-1".into(),
            "-e".into(),
            referer.to_string(),
            "-c".into(),
            jar.to_string_lossy().to_string(),
            "-b".into(),
            jar.to_string_lossy().to_string(),
            "-w".into(),
            "\n__DLKIT_META__%{http_code}|%{url_effective}".into(),
            item.url.clone(),
        ];
        if let Ok((out2, _e2, _c2)) = engine::curl_exe(&a) {
            let (h2, m2) = match out2.rsplit_once("__DLKIT_META__") {
                Some((h, m)) => (h.to_string(), m.trim().to_string()),
                None => (out2, String::new()),
            };
            let mut mp = m2.split('|');
            let st2 = mp.next().and_then(|s| s.trim().parse::<u16>().ok()).unwrap_or(0);
            let eff2 = mp.next().unwrap_or(&item.url).trim().to_string();
            let info2 = parse_head(&h2, st2, eff2);
            if info2.status > 0 && (info.status == 0 || info.status >= 400) {
                info.status = info2.status;
            }
            if info.len == 0 {
                info.len = info2.len;
            }
            if info.ctype.is_empty() {
                info.ctype = info2.ctype;
            }
            if info.filename.is_none() {
                info.filename = info2.filename;
            }
            if info.final_url.is_empty() {
                info.final_url = info2.final_url;
            }
        }
    }
    Some(info)
}

fn parse_head(text: &str, status: u16, final_url: String) -> HeadInfo {
    // 取最后一个响应头块（跟随重定向后可能有多组）
    let blocks: Vec<&str> = text.split("\r\n\r\n").filter(|b| !b.trim().is_empty()).collect();
    let block = blocks.last().copied().unwrap_or(text);
    let mut ctype = String::new();
    let mut len = 0u64;
    let mut filename = None;
    for line in block.lines() {
        let (k, v) = match line.split_once(':') {
            Some(x) => x,
            None => continue,
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        match k.as_str() {
            "content-type" => ctype = v.split(';').next().unwrap_or(v).trim().to_string(),
            "content-length" => len = v.parse().unwrap_or(0),
            "content-range" => {
                if let Some(total) = v.rsplit('/').next().and_then(|s| s.trim().parse::<u64>().ok()) {
                    if total > 0 {
                        len = total;
                    }
                }
            }
            "content-disposition" => {
                filename = v.split(';').find_map(|seg| {
                    let seg = seg.trim();
                    let raw = seg
                        .strip_prefix("filename*=")
                        .or_else(|| seg.strip_prefix("filename="))?;
                    let raw = raw.trim().trim_matches('"');
                    let raw = match raw.split_once("''") {
                        Some((_cs, rest)) => rest.to_string(),
                        None => raw.to_string(),
                    };
                    Some(engine::decode_percent(&raw))
                });
            }
            _ => {}
        }
    }
    HeadInfo {
        status,
        final_url,
        ctype,
        len,
        filename,
    }
}

// ---------------- 链接提取 ----------------

struct Link {
    href: String,
    text: String,
}

fn extract_links(html: &str) -> Vec<Link> {
    let bytes = html.as_bytes();
    let mut out: Vec<Link> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let end = match html[i..].find('>') {
            Some(k) => i + k,
            None => break,
        };
        let inner = &html[i + 1..end];
        let trimmed = inner.trim_start_matches(['/', '!', '?']);
        let tag: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        let attrs = parse_attrs(inner);

        // 跳过 script / style 的内容，避免把 JS 里的字符串当标签
        if tag == "script" || tag == "style" {
            let close = format!("</{}", tag);
            match find_ci(&html[end..], &close) {
                Some(k) => {
                    let body = &html[end + 1..end + k];
                    if tag == "script" {
                        for raw in inline_urls(body) {
                            out.push(Link {
                                href: raw,
                                text: String::new(),
                            });
                        }
                    }
                    // 跳过整个结束标签
                    let close_start = end + k;
                    i = html[close_start..]
                        .find('>')
                        .map(|x| close_start + x + 1)
                        .unwrap_or(html.len());
                    continue;
                }
                None => break,
            }
        }

        match tag.as_str() {
            "a" | "area" => {
                if let Some(href) = attrs.get("href") {
                    let text = text_after(html, end + 1);
                    out.push(Link {
                        href: href.clone(),
                        text,
                    });
                }
            }
            "img" | "video" | "audio" | "source" | "embed" | "iframe" | "track" => {
                if let Some(src) = attrs.get("src") {
                    out.push(Link {
                        href: src.clone(),
                        text: String::new(),
                    });
                }
                if let Some(srcset) = attrs.get("srcset") {
                    for part in srcset.split(',') {
                        if let Some(u) = part.trim().split_whitespace().next() {
                            out.push(Link {
                                href: u.to_string(),
                                text: String::new(),
                            });
                        }
                    }
                }
            }
            "meta" => {
                let key = attrs
                    .get("property")
                    .or_else(|| attrs.get("name"))
                    .map(|s| s.to_ascii_lowercase())
                    .unwrap_or_default();
                let wanted = key.contains("og:video")
                    || key.contains("og:audio")
                    || key.contains("twitter:player:stream")
                    || key == "download";
                if wanted {
                    if let Some(c) = attrs.get("content") {
                        out.push(Link {
                            href: c.clone(),
                            text: String::new(),
                        });
                    }
                }
            }
            "link" => {
                let rel = attrs.get("rel").map(|s| s.to_ascii_lowercase()).unwrap_or_default();
                if rel.contains("enclosure") || rel.contains("alternate") {
                    if let Some(href) = attrs.get("href") {
                        out.push(Link {
                            href: href.clone(),
                            text: String::new(),
                        });
                    }
                }
            }
            _ => {}
        }
        i = end + 1;
    }
    out
}

/// 从内联 JSON / JS 里挖出文件直链（Next.js、Nuxt、自写播放器常见）
fn inline_urls(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i + 7 < bytes.len() {
        let is_http = bytes[i..].starts_with(b"https://") || bytes[i..].starts_with(b"http://");
        if !is_http {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < bytes.len() {
            let b = bytes[j];
            if b >= 0x80 {
                break;
            }
            if matches!(
                b,
                b'"' | b'\'' | b' ' | b')' | b']' | b'}' | b',' | b'\n' | b'\r' | b'\t' | b'<' | b'>'
            ) {
                break;
            }
            if b == b'\\' {
                if j + 1 < bytes.len() && bytes[j + 1] == b'/' {
                    j += 2;
                    continue;
                }
                break;
            }
            j += 1;
        }
        let raw = body[i..j].replace("\\/", "/");
        if raw.len() > 12 {
            out.push(raw);
        }
        i = j.max(i + 1);
    }
    out
}

fn text_after(html: &str, start: usize) -> String {
    if start >= html.len() {
        return String::new();
    }
    let tail = &html[start..];
    let mut limit = tail.len().min(800);
    while limit > 0 && !tail.is_char_boundary(limit) {
        limit -= 1;
    }
    let end = find_ci(&tail[..limit], "</a").unwrap_or(limit);
    let raw = &tail[..end];
    // 去掉内部标签
    let mut cleaned = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => cleaned.push(c),
            _ => {}
        }
    }
    let t = decode_entities(&cleaned);
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    t.chars().take(160).collect()
}

fn parse_attrs(s: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // 跳过标签名与空白
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let ks = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b':') {
            i += 1;
        }
        if ks == i {
            i += 1;
            continue;
        }
        let key = s[ks..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let val = if bytes[i] == b'"' || bytes[i] == b'\'' {
            let q = bytes[i];
            i += 1;
            let vs = i;
            while i < bytes.len() && bytes[i] != q {
                i += 1;
            }
            let v = s[vs..i].to_string();
            i += 1;
            v
        } else {
            let vs = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                i += 1;
            }
            s[vs..i].to_string()
        };
        out.entry(key).or_insert(val);
    }
    out
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let h = hay.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    for i in 0..=h.len() - n.len() {
        if h[i..i + n.len()].eq_ignore_ascii_case(n) {
            return Some(i);
        }
    }
    None
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

// ---------------- URL 处理 ----------------

fn join_url(base: &str, href: &str) -> Option<String> {
    let href = decode_entities(href.trim());
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let low = href.to_ascii_lowercase();
    for bad in ["javascript:", "mailto:", "tel:", "data:", "blob:", "about:"] {
        if low.starts_with(bad) {
            return None;
        }
    }
    if low.starts_with("http://") || low.starts_with("https://") {
        return Some(href);
    }
    let (scheme, rest) = base.split_once("://")?;
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin = &base[..scheme.len() + 3 + host_end];
    if href.starts_with("//") {
        return Some(format!("{}:{}", scheme, href));
    }
    if href.starts_with('/') {
        return Some(format!("{}{}", origin, href));
    }
    // 相对路径：以 base 的目录为基准
    let base_path = &base[scheme.len() + 3 + host_end..];
    let base_path = base_path.split(['?', '#']).next().unwrap_or("/");
    let dir = match base_path.rfind('/') {
        Some(i) => &base_path[..=i],
        None => "/",
    };
    Some(format!("{}{}", origin, normalize_path(&format!("{}{}", dir, href))))
}

fn normalize_path(path: &str) -> String {
    let (p, tail) = match path.split_once(['?', '#']) {
        Some((p, t)) => (p, Some(t)),
        None => (path, None),
    };
    let mut segs: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    let mut out = format!("/{}", segs.join("/"));
    if p.ends_with('/') && !out.ends_with('/') {
        out.push('/');
    }
    match tail {
        Some(t) => out.push_str(&format!("?{}", t)),
        None => {}
    }
    out
}

fn ext_of(url: &str) -> String {
    let p = url.split(['?', '#']).next().unwrap_or(url);
    let last = p.rsplit('/').next().unwrap_or("");
    match last.rsplit_once('.') {
        // 扩展名必须含字母：排除 /v1.19.3 这类被误判成 ".3" 的版本号
        Some((_, e))
            if (2..=8).contains(&e.len())
                && e.chars().all(|c| c.is_ascii_alphanumeric())
                && e.chars().any(|c| c.is_ascii_alphabetic()) =>
        {
            e.to_ascii_lowercase()
        }
        _ => String::new(),
    }
}

fn file_name_from_url(url: &str) -> String {
    let p = url.split(['?', '#']).next().unwrap_or(url);
    let last = p.rsplit('/').next().unwrap_or("").trim();
    if last.is_empty() {
        String::new()
    } else {
        engine::decode_percent(last)
    }
}

fn is_file_ext(ext: &str) -> bool {
    matches!(
        ext,
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "zst" | "exe" | "msi"
            | "msix" | "appx" | "dmg" | "pkg" | "deb" | "rpm" | "apk" | "ipa" | "iso" | "img"
            | "bin" | "pdf" | "epub" | "mobi" | "doc" | "docx" | "xls" | "xlsx" | "ppt"
            | "pptx" | "psd" | "ai" | "sketch" | "fig" | "ttf" | "otf" | "woff" | "woff2"
            | "jar" | "whl" | "crx" | "xpi" | "vsix" | "torrent"
    )
}

fn is_media_ext(ext: &str) -> bool {
    matches!(
        ext,
        "m3u8" | "mpd" | "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "ts" | "m4v" | "wmv"
            | "mp3" | "flac" | "wav" | "m4a" | "aac" | "ogg" | "opus" | "srt" | "ass"
    )
}

fn is_page_like(ext: &str) -> bool {
    ext.is_empty() || matches!(ext, "html" | "htm" | "php" | "asp" | "aspx" | "jsp" | "do" | "action")
}

fn looks_download(url: &str, text: &str) -> bool {
    let low = url.to_ascii_lowercase();
    let path = low.split(['?', '#']).next().unwrap_or(&low);
    if [
        "/download",
        "/dl/",
        "/get/",
        "/file/",
        "/files/",
        "download.php",
        "download.asp",
        "download.jsp",
        "getfile",
        "down.php",
    ]
    .iter()
    .any(|k| path.contains(k))
    {
        return true;
    }
    let t = text.to_ascii_lowercase();
    text.contains("下载") || t.contains("download") || text.contains("点击下载")
}

fn pick_title(text: &str, filename: Option<&str>, url_name: &str) -> String {
    if let Some(f) = filename {
        let f = f.trim();
        if !f.is_empty() {
            return truncate_chars(f, 120);
        }
    }
    let generic = ["下载", "立即下载", "点击下载", "download", "download now", "click here"];
    let t = text.trim();
    if !t.is_empty() && !generic.contains(&t.to_ascii_lowercase().as_str()) {
        return truncate_chars(t, 120);
    }
    if !url_name.is_empty() {
        return truncate_chars(url_name, 120);
    }
    "未命名文件".into()
}

fn truncate_chars(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    t.trim().to_string()
}

fn type_label(ctype: &str, ext: &str) -> &'static str {
    if is_media_ext(ext) {
        return "视频";
    }
    match ext {
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "zst" => "压缩包",
        "exe" | "msi" | "msix" | "appx" | "dmg" | "pkg" | "deb" | "rpm" | "apk" | "ipa" => "安装包",
        "iso" | "img" => "镜像",
        "pdf" | "epub" | "mobi" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" => "文档",
        "ttf" | "otf" | "woff" | "woff2" => "字体",
        "" => {
            if ctype.contains("zip") || ctype.contains("compressed") {
                "压缩包"
            } else if ctype.contains("octet-stream") {
                "二进制"
            } else {
                "文件"
            }
        }
        _ => "文件",
    }
}
