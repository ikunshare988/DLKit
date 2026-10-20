//! DLKit 桌面界面（Rust + egui）。
//! 视觉语言对齐 Sakura：天蓝主色、樱花粉点缀、浅色面板、大圆角、柔和描边与过渡。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod accounts;
mod av;
mod deps;
mod diag;
mod engine;
mod mcp;
mod platform;
mod policy;
mod scanner;
mod setup;
mod tray;
mod update;
mod watch;

use engine::{Shared, Status, Task};
use std::collections::BTreeMap;
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
    /// 增强版：下载完成后自动做安全检查
    av_auto: bool,
    /// 增强版：调用 Windows Defender 单文件扫描
    av_defender: bool,
    /// 增强版：超过这个大小跳过 Defender
    av_max_gb: f64,
    /// 增强版：先下到沙箱，检查通过后再放行
    av_sandbox: bool,
}

/// 增强版编译特性：安全扫描、策略配置只在增强版生效
pub const ENHANCED: bool = cfg!(feature = "enhanced");

impl Default for Config {
    fn default() -> Self {
        Config {
            dir: setup::default_download_dir(&engine::find_root()),
            conn: 32,
            engine: "auto".into(),
            limit: 0.0,
            av_auto: true,
            av_defender: true,
            av_max_gb: 4.0,
            av_sandbox: true,
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
        av_auto: Option<bool>,
        av_defender: Option<bool>,
        av_max_gb: Option<f64>,
        av_sandbox: Option<bool>,
    }
    let mut cfg = Config::default();
    if let Ok(text) = std::fs::read_to_string(config_path()) {
        if let Ok(raw) = serde_json::from_str::<Raw>(&text) {
            if let Some(d) = raw.dir {
                cfg.dir = PathBuf::from(d);
            }
            if let Some(c) = raw.conn {
                cfg.conn = c.clamp(1, 128);
            }
            if let Some(e) = raw.engine {
                cfg.engine = e;
            }
            if let Some(l) = raw.limit {
                cfg.limit = l.clamp(0.0, 100.0);
            }
            if let Some(v) = raw.av_auto {
                cfg.av_auto = v;
            }
            if let Some(v) = raw.av_defender {
                cfg.av_defender = v;
            }
            if let Some(v) = raw.av_max_gb {
                cfg.av_max_gb = v.clamp(0.1, 100.0);
            }
            if let Some(v) = raw.av_sandbox {
                cfg.av_sandbox = v;
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
        "limit": cfg.limit,
        "av_auto": cfg.av_auto,
        "av_defender": cfg.av_defender,
        "av_max_gb": cfg.av_max_gb,
        "av_sandbox": cfg.av_sandbox
    });
    let _ = std::fs::write(config_path(), v.to_string());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Download,
    Watch,
    Accounts,
    Settings,
}

/// 任务卡片里点了动作后要做的事：先记下来，等遍历结束再改 self，
/// 避免和 `self.tasks.iter()` 的借用冲突。
enum CardAction {
    Services,
    Accounts,
    Settings,
    CopyError(String),
    Quarantine(PathBuf),
    AcceptAv(PathBuf),
    Release(PathBuf),
    Notice(String),
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
    page: Page,
    deps: deps::Installer,
    acc: accounts::Accounts,
    /// 账号页的保存/测试结果
    acc_msg: String,
    /// 用户点过「自己选」后，收起平台推荐横幅
    scan_hint_hidden: bool,
    /// 上一次看到的剪贴板内容，用来判断是否是新复制的链接
    clip_last: String,
    clip_at: Instant,
    /// 托盘图标（创建失败时为 None，此时关窗即退出）
    tray: Option<tray::Tray>,
    /// 关闭窗口 = 退出程序（托盘「退出」或更新重启时置位）
    allow_close: bool,
    close_sent: bool,
    tip_at: Instant,
    updater: update::Updater,
    /// 上一次看到的更新提示，避免同一个提示反复弹
    upd_msg: String,
    /// 链接跟踪（后台线程定时检查直链是否更新）
    watch: watch::Watcher,
    /// 首次运行检查结果（目录是否可写、是否自动生成了配置）
    layout: setup::Layout,
    /// 待安装的依赖 key（首次引导的「一键补齐」按顺序安装）
    install_queue: Vec<&'static str>,
    /// aria2 装好后是否要顺手启动服务（添加磁力时触发）
    want_services: bool,
    /// 环境检查的后台结果
    diag_rx: Option<std::sync::mpsc::Receiver<Vec<diag::Check>>>,
    diag_checks: Option<Vec<diag::Check>>,
    /// 上次异常退出的日志，提示过一次后清空
    crash_log: Option<PathBuf>,
    /// 已经安排过安全检查的文件（按路径去重）
    av_scanned: std::collections::HashSet<PathBuf>,
    /// 云端信誉配置（VirusTotal，可选）
    cloud: av::CloudCfg,
    /// 增强版策略：按站点覆盖参数、全局预算、时段限速
    policy: policy::Policy,
    /// 已经应用过的限速值（用于判断时段切换）
    sched_applied: Option<f64>,
    sched_at: Instant,
    /// 因资源预算而排队的任务（增强版）
    pending: Vec<(Shared, engine::SpawnArgs, i32)>,
}

/// 左侧导航项
fn nav_item(ui: &mut egui::Ui, label: &str, page: Page, cur: &mut Page) {
    let selected = *cur == page;
    let text = RichText::new(label)
        .size(13.5)
        .color(if selected { PRIMARY } else { SECONDARY });
    let btn = egui::Button::new(text)
        .fill(if selected {
            tint(PRIMARY, 34)
        } else {
            Color32::TRANSPARENT
        })
        .stroke(if selected {
            Stroke::new(1.0, tint(PRIMARY, 90))
        } else {
            Stroke::new(0.0, Color32::TRANSPARENT)
        })
        .corner_radius(CornerRadius::same(12))
        .min_size(egui::vec2(ui.available_width(), 32.0));
    if ui.add(btn).clicked() {
        *cur = page;
    }
}

fn kind_key(kind: engine::Kind) -> &'static str {
    match kind {
        engine::Kind::Http => "http",
        engine::Kind::Torrent => "torrent",
        engine::Kind::Media => "media",
        engine::Kind::Netdisk => "netdisk",
        engine::Kind::OpenList => "openlist",
    }
}

impl App {
    fn new() -> App {
        let root = engine::find_root();
        // 首次运行：补齐目录与 aria2 配置，用户不用手动复制示例文件
        let layout = setup::ensure_layout(&root);
        let crash_log = setup::last_crash(&root);
        let cfg = load_config();
        engine::set_speed_limit(cfg.limit);
        let tray = tray::Tray::new();
        let updater = update::Updater::new();
        updater.check();
        let acc = accounts::Accounts::load(&root);
        let watch = watch::Watcher::new(&root);
        let cloud = av::load_cloud(&root);
        let created_conf = layout.created_conf;
        let root_for_policy = root.clone();
        let app = App {
            url: String::new(),
            ol_path: String::new(),
            dir_text: cfg.dir.to_string_lossy().to_string(),
            cfg,
            tasks: Vec::new(),
            selected: 0,
            toast: String::new(),
            toast_at: Instant::now() - Duration::from_secs(10),
            root,
            svc_online: false,
            svc_checked: Instant::now() - Duration::from_secs(10),
            scan: None,
            page: Page::Download,
            deps: deps::Installer::new(),
            acc,
            acc_msg: String::new(),
            scan_hint_hidden: false,
            // 启动时已有的剪贴板内容不算「新复制」，避免一打开就自动填入
            clip_last: clipboard_text().unwrap_or_default(),
            clip_at: Instant::now(),
            tray,
            allow_close: false,
            close_sent: false,
            tip_at: Instant::now(),
            updater,
            upd_msg: String::new(),
            watch,
            layout,
            install_queue: Vec::new(),
            want_services: false,
            diag_rx: None,
            diag_checks: None,
            crash_log,
            av_scanned: std::collections::HashSet::new(),
            cloud,
            policy: if ENHANCED {
                policy::Policy::load(&root_for_policy)
            } else {
                policy::Policy::default()
            },
            sched_applied: None,
            sched_at: Instant::now() - Duration::from_secs(60),
            pending: Vec::new(),
        };
        let mut app = app;
        if created_conf {
            app.notify("已自动生成 aria2 配置，可以直接使用");
        }
        app
    }

    /// 任务保存目录：增强版开启沙箱时先落在沙箱里
    fn task_dir(&self) -> PathBuf {
        if ENHANCED && self.cfg.av_sandbox {
            av::sandbox_dir(&self.cfg.dir)
        } else {
            self.cfg.dir.clone()
        }
    }

    /// 下载完成后自动做安全检查（增强版）
    fn poll_av(&mut self) {
        if !ENHANCED || !self.cfg.av_auto {
            return;
        }
        let mut todo: Vec<(Shared, PathBuf)> = Vec::new();
        for t in &self.tasks {
            if let Ok(task) = t.lock() {
                if task.status == Status::Done && task.av.is_none() {
                    if let Some(p) = task.output.clone() {
                        if av::eligible(&p) && !self.av_scanned.contains(&p) {
                            todo.push((t.clone(), p));
                        }
                    }
                }
            }
        }
        for (shared, path) in todo {
            self.av_scanned.insert(path.clone());
            let defender = self.cfg.av_defender;
            let max_gb = self.cfg.av_max_gb;
            let cloud = self.cloud.clone();
            let dest = self.cfg.dir.clone();
            std::thread::spawn(move || {
                let mut rep = av::scan(&path, defender, max_gb, &cloud);
                let mut released = None;
                // 检查通过（或本来就不需要检查的类型）才放行到下载目录
                if matches!(rep.state, av::State::Clean | av::State::Skipped) {
                    match av::release(&path, &dest) {
                        Ok(p) => {
                            rep.findings.push(format!("已放行到 {}", p.display()));
                            released = Some(p);
                        }
                        Err(e) => rep.findings.push(format!("放行失败：{}", e)),
                    }
                }
                if let Ok(mut t) = shared.lock() {
                    t.av = Some(rep);
                    if let Some(p) = released {
                        t.output = Some(p);
                    }
                }
            });
        }
    }

    /// 修改某个已下载文件对应的安全检查结果
    fn update_av(&self, path: &std::path::Path, mut f: impl FnMut(&mut av::Report)) {
        for t in &self.tasks {
            if let Ok(mut task) = t.lock() {
                if task.output.as_deref() == Some(path) {
                    if let Some(r) = task.av.as_mut() {
                        f(r);
                    }
                }
            }
        }
    }

    /// 按策略取这个任务该用的并发数
    fn task_conn(&self, url: &str, kind: engine::Kind) -> usize {
        self.policy.conn_for(url, kind_key(kind), self.cfg.conn)
    }

    fn running_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| {
                t.lock()
                    .map(|x| x.status == Status::Running)
                    .unwrap_or(false)
            })
            .count()
    }

    /// 统一入口：有资源预算时先排队，否则立即开工
    fn submit(
        &mut self,
        shared: Shared,
        url: &str,
        kind: engine::Kind,
        dir: PathBuf,
        ol: Option<String>,
        extra: BTreeMap<String, String>,
    ) {
        let args = engine::SpawnArgs {
            dir,
            conn: self.task_conn(url, kind),
            engine: self.cfg.engine.clone(),
            ol_path: ol,
            extra,
        };
        // 策略里的单任务限速
        if let Some(v) = self.policy.limit_for(url, kind_key(kind)) {
            if let Ok(mut t) = shared.lock() {
                t.rate = Some(Arc::new(engine::TaskRate::new(v)));
            }
        }
        let limit = if ENHANCED { self.policy.max_active() } else { None };
        if let Some(limit) = limit {
            if self.running_count() >= limit {
                if let Ok(mut t) = shared.lock() {
                    t.status = Status::Queued;
                    t.message = format!("排队中（最多同时 {} 个任务）", limit);
                }
                let prio = self.policy.priority_for(url, kind_key(kind));
                self.pending.push((shared, args, prio));
                return;
            }
        }
        engine::spawn_ext(
            self.root.clone(),
            shared,
            args.dir,
            args.conn,
            args.engine,
            args.ol_path,
            args.extra,
        );
    }

    /// 有任务结束时，按优先级把排队的任务放出来
    fn pump_queue(&mut self) {
        if !ENHANCED {
            return;
        }
        let Some(limit) = self.policy.max_active() else {
            return;
        };
        while self.running_count() < limit && !self.pending.is_empty() {
            let idx = self
                .pending
                .iter()
                .enumerate()
                .max_by_key(|(i, (_, _, p))| (*p, usize::MAX - *i))
                .map(|(i, _)| i)
                .unwrap_or(0);
            let (task, args, _) = self.pending.remove(idx);
            engine::spawn_ext(
                self.root.clone(),
                task,
                args.dir,
                args.conn,
                args.engine,
                args.ol_path,
                args.extra,
            );
        }
    }

    /// 时段限速：命中规则时覆盖用户的限速值，离开时段后恢复
    fn apply_schedule(&mut self) {
        if self.sched_at.elapsed() < Duration::from_secs(30) {
            return;
        }
        self.sched_at = Instant::now();
        let slot = self.policy.limit_now();
        let want = slot.unwrap_or(self.cfg.limit);
        let changed = self
            .sched_applied
            .map(|v| (v - want).abs() > 0.01)
            .unwrap_or(true);
        if changed {
            engine::set_speed_limit(want);
            self.sched_applied = Some(want);
            if slot.is_some() {
                self.notify(format!("已进入限速时段：{} MB/s", want));
            }
        }
    }

    /// 依赖安装队列：一次装一个，装完自动接下一个
    fn poll_installs(&mut self) {
        let st = self.deps.state();
        if matches!(st.phase, deps::Phase::Downloading | deps::Phase::Extracting) {
            return;
        }
        if st.phase == deps::Phase::Error {
            let msg = st.message.clone();
            self.install_queue.clear();
            self.want_services = false;
            self.notify(msg);
            return;
        }
        let key = match self.install_queue.first().copied() {
            Some(k) => k,
            None => {
                // 队列空了，但用户加了磁力、aria2 是刚装好的：顺手起服务
                if self.want_services && self.aria2_installed() {
                    self.want_services = false;
                    self.start_services();
                }
                return;
            }
        };
        if st.phase == deps::Phase::Done && st.key == key {
            self.install_queue.remove(0);
            if key == "aria2" && self.want_services {
                self.want_services = false;
                self.start_services();
            }
            return;
        }
        if st.phase == deps::Phase::Idle {
            if let Some(dep) = deps::DEPS.iter().find(|d| d.key == key) {
                self.deps.start(self.root.clone(), dep);
            }
        }
    }

    fn aria2_installed(&self) -> bool {
        deps::DEPS
            .iter()
            .find(|d| d.key == "aria2")
            .map(|d| deps::installed(&self.root, d))
            .unwrap_or(false)
    }

    /// 补齐全缺的组件（按顺序安装，不并发）
    fn install_missing(&mut self) {
        let missing: Vec<&'static str> = deps::DEPS
            .iter()
            .filter(|d| !deps::installed(&self.root, d))
            .map(|d| d.key)
            .collect();
        if missing.is_empty() {
            self.notify("组件都齐了");
            return;
        }
        self.install_queue = missing;
        self.notify("开始补齐组件，请保持网络连接");
    }

    /// 磁力 / 种子依赖 aria2：没有就自动装，装完自动启动服务
    fn ensure_aria2(&mut self) {
        if self.aria2_installed() {
            self.notify("磁力 / 种子需要 aria2，正在启动服务…");
            self.start_services();
            return;
        }
        self.want_services = true;
        if !self.install_queue.iter().any(|k| *k == "aria2") {
            self.install_queue.insert(0, "aria2");
        }
        self.notify("磁力 / 种子需要 aria2，正在自动安装…");
    }

    fn poll_diag(&mut self) {
        let Some(rx) = &self.diag_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(v) => {
                self.diag_checks = Some(v);
                self.diag_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.diag_rx = None;
            }
        }
    }

    /// 环境检查放到后台线程，避免网络探测卡住界面
    fn run_diag(&mut self) {
        if self.diag_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.diag_rx = Some(rx);
        self.diag_checks = None;
        let root = self.root.clone();
        let dir = self.cfg.dir.clone();
        let online = self.svc_online;
        let limit = self.cfg.limit;
        std::thread::spawn(move || {
            let _ = tx.send(diag::run(&root, &dir, online, limit));
        });
    }

    /// 首次使用 / 异常退出 / 目录不可写 的顶部提示
    fn draw_first_run(&mut self, ui: &mut egui::Ui) {
        if let Some(p) = self.crash_log.clone() {
            egui::Frame::NONE
                .fill(DANGER_BG)
                .stroke(Stroke::new(1.0, tint(DANGER, 120)))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("上次没有正常退出")
                                .strong()
                                .size(12.5)
                                .color(DANGER),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.button("知道了").clicked() {
                                    setup::mark_crash_seen(&p);
                                    self.crash_log = None;
                                }
                                if ui.button("查看日志").clicked() {
                                    let _ = std::process::Command::new("notepad").arg(&p).spawn();
                                }
                            },
                        );
                    });
                });
            ui.add_space(8.0);
        }

        if !self.layout.writable {
            egui::Frame::NONE
                .fill(DANGER_BG)
                .stroke(Stroke::new(1.0, tint(DANGER, 120)))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("程序目录不可写")
                                .strong()
                                .size(12.5)
                                .color(DANGER),
                        );
                        ui.label(
                            RichText::new("配置和日志存不下来，请把 DLKit 移到桌面或 D:\\DLKit")
                                .size(11.0)
                                .color(SECONDARY),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.button("打开目录").clicked() {
                                    if let Err(e) = open_path(&self.root) {
                                        self.notify(e);
                                    }
                                }
                            },
                        );
                    });
                });
            ui.add_space(8.0);
        }

        let missing: Vec<&'static deps::Dep> = deps::DEPS
            .iter()
            .filter(|d| !deps::installed(&self.root, d))
            .collect();
        if missing.is_empty() {
            return;
        }
        let st = self.deps.state();
        let busy = matches!(st.phase, deps::Phase::Downloading | deps::Phase::Extracting);
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("首次使用").strong().size(14.0).color(TEXT));
                pill(
                    ui,
                    &format!("缺 {} 个组件", missing.len()),
                    ACCENT,
                    tint(ACCENT, 26),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if busy { "补齐中…" } else { "一键补齐" };
                    let btn = egui::Button::new(RichText::new(label).size(12.0).color(Color32::WHITE))
                        .fill(if busy { MUTED } else { PRIMARY })
                        .corner_radius(CornerRadius::same(12));
                    if ui.add_enabled(!busy, btn).clicked() {
                        self.install_missing();
                    }
                });
            });
            ui.add_space(3.0);
            ui.label(
                RichText::new("直链下载不需要这些；aria2 用于 BT / 磁力，yt-dlp + ffmpeg 用于视频站")
                    .size(11.0)
                    .color(MUTED),
            );
            if busy {
                ui.add_space(5.0);
                progress_bar(ui, st.progress);
                ui.label(RichText::new(&st.message).size(11.0).color(SECONDARY));
            }
        });
        ui.add_space(8.0);
    }

    /// 启动 aria2（带 start.ps1 的完整包会连 OpenList 一起起；单文件版直接拉 aria2c）
    fn start_services(&mut self) {
        let aria = self.root.join("bin").join("aria2").join("aria2c.exe");
        let conf = self.root.join("config").join("aria2.conf");
        if !aria.exists() {
            // 缺组件就自动装，装完 poll_installs 会再调一次这里
            self.want_services = true;
            if !self.install_queue.iter().any(|k| *k == "aria2") {
                self.install_queue.insert(0, "aria2");
            }
            self.notify("缺少 aria2，正在自动安装…");
            return;
        }
        if !conf.exists() {
            setup::ensure_layout(&self.root);
        }
        let log_dir = self.root.join("logs");
        let _ = std::fs::create_dir_all(&log_dir);
        let script = self.root.join("start.ps1");

        #[allow(unused_mut)]
        let mut c = if script.exists() {
            let mut c = std::process::Command::new("powershell");
            c.args(["-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
                .arg(&script);
            c
        } else {
            // 单文件发布：不依赖脚本，直接按配置拉起 aria2c
            let mut c = std::process::Command::new(&aria);
            c.arg(format!("--conf-path={}", conf.display()))
                .current_dir(&self.root);
            c
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        let err_path = log_dir.join("start.err.log");
        let err = std::fs::File::create(&err_path).ok();
        c.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null());
        if let Some(f) = err {
            c.stderr(f);
        }
        match c.spawn() {
            Ok(_) => self.notify("正在启动服务…几秒后仍离线请看 logs\\start.err.log"),
            Err(e) => self.notify(format!("启动失败：{}", e)),
        }
    }

    /// 账号页：B 站 Cookie 与网盘（OpenList）登录
    fn draw_accounts(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                card_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("B 站账号").strong().size(15.0).color(TEXT));
                        if self.acc.bili_ready() {
                            pill(ui, "已配置", SUCCESS, SUCCESS_BG);
                        } else {
                            pill(ui, "未配置", MUTED, PANEL);
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("1080P 及以上、番剧、大会员内容需要登录；公开视频不登录也能下")
                            .size(11.0)
                            .color(MUTED),
                    );
                    ui.add_space(8.0);

                    let mut mode = self.acc.bili_mode.clone();
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut mode, accounts::MODE_NONE.to_string(), "不使用");
                        ui.radio_value(&mut mode, accounts::MODE_BROWSER.to_string(), "从浏览器读取");
                        ui.radio_value(&mut mode, accounts::MODE_MANUAL.to_string(), "手动粘贴 Cookie");
                    });
                    self.acc.bili_mode = mode;

                    if self.acc.bili_mode == accounts::MODE_BROWSER {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("浏览器").size(12.0).color(SECONDARY));
                            egui::ComboBox::from_id_salt("bili-browser")
                                .selected_text(match self.acc.bili_browser.as_str() {
                                    "chrome" => "Chrome",
                                    "firefox" => "Firefox",
                                    _ => "Edge",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.acc.bili_browser,
                                        "edge".into(),
                                        "Edge",
                                    );
                                    ui.selectable_value(
                                        &mut self.acc.bili_browser,
                                        "chrome".into(),
                                        "Chrome",
                                    );
                                    ui.selectable_value(
                                        &mut self.acc.bili_browser,
                                        "firefox".into(),
                                        "Firefox",
                                    );
                                });
                            ui.label(
                                RichText::new("该浏览器登录过 B 站即可；Chrome 需要先完全退出")
                                    .size(11.0)
                                    .color(MUTED),
                            );
                        });
                    } else if self.acc.bili_mode == accounts::MODE_MANUAL {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "粘贴整段 Cookie（浏览器 F12 → Network → 任一请求 → Cookie）",
                            )
                            .size(11.0)
                            .color(MUTED),
                        );
                        ui.add(
                            egui::TextEdit::multiline(&mut self.acc.bili_cookie)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .hint_text("SESSDATA=...; bili_jct=...；"),
                        );
                    }

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let btn = egui::Button::new(
                            RichText::new("保存").size(12.0).color(Color32::WHITE),
                        )
                        .fill(PRIMARY)
                        .corner_radius(CornerRadius::same(12));
                        if ui.add(btn).clicked() {
                            self.acc_msg = match self.acc.save(&self.root) {
                                Ok(()) => "B 站账号设置已保存".into(),
                                Err(e) => e,
                            };
                        }
                        if !self.acc_msg.is_empty() {
                            ui.label(RichText::new(&self.acc_msg).size(11.0).color(SECONDARY));
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "Cookie 用 Windows DPAPI 加密后保存在 config/accounts.json，只有当前用户能解密",
                        )
                        .size(10.5)
                        .color(MUTED),
                    );
                });

                ui.add_space(12.0);

                card_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("网盘（OpenList）").strong().size(15.0).color(TEXT));
                        if self.svc_online {
                            pill(ui, "OpenList 端口在线", SUCCESS, SUCCESS_BG);
                        } else {
                            pill(ui, "服务未启动", MUTED, PANEL);
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "百度网盘 / 阿里云盘 / 夸克等先在 OpenList 后台扫码或登录一次，DLKit 只负责取直链",
                        )
                        .size(11.0)
                        .color(MUTED),
                    );
                    ui.add_space(8.0);
                    egui::Grid::new("ol-acc")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new("地址").size(12.0).color(SECONDARY));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.acc.ol_base)
                                    .desired_width(320.0)
                                    .hint_text("http://127.0.0.1:5244"),
                            );
                            ui.end_row();

                            ui.label(RichText::new("用户名").size(12.0).color(SECONDARY));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.acc.ol_user)
                                    .desired_width(220.0),
                            );
                            ui.end_row();

                            ui.label(RichText::new("密码").size(12.0).color(SECONDARY));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.acc.ol_pass)
                                    .desired_width(220.0)
                                    .password(true),
                            );
                            ui.end_row();
                        });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let btn = egui::Button::new(
                            RichText::new("保存并测试").size(12.0).color(Color32::WHITE),
                        )
                        .fill(PRIMARY)
                        .corner_radius(CornerRadius::same(12));
                        if ui.add(btn).clicked() {
                            self.acc_msg = match self.acc.save(&self.root) {
                                Ok(()) => match engine::openlist_test(&self.root) {
                                    Ok(()) => "OpenList 登录成功".into(),
                                    Err(e) => format!("已保存，但登录失败：{}", e),
                                },
                                Err(e) => e,
                            };
                        }
                        if ui.button("打开 OpenList 管理页").clicked() {
                            let url = self.acc.ol_base.trim().to_string();
                            let _ = std::process::Command::new("cmd")
                                .args(["/c", "start", "", &url])
                                .spawn();
                        }
                        if !self.svc_online && ui.button("启动服务").clicked() {
                            self.start_services();
                        }
                    });
                });
            });
    }

    /// 设置页：下载参数、依赖补齐、更新
    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        let st = self.deps.state();
        let busy = matches!(
            st.phase,
            deps::Phase::Downloading | deps::Phase::Extracting
        );
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                card_frame().show(ui, |ui| {
                    ui.label(RichText::new("下载").strong().size(15.0).color(TEXT));
                    ui.add_space(8.0);
                    egui::Grid::new("set-download")
                        .num_columns(2)
                        .spacing([14.0, 10.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new("下载目录").size(12.5).color(SECONDARY));
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.dir_text)
                                        .desired_width(430.0)
                                        .margin(egui::vec2(6.0, 3.0)),
                                )
                                .changed()
                            {
                                self.cfg.dir = PathBuf::from(self.dir_text.clone());
                                save_config(&self.cfg);
                            }
                            ui.end_row();

                            ui.label(RichText::new("同时下载").size(12.5).color(SECONDARY));
                            ui.horizontal(|ui| {
                                if ui
                                    .add(
                                        egui::DragValue::new(&mut self.cfg.conn)
                                            .range(1..=128)
                                            .suffix(" 份"),
                                    )
                                    .changed()
                                {
                                    save_config(&self.cfg);
                                }
                                ui.label(
                                    RichText::new("默认即可，连得多通常更快")
                                        .size(11.0)
                                        .color(MUTED),
                                );
                            });
                            ui.end_row();

                            ui.label(RichText::new("全局限速").size(12.5).color(SECONDARY));
                            ui.horizontal(|ui| {
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
                                let _ = resp.on_hover_text("只作用于内置下载引擎");
                                ui.label(RichText::new("0 = 不限速").size(11.0).color(MUTED));
                            });
                            ui.end_row();

                            ui.label(RichText::new("下载方式").size(12.5).color(SECONDARY));
                            egui::ComboBox::from_id_salt("engine-setting")
                                .selected_text(match self.cfg.engine.as_str() {
                                    "aria2" => "aria2 优先",
                                    _ => "自动（推荐）",
                                })
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_value(
                                            &mut self.cfg.engine,
                                            "auto".to_string(),
                                            "自动（推荐）",
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
                            ui.end_row();
                        });
                });

                ui.add_space(12.0);

                card_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("依赖").strong().size(15.0).color(TEXT));
                        ui.label(
                            RichText::new("下载到程序目录；缺什么补什么，补完即可用")
                                .size(11.0)
                                .color(MUTED),
                        );
                    });
                    ui.add_space(8.0);
                    for dep in deps::DEPS {
                        let ok = deps::installed(&self.root, dep);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(dep.name).strong().size(12.5).color(TEXT));
                            if ok {
                                pill(ui, "已安装", SUCCESS, SUCCESS_BG);
                            } else {
                                pill(
                                    ui,
                                    &format!("缺失 · 约 {} MB", dep.size_mb),
                                    DANGER,
                                    DANGER_BG,
                                );
                            }
                            ui.label(RichText::new(dep.note).size(11.0).color(MUTED));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if !ok {
                                        let running = st.key == dep.key && busy;
                                        let btn = egui::Button::new(
                                            RichText::new(if running {
                                                "安装中…"
                                            } else {
                                                "一键安装"
                                            })
                                            .size(12.0)
                                            .color(Color32::WHITE),
                                        )
                                        .fill(if running { MUTED } else { PRIMARY })
                                        .corner_radius(CornerRadius::same(12));
                                        if ui.add_enabled(!busy, btn).clicked() {
                                            let root = self.root.clone();
                                            self.deps.start(root, dep);
                                        }
                                    } else if dep.key == "aria2" && !self.svc_online {
                                        if ui.button("启动服务").clicked() {
                                            self.start_services();
                                        }
                                    }
                                },
                            );
                        });
                        if st.key == dep.key {
                            match st.phase {
                                deps::Phase::Downloading | deps::Phase::Extracting => {
                                    ui.add_space(4.0);
                                    progress_bar(ui, st.progress);
                                    ui.label(
                                        RichText::new(&st.message).size(11.0).color(SECONDARY),
                                    );
                                }
                                deps::Phase::Error => {
                                    ui.label(RichText::new(&st.message).size(11.0).color(DANGER));
                                }
                                deps::Phase::Done => {
                                    ui.label(RichText::new(&st.message).size(11.0).color(SUCCESS));
                                }
                                deps::Phase::Idle => {}
                            }
                        }
                        ui.add_space(6.0);
                    }
                });

                if ENHANCED {
                    ui.add_space(12.0);

                    let desc = self.policy.describe();
                    let perr = !self.policy.error.is_empty();
                    card_frame().show(ui, |ui| {
                        ui.label(RichText::new("策略（增强版）").strong().size(15.0).color(TEXT));
                        ui.add_space(3.0);
                        ui.label(
                            RichText::new(&desc)
                                .size(11.0)
                                .color(if perr { DANGER } else { MUTED }),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("重新加载").clicked() {
                                self.policy = policy::Policy::load(&self.root);
                                self.sched_applied = None;
                                self.notify("策略已重新加载");
                            }
                            ui.label(
                                RichText::new("config/policy.json：按站点覆盖并发、总预算、时段限速")
                                    .size(10.5)
                                    .color(MUTED),
                            );
                        });
                    });

                    ui.add_space(12.0);

                    card_frame().show(ui, |ui| {
                        ui.label(RichText::new("安全").strong().size(15.0).color(TEXT));
                        ui.add_space(3.0);
                        ui.label(
                            RichText::new("下载完成后检查可执行文件、压缩包和文档；只报告，不自动删除")
                                .size(11.0)
                                .color(MUTED),
                        );
                        ui.add_space(6.0);
                        if ui
                            .checkbox(&mut self.cfg.av_auto, "下载完成后自动检查")
                            .changed()
                        {
                            save_config(&self.cfg);
                        }
                        if ui
                            .checkbox(&mut self.cfg.av_defender, "使用 Windows Defender 扫描")
                            .changed()
                        {
                            save_config(&self.cfg);
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("超过").size(12.0).color(SECONDARY));
                            if ui
                                .add(
                                    egui::DragValue::new(&mut self.cfg.av_max_gb)
                                        .range(0.1..=100.0)
                                        .suffix(" GB"),
                                )
                                .changed()
                            {
                                save_config(&self.cfg);
                            }
                            ui.label(
                                RichText::new("跳过 Defender（只做启发式与签名检查）")
                                    .size(11.0)
                                    .color(MUTED),
                            );
                        });
                        ui.add_space(4.0);
                        if ui
                            .checkbox(
                                &mut self.cfg.av_sandbox,
                                "沙箱模式：先隔离到暂存区，检查通过后自动放行",
                            )
                            .changed()
                        {
                            save_config(&self.cfg);
                        }
                        ui.add_space(6.0);
                        ui.checkbox(
                            &mut self.cloud.enabled,
                            "云端信誉查询（VirusTotal，只发 SHA256，不上传文件）",
                        );
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("API key").size(11.5).color(SECONDARY));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.cloud.api_key)
                                    .password(true)
                                    .desired_width(240.0)
                                    .margin(egui::vec2(6.0, 3.0)),
                            );
                            if ui.button("保存云端设置").clicked() {
                                match av::save_cloud(&self.root, &self.cloud) {
                                    Ok(_) => self.notify("云端设置已保存"),
                                    Err(e) => self.notify(e),
                                }
                            }
                        });
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button("打开隔离目录").clicked() {
                                let dir = av::quarantine_dir(&self.root);
                                let _ = std::fs::create_dir_all(&dir);
                                if let Err(e) = open_path(&dir) {
                                    self.notify(e);
                                }
                            }
                            if ui.button("立即检查已下载文件").clicked() {
                                self.av_scanned.clear();
                                for t in &self.tasks {
                                    if let Ok(mut task) = t.lock() {
                                        if task.status == Status::Done {
                                            task.av = None;
                                        }
                                    }
                                }
                                self.notify("将重新检查已完成的任务");
                            }
                        });
                    });
                }

                ui.add_space(12.0);

                card_frame().show(ui, |ui| {
                    ui.label(RichText::new("环境检查").strong().size(15.0).color(TEXT));
                    ui.add_space(3.0);
                    ui.label(
                        RichText::new("下载不了时先点这里：检查目录、组件和网络，生成一份可复制的报告")
                            .size(11.0)
                            .color(MUTED),
                    );
                    ui.add_space(8.0);
                    let checks = self.diag_checks.clone();
                    ui.horizontal(|ui| {
                        let running = self.diag_rx.is_some();
                        let btn = egui::Button::new(
                            RichText::new(if running { "检查中…" } else { "开始检查" }).size(12.0),
                        );
                        if ui.add_enabled(!running, btn).clicked() {
                            self.run_diag();
                        }
                        if checks.is_some() && ui.button("复制报告").clicked() {
                            if let Some(list) = &checks {
                                clipboard_set(&diag::report(list));
                            }
                            self.notify("报告已复制");
                        }
                    });
                    if let Some(list) = &checks {
                        ui.add_space(6.0);
                        let color = if diag::has_blocking(list) { DANGER } else { SUCCESS };
                        ui.label(RichText::new(diag::summary(list)).size(12.0).color(color));
                        ui.add_space(4.0);
                        for c in list {
                            ui.horizontal(|ui| {
                                let (mark, fg) = if c.ok {
                                    ("✓", SUCCESS)
                                } else if c.required {
                                    ("!", DANGER)
                                } else {
                                    ("!", ACCENT)
                                };
                                ui.label(RichText::new(mark).size(11.5).color(fg));
                                ui.label(RichText::new(c.name).size(11.5).color(TEXT));
                                ui.label(RichText::new(&c.detail).size(11.0).color(SECONDARY));
                            });
                            if !c.ok && !c.hint.is_empty() {
                                ui.label(
                                    RichText::new(format!("   {}", c.hint))
                                        .size(10.5)
                                        .color(MUTED),
                                );
                            }
                        }
                    }
                });

                ui.add_space(12.0);

                card_frame().show(ui, |ui| {
                    ui.label(RichText::new("更新").strong().size(15.0).color(TEXT));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("当前版本 v{}", update::Updater::current()))
                                .size(12.5)
                                .color(TEXT),
                        );
                        if ui.button("检查更新").clicked() {
                            self.updater.check();
                            self.notify("正在检查更新…");
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.hyperlink_to("GitHub 仓库", "https://github.com/ikunshare988/DLKit");
                        });
                    });
                    let us = self.updater.state();
                    if !us.message.is_empty() {
                        ui.add_space(4.0);
                        let color = match us.phase {
                            update::Phase::Error => DANGER,
                            update::Phase::Available => ACCENT,
                            _ => SECONDARY,
                        };
                        ui.label(RichText::new(&us.message).size(11.5).color(color));
                    }
                    if matches!(
                        us.phase,
                        update::Phase::Available | update::Phase::Ready
                    ) {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if us.phase == update::Phase::Available
                                && ui.button("下载新版本").clicked()
                            {
                                self.updater.download();
                            }
                            if us.phase == update::Phase::Ready
                                && ui.button("立即重启更新").clicked()
                            {
                                match self.updater.install() {
                                    Ok(()) => std::process::exit(0),
                                    Err(e) => self.notify(e),
                                }
                            }
                        });
                    }
                });
            });
    }

    /// 托盘事件、关窗拦截、托盘提示刷新
    fn handle_shell(&mut self, ctx: &egui::Context) {
        use egui::ViewportCommand;

        // 隐藏时靠这个定时器继续驱动 logic（每帧 ui 也会各自请求重绘）
        ctx.request_repaint_after(Duration::from_millis(400));

        let mut action = tray::Action::None;
        if let Some(t) = &self.tray {
            action = t.poll();
        }
        match action {
            tray::Action::Show => {
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            tray::Action::Quit => self.allow_close = true,
            tray::Action::None => {}
        }

        // 点 X 不退出：收进托盘，下载继续
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            if self.tray.is_some() {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                self.notify("已隐藏到托盘，下载继续（托盘右键可退出）");
            } else {
                self.allow_close = true;
            }
        }
        if self.allow_close && !self.close_sent {
            self.close_sent = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }

        if self.tip_at.elapsed() >= Duration::from_millis(1500) {
            self.tip_at = Instant::now();
            let (n, speed) = self.active_stats();
            let mut tip = if n > 0 {
                format!("DLKit · {} 个任务 · {}", n, fmt_speed(speed))
            } else {
                "DLKit · 空闲".to_string()
            };
            let upd = self.watch.count_updated();
            if upd > 0 {
                tip.push_str(&format!(" · {} 个更新", upd));
            }
            if let Some(t) = &mut self.tray {
                t.set_tooltip(tip);
            }
        }
    }

    /// 正在下载/排队的任务数与合计速度
    fn active_stats(&self) -> (usize, f64) {
        let mut n = 0usize;
        let mut speed = 0.0f64;
        for t in &self.tasks {
            if let Some(s) = snapshot(t) {
                if s.status == Status::Running || s.status == Status::Queued {
                    n += 1;
                    speed += s.speed;
                }
            }
        }
        (n, speed)
    }

    /// 跟踪页：直链更新检测列表（只提示，不自动下载）
    fn draw_watch(&mut self, ui: &mut egui::Ui) {
        let items = self.watch.snapshot();
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("链接跟踪").strong().size(15.0).color(TEXT));
                pill(ui, &format!("{} 个", items.len()), PRIMARY, tint(PRIMARY, 26));
                let updated = items
                    .iter()
                    .filter(|i| i.state == watch::STATE_UPDATED)
                    .count();
                if updated > 0 {
                    pill(ui, &format!("{} 个有更新", updated), ACCENT, tint(ACCENT, 26));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("全部检查").clicked() {
                        self.watch.check_all();
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "默认每 2 分钟检查一次，打开软件时先扫一遍；检测到更新只弹通知，下载要你自己点",
                )
                .size(11.0)
                .color(MUTED),
            );
        });

        ui.add_space(8.0);

        if items.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("还没有跟踪的链接").size(14.0).color(SECONDARY));
                    ui.label(
                        RichText::new("在下载页粘贴直链后点「跟踪」")
                            .size(11.5)
                            .color(MUTED),
                    );
                });
            });
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for it in items {
                    card_frame().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&it.name).strong().size(13.5).color(TEXT));
                            let (tag, fg, bg) = match it.state.as_str() {
                                watch::STATE_UPDATED => ("有更新", ACCENT, tint(ACCENT, 26)),
                                watch::STATE_FAILED => ("检查失败", DANGER, DANGER_BG),
                                watch::STATE_GONE => ("已失效", DANGER, DANGER_BG),
                                _ => ("正常", SUCCESS, SUCCESS_BG),
                            };
                            pill(ui, tag, fg, bg);
                            if it.paused {
                                pill(ui, "已暂停", MUTED, PANEL);
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("删除").clicked() {
                                        self.watch.remove(&it.id);
                                    }
                                    if ui
                                        .button(if it.paused { "恢复" } else { "暂停" })
                                        .clicked()
                                    {
                                        self.watch.toggle(&it.id);
                                    }
                                    if ui.button("下载").clicked() {
                                        self.download_watched(&it);
                                    }
                                    if ui.button("立即检查").clicked() {
                                        self.watch.check_now(&it.id);
                                    }
                                },
                            );
                        });
                        ui.add_space(2.0);
                        ui.label(RichText::new(&it.url).size(10.5).color(MUTED).monospace());
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(format!(
                                "{} · 上次检查 {} · 每 {} 分钟",
                                it.message,
                                watch::ago(it.last_checked),
                                it.interval_min
                            ))
                            .size(11.0)
                            .color(SECONDARY),
                        );
                    });
                    ui.add_space(6.0);
                }
            });
    }

    /// 把跟踪项交给下载引擎（用户手动确认后才调用）
    fn download_watched(&mut self, it: &watch::WatchItem) {
        let (kind, _) = engine::classify(&it.url);
        let task = Task::new(it.url.clone(), kind, it.name.clone());
        let t: Shared = Arc::new(Mutex::new(task));
        self.submit(
            t.clone(),
            &it.url,
            kind,
            self.task_dir(),
            None,
            it.headers.clone(),
        );
        self.tasks.push(t);
        self.selected = self.tasks.len().saturating_sub(1);
        self.page = Page::Download;
        self.notify(format!("已开始下载 {}", it.name));
    }

    /// 把输入框里的直链加入跟踪
    fn add_watch(&mut self) {
        let url = self.url.trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.notify("跟踪只支持 http/https 直链");
            return;
        }
        let (kind, why) = engine::classify(&url);
        if kind != engine::Kind::Http {
            self.notify(format!("这是{}，不是直链，跟踪只适用于文件地址", why));
            return;
        }
        let name = url
            .split('?')
            .next()
            .and_then(|u| u.rsplit('/').next())
            .unwrap_or("")
            .to_string();
        self.watch.add(url, name, Default::default());
        self.notify("已加入跟踪，有更新时会弹通知");
    }

    /// 自动更新横幅
    fn draw_update(&mut self, ui: &mut egui::Ui) {
        let st = self.updater.state();
        if st.phase == update::Phase::Idle {
            if !st.message.is_empty() && st.message != self.upd_msg {
                self.upd_msg = st.message.clone();
                self.notify(st.message.clone());
            }
            return;
        }
        let active = self.active_stats().0;

        let mut download = false;
        let mut install = false;
        let mut dismiss = false;
        let mut retry = false;
        let mut open = false;

        egui::Frame::NONE
            .fill(tint(ACCENT, 14))
            .stroke(Stroke::new(1.2, tint(ACCENT, 120)))
            .corner_radius(CornerRadius::same(16))
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    match st.phase {
                        update::Phase::Checking => {
                            pill(ui, "检查更新", PRIMARY, tint(PRIMARY, 26));
                            ui.label(RichText::new(&st.message).size(12.5).color(SECONDARY));
                        }
                        update::Phase::Available => {
                            pill(ui, "新版本", ACCENT, tint(ACCENT, 42));
                            ui.label(
                                RichText::new(format!(
                                    "v{} 可用（当前 v{}）",
                                    st.latest,
                                    update::Updater::current()
                                ))
                                .size(13.0)
                                .strong()
                                .color(TEXT),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let btn = egui::Button::new(
                                        RichText::new("下载并更新")
                                            .size(12.5)
                                            .color(Color32::WHITE),
                                    )
                                    .fill(PRIMARY)
                                    .corner_radius(CornerRadius::same(13));
                                    if ui.add(btn).clicked() {
                                        download = true;
                                    }
                                    if ui.button("稍后").clicked() {
                                        dismiss = true;
                                    }
                                    let note = if st.notes.is_empty() {
                                        "在浏览器中打开 Release 页面".to_string()
                                    } else {
                                        st.notes.chars().take(400).collect::<String>()
                                    };
                                    if ui
                                        .button("更新说明")
                                        .on_hover_text(note)
                                        .clicked()
                                    {
                                        open = true;
                                    }
                                },
                            );
                        }
                        update::Phase::Downloading => {
                            pill(ui, "下载中", PRIMARY, tint(PRIMARY, 26));
                            ui.label(RichText::new(&st.message).size(12.5).color(SECONDARY));
                        }
                        update::Phase::Ready => {
                            pill(ui, "已就绪", SUCCESS, SUCCESS_BG);
                            ui.label(RichText::new(&st.message).size(12.5).color(SECONDARY));
                            if active > 0 {
                                ui.label(
                                    RichText::new(format!(
                                        "重启会中断 {} 个任务（进度已保留）",
                                        active
                                    ))
                                    .size(11.0)
                                    .color(ACCENT),
                                );
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let btn = egui::Button::new(
                                        RichText::new("立即重启更新")
                                            .size(12.5)
                                            .color(Color32::WHITE),
                                    )
                                    .fill(SUCCESS)
                                    .corner_radius(CornerRadius::same(13));
                                    if ui.add(btn).clicked() {
                                        install = true;
                                    }
                                    if ui.button("稍后").clicked() {
                                        dismiss = true;
                                    }
                                },
                            );
                        }
                        update::Phase::Error => {
                            pill(ui, "更新失败", DANGER, DANGER_BG);
                            ui.label(RichText::new(&st.message).size(12.5).color(DANGER));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("重试").clicked() {
                                        retry = true;
                                    }
                                    if ui.button("关闭").clicked() {
                                        dismiss = true;
                                    }
                                },
                            );
                        }
                        update::Phase::Idle => {}
                    }
                });
                if st.phase == update::Phase::Downloading {
                    ui.add_space(6.0);
                    progress_bar(ui, st.progress);
                }
            });
        ui.add_space(8.0);

        if download {
            self.updater.download();
        }
        if retry {
            self.updater.check();
        }
        if dismiss {
            self.updater.dismiss();
        }
        if open {
            update::open_release_page();
        }
        if install {
            match self.updater.install() {
                Ok(()) => std::process::exit(0),
                Err(e) => self.notify(e),
            }
        }
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
            self.submit(t.clone(), &c.url, kind, self.task_dir(), None, headers);
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
        // 磁力 / 种子走 aria2：缺组件时自动装，装好自动起服务，用户不用先看文档
        if kind == engine::Kind::Torrent && !self.svc_online {
            self.ensure_aria2();
        }
        let task = Task::new(url.clone(), kind, "解析中…".into());
        let shared: Shared = Arc::new(Mutex::new(task));
        self.submit(
            shared.clone(),
            &url,
            kind,
            self.task_dir(),
            ol,
            BTreeMap::new(),
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

/// 写入 Windows 剪贴板（用于「复制报告」「复制错误」）
#[cfg(windows)]
fn clipboard_set(text: &str) {
    use std::os::windows::ffi::OsStrExt;
    extern "system" {
        fn OpenClipboard(hwnd: isize) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(format: u32, mem: isize) -> isize;
        fn GlobalAlloc(flags: u32, bytes: usize) -> isize;
        fn GlobalLock(mem: isize) -> *const u16;
        fn GlobalUnlock(mem: isize) -> i32;
    }
    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;
    let wide: Vec<u16> = std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        if OpenClipboard(0) == 0 {
            return;
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        if h != 0 {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                GlobalUnlock(h);
                SetClipboardData(CF_UNICODETEXT, h);
            }
        }
        CloseClipboard();
    }
}

#[cfg(not(windows))]
fn clipboard_set(_text: &str) {}

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
    av: Option<av::Report>,
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
        av: t.av.clone(),
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
    /// 窗口收进托盘后 `ui` 不再运行，但 `logic` 仍会被定时调用：
    /// 托盘菜单、关窗拦截和提示刷新都放这里。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_shell(ctx);
        // 窗口收进托盘时也要继续推进依赖安装与检查结果
        self.poll_installs();
        self.poll_diag();
        self.poll_av();
        self.pump_queue();
        self.apply_schedule();
    }

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
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("v{}", update::Updater::current()))
                            .size(11.0)
                            .color(MUTED),
                    );
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(format!("root: {}", self.root.display()))
                            .size(11.0)
                            .color(MUTED),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("检查更新")
                            .on_hover_text("从 GitHub Releases 检查新版本")
                            .clicked()
                        {
                            self.updater.check();
                            self.notify("正在检查更新…");
                        }
                        if ui.button("打开下载目录").clicked() {
                            if let Err(e) = open_path(&self.cfg.dir) {
                                self.notify(e);
                            }
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
                            self.start_services();
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

        // ---------- 左侧导航 ----------
        let missing = deps::DEPS
            .iter()
            .filter(|d| !deps::installed(&self.root, d))
            .count();
        egui::Panel::left("nav")
            .exact_size(146.0)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(10, 12)),
            )
            .show(ui, |ui| {
                nav_item(ui, "下载", Page::Download, &mut self.page);
                ui.add_space(4.0);
                let updated = self.watch.count_updated();
                let watch_label = if updated > 0 {
                    format!("跟踪 · {}", updated)
                } else {
                    "跟踪".to_string()
                };
                nav_item(ui, &watch_label, Page::Watch, &mut self.page);
                ui.add_space(4.0);
                nav_item(ui, "账号", Page::Accounts, &mut self.page);
                ui.add_space(4.0);
                nav_item(ui, "设置", Page::Settings, &mut self.page);
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    if missing > 0 && ui.button(format!("缺 {} 个依赖", missing)).clicked() {
                        self.page = Page::Settings;
                    }
                    ui.label(
                        RichText::new(format!("v{}", update::Updater::current()))
                            .size(10.5)
                            .color(MUTED),
                    );
                });
            });

        if self.page == Page::Download {
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
                                    .desired_width(ui.available_width() - 200.0)
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
                            if pill_button(ui, "跟踪")
                                .on_hover_text("检测到这个直链有更新时弹通知（不会自动下载）")
                                .clicked()
                            {
                                self.add_watch();
                            }
                            if circle_button(ui, "＋").on_hover_text("添加任务").clicked() || enter {
                                self.add_task();
                            }
                        });
                        ui.add_space(5.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("OpenList 路径").size(11.5).color(MUTED));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.ol_path)
                                    .hint_text(RichText::new("/挂载点/文件名").color(MUTED))
                                    .desired_width(200.0)
                                    .margin(egui::vec2(6.0, 3.0)),
                            );
                            ui.label(
                                RichText::new("并发 / 限速 / 目录 / 引擎在「设置」里")
                                    .size(10.5)
                                    .color(MUTED),
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

        }

        // ---------- 任务列表 / 设置 ----------
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(PAGE)
                    .inner_margin(Margin { left: 18, right: 18, top: 10, bottom: 10 }),
            )
            .show(ui, |ui| {
                if self.page == Page::Settings {
                    self.draw_settings(ui);
                    return;
                }
                if self.page == Page::Accounts {
                    self.draw_accounts(ui);
                    return;
                }
                if self.page == Page::Watch {
                    self.draw_watch(ui);
                    return;
                }
                self.draw_first_run(ui);
                self.draw_update(ui);
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
                let mut card_action: Option<CardAction> = None;
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
                                        let name = ui.add(
                                            egui::Label::new(
                                                RichText::new(&snap.name)
                                                    .strong()
                                                    .size(14.5)
                                                    .color(TEXT),
                                            )
                                            .sense(egui::Sense::click()),
                                        );
                                        if name.double_clicked() && snap.status == Status::Done {
                                            if let Some(out) = &snap.output {
                                                if let Err(e) = open_path(out) {
                                                    card_action =
                                                        Some(CardAction::Notice(e));
                                                }
                                            }
                                        }
                                        if name.hovered() && snap.status == Status::Done {
                                            name.on_hover_text("双击打开文件");
                                        }
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
                                                if snap.status == Status::Done {
                                                    if let Some(out) = &snap.output {
                                                        if ui.button("打开文件").clicked() {
                                                            if let Err(e) = open_path(out) {
                                                                card_action = Some(
                                                                    CardAction::Notice(e),
                                                                );
                                                            }
                                                        }
                                                    }
                                                    if ui.button("打开目录").clicked() {
                                                        let dir = snap
                                                            .output
                                                            .as_ref()
                                                            .and_then(|p| p.parent())
                                                            .map(|p| p.to_path_buf())
                                                            .unwrap_or_else(|| {
                                                                self.cfg.dir.clone()
                                                            });
                                                        if let Err(e) = open_path(&dir) {
                                                            card_action =
                                                                Some(CardAction::Notice(e));
                                                        }
                                                    }
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
                                        // 失败时给一个能点的下一步，而不是只报错
                                        if snap.status == Status::Failed {
                                            ui.add_space(4.0);
                                            ui.horizontal(|ui| {
                                                let m = snap.message.as_str();
                                                if m.contains("aria2") {
                                                    if ui.button("启动服务").clicked() {
                                                        card_action = Some(CardAction::Services);
                                                    }
                                                } else if m.contains("403")
                                                    || m.contains("401")
                                                    || m.contains("Cookie")
                                                    || m.contains("登录")
                                                {
                                                    if ui.button("去账号页").clicked() {
                                                        card_action = Some(CardAction::Accounts);
                                                    }
                                                } else if m.contains("磁盘空间") {
                                                    if ui.button("去设置换目录").clicked() {
                                                        card_action = Some(CardAction::Settings);
                                                    }
                                                } else if m.contains("yt-dlp")
                                                    || m.contains("ffmpeg")
                                                    || m.contains("缺少")
                                                {
                                                    if ui.button("补齐组件").clicked() {
                                                        card_action = Some(CardAction::Settings);
                                                    }
                                                }
                                                if ui.button("复制错误").clicked() {
                                                    card_action = Some(CardAction::CopyError(
                                                        snap.message.clone(),
                                                    ));
                                                }
                                            });
                                        }
                                    }
                                    if let Some(out) = &snap.output {
                                        ui.label(
                                            RichText::new(out.to_string_lossy())
                                                .size(11.0)
                                                .color(MUTED),
                                        );
                                        // 增强版：文件还在沙箱里
                                        if ENHANCED
                                            && snap.status == Status::Done
                                            && out
                                                .to_string_lossy()
                                                .contains(".dlkit-sandbox")
                                        {
                                            ui.horizontal(|ui| {
                                                pill(
                                                    ui,
                                                    "沙箱隔离中",
                                                    ACCENT,
                                                    tint(ACCENT, 26),
                                                );
                                                ui.label(
                                                    RichText::new("检查通过后自动放行")
                                                        .size(10.5)
                                                        .color(MUTED),
                                                );
                                                if ui.button("立即放行").clicked() {
                                                    card_action =
                                                        Some(CardAction::Release(out.clone()));
                                                }
                                            });
                                        }
                                    }
                                    // 增强版：安全检查结果
                                    if ENHANCED {
                                        if let Some(rep) = &snap.av {
                                            ui.add_space(4.0);
                                            let (fg, bg) = match rep.state {
                                                av::State::Clean => (SUCCESS, SUCCESS_BG),
                                                av::State::Suspicious => (ACCENT, tint(ACCENT, 26)),
                                                av::State::Threat => (DANGER, DANGER_BG),
                                                _ => (MUTED, PANEL),
                                            };
                                            ui.horizontal(|ui| {
                                                pill(ui, "安全检查", fg, bg);
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{}（{}）",
                                                        rep.summary(),
                                                        rep.engine
                                                    ))
                                                    .size(11.0)
                                                    .color(fg),
                                                );
                                                if rep.alarming() {
                                                    if ui.button("隔离").clicked() {
                                                        if let Some(p) = &snap.output {
                                                            card_action = Some(
                                                                CardAction::Quarantine(p.clone()),
                                                            );
                                                        }
                                                    }
                                                    if ui.button("保留").clicked() {
                                                        if let Some(p) = &snap.output {
                                                            card_action = Some(
                                                                CardAction::AcceptAv(p.clone()),
                                                            );
                                                        }
                                                    }
                                                }
                                            });
                                            for f in &rep.findings {
                                                ui.label(
                                                    RichText::new(format!("· {}", f))
                                                        .size(10.5)
                                                        .color(MUTED),
                                                );
                                            }
                                        } else if snap.status == Status::Done
                                            && snap
                                                .output
                                                .as_ref()
                                                .map(|p| av::eligible(p))
                                                .unwrap_or(false)
                                        {
                                            ui.add_space(3.0);
                                            ui.label(
                                                RichText::new("安全检查中…")
                                                    .size(11.0)
                                                    .color(MUTED),
                                            );
                                        }
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
                match card_action {
                    Some(CardAction::Services) => self.start_services(),
                    Some(CardAction::Accounts) => self.page = Page::Accounts,
                    Some(CardAction::Settings) => self.page = Page::Settings,
                    Some(CardAction::CopyError(m)) => {
                        clipboard_set(&m);
                        self.notify("错误信息已复制");
                    }
                    Some(CardAction::Quarantine(p)) => {
                        match av::quarantine(&self.root, &p, "用户确认隔离") {
                            Ok(dest) => {
                                self.update_av(&p, |r| {
                                    r.state = av::State::Skipped;
                                    r.accepted = true;
                                    r.findings
                                        .push(format!("已隔离到 {}", dest.display()));
                                });
                                self.notify(format!("已隔离到 {}", dest.display()));
                            }
                            Err(e) => self.notify(e),
                        }
                    }
                    Some(CardAction::AcceptAv(p)) => {
                        self.update_av(&p, |r| r.accepted = true);
                        self.notify("已保留该文件");
                    }
                    Some(CardAction::Release(p)) => {
                        match av::release(&p, &self.cfg.dir) {
                            Ok(dest) => {
                                for t in &self.tasks {
                                    if let Ok(mut task) = t.lock() {
                                        if task.output.as_deref() == Some(p.as_path()) {
                                            task.output = Some(dest.clone());
                                            if let Some(r) = task.av.as_mut() {
                                                r.accepted = true;
                                                r.findings.push(format!(
                                                    "手动放行到 {}",
                                                    dest.display()
                                                ));
                                            }
                                        }
                                    }
                                }
                                self.notify(format!("已放行到 {}", dest.display()));
                            }
                            Err(e) => self.notify(e),
                        }
                    }
                    Some(CardAction::Notice(m)) => self.notify(m),
                    None => {}
                }
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

// ---------------- 基准测试 ----------------

/// `--bench <url>`：跑下载并输出可对比的指标（每个 run 一行 JSON）
fn bench() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let url = match args.get(2) {
        Some(u) => u.clone(),
        None => {
            println!("用法: DLKit.exe --bench <url> [-c 32] [--repeat 3] [-d 目录]");
            return 2;
        }
    };
    let mut conn = 32usize;
    let mut repeat = 1usize;
    let mut base = std::env::temp_dir().join("dlkit-bench");
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "-c" => {
                if let Some(v) = args.get(i + 1).and_then(|v| v.parse().ok()) {
                    conn = v;
                }
                i += 2;
            }
            "--repeat" => {
                if let Some(v) = args.get(i + 1).and_then(|v| v.parse().ok()) {
                    repeat = v;
                }
                i += 2;
            }
            "-d" => {
                if let Some(v) = args.get(i + 1) {
                    base = PathBuf::from(v);
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    for run in 0..repeat {
        match bench_once(&url, conn, &base, run) {
            Some(v) => println!("{}", v),
            None => println!("{{\"run\":{},\"error\":\"读不到任务状态\"}}", run),
        }
    }
    0
}

fn bench_once(
    url: &str,
    conn: usize,
    base: &std::path::Path,
    run: usize,
) -> Option<serde_json::Value> {
    let dir = base.join(format!("c{}-r{}", conn, run));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let (kind, _) = engine::classify(url);
    let task = Task::new(url.to_string(), kind, "解析中…".into());
    let shared: Shared = Arc::new(Mutex::new(task));
    let t0 = Instant::now();
    engine::spawn(
        engine::find_root(),
        shared.clone(),
        dir,
        conn,
        "auto".into(),
        None,
    );
    let mut peak = 0.0f64;
    let mut ttfb = 0u64;
    let mut tail_at: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let snap = snapshot(&shared)?;
        if snap.done > 0 && ttfb == 0 {
            ttfb = t0.elapsed().as_millis() as u64;
        }
        peak = peak.max(snap.net_speed.max(snap.speed));
        if snap.total > 0 && snap.done as f64 >= snap.total as f64 * 0.95 && tail_at.is_none() {
            tail_at = Some(Instant::now());
        }
        if snap.status != Status::Running && snap.status != Status::Queued {
            let elapsed = t0.elapsed();
            let secs = elapsed.as_secs_f64();
            let size = snap.total.max(snap.done);
            let avg = if secs > 0.0 {
                size as f64 / 1048576.0 / secs
            } else {
                0.0
            };
            let tail = tail_at.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
            return Some(serde_json::json!({
                "run": run,
                "conn": conn,
                "status": format!("{:?}", snap.status),
                "size_bytes": size,
                "time_ms": elapsed.as_millis() as u64,
                "avg_mbps": (avg * 100.0).round() / 100.0,
                "peak_mbps": (peak * 100.0).round() / 100.0,
                "ttfb_ms": ttfb,
                "tail_ms": tail,
                "message": snap.message,
            }));
        }
    }
}

/// `--bench-scan <页面>`：测解析耗时与候选数量
fn bench_scan() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let url = match args.get(2) {
        Some(u) => u.clone(),
        None => {
            println!("用法: DLKit.exe --bench-scan <页面> [--repeat 3]");
            return 2;
        }
    };
    let mut repeat = 1usize;
    if args.get(3).map(|s| s == "--repeat").unwrap_or(false) {
        repeat = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(1);
    }
    for run in 0..repeat {
        let t = Instant::now();
        let st = scanner::start(url.clone());
        loop {
            std::thread::sleep(Duration::from_millis(20));
            let (running, error, n) = {
                let s = match st.lock() {
                    Ok(s) => s,
                    Err(_) => return 1,
                };
                (s.running, s.error.clone(), s.candidates.len())
            };
            if running {
                continue;
            }
            println!(
                "{}",
                serde_json::json!({
                    "run": run,
                    "ms": t.elapsed().as_millis() as u64,
                    "candidates": n,
                    "error": error,
                })
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    0
}

/// `--av-scan <文件>`：手动跑一次安全检查（排查误报时用）
fn av_scan_cmd() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let path = match args.get(2) {
        Some(p) => PathBuf::from(p),
        None => {
            println!("用法: DLKit.exe --av-scan <文件>");
            return 2;
        }
    };
    let rep = av::scan(&path, true, 4.0, &av::load_cloud(&engine::find_root()));
    match serde_json::to_string_pretty(&rep) {
        Ok(s) => println!("{}", s),
        Err(e) => println!("序列化失败：{}", e),
    }
    0
}

/// 用系统外壳打开文件/目录。
/// 不用 `Command::new("explorer")`：程序以管理员身份运行时，explorer 的 DDE 请求
/// 会被已有的资源管理器实例忽略，表现为「点了没反应」。ShellExecuteW 交给外壳处理，
/// 两种情况都能正常打开，失败时还能拿到错误码。
#[cfg(windows)]
fn shell_exec(path: &std::path::Path, params: Option<&str>) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: isize,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
    }
    let wide = |s: &std::ffi::OsStr| -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    };
    let file = wide(path.as_os_str());
    let op = wide(std::ffi::OsStr::new("open"));
    let par = params.map(|p| wide(std::ffi::OsStr::new(p)));
    let rc = unsafe {
        ShellExecuteW(
            0,
            op.as_ptr(),
            file.as_ptr(),
            par.as_ref().map(|v| v.as_ptr()).unwrap_or(std::ptr::null()),
            std::ptr::null(),
            1,
        )
    };
    if rc > 32 {
        Ok(())
    } else {
        Err(format!("系统无法打开：{}（错误码 {}）", path.display(), rc))
    }
}

#[cfg(not(windows))]
fn shell_exec(path: &std::path::Path, _params: Option<&str>) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn open_path(path: &std::path::Path) -> Result<(), String> {
    shell_exec(path, None)
}

/// 打开所在文件夹并选中该文件
fn reveal_path(path: &std::path::Path) -> Result<(), String> {
    let params = format!("/select,\"{}\"", path.display());
    shell_exec(std::path::Path::new("explorer.exe"), Some(&params))
}

fn main() -> eframe::Result {
    setup::install_panic_hook();
    if std::env::args().nth(1).as_deref() == Some("--open") {
        let p = std::env::args().nth(2).unwrap_or_default();
        match open_path(std::path::Path::new(&p)) {
            Ok(_) => {
                println!("ok");
                std::process::exit(0);
            }
            Err(e) => {
                println!("{}", e);
                std::process::exit(1);
            }
        }
    }
    if std::env::args().nth(1).as_deref() == Some("--av-scan") {
        std::process::exit(av_scan_cmd());
    }
    if std::env::args().nth(1).as_deref() == Some("--bench") {
        std::process::exit(bench());
    }
    if std::env::args().nth(1).as_deref() == Some("--bench-scan") {
        std::process::exit(bench_scan());
    }
    if std::env::args().nth(1).as_deref() == Some("--headless") {
        std::process::exit(headless());
    }
    if std::env::args().nth(1).as_deref() == Some("--scan") {
        std::process::exit(headless_scan());
    }
    if std::env::args().nth(1).as_deref() == Some("--mcp") {
        std::process::exit(mcp::run());
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

