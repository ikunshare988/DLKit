//! 账号：B 站 Cookie 与网盘（OpenList）登录信息。
//!
//! - B 站：可以用「从浏览器读取」（交给 yt-dlp 的 --cookies-from-browser），
//!   也可以手动粘贴 Cookie。手动粘贴的内容用 Windows DPAPI 加密后落盘，
//!   只在当前用户下可解密，配置文件被拷走也读不出明文。
//! - 网盘：编辑 config/openlist.json（地址 / 账号 / 密码）。
//!   百度、阿里、夸克等网盘的实际登录在 OpenList 自己的后台完成，
//!   DLKit 只负责登录 OpenList 并取直链。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

pub const MODE_NONE: &str = "none";
pub const MODE_BROWSER: &str = "browser";
pub const MODE_MANUAL: &str = "manual";

#[derive(Clone)]
pub struct Accounts {
    pub bili_mode: String,
    pub bili_browser: String,
    pub bili_cookie: String,
    pub ol_base: String,
    pub ol_user: String,
    pub ol_pass: String,
}

impl Default for Accounts {
    fn default() -> Self {
        Accounts {
            bili_mode: MODE_NONE.into(),
            bili_browser: "edge".into(),
            bili_cookie: String::new(),
            ol_base: "http://127.0.0.1:5244".into(),
            ol_user: "admin".into(),
            ol_pass: String::new(),
        }
    }
}

impl Accounts {
    pub fn load(root: &Path) -> Accounts {
        let mut a = Accounts::default();
        if let Ok(text) = fs::read_to_string(accounts_path(root)) {
            #[derive(Deserialize, Default)]
            struct Raw {
                bili_mode: Option<String>,
                bili_browser: Option<String>,
                bili_cookie_enc: Option<String>,
            }
            if let Ok(r) = serde_json::from_str::<Raw>(&text) {
                if let Some(m) = r.bili_mode {
                    if m == MODE_BROWSER || m == MODE_MANUAL {
                        a.bili_mode = m;
                    }
                }
                if let Some(b) = r.bili_browser {
                    a.bili_browser = b;
                }
                if let Some(enc) = r.bili_cookie_enc {
                    if let Some(raw) = b64_decode(&enc) {
                        if let Some(plain) = dpapi::unprotect(&raw) {
                            a.bili_cookie = String::from_utf8_lossy(&plain).to_string();
                        }
                    }
                }
            }
        }
        if let Ok(text) = fs::read_to_string(openlist_path(root)) {
            #[derive(Deserialize, Default)]
            struct Ol {
                base: Option<String>,
                username: Option<String>,
                password: Option<String>,
            }
            if let Ok(o) = serde_json::from_str::<Ol>(&text) {
                if let Some(v) = o.base {
                    if !v.trim().is_empty() {
                        a.ol_base = v;
                    }
                }
                if let Some(v) = o.username {
                    a.ol_user = v;
                }
                if let Some(v) = o.password {
                    a.ol_pass = v;
                }
            }
        }
        a
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let dir = root.join("config");
        fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败：{}", e))?;
        let enc = if self.bili_cookie.trim().is_empty() {
            String::new()
        } else {
            let raw = dpapi::protect(self.bili_cookie.trim().as_bytes())
                .ok_or("Cookie 加密失败（DPAPI）")?;
            b64_encode(&raw)
        };
        let v = serde_json::json!({
            "bili_mode": self.bili_mode,
            "bili_browser": self.bili_browser,
            "bili_cookie_enc": enc,
        });
        fs::write(accounts_path(root), v.to_string())
            .map_err(|e| format!("写入账号配置失败：{}", e))?;
        let ol = serde_json::json!({
            "base": self.ol_base.trim(),
            "username": self.ol_user.trim(),
            "password": self.ol_pass,
        });
        fs::write(openlist_path(root), ol.to_string())
            .map_err(|e| format!("写入 OpenList 配置失败：{}", e))?;
        Ok(())
    }

    /// B 站账号是否已经配置（浏览器读取模式视为已配置）
    pub fn bili_ready(&self) -> bool {
        self.bili_mode == MODE_BROWSER
            || (self.bili_mode == MODE_MANUAL && !self.bili_cookie.trim().is_empty())
    }

    /// 把 B 站登录方式加到 yt-dlp 命令行
    pub fn apply_media(&self, cmd: &mut Command) {
        match self.bili_mode.as_str() {
            MODE_BROWSER => {
                cmd.arg("--cookies-from-browser").arg(self.bili_browser.clone());
            }
            MODE_MANUAL => {
                let c = self.bili_cookie.trim();
                if !c.is_empty() {
                    cmd.arg("--add-header").arg(format!("Cookie: {}", c));
                }
            }
            _ => {}
        }
    }
}

fn accounts_path(root: &Path) -> PathBuf {
    root.join("config").join("accounts.json")
}

fn openlist_path(root: &Path) -> PathBuf {
    root.join("config").join("openlist.json")
}

// ---------------- base64 ----------------

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut vals = Vec::new();
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        vals.push(v);
    }
    let mut out = Vec::new();
    for chunk in vals.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let n = ((chunk[0] as u32) << 18)
            | ((chunk[1] as u32) << 12)
            | ((chunk.get(2).copied().unwrap_or(0) as u32) << 6)
            | chunk.get(3).copied().unwrap_or(0) as u32;
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

// ---------------- Windows DPAPI ----------------

#[cfg(windows)]
mod dpapi {
    use std::ptr;

    #[repr(C)]
    struct Blob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    extern "system" {
        fn CryptProtectData(
            data_in: *const Blob,
            desc: *const u16,
            entropy: *const Blob,
            reserved: *mut core::ffi::c_void,
            prompt: *mut core::ffi::c_void,
            flags: u32,
            data_out: *mut Blob,
        ) -> i32;
        fn CryptUnprotectData(
            data_in: *const Blob,
            desc: *mut *mut u16,
            entropy: *const Blob,
            reserved: *mut core::ffi::c_void,
            prompt: *mut core::ffi::c_void,
            flags: u32,
            data_out: *mut Blob,
        ) -> i32;
        fn LocalFree(h: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }

    const UI_FORBIDDEN: u32 = 0x1;

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        unsafe {
            let in_blob = Blob {
                cb_data: data.len() as u32,
                pb_data: data.as_ptr() as *mut u8,
            };
            let mut out = Blob {
                cb_data: 0,
                pb_data: ptr::null_mut(),
            };
            let ok = CryptProtectData(
                &in_blob,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                UI_FORBIDDEN,
                &mut out,
            );
            if ok == 0 || out.pb_data.is_null() {
                return None;
            }
            let v = std::slice::from_raw_parts(out.pb_data, out.cb_data as usize).to_vec();
            LocalFree(out.pb_data as *mut core::ffi::c_void);
            Some(v)
        }
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        unsafe {
            let in_blob = Blob {
                cb_data: data.len() as u32,
                pb_data: data.as_ptr() as *mut u8,
            };
            let mut out = Blob {
                cb_data: 0,
                pb_data: ptr::null_mut(),
            };
            let ok = CryptUnprotectData(
                &in_blob,
                ptr::null_mut(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                UI_FORBIDDEN,
                &mut out,
            );
            if ok == 0 || out.pb_data.is_null() {
                return None;
            }
            let v = std::slice::from_raw_parts(out.pb_data, out.cb_data as usize).to_vec();
            LocalFree(out.pb_data as *mut core::ffi::c_void);
            Some(v)
        }
    }
}

#[cfg(not(windows))]
mod dpapi {
    pub fn protect(_data: &[u8]) -> Option<Vec<u8>> {
        None
    }
    pub fn unprotect(_data: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        for raw in [b"a".as_slice(), b"ab", b"abc", b"hello world!"] {
            let enc = b64_encode(raw);
            assert_eq!(b64_decode(&enc).unwrap(), raw);
        }
    }

    #[test]
    fn dpapi_roundtrip() {
        let raw = "SESSDATA=abc; bili_jct=def".as_bytes();
        let enc = dpapi::protect(raw).expect("加密失败");
        let back = dpapi::unprotect(&enc).expect("解密失败");
        assert_eq!(back, raw);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let root = std::env::temp_dir().join(format!("dlkit-acc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let a = Accounts {
            bili_mode: MODE_MANUAL.into(),
            bili_browser: "chrome".into(),
            bili_cookie: "SESSDATA=xyz; bili_jct=123".into(),
            ol_base: "http://127.0.0.1:5999".into(),
            ol_user: "tester".into(),
            ol_pass: "secret".into(),
        };
        a.save(&root).unwrap();
        // 落盘内容里不应该出现明文 Cookie
        let disk = fs::read_to_string(root.join("config/accounts.json")).unwrap();
        assert!(!disk.contains("SESSDATA=xyz"));
        let b = Accounts::load(&root);
        assert_eq!(b.bili_mode, MODE_MANUAL);
        assert_eq!(b.bili_cookie, a.bili_cookie);
        assert_eq!(b.ol_base, "http://127.0.0.1:5999");
        assert_eq!(b.ol_user, "tester");
        assert_eq!(b.ol_pass, "secret");
        assert!(b.bili_ready());
        let _ = fs::remove_dir_all(&root);
    }
}
