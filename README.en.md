# DLKit

[中文](README.md)

A multi-purpose downloader for Windows: segmented direct downloads, download-page scanning, and built-in mirrors that let several sources share the work. Written in Rust + egui, shipped as a single exe — no browser engine, no Python runtime.

[Download](https://github.com/ikunshare988/DLKit/releases) · [Command line](#command-line) · [Limitations](#limitations)

What it is for:

- **Per-IP throttling**: files are split into blocks and fetched in parallel; when a mirror exists, blocks are spread across sources so the total can add up
- **Download pages without direct links**: paste the page, get the list of files, and the build for your OS is selected for you
- **Too many sources**: direct links, torrents, video sites and cloud drives all queue in one window

## Download

Get the build you need from [Releases](https://github.com/ikunshare988/DLKit/releases) and unzip it anywhere:

- `DLKit.exe` — Lite, for short downloads and everyday use
- `DLKit-enhanced.exe` — Enhanced, adding policy configuration, post-download security checks, cloud reputation and sandboxing

Both share the same core (pooled connections, splitting, faster scanning, link tracking); only the enhanced-only capabilities differ — see [Two editions](#two-editions).

The exe is not code-signed, so Windows SmartScreen may warn on first launch — choose "More info → Run anyway". Every release ships a `SHA256.txt` you can verify.

Each release also ships `DLKit.exe.bundle` and `DLKit-enhanced.exe.bundle`, keyless Sigstore signatures produced in GitHub Actions. The certificate records this repository, workflow and tag. With [cosign](https://docs.sigstore.dev/cosign/installation/) installed:

```powershell
cosign verify-blob --bundle DLKit.exe.bundle `
  --certificate-identity-regexp "https://github.com/ikunshare988/DLKit/.github/workflows/release.yml@refs/tags/.*" `
  --certificate-oidc-issuer https://token.actions.githubusercontent.com `
  DLKit.exe
```

## Setup

The simplest path: drop `DLKit.exe` into any writable folder (Desktop, `D:\DLKit`) and double-click it. The first launch creates `config/`, `logs/` and `aria2.conf` with a random `rpc-secret`; downloads go to your system Downloads folder by default.

Direct downloads only need the system `curl`. The command-line tools used by torrents and video sites can be installed from Settings → Dependencies, or with the one-click banner shown on first use:

```
DLKit.exe
bin/
  aria2/aria2c.exe
  yt-dlp/yt-dlp.exe
  ffmpeg/bin/ffmpeg.exe
  ffmpeg/bin/ffprobe.exe
  openlist/openlist.exe      # optional, cloud drives only
config/
  aria2.conf                 # generated on first launch
  openlist.json              # optional
logs/
```

- Adding a magnet link auto-installs aria2 if needed and starts the service once it is ready
- The full package (with `start.ps1`) also starts OpenList; an exe-only install launches aria2c directly
- For cloud drives, copy `config/openlist.json.example` to `config/openlist.json` and fill in your OpenList address and account

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

- Probing uses an in-process keep-alive pool instead of spawning curl per candidate; 40 candidates resolve in ~0.15s locally
- GitHub repository / release pages are read through the official API (assets, sizes, digests), falling back to HTML when the API is unavailable
- npm package pages go through the registry (npmmirror first) for the tarball; PyPI project pages use `pypi.org/pypi/<name>/json` and list wheels / sdists with their official SHA256
- Re-scanning the same page within 10 minutes reuses the cached result, including Referer / Cookie
- Paste a download page (not a direct link) and hit "Scan page"
- Extracts links from `a`, `video/audio/source`, `iframe`, `meta og:video`, `link rel=enclosure`, plus URLs embedded in inline JSON
- Sends a HEAD request per candidate, follows redirects, and reads the real name, size and type
- Up to 24 probes run concurrently and the manifest is fetched in parallel; a page with 40+ candidates usually resolves in 1–3 seconds, depending on round-trip time
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

- The UI has four pages — Download, Tracking, Accounts and Settings; the download page only handles links and tasks
- Closing the window does not quit: the app stays in the system tray and downloads keep running
- Hover the tray icon for task count and current speed; click to restore the window, right-click for "Show window" / "Quit DLKit"
- GitHub Releases are checked on startup; a new version shows a banner with download, SHA256 verification and restart-to-install, or you can pick "Later"
- The "Check updates" button in the header triggers a manual check

### First run and self-service

- The first launch lists missing components and installs them all with one click; direct downloads need none of them
- Downloads default to your system Downloads folder
- Failed tasks offer a next step instead of a bare error: 403/401 → Accounts, not enough disk → Settings, aria2 offline → Start service, plus "Copy error"
- Settings → Environment check verifies writable folder, config, download folder and free space, the three tools, aria2, and network, with a one-line verdict and a copyable report
- If DLKit sits in a read-only folder such as `Program Files`, the app says so at the top and offers to open the folder
- Crashes are written to `logs\crash-*.log`; the next launch offers to view or dismiss them

### Policy and resource allocation

Copy `config/policy.example.json` to `config/policy.json`; reload it from Settings → Policy:

- `profiles`: override concurrency (`conn`), per-task bandwidth (`limit_mbps`) and queue priority (`high` / `normal` / `low`) by host or kind
- `budget.max_connections`: shared ceiling across tasks; profile values are capped by it
- `budget.max_active_tasks`: concurrent task limit; extra tasks queue and take freed slots by priority
- `schedule`: time-of-day limits, midnight-crossing supported (`23:00`–`07:00`); an active slot overrides the global limit and restores it afterwards
- A malformed file never blocks startup: Settings shows the line number and the defaults stay active

### Security (enhanced)

- Executables, archives and documents are checked after download; video, images and audio are skipped
- Three layers: filename/header heuristics (double extensions, RLO spoofing, content/extension mismatch, macro documents), embedded Authenticode signature, and a Windows Defender single-file scan
- Optional cloud reputation: VirusTotal v3 by SHA256 only — the file is never uploaded; the API key lives in `config/security.json` and the feature is off by default
- Sandbox mode: downloads land in `<download dir>\.dlkit-sandbox` and are released automatically once they pass; risky or unchecked files stay there until you release them from the task card
- Defender runs at idle priority and only detects (`-DisableRemediation`) — nothing is ever deleted for you
- Verdicts are phrased as "no threats found / suspicious traits / risk found / check incomplete"
- On risk you can quarantine or keep the file; quarantining moves it into `quarantine/` with a JSON record so it can be restored
- Files above the size limit (4GB by default) skip Defender and get heuristics plus signature checks only
- If Defender is disabled by policy or another AV, the report says the check was incomplete rather than claiming safety
- Check a single file manually: `DLKit.exe --av-scan "D:\path\file.exe"`

### Link tracking

- Any direct link can be tracked: while the app sits in the tray it polls in the background and shows a Windows notification when the file changes
- Every 2 minutes by default; all entries are checked once whenever the app starts
- Notification only — nothing is downloaded until you press "Download" on the Tracking page (or in the toast's target folder)
- Checks use conditional requests (`If-None-Match` / `If-Modified-Since`); a 304 means no body is transferred
- Change signals in order: `ETag` → file size → final URL after redirects (so `releases/latest`-style links reveal the version in the path). A `Last-Modified` or filename change alone is a weak signal and has to occur twice, which keeps CDN jitter from raising false alarms
- Query strings are stripped before comparison, so pre-signed links do not look new on every poll
- 404/410 marks the entry as gone and drops to hourly checks; failures back off 1→2→4… minutes, up to 32
- The list lives in `config/watches.json` and can be edited or backed up directly

### While downloading

- Network throughput and disk write speed are shown separately
- The log panel has two tabs, Program and Task, both timestamped
- Program logs cover startup, paths, parameters, dependencies, aria2 status, accounts, policy and update checks through initialization, and can be copied in one click
- Task logs record the probe result (HTTP status, range support, size, final URL), resume state, pool parameters, retries and source switches, plus final duration and average speed
- If a domain resolves to 127.0.0.1 via hosts, the app says so and explains it may be a block or an accelerator that is not active

## Command line

```powershell
# Download a single link
DLKit.exe --headless "https://example.com/file.zip" -c 32 -d "D:\Downloads"

# Scan a download page and list candidates
DLKit.exe --scan "https://www.7-zip.org/download.html"
```

## Two editions

One codebase, two builds:

| | Lite (default) | Enhanced (`--features enhanced`) |
| --- | --- | --- |
| Direct links / torrents / video sites / cloud drives | yes | yes |
| Pooled connections, byte-level splitting, standby lanes | yes | yes |
| Faster scanning, GitHub API, cache | yes | yes |
| Throughput-weighted source selection | yes | yes |
| Link tracking | yes | yes |
| Policy (per-host concurrency, budget, time-of-day limits) | — | yes |
| Post-download security scan (heuristics / signature / Defender / quarantine) | — | yes |

```powershell
cd ui
cargo build --release                    # lite
cargo build --release --features enhanced  # enhanced
```

## Benchmarks

Use the built-in benchmark commands instead of guessing whether a change helped:

```powershell
# Downloads: time, average/peak speed, time to first byte, tail time
DLKit.exe --bench "https://example.com/file.zip" -c 32 --repeat 3

# Scanning: elapsed time and candidate count
DLKit.exe --bench-scan "https://www.7-zip.org/download.html" --repeat 3
```

A reproducible local target (per-connection rate limit, Range/ETag, `/bump` to change the version):

```powershell
python tools/bench_server.py --port 8907 --rate-mbps 4 --latency-ms 20
```

## MCP (for AI clients)

The same exe can run as an MCP server, handing the downloader directly to AI clients (Trae, Claude Desktop, Cursor and others):

```powershell
DLKit.exe --mcp
```

It speaks JSON-RPC over stdin/stdout, one message per line. No Python, Node or extra service is needed, and the normal GUI still works by double-clicking the exe.

Add this to your client's MCP configuration (point the path at your exe):

```json
{
  "mcpServers": {
    "dlkit": {
      "command": "D:\\DLKit\\DLKit.exe",
      "args": ["--mcp"]
    }
  }
}
```

You can then say "download the Windows build from this page and verify its SHA256" and the client will compose the tools itself:

| Tool | Purpose |
| --- | --- |
| `scan_page` | Scan a download page; returns files, sizes, types, official SHA256, and the build recommended for this machine |
| `download` | Start a direct link / magnet / media page / `openlist:` path, returns a `task_id` immediately |
| `download_from_page` | One step: scan → pick by `index` or the recommendation → start downloading |
| `task_status` | Status, percent, speed, output path and recent log lines for one task |
| `list_tasks` | All tasks in this process |
| `cancel_task` | Cancel a task (written bytes are kept and resumable) |
| `verify_file` | Compute a local file's SHA256, optionally against an expected value |

Notes:

- Tasks live inside the MCP process; quitting it stops them. Use the GUI for long background downloads
- Output directory and default concurrency come from the Settings page (`config/ui.json`)
- Tool failures return `isError: true` instead of breaking the session
- For large files, take the `task_id` and poll instead of one long `wait_seconds`

## Build

Requires Windows, stable Rust, and the system `curl` (Windows 10 1803+).

```powershell
cd ui
cargo build --release
```

The binary lands at `ui/target/release/dlkit.exe`; copy it to the app folder as `DLKit.exe`.

Pushing a `v*` tag triggers `.github/workflows/release.yml`, which builds, writes `SHA256.txt`, signs the exe keylessly with Sigstore into `DLKit.exe.bundle`, and publishes a release.

## Layout

```
ui/src/main.rs      UI, config, task scheduling
ui/src/engine.rs    download engine (pooled blocks, mirrors, aria2, yt-dlp, OpenList)
ui/src/scanner.rs   download-page scanning (fetch, extract, probe)
ui/src/platform.rs  picks the download matching the current OS
ui/src/accounts.rs  accounts (DPAPI-encrypted Bilibili cookie, OpenList credentials)
ui/src/policy.rs    policy (per-host concurrency, budget, time-of-day limits)
ui/src/deps.rs      dependency check and one-click install
ui/src/mcp.rs       MCP server (lets AI clients drive downloads)
ui/src/watch.rs     link tracking (polls direct links and notifies on change)
ui/src/setup.rs     first-run setup (folders, config, crash log)
ui/src/diag.rs      environment checks and report
ui/src/tray.rs      system tray (stay in background)
ui/src/update.rs    auto-update (check, download, verify, restart)
start.ps1           starts aria2 and OpenList
tools/bench_server.py  reproducible throttled test server
config/             configuration files and examples
```

## Limitations

- Windows only (uses positioned writes and hidden child-process windows)
- Page scanning does not execute JavaScript. Purely client-rendered pages (e.g. the VS Code site) only expose links literally present in the HTML, unless the directory has a checksum manifest
- Speed depends on how the site limits a single IP. If a site already saturates one connection, or throttles by total bytes, more connections may not beat your browser
- Login is only supported for Bilibili, through the cookie field on the Accounts page; other sites, captchas and special headers have no input yet
- Cloud-drive support depends on which providers OpenList supports

## Disclaimer

This project is only a download tool and hosts no content. Download only what you have the right to access, and follow the terms of service of the sites you use.

## License

[MIT](LICENSE)
