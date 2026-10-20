//! 环境检查：把「为什么用不了」变成一份用户能看懂、也能贴给别人的报告。

use crate::deps;
use crate::engine;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

#[derive(Clone)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    /// 必需项失败时，报告顶部直接给结论
    pub required: bool,
    pub detail: String,
    pub hint: String,
}

pub fn run(root: &Path, dir: &Path, svc_online: bool, limit: f64) -> Vec<Check> {
    let mut out = Vec::new();

    let writable = crate::setup::can_write(root);
    out.push(Check {
        name: "程序目录",
        ok: writable,
        required: true,
        detail: if writable {
            format!("{}（可写）", root.display())
        } else {
            format!("{}（不可写）", root.display())
        },
        hint: if writable {
            String::new()
        } else {
            "把 DLKit 移到桌面或 D:\\DLKit 这类有写入权限的目录，不要放在 Program Files".into()
        },
    });

    let conf = root.join("config").join("aria2.conf");
    out.push(Check {
        name: "aria2 配置",
        ok: conf.exists(),
        required: false,
        detail: if conf.exists() {
            "已生成".into()
        } else {
            "缺少 config\\aria2.conf".into()
        },
        hint: if conf.exists() {
            String::new()
        } else {
            "重新启动一次程序会自动生成；只有 BT / 磁力需要".into()
        },
    });

    let dir_ok = std::fs::create_dir_all(dir).is_ok() && crate::setup::can_write(dir);
    let free = engine::disk_free(dir);
    let mut detail = if dir_ok {
        format!("{}（可写）", dir.display())
    } else {
        format!("{}（不可写）", dir.display())
    };
    if let Some(f) = free {
        detail.push_str(&format!("，剩余 {}", human(f)));
    }
    out.push(Check {
        name: "下载目录",
        ok: dir_ok,
        required: true,
        detail,
        hint: if dir_ok {
            String::new()
        } else {
            "到「设置」换一个有写入权限的目录".into()
        },
    });

    for dep in deps::DEPS {
        let ok = deps::installed(root, dep);
        out.push(Check {
            name: dep.name,
            ok,
            required: false,
            detail: if ok {
                format!("已就位（{}）", dep.note)
            } else {
                format!("未安装（{}）", dep.note)
            },
            hint: if ok {
                String::new()
            } else {
                "到「设置 → 依赖」点一键安装".into()
            },
        });
    }

    out.push(Check {
        name: "aria2 服务",
        ok: svc_online,
        required: false,
        detail: if svc_online {
            "端口 6800 可连接".into()
        } else {
            "未运行（只有 BT / 磁力需要）".into()
        },
        hint: if svc_online {
            String::new()
        } else {
            "添加磁力链接时会自动启动；也可以点顶部「启动服务」".into()
        },
    });

    let net = "github.com:443"
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next())
        .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok())
        .unwrap_or(false);
    out.push(Check {
        name: "网络",
        ok: net,
        required: true,
        detail: if net {
            "可以连接 GitHub".into()
        } else {
            "连不上 GitHub（2 秒超时）".into()
        },
        hint: if net {
            String::new()
        } else {
            "检查代理 / 防火墙；镜像站仍可尝试下载".into()
        },
    });

    out.push(Check {
        name: "限速",
        ok: true,
        required: false,
        detail: if limit <= 0.0 {
            "不限速".into()
        } else {
            format!("{} MB/s", limit)
        },
        hint: String::new(),
    });

    out
}

/// 有没有必需项失败
pub fn has_blocking(checks: &[Check]) -> bool {
    checks.iter().any(|c| c.required && !c.ok)
}

pub fn summary(checks: &[Check]) -> String {
    if has_blocking(checks) {
        "有项目需要处理，见下面的提示".to_string()
    } else if checks.iter().any(|c| !c.ok) {
        "基本可用，缺少的组件只影响对应功能".to_string()
    } else {
        "一切正常".to_string()
    }
}

pub fn report(checks: &[Check]) -> String {
    let mut s = format!(
        "DLKit v{} 环境检查\n{}\n\n",
        env!("CARGO_PKG_VERSION"),
        summary(checks)
    );
    for c in checks {
        s.push_str(&format!(
            "[{}] {}：{}",
            if c.ok { "OK" } else { "!!" },
            c.name,
            c.detail
        ));
        if !c.ok && !c.hint.is_empty() {
            s.push_str(&format!("  → {}", c.hint));
        }
        s.push('\n');
    }
    s
}

fn human(n: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < U.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n, U[i])
    } else {
        format!("{:.1} {}", v, U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chk(ok: bool, required: bool) -> Check {
        Check {
            name: "x",
            ok,
            required,
            detail: String::new(),
            hint: String::new(),
        }
    }

    #[test]
    fn blocking_only_counts_required() {
        assert!(!has_blocking(&[chk(true, true), chk(false, false)]));
        assert!(has_blocking(&[chk(false, true)]));
    }

    #[test]
    fn report_lists_every_check() {
        let list = vec![chk(true, true), chk(false, false)];
        let r = report(&list);
        assert!(r.contains("[OK]"));
        assert!(r.contains("[!!]"));
        assert!(r.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    #[ignore]
    fn real_environment_check() {
        let root = std::env::temp_dir().join("dlkit-diag-probe");
        std::fs::create_dir_all(&root).unwrap();
        let checks = run(&root, &root, false, 0.0);
        println!("{}", report(&checks));
        assert!(checks.len() >= 8);
    }
}
