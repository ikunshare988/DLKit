//! 运行环境识别：判断当前系统，并从页面解析结果中挑出最匹配的下载项。
//!
//! 只在「同一页面同时提供多个平台版本」时才做推荐（例如下载页同时有
//! Windows / macOS / Linux 安装包）。资源列表页（一整页 mp3、图片等）
//! 不参与推荐，保持全选，避免打扰用户。

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Windows,
    Mac,
    Linux,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Os::Windows => "Windows",
            Os::Mac => "macOS",
            Os::Linux => "Linux",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    X64,
    Arm64,
    X86,
}

impl Arch {
    pub fn current() -> Arch {
        if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else if cfg!(target_arch = "x86") {
            Arch::X86
        } else {
            Arch::X64
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Arch::X64 => "64 位",
            Arch::Arm64 => "ARM64",
            Arch::X86 => "32 位",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub os: Os,
    pub arch: Arch,
}

impl Target {
    pub fn detect() -> Target {
        Target {
            os: Os::current(),
            arch: Arch::current(),
        }
    }

    pub fn label(self) -> String {
        format!("{} {}", self.os.label(), self.arch.label())
    }
}

// 平台关键词。刻意不收 `.zip` / `.tar.gz`：这两类在所有平台都会出现，
// 拿来当平台信号会把 Windows 的源码包误判成 Linux 包。
const WIN_OS: &[&str] = &[
    "windows", "win64", "win32", "win-x64", "win-x86", "win_", "-win", "win10", "win11",
    ".exe", ".msi",
];
const MAC_OS: &[&str] = &[
    "macos", "mac-os", "osx", "os x", "-mac", "_mac", ".dmg", ".pkg", "apple silicon",
];
const LINUX_OS: &[&str] = &[
    "linux", "-linux", "_linux", ".deb", ".rpm", ".appimage", ".snap", ".flatpak",
];

// 架构关键词。顺序有意义：先认 x86_64，再认 x86，否则 64 位包会被当成 32 位。
const ARCH_X64: &[&str] = &["x86_64", "x86-64", "x64", "amd64", "win64", "64bit", "64-bit"];
const ARCH_ARM: &[&str] = &["arm64", "aarch64", "universal2", "apple silicon"];
const ARCH_X86: &[&str] = &["x86", "i386", "i686", "win32", "32bit", "32-bit"];

/// 明确属于其他平台的候选得这个分，会被排除在推荐之外。
const OTHER_OS: i32 = -100;

fn has_any(hay: &str, words: &[&str]) -> bool {
    words.iter().any(|w| hay.contains(w))
}

fn arch_hint(hay: &str) -> Option<Arch> {
    if has_any(hay, ARCH_X64) {
        Some(Arch::X64)
    } else if has_any(hay, ARCH_ARM) {
        Some(Arch::Arm64)
    } else if has_any(hay, ARCH_X86) {
        Some(Arch::X86)
    } else {
        None
    }
}

/// 系统安装包：双击就能装，普通用户优先要这个
fn is_installer(hay: &str) -> bool {
    [".msi", ".dmg", ".pkg", ".deb", ".rpm", ".appimage"]
        .iter()
        .any(|ext| hay.contains(ext))
        || hay.contains("setup")
        || hay.contains("installer")
}

/// 给单个候选打分。<= OTHER_OS 表示明确是别的平台，必须排除。
pub fn score(target: Target, name: &str, url: &str) -> i32 {
    // "darwin" 里含 "win"，先替换掉，否则 macOS 包会被认成 Windows 包
    let hay = format!("{} {}", name, url)
        .to_lowercase()
        .replace("darwin", "macos");

    let win = has_any(&hay, WIN_OS);
    let mac = has_any(&hay, MAC_OS);
    let lin = has_any(&hay, LINUX_OS);

    let (same, other) = match target.os {
        Os::Windows => (win, mac || lin),
        Os::Mac => (mac, win || lin),
        Os::Linux => (lin, win || mac),
    };
    // 同时命中当前平台和其他平台（如 "win-mac"）时不算冲突，按当前平台处理
    if !same && other {
        return OTHER_OS;
    }

    let mut score = if same { 10 } else { 0 };

    match (arch_hint(&hay), target.arch) {
        (Some(a), b) if a == b => score += 4,
        // 32 位包能在 64 位系统上跑，但不该优先于原生包
        (Some(Arch::X86), Arch::X64) => {}
        // 架构互斥：ARM64 包装不到 x64 机器上，反之亦然
        (Some(_), _) => return OTHER_OS,
        (None, _) => {}
    }

    // 安装包优先于压缩包；.exe 可能是安装程序也可能是单文件，权重减半。
    // 权重与架构差异同量级，保证「同平台同架构时也选安装包」。
    if is_installer(&hay) {
        score += 4;
    } else if hay.contains(".exe") {
        score += 2;
    }
    score
}

pub struct Plan {
    /// 推荐候选在列表中的下标
    pub pick: usize,
    /// 原因文案，例如「检测到 Windows 64 位」
    pub reason: String,
    /// 被排除的其他平台候选数量
    pub filtered: usize,
}

/// 从候选里挑一个推荐项。返回 None 表示「这页不需要推荐」。
pub fn plan(target: Target, items: &[(&str, &str)]) -> Option<Plan> {
    if items.len() < 2 {
        return None;
    }

    let scores: Vec<i32> = items
        .iter()
        .map(|(name, url)| score(target, name, url))
        .collect();

    let filtered = scores.iter().filter(|s| **s <= OTHER_OS).count();

    let mut best: Option<(usize, i32)> = None;
    let mut second = 0i32;
    for (i, s) in scores.iter().copied().enumerate() {
        if s <= OTHER_OS {
            continue;
        }
        match best {
            None => best = Some((i, s)),
            Some((_, b)) if s > b => {
                second = second.max(b);
                best = Some((i, s));
            }
            Some(_) => second = second.max(s),
        }
    }

    let (pick, top) = best?;
    if top <= 0 {
        return None;
    }

    // 没有平台冲突、且最优项也没明显优于次优项 → 没什么可建议的
    if filtered == 0 && top - second < 4 {
        return None;
    }

    let reason = if filtered > 0 {
        format!("检测到 {}，已排除 {} 个其他平台的安装包", target.label(), filtered)
    } else {
        format!("检测到 {}", target.label())
    };

    Some(Plan {
        pick,
        reason,
        filtered,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win64() -> Target {
        Target {
            os: Os::Windows,
            arch: Arch::X64,
        }
    }

    #[test]
    fn picks_windows_package_from_mixed_page() {
        let items = [
            ("7z2409-x64.exe", "https://www.7-zip.org/a/7z2409-x64.exe"),
            ("7z2409-mac.dmg", "https://www.7-zip.org/a/7z2409-mac.dmg"),
            ("7z2409-linux-x64.tar.xz", "https://www.7-zip.org/a/7z2409-linux-x64.tar.xz"),
        ];
        let p = plan(win64(), &items).expect("应推荐 Windows 包");
        assert_eq!(p.pick, 0);
        assert_eq!(p.filtered, 2);
    }

    #[test]
    fn prefers_native_over_32bit() {
        let items = [
            ("tool-win32.exe", "https://x/tool-win32.exe"),
            ("tool-win64.exe", "https://x/tool-win64.exe"),
        ];
        let p = plan(win64(), &items).expect("应推荐 64 位包");
        assert_eq!(p.pick, 1);
    }

    #[test]
    fn ignores_plain_resource_list() {
        let items = [
            ("01 - Opening.mp3", "https://x/01.mp3"),
            ("02 - Theme.mp3", "https://x/02.mp3"),
        ];
        assert!(plan(win64(), &items).is_none());
    }

    #[test]
    fn prefers_installer_over_archive() {
        let items = [
            ("node-v24-win-x64.7z", "https://x/node-v24-win-x64.7z"),
            ("node-v24-win-x64.zip", "https://x/node-v24-win-x64.zip"),
            ("node-v24-x64.msi", "https://x/node-v24-x64.msi"),
        ];
        let p = plan(win64(), &items).expect("应推荐安装包");
        assert_eq!(p.pick, 2);
    }

    #[test]
    fn darwin_is_not_windows() {
        assert!(score(win64(), "tool-darwin-arm64", "https://x/a") <= OTHER_OS);
    }

    #[test]
    fn arm_package_rejected_on_x64() {
        assert!(score(win64(), "tool-win-arm64.zip", "https://x/a") <= OTHER_OS);
    }
}
