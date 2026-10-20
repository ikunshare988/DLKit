//! 增强版策略配置：按站点/类型覆盖下载参数、全局预算、时段限速。
//!
//! 对应 `config/policy.json`。读不到或格式错误时退回默认值，
//! 不会因为一个字段写错就启动不了。

use serde::Deserialize;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Default, Deserialize)]
pub struct Budget {
    #[serde(default)]
    pub max_connections: Option<usize>,
    #[serde(default)]
    pub max_active_tasks: Option<usize>,
}

#[derive(Clone, Deserialize)]
pub struct Slot {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub limit_mbps: f64,
}

#[derive(Clone, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub match_hosts: Vec<String>,
    /// http / torrent / media / netdisk / openlist
    #[serde(default)]
    pub match_kinds: Vec<String>,
    #[serde(default)]
    pub conn: Option<usize>,
    #[serde(default)]
    pub limit_mbps: Option<f64>,
    /// high / normal / low：任务排队时的优先级
    #[serde(default)]
    pub priority: Option<String>,
}

#[derive(Default, Deserialize)]
struct Raw {
    #[serde(default)]
    budget: Budget,
    #[serde(default)]
    schedule: Vec<Slot>,
    #[serde(default)]
    profiles: Vec<Profile>,
}

#[derive(Default)]
pub struct Policy {
    pub budget: Budget,
    schedule: Vec<Slot>,
    profiles: Vec<Profile>,
    /// 配置解析失败的原因，界面上提示用
    pub error: String,
}

impl Policy {
    pub fn load(root: &Path) -> Policy {
        let path = root.join("config").join("policy.json");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return Policy::default(),
        };
        let text = text.trim_start_matches('\u{feff}');
        match serde_json::from_str::<Raw>(text) {
            Ok(raw) => Policy {
                budget: raw.budget,
                schedule: raw.schedule,
                profiles: raw.profiles,
                error: String::new(),
            },
            Err(e) => Policy {
                error: format!("config/policy.json 第 {} 行附近格式有误：{}", e.line(), e),
                ..Default::default()
            },
        }
    }

    /// 同时下载的任务上限
    pub fn max_active(&self) -> Option<usize> {
        self.budget.max_active_tasks.filter(|v| *v > 0)
    }

    fn profile_for(&self, url: &str, kind: &str) -> Option<&Profile> {
        let host = host_of(url);
        self.profiles.iter().find(|p| {
            let host_ok = p.match_hosts.is_empty()
                || p
                    .match_hosts
                    .iter()
                    .any(|h| host == *h || host.ends_with(&format!(".{}", h)));
            let kind_ok = p.match_kinds.is_empty() || p.match_kinds.iter().any(|k| k == kind);
            host_ok && kind_ok
        })
    }

    /// 排队优先级：2 高 / 1 普通 / 0 低
    pub fn priority_for(&self, url: &str, kind: &str) -> i32 {
        match self
            .profile_for(url, kind)
            .and_then(|p| p.priority.as_deref())
        {
            Some("high") => 2,
            Some("low") => 0,
            _ => 1,
        }
    }

    /// profile 里的单任务限速（MB/s）
    pub fn limit_for(&self, url: &str, kind: &str) -> Option<f64> {
        self.profile_for(url, kind)
            .and_then(|p| p.limit_mbps)
            .filter(|v| *v > 0.0)
    }

    /// 按 profile 覆盖并发数；再受全局预算约束
    pub fn conn_for(&self, url: &str, kind: &str, fallback: usize) -> usize {
        let mut conn = self
            .profile_for(url, kind)
            .and_then(|p| p.conn)
            .unwrap_or(fallback);
        if let Some(max) = self.budget.max_connections {
            conn = conn.min(max.max(1));
        }
        conn.clamp(1, 128)
    }

    /// 当前时段是否命中限速规则；返回 MB/s
    pub fn limit_now(&self) -> Option<f64> {
        let now = local_minutes();
        self.schedule
            .iter()
            .find(|s| in_slot(&s.from, &s.to, now))
            .map(|s| s.limit_mbps)
            .filter(|v| *v > 0.0)
    }

    /// 界面上的一句话描述
    pub fn describe(&self) -> String {
        if !self.error.is_empty() {
            return self.error.clone();
        }
        if self.profiles.is_empty() && self.schedule.is_empty() {
            return "未配置策略（可选）：config/policy.json".into();
        }
        let mut parts = Vec::new();
        if !self.profiles.is_empty() {
            let names: Vec<&str> = self
                .profiles
                .iter()
                .map(|p| p.name.as_str())
                .filter(|n| !n.is_empty())
                .collect();
            if names.is_empty() {
                parts.push(format!("{} 条按站点规则", self.profiles.len()));
            } else {
                parts.push(format!(
                    "{} 条规则（{}）",
                    self.profiles.len(),
                    names.join("、")
                ));
            }
        }
        if let Some(m) = self.budget.max_connections {
            parts.push(format!("总连接上限 {}", m));
        }
        if let Some(t) = self.budget.max_active_tasks {
            parts.push(format!("同时任务 {}", t));
        }
        if let Some(v) = self.limit_now() {
            parts.push(format!("当前时段限速 {} MB/s", v));
        }
        if parts.is_empty() {
            "已加载策略".into()
        } else {
            parts.join(" · ")
        }
    }
}

fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .map(|h| h.split('@').last().unwrap_or(h))
        .map(|h| h.split(':').next().unwrap_or(h))
        .unwrap_or("")
        .to_lowercase()
}

fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(h * 60 + m)
}

fn in_slot(from: &str, to: &str, now: u32) -> bool {
    let a = match parse_hhmm(from) {
        Some(v) => v,
        None => return false,
    };
    let b = match parse_hhmm(to) {
        Some(v) => v,
        None => return false,
    };
    if a <= b {
        now >= a && now < b
    } else {
        // 跨零点，例如 23:00–07:00
        now >= a || now < b
    }
}

fn local_minutes() -> u32 {
    // 用 UTC 偏移近似本地时间：Windows 上通过 TZ 不可靠，这里直接读系统时区偏移
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let offset = local_offset_minutes() as i64;
    let mins = ((secs as i64 / 60) + offset).rem_euclid(24 * 60);
    mins as u32
}

#[cfg(windows)]
fn local_offset_minutes() -> i32 {
    // GetTimeZoneInformation: Bias 是 UTC→本地 的分钟数（西正东负）
    #[repr(C)]
    struct TzInfo {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: [u16; 8],
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: [u16; 8],
        daylight_bias: i32,
    }
    extern "system" {
        fn GetTimeZoneInformation(info: *mut TzInfo) -> u32;
    }
    let mut info: TzInfo = unsafe { std::mem::zeroed() };
    let rc = unsafe { GetTimeZoneInformation(&mut info) };
    let extra = if rc == 2 { info.daylight_bias } else { 0 };
    -(info.bias + extra)
}

#[cfg(not(windows))]
fn local_offset_minutes() -> i32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(json: &str) -> Policy {
        let raw: Raw = serde_json::from_str(json).unwrap();
        Policy {
            budget: raw.budget,
            schedule: raw.schedule,
            profiles: raw.profiles,
            error: String::new(),
        }
    }

    #[test]
    fn host_profile_overrides_conn() {
        let p = policy(
            r#"{"profiles":[{"name":"gh","match_hosts":["github.com"],"conn":64}]}"#,
        );
        assert_eq!(p.conn_for("https://github.com/a/b", "http", 32), 64);
        assert_eq!(p.conn_for("https://example.com/a", "http", 32), 32);
    }

    #[test]
    fn kind_profile_matches() {
        let p = policy(r#"{"profiles":[{"name":"bt","match_kinds":["torrent"],"conn":16}]}"#);
        assert_eq!(p.conn_for("magnet:?xt=abc", "torrent", 32), 16);
        assert_eq!(p.conn_for("https://x/y.zip", "http", 32), 32);
    }

    #[test]
    fn budget_caps_connections() {
        let p = policy(
            r#"{"budget":{"max_connections":48},"profiles":[{"match_hosts":["github.com"],"conn":128}]}"#,
        );
        assert_eq!(p.conn_for("https://github.com/a/b", "http", 32), 48);
    }

    #[test]
    fn schedule_handles_midnight() {
        assert!(in_slot("09:00", "23:00", 10 * 60));
        assert!(!in_slot("09:00", "23:00", 23 * 60 + 30));
        assert!(in_slot("23:00", "07:00", 23 * 60 + 30));
        assert!(in_slot("23:00", "07:00", 6 * 60));
        assert!(!in_slot("23:00", "07:00", 12 * 60));
    }

    #[test]
    fn example_config_parses() {
        let raw: Raw =
            serde_json::from_str(include_str!("../../config/policy.example.json")).unwrap();
        assert!(!raw.profiles.is_empty());
        assert!(raw.budget.max_connections.is_some());
        assert!(!raw.schedule.is_empty());
    }

    #[test]
    fn bad_json_keeps_defaults() {
        let text = r#"{"profiles": [ oops ]}"#;
        let r = serde_json::from_str::<Raw>(text);
        assert!(r.is_err());
    }
}
