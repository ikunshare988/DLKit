//! DLKit 桌面界面（Rust + egui）。
//! 视觉语言对齐 Sakura：天蓝主色、樱花粉点缀、浅色面板、大圆角、柔和描边与过渡。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod platform;
mod scanner;

use engine::{Shared, Status, Task};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, CornerRadius, Margin, RichText, Shadow, Stroke};

// ---------------- Sakura 设计令牌 ----------------
mod theme {
    use eframe::egui::Color32;

    pub const PRIMARY: Color32 = Color32::from_rgb(0x4b, 0x9a, 0xc4);
    pub const PRIMARY_HOVER: Color32 = Color32::from_rgb(0x3b, 0x83, 0xaa);
    pub const ACCENT: Color32 = Color32::from_rgb(0xe3, 0x6c, 0x96);
    pub const TEXT: Color32 = Color32::from_rgb(0x27, 0x44, 0x5a);
    pub const SECONDARY: Color32 = Color32::from_rgb(0x54, 0x76, 0x8b);
    pub const MUTED: Color32 = Color32::from_rgb(0x7d, 0x99, 0xa9);
    pub const PAGE: Color32 = Color32::from_rgb(0xf8, 0xfc, 0xfe);
    pub const PANEL: Color32 = Color32::from_rgb(0xea, 0xf5, 0xfa);
    pub const INPUT: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
    pub const BUBBLE: Color32 = Color32::from_rgb(0xe3, 0xf1, 0xf7);
    pub const BORDER: Color32 = Color32::from_rgb(0xac, 0xcf, 0xde);
    pub const TRACK: Color32 = Color32::from_rgb(0xdc, 0xeb, 0xf3);
    pub const SUCCESS: Color32 = Color32::from_rgb(0x2e, 0x9e, 0x68);
    pub const SUCCESS_BG: Color32 = Color32::from_rgb(0xe8, 0xf7, 0xee);
    pub const DANGER: Color32 = Color32::from_rgb(0xd9, 0x53, 0x6f);
    pub const DANGER_BG: Color32 = Color32::from_rgb(0xfd, 0xec, 0xef);

    pub fn tint(c: Color32, a: u8) -> Color32 {
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
    }
}

use theme::*;

fn soft_shadow() -> Shadow {
    Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_rgba_unmultiplied(54, 24, 43, 14),
    }
}

fn card_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(Color32::WHITE)
        .stroke(Stroke::new(1.0, tint(BORDER, 150)))
        .corner_radius(CornerRadius::same(18))
        .inner_margin(Margin::same(14))
        .shadow(soft_shadow())
}

struct Config {
    dir: PathBuf,
    conn: usize,
    engine: String,
    /// 全局限速（MB/s），0 表示不限
    limit: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            dir: engine::find_root().join("downloads"),
            conn: 32,
            engine: "auto".into(),
            limit: 0.0,
        }
    }
}

fn config_path() -> PathBuf {
    engine::find_root().join("config").join("ui.json")
}

fn load_config() -> Config {
    #[derive(serde::Deserialize)]
    struct Raw {
        dir: Option<String>,
        conn: Option<usize>,
        engine: Option<String>,
        limit: Option<f64>,
    }
    let mut cfg = Config::default();
    if let Ok(text) = std::fs::read_to_string(config_path()) {
        if let Ok(raw) = serde_json::from_str::<Raw>(&text) {
            if let Some(d) = raw.dir {
                cfg.dir = PathBuf::from(d);
            }
            if let Some(c) = raw.conn {
                cfg.conn = c.clamp(1, 64);
            }
            if let Some(e) = raw.engine {
                cfg.engine = e;
            }
            if let Some(l) = raw.limit {
                cfg.limit = l.clamp(0.0, 100.0);
            }
        }
    }
    cfg
}

fn save_config(cfg: &Config) {
    let v = serde_json::json!({
        "dir": cfg.dir.to_string_lossy(),
        "conn": cfg.conn,
        "engine": cfg.engine,
        "limit": cfg.limit
    });
    let _ = std::fs::write(config_path(), v.to_string());
}

struct App {
    url: String,
    ol_path: String,
    dir_text: String,
    cfg: Config,
    tasks: Vec<Shared>,
    selected: usize,
    toast: String,
    toast_at: Instant,
    root: PathBuf,
    svc_online: bool,
    svc_checked: Instant,
    scan: Option<scanner::ScanShared>,
    /// 用户点过「自己选」后，收起平台推荐横幅
    scan_hint_hidden: bool,
    /// 上一次看到的剪贴板内容，用来判断是否是新复制的链接
    clip_last: String,
    clip_at: Instant,
}

impl App {
    fn new() -> App {
        let cfg = load_config();
        engine::set_speed_limit(cfg.limit);
        let app = App {
            url: String::new(),
            ol_path: String::new(),
            dir_text: cfg.dir.to_string_lossy().to_string(),
            cfg,
            tasks: Vec::new(),
            selected: 0,
            toast: String::new(),
            toast_at: Instant::now() - Duration::from_secs(10),
            root: engine::find_root(),
            svc_online: false,
            svc_checked: Instant::now() - Duration::from_secs(10),
            scan: None,
            scan_hint_hidden: false,
            // 启动时已有的剪贴板内容不算「新复制」，避免一打开就自动填入
            clip_last: clipboard_text().unwrap_or_default(),
            clip_at: Instant::now(),
        };
        app
    }

    /// 解析下载页：把页面里的可下载文件列出来
    fn start_scan(&mut self) {
        let url = self.url.trim().to_string();
        if url.is_empty() {
            self.notify("请先粘贴下载页链接，再点「解析页面」");
            return;
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            self.notify("页面解析只支持 http/https 链接");
            return;
        }
        self.scan = Some(scanner::start(url));
        self.scan_hint_hidden = false;
        self.notify("正在解析页面…");
    }

    /// 把勾选的候选加入下载队列
    fn download_checked(&mut self) {
        let shared = match &self.scan {
            Some(s) => s.clone(),
            None => return,
        };
        let (picked, headers) = {
            let st = shared.lock().unwrap();
            let picked: Vec<scanner::Candidate> =
                st.candidates.iter().filter(|c| c.checked).cloned().collect();
            (picked, st.headers.clone())
        };
        if picked.is_empty() {
            self.notify("请先勾选要下载的文件");
            return;
        }
        let n = picked.len();
        for c in picked {
            let kind = match c.kind {
                scanner::CandKind::Media => engine::Kind::Media,
                scanner::CandKind::File => engine::Kind::Http,
            };
            // 优先用该文件所在页作为 Referer（深度探测时它可能不是入口页）
            let mut headers = headers.clone();
            if !c.page.is_empty() {
                headers.insert("Referer".into(), c.page.clone());
            }
            let mut task = Task::new(c.url.clone(), kind, c.title.clone());
            task.sha256 = c.sha256.clone();
            let t: Shared = Arc::new(Mutex::new(task));
            engine::spawn_ext(
                self.root.clone(),
                t.clone(),
                self.cfg.dir.clone(),
                self.cfg.conn,
                self.cfg.engine.clone(),
                None,
                headers,
            );
            self.tasks.push(t);
        }
        self.selected = self.tasks.len().saturating_sub(n);
        self.scan_hint_hidden = true;
        self.notify(format!("已加入 {} 个下载任务", n));
    }

    /// 渲染页面解析结果（进度 / 候选勾选 / 批量下载）
    fn draw_scan(&mut self, ui: &mut egui::Ui) {
        let shared = match &self.scan {
            Some(s) => s.clone(),
            None => return,
        };
        let (running, message, error, candidates, pages, recommended, recommend_reason) = {
            let st = match shared.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            (
                st.running,
                st.message.clone(),
                st.error.clone(),
                st.candidates.clone(),
                st.pages_done,
                st.recommended,
                st.recommend_reason.clone(),
            )
        };
        let hint_hidden = self.scan_hint_hidden;

        let mut close = false;
        let mut cancel = false;
        let mut toggles: Vec<(usize, bool)> = Vec::new();
        let mut select_all: Option<bool> = None;
        let mut download_now = false;
        let mut reveal_all = false;

        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("页面解析").strong().size(14.5).color(TEXT));
                if running {
                    pill(ui, "解析中", PRIMARY, tint(PRIMARY, 26));
                } else if !error.is_empty() {
                    pill(ui, "失败", DANGER, DANGER_BG);
                } else {
                    pill(
                        ui,
                        &format!("{} 个文件", candidates.len()),
                        SUCCESS,
                        SUCCESS_BG,
                    );
                }
                if pages > 0 {
                    ui.label(
                        RichText::new(format!("已扫描 {} 页", pages))
                            .size(11.0)
                            .color(MUTED),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                    if running && ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });

            ui.add_space(2.0);
            ui.label(RichText::new(&message).size(11.5).color(SECONDARY));
            if !error.is_empty() {
                ui.label(RichText::new(&error).size(11.5).color(DANGER));
            }

            if candidates.is_empty() {
                return;
            }

            // 平台推荐横幅：解析完成后一次性出现，点「开始下载」直接开，
            // 点「自己选」收起并恢复全选
            if let Some(idx) = recommended {
                if !hint_hidden && !running && error.is_empty() {
                    if let Some(c) = candidates.get(idx) {
                        ui.add_space(6.0);
                        egui::Frame::NONE
                            .fill(tint(PRIMARY, 20))
                            .stroke(Stroke::new(1.6, tint(PRIMARY, 210)))
                            .corner_radius(CornerRadius::same(14))
                            .inner_margin(Margin::symmetric(12, 10))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    pill(ui, "已按本机选中", PRIMARY, tint(PRIMARY, 52));
                                    ui.label(
                                        RichText::new(&recommend_reason)
                                            .size(11.0)
                                            .color(SECONDARY),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let btn = egui::Button::new(
                                                RichText::new("开始下载")
                                                    .size(12.5)
                                                    .color(Color32::WHITE),
                                            )
                                            .fill(PRIMARY)
                                            .corner_radius(CornerRadius::same(13));
                                            if ui.add(btn).clicked() {
                                                download_now = true;
                                            }
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        RichText::new("自己选")
                                                            .size(12.5)
                                                            .color(PRIMARY),
                                                    )
                                                    .fill(Color32::TRANSPARENT)
                                                    .corner_radius(CornerRadius::same(13)),
                                                )
                                                .clicked()
                                            {
                                                reveal_all = true;
                                            }
                                        },
                                    );
                                });
                                ui.add_space(2.0);
                                let detail = if c.size > 0 {
                                    format!("{}  ·  {}", c.title, fmt_size(c.size))
                                } else {
                                    c.title.clone()
                                };
                                ui.label(RichText::new(detail).size(14.0).strong().color(TEXT));
                            });
                        ui.add_space(6.0);
                    }
                }
            }

            ui.add_space(6.0);
            let checked_n = candidates.iter().filter(|c| c.checked).count();
            ui.horizontal(|ui| {
                if ui.button("全选").clicked() {
                    select_all = Some(true);
                }
                if ui.button("全不选").clicked() {
                    select_all = Some(false);
                }
                ui.add_space(6.0);
                let text = format!("下载选中 ({})", checked_n);
                let btn = egui::Button::new(RichText::new(text).size(12.5).color(Color32::WHITE))
                    .fill(if checked_n > 0 { PRIMARY } else { MUTED })
                    .corner_radius(CornerRadius::same(13));
                if ui.add(btn).clicked() {
                    download_now = true;
                }
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt("scan-list")
                .max_height(250.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (i, c) in candidates.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let mut checked = c.checked;
                            if ui.checkbox(&mut checked, "").changed() {
                                toggles.push((i, checked));
                            }
                            ui.label(RichText::new(&c.title).size(12.5).color(TEXT));
                            if Some(i) == recommended {
                                pill(ui, "推荐", SUCCESS, SUCCESS_BG);
                            }
                            pill(ui, &c.type_label, SECONDARY, PANEL);
                            if c.sha256.is_some() {
                                pill(ui, "SHA256", PRIMARY, tint(PRIMARY, 26));
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if c.size > 0 {
                                        ui.label(
                                            RichText::new(fmt_size(c.size)).size(11.0).color(MUTED),
                                        );
                                    } else {
                                        ui.label(
                                            RichText::new("大小未知").size(11.0).color(MUTED),
                                        );
                                    }
                                    if !c.note.is_empty() {
                                        ui.label(
                                            RichText::new(&c.note).size(10.5).color(ACCENT),
                                        );
                                    }
                                },
                            );
                        });
                    }
                });
        });

        if reveal_all {
            // 收起横幅，并把列表恢复成默认全选，让用户自己调整
            self.scan_hint_hidden = true;
            select_all = Some(true);
        }
        if let Ok(mut st) = shared.lock() {
            for (i, v) in toggles {
                if let Some(c) = st.candidates.get_mut(i) {
                    c.checked = v;
                }
            }
            if let Some(v) = select_all {
                for c in st.candidates.iter_mut() {
                    c.checked = v;
                }
            }
        }
        if cancel {
            scanner::stop(&shared);
        }
        if close {
            self.scan = None;
        }
        if download_now {
            self.download_checked();
        }
    }

    fn add_task(&mut self) {
        let raw = self.url.trim().to_string();
        let ol = self.ol_path.trim().to_string();
        if raw.is_empty() && ol.is_empty() {
            self.notify("请输入下载链接或 OpenList 路径");
            return;
        }

        // 一次粘贴多行链接：逐行入队，空行和 # 注释跳过
        if ol.is_empty() && raw.contains('\n') {
            let lines: Vec<String> = raw
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect();
            if lines.len() > 1 {
                let mut n = 0;
                for l in lines {
                    let (kind, _) = engine::classify(&l);
                    self.push_task(l, kind, None);
                    n += 1;
                }
                self.url.clear();
                self.notify(format!("已加入 {} 个任务", n));
                return;
            }
        }

        if !ol.is_empty() {
            let task_url = format!("openlist:{}", ol);
            self.push_task(task_url, engine::Kind::OpenList, Some(ol));
            self.ol_path.clear();
        } else {
            let (kind, _) = engine::classify(&raw);
            self.push_task(raw, kind, None);
        }
        self.url.clear();
        save_config(&self.cfg);
    }

    fn push_task(&mut self, url: String, kind: engine::Kind, ol: Option<String>) {
        let task = Task::new(url, kind, "解析中…".into());
        let shared: Shared = Arc::new(Mutex::new(task));
        engine::spawn(
            self.root.clone(),
            shared.clone(),
            self.cfg.dir.clone(),
            self.cfg.conn,
            self.cfg.engine.clone(),
            ol,
        );
        self.tasks.push(shared);
        self.selected = self.tasks.len() - 1;
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.toast = msg.into();
        self.toast_at = Instant::now();
    }

    /// 复制链接后自动填入输入框。只在新复制、且输入框没有被手动改过时覆盖。
    fn poll_clipboard(&mut self) {
        if self.clip_at.elapsed() < Duration::from_millis(700) {
            return;
        }
        self.clip_at = Instant::now();
        let text = match clipboard_text() {
            Some(t) => t,
            None => return,
        };
        let t = text.trim().to_string();
        if t == self.clip_last {
            return;
        }
        let prev = std::mem::replace(&mut self.clip_last, t.clone());
        if t.chars().count() > 2048 || t.contains('\n') {
            return;
        }
        let is_link = t.starts_with("http://") || t.starts_with("https://") || t.starts_with("magnet:");
        if !is_link {
            return;
        }
        if self.url.trim().is_empty() || self.url == prev {
            self.url = t;
            self.notify("已从剪贴板填入链接");
        }
    }

    /// 把拖进窗口的 .torrent 或链接直接加入/填入
    fn take_dropped(&mut self, ui: &egui::Ui) {
        let dropped: Vec<String> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_string_lossy().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        });
        for d in dropped {
            let low = d.to_ascii_lowercase();
            if low.ends_with(".torrent") {
                self.url = d;
                self.add_task();
            } else if low.starts_with("http://")
                || low.starts_with("https://")
                || low.starts_with("magnet:")
            {
                self.url = d;
                self.notify("已填入拖入的链接");
            }
        }
    }
}

/// 读取 Windows 剪贴板里的文本
#[cfg(windows)]
fn clipboard_text() -> Option<String> {
    extern "system" {
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn OpenClipboard(hwnd: isize) -> i32;
        fn CloseClipboard() -> i32;
        fn GetClipboardData(format: u32) -> isize;
        fn GlobalLock(mem: isize) -> *const u16;
        fn GlobalUnlock(mem: isize) -> i32;
        fn lstrlenW(s: *const u16) -> i32;
    }
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
            return None;
        }
        if OpenClipboard(0) == 0 {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle == 0 {
            CloseClipboard();
            return None;
        }
        let p = GlobalLock(handle);
        let text = if p.is_null() {
            None
        } else {
            let n = lstrlenW(p);
            let slice = std::slice::from_raw_parts(p, n.max(0) as usize);
            let s = String::from_utf16_lossy(slice);
            GlobalUnlock(handle);
            Some(s)
        };
        CloseClipboard();
        text
    }
}

#[cfg(not(windows))]
fn clipboard_text() -> Option<String> {
    None
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in [
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\simhei.ttf",
        "C:\\Windows\\Fonts\\Deng.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("cjk".to_owned(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "cjk".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("cjk".to_owned());
            break;
        }
    }
    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Light);
    let mut style = (*ctx.global_style()).clone();
    style.animation_time = 0.18;
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 5.0);

    style.text_styles = [
        (egui::TextStyle::Heading, egui::FontId::proportional(20.0)),
        (egui::TextStyle::Body, egui::FontId::proportional(14.5)),
        (egui::TextStyle::Button, egui::FontId::proportional(14.0)),
        (egui::TextStyle::Small, egui::FontId::proportional(12.0)),
        (egui::TextStyle::Monospace, egui::FontId::monospace(12.0)),
    ]
    .into();

    let mut v = egui::Visuals::light();
    v.panel_fill = PAGE;
    v.window_fill = PAGE;
    v.extreme_bg_color = INPUT;
    v.faint_bg_color = PANEL;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = tint(PRIMARY, 46);
    v.selection.stroke = Stroke::new(1.0, PRIMARY);
    v.hyperlink_color = PRIMARY;
    v.window_shadow = soft_shadow();
    v.popup_shadow = soft_shadow();

    let rounded = CornerRadius::same(14);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = rounded;
    }
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.corner_radius = CornerRadius::same(10);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, tint(BORDER, 120));

    v.widgets.inactive.bg_fill = Color32::from_rgb(0xf2, 0xf9, 0xfd);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0xf2, 0xf9, 0xfd);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, tint(BORDER, 150));
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);

    v.widgets.hovered.bg_fill = tint(PRIMARY, 30);
    v.widgets.hovered.weak_bg_fill = tint(PRIMARY, 26);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, tint(PRIMARY, 170));
    v.widgets.hovered.fg_stroke = Stroke::new(1.2, PRIMARY_HOVER);

    v.widgets.active.bg_fill = tint(PRIMARY, 46);
    v.widgets.active.weak_bg_fill = tint(PRIMARY, 40);
    v.widgets.active.bg_stroke = Stroke::new(1.0, PRIMARY);
    v.widgets.active.fg_stroke = Stroke::new(1.4, PRIMARY_HOVER);

    style.visuals = v;
    ctx.set_style_of(egui::Theme::Light, style);
}

fn fmt_size(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.2} {}", v, U[i])
    }
}

fn fmt_speed(mbps: f64) -> String {
    if mbps <= 0.0 {
        "--".into()
    } else {
        format!("{:.2} MB/s", mbps)
    }
}

fn fmt_eta(done: u64, total: u64, mbps: f64) -> String {
    if total == 0 || mbps <= 0.01 || done >= total {
        return "--".into();
    }
    let sec = ((total - done) as f64 / 1048576.0 / mbps) as u64;
    if sec < 60 {
        format!("{}s", sec)
    } else if sec < 3600 {
        format!("{}m{:02}s", sec / 60, sec % 60)
    } else {
        format!("{}h{:02}m", sec / 3600, (sec % 3600) / 60)
    }
}

struct Snap {
    name: String,
    kind: engine::Kind,
    status: Status,
    message: String,
    done: u64,
    total: u64,
    speed: f64,
    net_speed: f64,
    disk_speed: f64,
    log: Vec<String>,
    output: Option<PathBuf>,
}

fn snapshot(task: &Shared) -> Option<Snap> {
    let t = task.lock().ok()?;
    Some(Snap {
        name: t.name.clone(),
        kind: t.kind,
        status: t.status,
        message: t.message.clone(),
        done: t.done,
        total: t.total,
        speed: t.speed,
        net_speed: t.net_speed,
        disk_speed: t.disk_speed,
        log: t.log.clone(),
        output: t.output.clone(),
    })
}

/// 网络 / 磁盘速度文案（磁盘为 0 表示该引擎无法拆分阶段）
fn io_text(net: f64, disk: f64) -> String {
    if disk > 0.0 {
        format!("网络 {} · 磁盘 {}", fmt_speed(net), fmt_speed(disk))
    } else {
        format!("网络 {}", fmt_speed(net))
    }
}

/// Sakura 风格的胶囊标签
fn pill(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) {
    egui::Frame::NONE
        .fill(bg)
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.5).color(fg));
        });
}

/// 圆角进度条（自绘，贴合 Sakura 的柔和风格）
fn progress_bar(ui: &mut egui::Ui, frac: f32) -> egui::Response {
    let height = 10.0;
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let radius = CornerRadius::same(5);
    ui.painter().rect_filled(rect, radius, TRACK);
    let frac = frac.clamp(0.0, 1.0);
    if frac > 0.0 {
        let w = (rect.width() * frac).max(height);
        let fill = egui::Rect::from_min_size(rect.min, egui::vec2(w, height));
        ui.painter().rect_filled(fill, radius, PRIMARY);
        // 顶部高光，营造轻微玻璃感
        let gloss = egui::Rect::from_min_size(
            egui::pos2(fill.left(), fill.top()),
            egui::vec2(fill.width(), height * 0.42),
        );
        ui.painter()
            .rect_filled(gloss, CornerRadius::same(5), tint(Color32::WHITE, 42));
    }
    resp
}

/// 圆形主按钮（Sakura 的发送键样式）
fn circle_button(ui: &mut egui::Ui, glyph: &str) -> egui::Response {
    let size = egui::vec2(34.0, 34.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let bg = if resp.clicked() {
        tint(PRIMARY, 66)
    } else if resp.hovered() {
        tint(PRIMARY, 48)
    } else {
        tint(PRIMARY, 26)
    };
    ui.painter().circle_filled(rect.center(), size.x * 0.5, bg);
    if resp.hovered() {
        ui.painter().circle_stroke(
            rect.center(),
            size.x * 0.5 - 0.5,
            Stroke::new(1.0, tint(ACCENT, 150)),
        );
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(19.0),
        PRIMARY,
    );
    resp
}

/// 胶囊次按钮（浅色描边，用于「解析页面」等次要动作）
fn pill_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).size(12.0).color(PRIMARY))
            .fill(tint(PRIMARY, 16))
            .stroke(Stroke::new(1.0, tint(PRIMARY, 95)))
            .corner_radius(CornerRadius::same(14))
            .min_size(egui::vec2(60.0, 27.0)),
    )
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().request_repaint_after(Duration::from_millis(80));

        self.poll_clipboard();
        self.take_dropped(ui);

        if self.svc_checked.elapsed() > Duration::from_secs(2) {
            self.svc_online = "127.0.0.1:6800"
                .parse::<std::net::SocketAddr>()
                .ok()
                .and_then(|addr| {
                    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).ok()
                })
                .is_some();
            self.svc_checked = Instant::now();
        }

        // ---------- 顶栏 ----------
        egui::Panel::top("header")
            .frame(
                egui::Frame::NONE
                    .fill(PAGE)
                    .inner_margin(Margin { left: 18, right: 18, top: 12, bottom: 8 }),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").size(11.0).color(ACCENT));
                    ui.label(
                        RichText::new("DLKit")
                            .size(21.0)
                            .strong()
                            .color(PRIMARY),
                    );
                    ui.add_space(2.0);
                    ui.label(RichText::new("多功能下载器").size(12.5).color(SECONDARY));
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(format!("root: {}", self.root.display()))
                            .size(11.0)
                            .color(MUTED),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("打开下载目录").clicked() {
                            let _ = std::process::Command::new("explorer")
                                .arg(&self.cfg.dir)
                                .spawn();
                        }
                        if ui.button("全部清空已完成").clicked() {
                            self.tasks.retain(|t| {
                                t.lock()
                                    .map(|x| {
                                        !matches!(
                                            x.status,
                                            Status::Done | Status::Failed | Status::Cancelled
                                        )
                                    })
                                    .unwrap_or(true)
                            });
                            self.selected = 0;
                        }
                        if !self.svc_online && ui.button("启动服务").clicked() {
                            let script = self.root.join("start.ps1");
                            #[allow(unused_mut)]
                            let mut c = std::process::Command::new("powershell");
                            #[cfg(windows)]
                            {
                                use std::os::windows::process::CommandExt;
                                c.creation_flags(0x0800_0000);
                            }
                            let _ = c
                                .args(["-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
                                .arg(script)
                                .spawn();
                            self.notify("正在启动 aria2 / OpenList…");
                        }
                        let (txt, fg, bg) = if self.svc_online {
                            ("aria2 在线", SUCCESS, SUCCESS_BG)
                        } else {
                            ("aria2 离线", DANGER, DANGER_BG)
                        };
                        pill(ui, txt, fg, bg);
                    });
                });
                // 顶栏底部细分隔线
                let r = ui.max_rect();
                ui.painter().hline(
                    r.left()..=r.right(),
                    r.bottom() + 6.0,
                    Stroke::new(1.0, tint(BORDER, 110)),
                );
            });

        // ---------- 输入区（Sakura composer 卡片） ----------
        egui::Panel::top("input")
            .frame(
                egui::Frame::NONE
                    .fill(PAGE)
                    .inner_margin(Margin { left: 18, right: 18, top: 10, bottom: 10 }),
            )
            .show(ui, |ui| {
                egui::Frame::NONE
                    .fill(Color32::from_rgb(0xfb, 0xfe, 0xff))
                    .stroke(Stroke::new(1.0, tint(BORDER, 170)))
                    .corner_radius(CornerRadius::same(22))
                    .inner_margin(Margin::symmetric(14, 9))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.url)
                                    .frame(egui::Frame::NONE)
                                    .hint_text(
                                        RichText::new("粘贴直链 / 磁力 / 下载页链接 / OpenList…")
                                            .color(MUTED),
                                    )
                                    .desired_width(ui.available_width() - 132.0)
                                    .margin(egui::vec2(2.0, 5.0)),
                            );
                            let enter =
                                resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if pill_button(ui, "解析页面")
                                .on_hover_text("抓取页面并列出其中的可下载文件")
                                .clicked()
                            {
                                self.start_scan();
                            }
                            if circle_button(ui, "＋").on_hover_text("添加任务").clicked() || enter {
                                self.add_task();
                            }
                        });
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("并发").size(11.5).color(MUTED));
                            ui.scope(|ui| {
                                ui.spacing_mut().interact_size.y = 22.0;
                                ui.add(
                                    egui::DragValue::new(&mut self.cfg.conn)
                                        .range(1..=64)
                                        .suffix(" 路"),
                                );
                            });
                            ui.add_space(6.0);
                            ui.label(RichText::new("限速").size(11.5).color(MUTED));
                            ui.scope(|ui| {
                                ui.spacing_mut().interact_size.y = 22.0;
                                let resp = ui.add(
                                    egui::DragValue::new(&mut self.cfg.limit)
                                        .range(0.0..=100.0)
                                        .speed(0.5)
                                        .suffix(" MB/s"),
                                );
                                if resp.changed() {
                                    engine::set_speed_limit(self.cfg.limit);
                                    save_config(&self.cfg);
                                }
                                let _ = resp.on_hover_text("0 表示不限速；只作用于内置下载引擎");
                            });
                            ui.add_space(6.0);
                            ui.label(RichText::new("目录").size(11.5).color(MUTED));
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.dir_text)
                                        .desired_width(250.0)
                                        .margin(egui::vec2(6.0, 3.0)),
                                )
                                .changed()
                            {
                                self.cfg.dir = PathBuf::from(self.dir_text.clone());
                                save_config(&self.cfg);
                            }
                            ui.add_space(6.0);
                            ui.label(RichText::new("引擎").size(11.5).color(MUTED));
                            egui::ComboBox::from_id_salt("engine")
                                .selected_text(match self.cfg.engine.as_str() {
                                    "aria2" => "aria2 优先",
                                    _ => "自动（最快）",
                                })
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_value(
                                            &mut self.cfg.engine,
                                            "auto".to_string(),
                                            "自动（最快）",
                                        )
                                        .clicked()
                                    {
                                        save_config(&self.cfg);
                                    }
                                    if ui
                                        .selectable_value(
                                            &mut self.cfg.engine,
                                            "aria2".to_string(),
                                            "aria2 优先",
                                        )
                                        .clicked()
                                    {
                                        save_config(&self.cfg);
                                    }
                                });
                            ui.add_space(6.0);
                            ui.label(RichText::new("OpenList 路径").size(11.5).color(MUTED));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.ol_path)
                                    .hint_text(RichText::new("/挂载点/文件名").color(MUTED))
                                    .desired_width(160.0)
                                    .margin(egui::vec2(6.0, 3.0)),
                            );
                        });
                    });
                if self.toast_at.elapsed() < Duration::from_secs(4) {
                    ui.add_space(4.0);
                    pill(ui, &self.toast, ACCENT, tint(ACCENT, 26));
                }
            });

        let log_lines: Vec<String> = self
            .tasks
            .get(self.selected)
            .and_then(snapshot)
            .map(|s| s.log)
            .unwrap_or_default();

        // ---------- 日志 ----------
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(140.0)
            .frame(
                egui::Frame::NONE
                    .fill(PANEL)
                    .inner_margin(Margin { left: 18, right: 18, top: 10, bottom: 10 }),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("日志").size(11.5).color(SECONDARY));
                    ui.label(
                        RichText::new(format!("{} 条", log_lines.len()))
                            .size(11.0)
                            .color(MUTED),
                    );
                });
                ui.add_space(2.0);
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for line in log_lines.iter().rev().take(200).rev() {
                            ui.label(
                                RichText::new(line)
                                    .size(11.5)
                                    .color(SECONDARY)
                                    .monospace(),
                            );
                        }
                    });
            });

        // ---------- 任务列表 ----------
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(PAGE)
                    .inner_margin(Margin { left: 18, right: 18, top: 10, bottom: 10 }),
            )
            .show(ui, |ui| {
                if self.scan.is_some() {
                    self.draw_scan(ui);
                    ui.add_space(10.0);
                }
                if self.tasks.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(46.0, 46.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().circle_filled(rect.center(), 23.0, tint(PRIMARY, 28));
                            ui.painter().circle_stroke(
                                rect.center(),
                                22.5,
                                Stroke::new(1.0, tint(BORDER, 170)),
                            );
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "↓",
                                egui::FontId::proportional(20.0),
                                PRIMARY,
                            );
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("把链接粘到上方，点「＋」开始下载")
                                    .size(14.0)
                                    .color(SECONDARY),
                            );
                            ui.label(
                                RichText::new("支持直链 / 下载页解析 / 磁力 / 媒体站 / OpenList")
                                    .size(11.5)
                                    .color(MUTED),
                            );
                        });
                    });
                    return;
                }

                // 多任务汇总：正在下载/排队的合计进度、总速度与剩余时间
                {
                    let mut done = 0u64;
                    let mut total = 0u64;
                    let mut speed = 0.0f64;
                    let mut active = 0usize;
                    for t in &self.tasks {
                        if let Some(s) = snapshot(t) {
                            if s.status == Status::Running || s.status == Status::Queued {
                                active += 1;
                                done += s.done;
                                total += s.total;
                                speed += s.speed;
                            }
                        }
                    }
                    if active > 0 {
                        let left = if total > 0 {
                            format!("{} / {}", fmt_size(done), fmt_size(total))
                        } else {
                            fmt_size(done)
                        };
                        ui.horizontal(|ui| {
                            pill(ui, &format!("{} 个任务", active), PRIMARY, tint(PRIMARY, 26));
                            ui.label(RichText::new(left).size(11.5).color(SECONDARY));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} · ETA {}",
                                            fmt_speed(speed),
                                            fmt_eta(done, total, speed)
                                        ))
                                        .size(11.5)
                                        .color(PRIMARY),
                                    );
                                },
                            );
                        });
                        ui.add_space(6.0);
                    }
                }

                let mut selected_now = self.selected;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (idx, task) in self.tasks.iter().enumerate() {
                            let snap = match snapshot(task) {
                                Some(s) => s,
                                None => continue,
                            };
                            let resp = card_frame()
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        let (tag, fg, bg) = match snap.status {
                                            Status::Queued => ("排队", SECONDARY, PANEL),
                                            Status::Running => ("下载中", PRIMARY, tint(PRIMARY, 26)),
                                            Status::Paused => ("已暂停", SECONDARY, BUBBLE),
                                            Status::Done => ("完成", SUCCESS, SUCCESS_BG),
                                            Status::Failed => ("失败", DANGER, DANGER_BG),
                                            Status::Cancelled => ("已取消", MUTED, PANEL),
                                        };
                                        ui.label(
                                            RichText::new(&snap.name).strong().size(14.5).color(TEXT),
                                        );
                                        pill(ui, snap.kind.label(), SECONDARY, BUBBLE);
                                        pill(ui, tag, fg, bg);

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                // 暂停/继续只对内置引擎生效，aria2 任务交给它自己调度
                                                let pausable = self.cfg.engine != "aria2"
                                                    && snap.kind == engine::Kind::Http;
                                                if snap.status == Status::Running {
                                                    if ui.button("取消").clicked() {
                                                        engine::cancel_task(task);
                                                    }
                                                    if pausable && ui.button("暂停").clicked() {
                                                        engine::pause_task(task);
                                                    }
                                                } else if snap.status == Status::Paused {
                                                    if ui.button("取消").clicked() {
                                                        engine::cancel_task(task);
                                                    }
                                                    if ui.button("继续").clicked() {
                                                        engine::resume_task(
                                                            self.root.clone(),
                                                            task.clone(),
                                                        );
                                                    }
                                                }
                                                if snap.status == Status::Done
                                                    && ui.button("打开目录").clicked()
                                                {
                                                    let _ = std::process::Command::new("explorer")
                                                        .arg(&self.cfg.dir)
                                                        .spawn();
                                                }
                                            },
                                        );
                                    });

                                    ui.add_space(6.0);
                                    let frac = if snap.total > 0 {
                                        (snap.done as f32 / snap.total as f32).clamp(0.0, 1.0)
                                    } else if snap.status == Status::Done {
                                        1.0
                                    } else {
                                        0.0
                                    };
                                    progress_bar(ui, frac);
                                    ui.add_space(5.0);
                                    ui.horizontal(|ui| {
                                        let left = if snap.total > 0 {
                                            format!("{} / {}", fmt_size(snap.done), fmt_size(snap.total))
                                        } else {
                                            fmt_size(snap.done)
                                        };
                                        ui.label(RichText::new(left).size(11.5).color(SECONDARY));
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if snap.status == Status::Running {
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "{} · ETA {}",
                                                            io_text(snap.net_speed, snap.disk_speed),
                                                            fmt_eta(snap.done, snap.total, snap.speed)
                                                        ))
                                                        .size(11.5)
                                                        .color(PRIMARY),
                                                    );
                                                } else if snap.status == Status::Done {
                                                    ui.label(
                                                        RichText::new("已保存")
                                                            .size(11.5)
                                                            .color(SUCCESS),
                                                    );
                                                }
                                            },
                                        );
                                    });

                                    if !snap.message.is_empty()
                                        && snap.status != Status::Running
                                        && snap.status != Status::Done
                                    {
                                        ui.add_space(4.0);
                                        let color = if snap.status == Status::Failed {
                                            DANGER
                                        } else {
                                            MUTED
                                        };
                                        ui.label(
                                            RichText::new(&snap.message).size(11.5).color(color),
                                        );
                                    }
                                    if let Some(out) = &snap.output {
                                        ui.label(
                                            RichText::new(out.to_string_lossy())
                                                .size(11.0)
                                                .color(MUTED),
                                        );
                                    }
                                })
                                .response;

                            // 运行中卡片顶部的流光（对应 Sakura 的 bubble-thinking-sheen）
                            if snap.status == Status::Running {
                                let rect = resp.rect;
                                let t = ui.input(|i| i.time) as f32;
                                let w = 96.0_f32.min(rect.width() * 0.4);
                                let inner = rect.shrink2(egui::vec2(6.0, 0.0));
                                let x = inner.left() - w + (t * 170.0) % (inner.width() + w);
                                let seg = egui::Rect::from_min_max(
                                    egui::pos2(x.max(inner.left()), inner.top() + 1.0),
                                    egui::pos2((x + w).min(inner.right()), inner.top() + 2.6),
                                );
                                if seg.width() > 0.0 {
                                    ui.painter().rect_filled(
                                        seg,
                                        CornerRadius::same(1),
                                        tint(PRIMARY, 90),
                                    );
                                }
                            }

                            if resp.interact(egui::Sense::click()).clicked() {
                                selected_now = idx;
                            }
                            ui.add_space(8.0);
                        }
                    });
                self.selected = selected_now.min(self.tasks.len().saturating_sub(1));
            });
    }
}

fn headless() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let url = match args.get(2) {
        Some(u) => u.clone(),
        None => {
            println!("用法: DLKit.exe --headless <url> [openlist:/path] [-c 32] [-d 目录]");
            return 2;
        }
    };
    let mut conn = 32usize;
    let mut dir = engine::find_root().join("downloads");
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "-c" => {
                if let Some(v) = args.get(i + 1).and_then(|v| v.parse().ok()) {
                    conn = v;
                }
                i += 2;
            }
            "-d" => {
                if let Some(v) = args.get(i + 1) {
                    dir = PathBuf::from(v);
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    let (kind, why, ol_path) = if let Some(rest) = url.strip_prefix("openlist:") {
        (engine::Kind::OpenList, "OpenList 路径", Some(rest.to_string()))
    } else {
        let (k, w) = engine::classify(&url);
        (k, w, None)
    };
    println!("[识别] {} -> {:?}（{}）", url, kind, why);
    let task = Task::new(url, kind, "解析中…".into());
    let shared: Shared = Arc::new(Mutex::new(task));
    engine::spawn(
        engine::find_root(),
        shared.clone(),
        dir,
        conn,
        "auto".into(),
        ol_path,
    );
    let mut last = String::new();
    let mut logged = 0usize;
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let snap = match snapshot(&shared) {
            Some(s) => s,
            None => break,
        };
        while logged < snap.log.len() {
            println!("  [日志] {}", snap.log[logged]);
            logged += 1;
        }
        let line = format!(
            "{:>6.2}%  {:>26}  {}",
            if snap.total > 0 {
                snap.done as f64 * 100.0 / snap.total as f64
            } else {
                0.0
            },
            io_text(snap.net_speed, snap.disk_speed),
            snap.message
        );
        if line != last {
            println!("{}", line);
            last = line;
        }
        if snap.status != Status::Running && snap.status != Status::Queued {
            println!("结果: {:?}  {}", snap.status, snap.message);
            return if snap.status == Status::Done { 0 } else { 1 };
        }
    }
    1
}

fn headless_scan() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let url = match args.get(2) {
        Some(u) => u.clone(),
        None => {
            println!("用法: DLKit.exe --scan <下载页链接>");
            return 2;
        }
    };
    println!("[解析] {}", url);
    let st = scanner::start(url);
    let mut last = String::new();
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let (running, msg, error, candidates, recommended) = {
            let s = match st.lock() {
                Ok(s) => s,
                Err(_) => return 1,
            };
            (
                s.running,
                s.message.clone(),
                s.error.clone(),
                s.candidates.clone(),
                s.recommended,
            )
        };
        if msg != last {
            println!("  {}", msg);
            last = msg;
        }
        if running {
            continue;
        }
        if !error.is_empty() {
            println!("错误: {}", error);
            return 1;
        }
        if candidates.is_empty() {
            println!("没有找到可下载文件");
            return 1;
        }
        for (i, c) in candidates.iter().enumerate() {
            let size = if c.size > 0 {
                fmt_size(c.size)
            } else {
                "未知".to_string()
            };
            let note = if c.note.is_empty() {
                String::new()
            } else {
                format!("  [{}]", c.note)
            };
            let hash = if c.sha256.is_some() { "  [SHA256]" } else { "" };
            println!(
                "{:>3}. [{}] {:>9}  {}{}{}{}\n     {}",
                i + 1,
                c.type_label,
                size,
                c.title,
                if Some(i) == recommended { "  ★推荐" } else { "" },
                hash,
                note,
                c.url
            );
        }
        return 0;
    }
}

fn main() -> eframe::Result {
    if std::env::args().nth(1).as_deref() == Some("--headless") {
        std::process::exit(headless());
    }
    if std::env::args().nth(1).as_deref() == Some("--scan") {
        std::process::exit(headless_scan());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 620.0])
            .with_min_inner_size([800.0, 540.0])
            .with_title("DLKit"),
        ..Default::default()
    };
    eframe::run_native(
        "DLKit",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            setup_style(&cc.egui_ctx);
            Ok(Box::new(App::new()))
        }),
    )
}

