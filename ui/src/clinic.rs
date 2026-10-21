//! 下载诊所：把引擎的错误文案归类成用户能看懂的原因和可执行的动作。
//!
//! 只做"判断 + 建议"，不在这里碰网络或文件；界面根据 [`Issue`] 决定显示哪些按钮。

use crate::engine::Kind;

/// 重试方式
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Retry {
    /// 沿用原参数
    Keep,
    /// 补一个来源页 Referer 再试
    WithReferer,
    /// 只用一条连接
    Single,
}

/// 归类后的一个问题
pub struct Issue {
    /// 稳定的错误码，报告和文档里用它
    pub code: &'static str,
    /// 一句话标题
    pub title: &'static str,
    /// 人话解释
    pub detail: &'static str,
    /// 建议动作
    pub advice: &'static str,
    pub retryable: bool,
    /// 403/401 这类通常和来源页有关
    pub use_referer: bool,
    /// 服务器不支持分片时的单连接重试
    pub single: bool,
    /// 需要登录的场景，给一个"打开网页登录"
    pub login: bool,
    /// 需要换下载目录
    pub settings: bool,
    /// 需要启动 aria2 服务（增强版）
    pub start_service: bool,
    /// 缺少 yt-dlp / ffmpeg（增强版）
    pub install_deps: bool,
}

fn hit(msg: &str, words: &[&str]) -> bool {
    words.iter().any(|w| msg.contains(w))
}

/// 从任务文案、任务类型和是否已带 Referer 推导问题。
///
/// 顺序有意义：HTTP 状态码和磁盘类错误要先判，避免被后面的关键词误分类。
pub fn classify(message: &str, kind: Kind, has_referer: bool) -> Issue {
    let m = message.to_lowercase();

    if hit(&m, &["磁盘空间不足", "空间不足"]) {
        return Issue {
            code: "DISK_FULL",
            title: "磁盘空间不足",
            detail: "目标磁盘剩下的空间放不下这个文件。",
            advice: "到设置里换一个空间充足的下载目录，再重试。",
            retryable: false,
            use_referer: false,
            single: false,
            login: false,
            settings: true,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(
        &m,
        &[
            "拒绝访问",
            "access is denied",
            "os error 5",
            "预分配磁盘空间失败",
        ],
    ) {
        return Issue {
            code: "DISK_DENIED",
            title: "下载目录不可写",
            detail: "目录权限不足，或者文件正被系统保护。",
            advice: "换到桌面、D:\\Downloads 这类普通目录，再重试。",
            retryable: false,
            use_referer: false,
            single: false,
            login: false,
            settings: true,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(&m, &["http 401", "401，请填写", "需要登录或授权"]) {
        return Issue {
            code: "HTTP_401",
            title: "需要登录",
            detail: "这个地址需要登录或授权后才能下载。",
            advice: "先用浏览器登录该站，再回来重试；也可以从下载页重新解析。",
            retryable: true,
            use_referer: true,
            single: false,
            login: true,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(&m, &["http 403", "请求被拒绝"]) {
        return Issue {
            code: "HTTP_403",
            title: "站点拒绝了这次请求",
            detail: if has_referer {
                "可能是 Cookie 过期，或者站点只允许从它自己的页面发起下载。"
            } else {
                "可能是缺少来源页（Referer）、Cookie 过期，或站点禁止直接下载。"
            },
            advice: "先补来源页重试；仍然失败就打开原网页登录，或重新解析下载页。",
            retryable: true,
            use_referer: true,
            single: false,
            login: true,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(&m, &["http 404", "http 410", "链接失效", "文件已被删除"]) {
        return Issue {
            code: "HTTP_404",
            title: "链接已经失效",
            detail: "服务器上找不到这个文件了。",
            advice: "回到下载页重新解析，或向文件发布者确认新地址。",
            retryable: false,
            use_referer: false,
            single: false,
            login: false,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(&m, &["http 429", "请求过于频繁"]) {
        return Issue {
            code: "HTTP_429",
            title: "请求太频繁被限流",
            detail: "站点暂时不接受更多请求。",
            advice: "等几分钟再试，或者把并发调低一些。",
            retryable: true,
            use_referer: false,
            single: false,
            login: false,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(
        &m,
        &["http 500", "http 502", "http 503", "http 504", "网关错误", "服务暂不可用"],
    ) {
        return Issue {
            code: "HTTP_5XX",
            title: "服务器暂时不可用",
            detail: "问题在对方服务器，不是你这边。",
            advice: "过一会儿重试；持续失败可以换镜像或稍后再来。",
            retryable: true,
            use_referer: false,
            single: false,
            login: false,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(&m, &["aria2 未运行", "aria2 rpc", "无法连接 aria2", "aria2 异常"]) {
        return Issue {
            code: "ARIA2_OFFLINE",
            title: "aria2 服务没有运行",
            detail: "BT / 磁力 / aria2 引擎都依赖这个本地服务。",
            advice: "点「启动服务」，几秒后重试。",
            retryable: true,
            use_referer: false,
            single: false,
            login: false,
            settings: false,
            start_service: true,
            install_deps: false,
        };
    }
    if hit(
        &m,
        &[
            "failed to connect",
            "connection refused",
            "actively refused",
            "无法连接",
            "拒绝连接",
        ],
    ) {
        return Issue {
            code: "NET_REFUSED",
            title: "连接被拒绝",
            detail: "目标端口没有服务在监听，或者连接被本机防火墙拦住了。",
            advice: "确认地址和端口正确；如果是本机服务，先把它启动起来再重试。",
            retryable: true,
            use_referer: false,
            single: false,
            login: false,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(&m, &["无法启动 yt-dlp", "未找到 yt-dlp", "yt-dlp 异常"]) {
        return Issue {
            code: "DEP_MISSING",
            title: "缺少视频下载组件",
            detail: "yt-dlp 或 ffmpeg 没有就位，媒体站任务无法开始。",
            advice: "到设置里补齐组件；离线用户可解压依赖组合包到程序目录。",
            retryable: true,
            use_referer: false,
            single: false,
            login: false,
            settings: false,
            start_service: false,
            install_deps: true,
        };
    }
    if hit(
        &m,
        &[
            "无法解析",
            "could not resolve",
            "name or service not known",
            "dns",
        ],
    ) {
        return Issue {
            code: "DNS",
            title: "域名解析失败",
            detail: "当前网络解析不到这个域名。",
            advice: "检查网络、代理或 VPN；如果改过 hosts，先确认地址没有被指向 127.0.0.1。",
            retryable: true,
            use_referer: false,
            single: false,
            login: false,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(
        &m,
        &["tls", "ssl", "证书", "certificate", "schannel", "handshake"],
    ) {
        return Issue {
            code: "TLS",
            title: "加密连接没有建立",
            detail: "证书校验失败，常见于代理、系统时间不准或网络被劫持。",
            advice: "检查系统时间和代理设置，然后再重试。",
            retryable: true,
            use_referer: false,
            single: false,
            login: false,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(
        &m,
        &[
            "超时",
            "timed out",
            "没有返回响应头",
            "connect timeout",
            "连接超时",
        ],
    ) {
        return Issue {
            code: "TIMEOUT",
            title: "连接超时",
            detail: "服务器长时间没有响应，可能是网络不稳定或站点限速。",
            advice: "先重试；持续失败时检查代理 / VPN，或用单连接再试一次。",
            retryable: true,
            use_referer: false,
            single: true,
            login: false,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(
        &m,
        &[
            "不支持 range",
            "不支持分片",
            "无法分片",
            "忽略了 range",
        ],
    ) {
        return Issue {
            code: "RANGE",
            title: "服务器不支持分片",
            detail: "这个地址只能单连接下载，速度会受服务器限制。",
            advice: "用单连接重试即可，已下载的部分不会重下。",
            retryable: true,
            use_referer: false,
            single: true,
            login: false,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(&m, &["镜像"]) && hit(&m, &["不一致", "不是同一文件", "内容不符"]) {
        return Issue {
            code: "MIRROR",
            title: "镜像内容和主站不一致",
            detail: "备用镜像返回了不同的文件，程序已经停止混用。",
            advice: "重试，程序会跳过有问题的镜像。",
            retryable: true,
            use_referer: false,
            single: false,
            login: false,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }
    if hit(&m, &["文件被占用", "正在使用", "另一个程序"]) {
        return Issue {
            code: "FILE_LOCKED",
            title: "目标文件被占用",
            detail: "有别的程序正在使用这个文件。",
            advice: "关掉占用它的程序（播放器、杀毒扫描等），再重试。",
            retryable: true,
            use_referer: false,
            single: false,
            login: false,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }
    if kind == Kind::Media && hit(&m, &["cookie", "登录"]) {
        return Issue {
            code: "MEDIA_AUTH",
            title: "视频站要求登录",
            detail: "这个视频需要账号权限才能取到清晰度或音轨。",
            advice: "到账号页配置 Cookie 后重试。",
            retryable: true,
            use_referer: false,
            single: false,
            login: true,
            settings: false,
            start_service: false,
            install_deps: false,
        };
    }

    Issue {
        code: "UNKNOWN",
        title: "下载失败",
        detail: "没有匹配到已知原因。",
        advice: "重试一次；仍然失败就复制报告，日志里会有更细的原因。",
        retryable: true,
        use_referer: false,
        single: false,
        login: false,
        settings: false,
        start_service: false,
        install_deps: false,
    }
}

/// 诊断报告需要的任务信息
pub struct ReportCtx<'a> {
    pub name: &'a str,
    pub url: &'a str,
    pub kind: &'a str,
    pub conn: usize,
    pub attempts: u32,
    pub elapsed: f32,
    pub message: &'a str,
    pub log: &'a [String],
}

/// 生成一份可复制、可贴给别人的报告。
/// 地址里的 query 会整体省略，避免把签名、token 带出去。
pub fn report(ctx: &ReportCtx, issue: &Issue) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "DLKit v{} · 下载诊所报告\n",
        env!("CARGO_PKG_VERSION")
    ));
    out.push_str(&format!("时间：{}\n", utc_stamp()));
    out.push_str(&format!(
        "系统：{} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    out.push_str(&format!("任务：{}\n", ctx.name));
    out.push_str(&format!("地址：{}\n", redact_url(ctx.url)));
    out.push_str(&format!(
        "类型：{} · {} 路 · 已重试 {} 次 · 用时 {:.1}s\n",
        ctx.kind, ctx.conn, ctx.attempts, ctx.elapsed
    ));
    out.push_str(&format!("结论：{} {}\n", issue.code, issue.title));
    out.push_str(&format!("说明：{}\n", issue.detail));
    out.push_str(&format!("建议：{}\n", issue.advice));
    out.push_str(&format!("原始信息：{}\n", ctx.message));
    if !ctx.log.is_empty() {
        out.push_str("最近日志：\n");
        let start = ctx.log.len().saturating_sub(12);
        for line in &ctx.log[start..] {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("（报告只在本机生成，不会自动上传）\n");
    out
}

/// 去掉 query 和用户信息，保留主机与路径
pub fn redact_url(url: &str) -> String {
    let mut s = url.to_string();
    if let Some(q) = s.find('?') {
        s.truncate(q);
        s.push_str("?<参数已省略>");
    }
    if let Some(a) = s.find("://") {
        let rest = &s[a + 3..];
        if let Some(at) = rest.find('@') {
            let host = rest[at + 1..].to_string();
            s = format!("{}://***@{}", &s[..a], host);
        }
    }
    s
}

fn utc_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// 把 1970-01-01 起的天数转成公历日期（Howard Hinnant 的算法）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_suggests_referer_and_login() {
        let i = classify(
            "请求被拒绝（HTTP 403）：可能需要 Cookie/Referer，或该站禁止直接下载",
            Kind::Http,
            false,
        );
        assert_eq!(i.code, "HTTP_403");
        assert!(i.use_referer && i.login && i.retryable);
    }

    #[test]
    fn disk_full_is_not_retryable() {
        let i = classify("磁盘空间不足：需要约 1.0 GB，可用 200 MB", Kind::Http, false);
        assert_eq!(i.code, "DISK_FULL");
        assert!(!i.retryable && i.settings);
    }

    #[test]
    fn aria2_offline_offers_service() {
        let i = classify(
            "aria2 未运行：请确认程序目录下有 bin\\aria2\\aria2c.exe",
            Kind::Torrent,
            false,
        );
        assert_eq!(i.code, "ARIA2_OFFLINE");
        assert!(i.start_service);
    }

    #[test]
    fn refused_connection_is_not_unknown() {
        let i = classify("Failed to connect to 127.0.0.1 port 8099", Kind::Http, false);
        assert_eq!(i.code, "NET_REFUSED");
        assert!(i.retryable);
    }

    #[test]
    fn timeout_offers_single() {
        let i = classify("目标在 25 秒内没有返回响应头", Kind::Http, false);
        assert_eq!(i.code, "TIMEOUT");
        assert!(i.single);
    }

    #[test]
    fn redacts_query_and_userinfo() {
        let u = redact_url("https://user:pass@example.com/a/b.zip?token=secret&x=1");
        assert!(!u.contains("secret"));
        assert!(!u.contains("user:pass"));
        assert!(u.contains("example.com/a/b.zip"));
    }

    #[test]
    fn report_has_code_and_no_token() {
        let issue = classify("请求被拒绝（HTTP 403）", Kind::Http, true);
        let logs = vec!["[  0.01s] 探测：HTTP 403".to_string()];
        let text = report(
            &ReportCtx {
                name: "a.zip",
                url: "https://example.com/a.zip?token=abc",
                kind: "直链",
                conn: 32,
                attempts: 1,
                elapsed: 2.5,
                message: "请求被拒绝（HTTP 403）",
                log: &logs,
            },
            &issue,
        );
        assert!(text.contains("HTTP_403"));
        assert!(!text.contains("token=abc"));
        assert!(text.contains("最近日志"));
    }
}
