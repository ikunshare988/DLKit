# DLKit

[中文](README.md)

A multi-purpose downloader for Windows: segmented direct downloads, download-page scanning, and built-in mirrors that let several sources share the work. Written in Rust + egui, shipped as a single exe — no browser engine, no Python runtime.

[Download](https://github.com/ikunshare988/DLKit/releases) · [Command line](#command-line) · [Limitations](#limitations)

What it is for:

- **Per-IP throttling**: files are split into blocks and fetched in parallel; when a mirror exists, blocks are spread across sources so the total can add up
- **Download pages without direct links**: paste the page, get the list of files, and the build for your OS is selected for you
- **Too many sources**: direct links, torrents, video sites and cloud drives all queue in one window

## Download

Get `DLKit.exe` from [Releases](https://github.com/ikunshare988/DLKit/releases) and put it in any folder.

The exe is not code-signed, so Windows SmartScreen may warn on first launch — choose "More info → Run anyway". Every release ships a `SHA256.txt` you can verify.

Each release also ships `DLKit.exe.bundle`, a keyless Sigstore signature produced in GitHub Actions. The certificate records this repository, workflow and tag. With [cosign](https://docs.sigstore.dev/cosign/installation/) installed:

```powershell
cosign verify-blob --bundle DLKit.exe.bundle `
  --certificate-identity-regexp "https://github.com/ikunshare988/DLKit/.github/workflows/release.yml@refs/tags/.*" `
  --certificate-oidc-issuer https://token.actions.githubusercontent.com `
  DLKit.exe
```

## Setup

The binary is a single exe, but downloading relies on a few command-line tools. Put them next to it:

```
DLKit.exe
bin/
  aria2/aria2c.exe
  yt-dlp/yt-dlp.exe
  ffmpeg/bin/ffmpeg.exe
  ffmpeg/bin/ffprobe.exe
  openlist/openlist.exe      # optional, cloud drives only
config/
  aria2.conf
  openlist.json              # optional
downloads/
```

Anything missing can be installed with one click from Settings → Dependencies; the app downloads aria2 / yt-dlp / ffmpeg into `bin/` itself and needs no extra runtime.

Configuration:

1. Copy `config/aria2.conf.example` to `config/aria2.conf` and set `rpc-secret` (paths are relative, so the folder is portable across drives and machines)
2. For cloud drives, copy `config/openlist.json.example` to `config/openlist.json` and fill in your OpenList address and account

Start:

- Direct links and page scanning: just run `DLKit.exe`
- Torrents, cloud drives or aria2 fallback: run `start.ps1` first, then open the app

## Features

### Direct downloads

- Files are split into blocks fetched in parallel (32 by default, 1–128 in the UI)
- On fast links, raising concurrency to 64–128 keeps adding bandwidth: each connection gets its own block over HTTP/1.1, so HTTP/2 cannot funnel dozens of streams into one congestion window
- A persistent connection pool (keep-alive) reuses connections instead of restarting a client per block
- Blocks are split at byte level on the fly: a slow stream only hands over its unfinished half, already written bytes are never re-downloaded
- 25% standby connections (e.g. 80 for 64 lanes) take over as soon as one stream falls below the median or overall speed drops under 60% of peak
- Space is preallocated and every block is written straight to its offset — no temp files, no merge step
- Resume: re-add the same link and finished blocks are skipped
- Pause / resume: in-flight connections are dropped and block progress is saved; resume only fetches what is missing
- Global speed limit from 0 to 100 MB/s (0 = unlimited; applies to the built-in engine)
- Copying a link fills the input box; links and `.torrent` files can be dropped onto the window
- Paste several links at once; each line becomes a task
- Existing files are never overwritten — a ` (1)` suffix is added automatically
- Free disk space is checked before preallocation, with the required and available sizes in the error
- Falls back to a single connection when the server ignores Range requests
- Real file names via redirects and `Content-Disposition`
- 429/403 back off and lower concurrency; 404/410 fail fast instead of retrying

### Download-page scanning

- Paste a download page (not a direct link) and hit "Scan page"
- Extracts links from `a`, `video/audio/source`, `iframe`, `meta og:video`, `link rel=enclosure`, plus URLs embedded in inline JSON
- Sends a HEAD request per candidate, follows redirects, and reads the real name, size and type
- Up to 24 probes run concurrently and the manifest is fetched in parallel; a page with 40+ candidates usually resolves in 5–6 seconds
- Follows one more level of same-origin download pages
- When the page is rendered by JavaScript (nodejs.org only exposes the source tarball), it also reads the directory's checksum manifest (`SHASUMS256.txt` and friends) to list every artifact, including `.zip` / `.msi`; the manifest is fetched in parallel with probing
- SHA256 values from the manifest are carried along (marked `SHA256`) and verified after the download; a mismatch fails the task and reports both hashes
- When a page offers several platforms, the package matching your OS is pre-selected; "Choose myself" expands everything
- Results keep page order and can be ticked and queued in bulk
- Legacy GBK-encoded sites display titles correctly

### Mirrors (multi-source)

If a mirror hosts the same file, blocks are spread across sources. Built-in mirrors need no configuration:

| Source | Mirror |
| --- | --- |
| HuggingFace models / datasets | hf-mirror.com |
| npm packages | registry.npmmirror.com |
| Node.js releases | npmmirror.com/mirrors/node |
| PyPI packages | pypi.tuna.tsinghua.edu.cn |
| GitHub downloads | ghfast.top / gh-proxy.com / ghproxy.net (mutual backups) |

Correctness is never traded for speed:

- A mirror must answer a 1KB Range request with 206, report the same total length, and return matching leading bytes; mismatched content is rejected
- A newly added mirror only takes a few blocks until it proves it can deliver
- A mirror with no data for 1 second, or far below the fastest source, is dropped and its blocks are reassigned
- When the primary source is already fast, the app does not wait for mirrors

**Custom mirrors**: drop a `config/mirrors.json` (copy `config/mirrors.example.json`):

```json
{
  "enabled": true,
  "add": [
    { "from": "https://example.com/files/", "to": "https://mirror.example.com/files/" }
  ]
}
```

- `add` appends to the built-in rules
- `rules` replaces all built-in rules
- `enabled: false` disables mirrors entirely
- `DLKIT_NO_MIRROR=1` disables them for one run

A rule means: when a URL starts with `from`, also try the same URL with that prefix replaced by `to`. Mirrors must support Range requests. Mirrors route part of your traffic through third parties, so keep only ones you trust.

### Other sources

- Torrents / magnet links: handled by aria2
- Video sites (Bilibili, YouTube, Douyin and others): handled by yt-dlp, merged with ffmpeg
- Cloud drives: direct links via OpenList (you host OpenList yourself)

### Accounts

- The Accounts page manages Bilibili login: read cookies from Edge / Chrome / Firefox (passed to yt-dlp), or paste them manually
- Signing in enables 1080P, bangumi and premium content (resolution still depends on your account)
- Manually pasted cookies are encrypted with Windows DPAPI in `config/accounts.json`
- Cloud drives: enter the OpenList address and account on the same page and press "Save and test"; sign in to Baidu / Aliyun / Quark inside OpenList once, DLKit only fetches direct links

### Background and updates

- The UI is split into Download and Settings pages: the download page only handles links and tasks
- Closing the window does not quit: the app stays in the system tray and downloads keep running
- Hover the tray icon for task count and current speed; click to restore the window, right-click for "Show window" / "Quit DLKit"
- GitHub Releases are checked on startup; a new version shows a banner with download, SHA256 verification and restart-to-install, or you can pick "Later"
- The "Check updates" button in the header triggers a manual check

### While downloading

- Network throughput and disk write speed are shown separately
- The log panel records every retry and source switch
- If a domain resolves to 127.0.0.1 via hosts, the app says so and explains it may be a block or an accelerator that is not active

## Command line

```powershell
# Download a single link
DLKit.exe --headless "https://example.com/file.zip" -c 32 -d "D:\Downloads"

# Scan a download page and list candidates
DLKit.exe --scan "https://www.7-zip.org/download.html"
```

## Build

Requires Windows, stable Rust, and the system `curl` (Windows 10 1803+).

```powershell
cd ui
cargo build --release
```

The binary lands at `ui/target/release/dlkit.exe`; copy it to the app folder as `DLKit.exe`.

Pushing a `v*` tag triggers `.github/workflows/release.yml`, which builds, writes `SHA256.txt`, and publishes a release.

## Layout

```
ui/src/main.rs      UI, config, task scheduling
ui/src/engine.rs    download engine (blocks, mirrors, aria2, yt-dlp, OpenList)
ui/src/scanner.rs   download-page scanning (fetch, extract, probe)
ui/src/platform.rs  picks the download matching the current OS
ui/src/tray.rs      system tray (stay in background)
ui/src/update.rs    auto-update (check, download, verify, restart)
start.ps1           starts aria2 and OpenList
config/             configuration files and examples
```

## Limitations

- Windows only (uses positioned writes and hidden child-process windows)
- Page scanning does not execute JavaScript. Purely client-rendered pages (e.g. the VS Code site) only expose links literally present in the HTML, unless the directory has a checksum manifest
- Speed depends on how the site limits a single IP. If a site already saturates one connection, or throttles by total bytes, more connections may not beat your browser
- Resources behind a login, captcha or special headers are not supported yet — there is no Cookie input in the UI
- Cloud-drive support depends on which providers OpenList supports

## Disclaimer

This project is only a download tool and hosts no content. Download only what you have the right to access, and follow the terms of service of the sites you use.

## License

[MIT](LICENSE)
