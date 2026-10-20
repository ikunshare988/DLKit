//! MCP（Model Context Protocol）服务端：让 AI 客户端直接驱动 DLKit 下载。
//!
//! 启动方式：`DLKit.exe --mcp`，走 stdin/stdout，一行一条 JSON-RPC 消息。
//! 约定：stdout 只写协议消息，任何诊断信息都走 stderr，避免污染通道。

use crate::{engine, load_config, scanner};
use engine::{Shared, Status, Task};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 支持的协议版本，从新到旧；客户端请求哪个就回哪个
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// 单次解析页面的最长等待
const SCAN_TIMEOUT: u64 = 120;
/// download 的 wait_seconds 上限，避免 AI 一次调用挂太久
const MAX_WAIT: u64 = 600;

struct Entry {
    id: String,
    task: Shared,
}

/// 一次解析的完整结果，供 scan_page 与 download_from_page 共用
struct ScanOut {
    candidates: Vec<scanner::Candidate>,
    headers: BTreeMap<String, String>,
    recommended: Option<usize>,
    reason: String,
}

pub struct Server {
    root: PathBuf,
    dir: PathBuf,
    conn: usize,
    tasks: Vec<Entry>,
    seq: u64,
}

pub fn run() -> i32 {
    let mut srv = Server::new();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }
        if let Some(reply) = srv.handle(raw) {
            if serde_json::to_writer(&mut out, &reply).is_err() {
                break;
            }
            let _ = out.write_all(b"\n");
            if out.flush().is_err() {
                break;
            }
        }
    }
    0
}

impl Server {
    fn new() -> Server {
        let cfg = load_config();
        Server {
            root: engine::find_root(),
            dir: cfg.dir,
            conn: cfg.conn,
            tasks: Vec::new(),
            seq: 0,
        }
    }

    fn handle(&mut self, raw: &str) -> Option<Value> {
        let msg: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => return Some(error_msg(Value::Null, -32700, format!("JSON 解析失败：{}", e))),
        };
        // 通知（没有 id）不需要回应
        let id = msg.get("id")?.clone();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        match self.dispatch(method, &params) {
            Ok(result) => Some(json!({"jsonrpc": "2.0", "id": id, "result": result})),
            Err((code, message)) => Some(error_msg(id, code, message)),
        }
    }

    fn dispatch(&mut self, method: &str, p: &Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => {
                let asked = p
                    .get("protocolVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or(VERSIONS[0]);
                let version = if VERSIONS.contains(&asked) {
                    asked
                } else {
                    VERSIONS[0]
                };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "dlkit", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "DLKit 是 Windows 下载器。用户给下载页链接时先用 scan_page 看有哪些文件，\
再用 download_from_page 挑一个开始下载；给直链时用 download。下载是异步的，返回 task_id，\
用 task_status 或 list_tasks 查询进度；文件较大时不要一次 wait_seconds 等太久，先拿 task_id 再轮询。"
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => Ok(self.call_tool(p)),
            "resources/list" => Ok(json!({"resources": []})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            other => Err((-32601, format!("不支持的方法：{}", other))),
        }
    }

    fn call_tool(&mut self, p: &Value) -> Value {
        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        let args = p.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let outcome = match name {
            "scan_page" => self.scan_page(&args),
            "download" => self.download(&args),
            "download_from_page" => self.download_from_page(&args),
            "task_status" => self.task_status(&args),
            "list_tasks" => self.list_tasks(),
            "cancel_task" => self.cancel_task(&args),
            "verify_file" => verify_file(&args),
            other => Err(format!("未知工具：{}", other)),
        };
        match outcome {
            Ok(v) => {
                let text = serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into());
                json!({
                    "content": [{"type": "text", "text": text}],
                    "structuredContent": v,
                    "isError": false
                })
            }
            Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
        }
    }

    fn scan_page(&self, args: &Value) -> Result<Value, String> {
        let page = text_arg(args, "url")?;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(40)
            .clamp(1, 200) as usize;
        let out = run_scan(&page)?;
        Ok(scan_json(&page, &out, limit))
    }

    fn download(&mut self, args: &Value) -> Result<Value, String> {
        let url = text_arg(args, "url")?;
        let dir = args
            .get("dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.dir.clone());
        let conn = args
            .get("conn")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.conn as u64)
            .clamp(1, 128) as usize;
        let wait = wait_arg(args);
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("解析中…")
            .to_string();
        let sha = args
            .get("sha256")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| s.len() == 64);
        let id = self.start_download(&url, &name, sha, dir, conn, BTreeMap::new());
        self.report(&id, wait)
    }

    fn download_from_page(&mut self, args: &Value) -> Result<Value, String> {
        let page = text_arg(args, "url")?;
        let wait = wait_arg(args);
        let dir = args
            .get("dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.dir.clone());
        let conn = args
            .get("conn")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.conn as u64)
            .clamp(1, 128) as usize;
        let out = run_scan(&page)?;
        if out.candidates.is_empty() {
            return Err("页面里没有找到可直接下载的文件".into());
        }
        let pick = match args.get("index").and_then(|v| v.as_u64()) {
            Some(i) => {
                let i = i as usize;
                if i >= out.candidates.len() {
                    return Err(format!("index 越界：{}（共 {} 项）", i, out.candidates.len()));
                }
                i
            }
            None => out
                .recommended
                .filter(|i| *i < out.candidates.len())
                .unwrap_or(0),
        };
        let c = out.candidates[pick].clone();
        // 优先用该文件所在页当 Referer（深度探测时它可能不是入口页）
        let mut headers = out.headers.clone();
        if !c.page.is_empty() {
            headers.insert("Referer".into(), c.page.clone());
        }
        let id = self.start_download(&c.url, &c.title, c.sha256.clone(), dir, conn, headers);
        let mut v = self.report(&id, wait)?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("page".into(), json!(page));
            obj.insert("picked_index".into(), json!(pick));
            obj.insert("picked_title".into(), json!(c.title));
        }
        Ok(v)
    }

    fn task_status(&self, args: &Value) -> Result<Value, String> {
        let id = text_arg(args, "task_id")?;
        let entry = self
            .tasks
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("任务不存在：{}", id))?;
        Ok(snap_json(&entry.id, &entry.task))
    }

    fn list_tasks(&self) -> Result<Value, String> {
        let tasks: Vec<Value> = self
            .tasks
            .iter()
            .map(|e| snap_json(&e.id, &e.task))
            .collect();
        Ok(json!({"count": tasks.len(), "tasks": tasks}))
    }

    fn cancel_task(&self, args: &Value) -> Result<Value, String> {
        let id = text_arg(args, "task_id")?;
        let entry = self
            .tasks
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("任务不存在：{}", id))?;
        if let Ok(t) = entry.task.lock() {
            t.cancel.store(true, Ordering::SeqCst);
        }
        Ok(snap_json(&entry.id, &entry.task))
    }

    fn start_download(
        &mut self,
        url: &str,
        name: &str,
        sha: Option<String>,
        dir: PathBuf,
        conn: usize,
        headers: BTreeMap<String, String>,
    ) -> String {
        self.seq += 1;
        let id = format!("t{}", self.seq);
        let (kind, _) = engine::classify(url);
        let mut task = Task::new(url.to_string(), kind, name.to_string());
        task.sha256 = sha;
        let shared: Shared = Arc::new(Mutex::new(task));
        engine::spawn_ext(
            self.root.clone(),
            shared.clone(),
            dir,
            conn,
            "auto".into(),
            None,
            headers,
        );
        self.tasks.push(Entry {
            id: id.clone(),
            task: shared,
        });
        id
    }

    /// 取一次快照；wait > 0 时最多阻塞到任务结束或超时。
    /// 大文件只等一部分时间就返回 task_id，让 AI 自己轮询，避免一次调用卡死。
    fn report(&self, id: &str, wait: u64) -> Result<Value, String> {
        let entry = self
            .tasks
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("任务不存在：{}", id))?;
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let snap = snap_json(&entry.id, &entry.task);
            let finished = !matches!(snap["status"].as_str(), Some("queued") | Some("running"));
            if finished || wait == 0 || Instant::now() >= deadline {
                return Ok(snap);
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }
}

fn run_scan(page: &str) -> Result<ScanOut, String> {
    let st = scanner::start(page.to_string());
    let deadline = Instant::now() + Duration::from_secs(SCAN_TIMEOUT);
    loop {
        let (running, message, error, candidates, headers, recommended, reason) = {
            let s = st.lock().map_err(|_| "解析线程状态丢失".to_string())?;
            (
                s.running,
                s.message.clone(),
                s.error.clone(),
                s.candidates.clone(),
                s.headers.clone(),
                s.recommended,
                s.recommend_reason.clone(),
            )
        };
        if !running {
            if !error.is_empty() {
                return Err(format!("解析失败：{}", error));
            }
            return Ok(ScanOut {
                candidates,
                headers,
                recommended,
                reason,
            });
        }
        if Instant::now() >= deadline {
            scanner::stop(&st);
            return Err(format!("解析超时（{} 秒）：{}", SCAN_TIMEOUT, message));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn scan_json(page: &str, out: &ScanOut, limit: usize) -> Value {
    let list: Vec<Value> = out
        .candidates
        .iter()
        .take(limit)
        .enumerate()
        .map(|(i, c)| {
            json!({
                "index": i,
                "title": c.title,
                "url": c.url,
                "size_bytes": c.size,
                "size": if c.size > 0 { crate::fmt_size(c.size) } else { "未知".to_string() },
                "type": c.type_label,
                "note": c.note,
                "sha256": c.sha256,
                "recommended": out.recommended == Some(i),
            })
        })
        .collect();
    json!({
        "page": page,
        "total": out.candidates.len(),
        "returned": list.len(),
        "recommended_index": out.recommended,
        "recommend_reason": out.reason,
        "candidates": list,
        "next": "用 download_from_page 传 index（省略则用推荐项）直接下载，或把 candidates[].url 交给 download"
    })
}

fn snap_json(id: &str, task: &Shared) -> Value {
    let t = match task.lock() {
        Ok(t) => t,
        Err(_) => return json!({"id": id, "status": "unknown"}),
    };
    let percent = if t.total > 0 {
        (t.done as f64 * 1000.0 / t.total as f64).round() / 10.0
    } else {
        0.0
    };
    let log_tail: Vec<String> = t.log.iter().rev().take(8).rev().cloned().collect();
    json!({
        "id": id,
        "status": status_text(t.status),
        "name": t.name,
        "url": t.url,
        "kind": t.kind.label(),
        "percent": percent,
        "downloaded_bytes": t.done,
        "total_bytes": t.total,
        "speed_mbps": round2(t.net_speed.max(t.speed)),
        "disk_mbps": round2(t.disk_speed),
        "message": t.message,
        "output": t.output.as_ref().map(|p| p.to_string_lossy().to_string()),
        "sha256_expected": t.sha256,
        "log_tail": log_tail,
    })
}

fn status_text(s: Status) -> &'static str {
    match s {
        Status::Queued => "queued",
        Status::Running => "running",
        Status::Paused => "paused",
        Status::Done => "done",
        Status::Failed => "failed",
        Status::Cancelled => "cancelled",
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn verify_file(args: &Value) -> Result<Value, String> {
    let path = PathBuf::from(text_arg(args, "path")?);
    if !path.is_file() {
        return Err(format!("文件不存在：{}", path.display()));
    }
    let got = engine::sha256_of(&path).ok_or_else(|| format!("读取失败：{}", path.display()))?;
    let want = args
        .get("expected_sha256")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    let matched = want.as_ref().map(|w| w == &got);
    Ok(json!({
        "path": path.to_string_lossy(),
        "size_bytes": std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        "sha256": got,
        "expected_sha256": want,
        "matched": matched,
    }))
}

fn text_arg(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("缺少参数 {}（需要非空字符串）", key))
}

fn wait_arg(args: &Value) -> u64 {
    args.get("wait_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(MAX_WAIT)
}

fn error_msg(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

fn tools() -> Value {
    json!([
        {
            "name": "scan_page",
            "description": "解析一个下载页，列出其中可下载的文件：文件名、直链、大小、类型、官方 SHA256，\
并标出按本机平台自动选中的推荐项。用户给的是网页（下载站、GitHub Release 页、软件官网）而不是直链时先用它。只读，不会开始下载。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "下载页 URL，必须是 http/https"},
                    "limit": {"type": "integer", "description": "最多返回多少个候选，默认 40，上限 200"}
                },
                "required": ["url"]
            }
        },
        {
            "name": "download",
            "description": "开始下载一个链接（http/https 直链、磁力、B站等媒体页、openlist:/path 网盘路径）。\
调用立即返回 task_id 与当前进度，后续用 task_status 轮询。同一链接重复调用会新建任务，续传由引擎自动处理。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "要下载的链接"},
                    "dir": {"type": "string", "description": "保存目录，默认用设置页里配置的目录"},
                    "conn": {"type": "integer", "description": "并发连接数 1-128，默认跟随设置页"},
                    "name": {"type": "string", "description": "可选，指定保存的文件名"},
                    "sha256": {"type": "string", "description": "可选，64 位十六进制；下载完成后自动核对，不一致判为失败"},
                    "wait_seconds": {"type": "integer", "description": "最多阻塞等待多少秒（0 表示立刻返回，默认 0，上限 600）"}
                },
                "required": ["url"]
            }
        },
        {
            "name": "download_from_page",
            "description": "一步完成“解析下载页 → 挑文件 → 开始下载”。适合“帮我把这个页面的 Windows 版下下来”。\
index 省略时用按本机平台自动选中的推荐项；返回里会说明实际选了哪一项。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "下载页 URL"},
                    "index": {"type": "integer", "description": "候选项下标，来自 scan_page 的 index；省略则用推荐项"},
                    "dir": {"type": "string", "description": "保存目录，默认用设置页里配置的目录"},
                    "conn": {"type": "integer", "description": "并发连接数 1-128"},
                    "wait_seconds": {"type": "integer", "description": "最多阻塞等待多少秒，默认 0"}
                },
                "required": ["url"]
            }
        },
        {
            "name": "task_status",
            "description": "查询单个下载任务：状态（queued/running/paused/done/failed/cancelled）、进度百分比、速度、\
已下载与总字节数、保存路径、近期日志。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": {"type": "string", "description": "download / download_from_page 返回的 task_id"}
                },
                "required": ["task_id"]
            }
        },
        {
            "name": "list_tasks",
            "description": "列出本进程内所有下载任务及其进度，用于总览或找出失败的任务。",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "cancel_task",
            "description": "取消一个下载任务（已写入的部分保留，可用同一链接重新添加继续）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": {"type": "string", "description": "要取消的 task_id"}
                },
                "required": ["task_id"]
            }
        },
        {
            "name": "verify_file",
            "description": "计算本地文件的 SHA256，可与期望值比对。用户要求“核对下载的文件是否完整/是否官方”时使用。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "本地文件路径"},
                    "expected_sha256": {"type": "string", "description": "可选，期望的 SHA256（不区分大小写）"}
                },
                "required": ["path"]
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_initialize_and_lists_tools() {
        let mut srv = Server::new();
        let init = srv
            .handle(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#)
            .expect("initialize 应有回应");
        assert_eq!(init["result"]["serverInfo"]["name"], "dlkit");
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");

        let list = srv
            .handle(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .expect("tools/list 应有回应");
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"scan_page"));
        assert!(names.contains(&"download_from_page"));
        assert!(names.contains(&"verify_file"));
    }

    #[test]
    fn notifications_get_no_reply() {
        let mut srv = Server::new();
        assert!(srv
            .handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
    }

    #[test]
    fn unknown_method_is_an_error() {
        let mut srv = Server::new();
        let r = srv
            .handle(r#"{"jsonrpc":"2.0","id":9,"method":"no/such"}"#)
            .unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }

    #[test]
    fn bad_arguments_stay_inside_tool_result() {
        let mut srv = Server::new();
        let r = srv
            .handle(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"download","arguments":{}}}"#)
            .unwrap();
        assert!(r.get("result").is_some(), "工具参数错误应返回 result 而不是协议错误");
        assert_eq!(r["result"]["isError"], true);
    }
}
