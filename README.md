# DLKit

[English](README.en.md)

多功能下载器：直链分片下载、下载页解析、内置镜像多源加速。Rust + egui 写的单文件 exe，不需要浏览器内核，也不需要 Python 运行时。

[下载最新版](https://github.com/ikunshare988/DLKit/releases) · [命令行用法](#命令行) · [已知限制](#已知限制)

它主要解决三件事：

- **单站限速**：一个文件切成多块并发拉取；有镜像时把分块分散到多个源，总速度可以叠加
- **下载页没有直链**：粘贴下载页，程序把页面里的文件列出来，本机平台的版本会被自动选中
- **来源太杂**：直链、BT/磁力、视频站、网盘在同一个界面里排队下载

## 下载

从 [Releases](https://github.com/ikunshare988/DLKit/releases) 下载，解压到任意目录：

- `DLKit.exe` —— 轻量版，适合短期下载和普通用户
- `DLKit-enhanced.exe` —— 增强版，多出策略配置、下载后安全检查、云端信誉和沙箱

两个版本共用同一套核心（连接池、分片、解析加速、链接跟踪），只有增强版专属能力不同，详见[两个版本](#两个版本)。

exe 未做代码签名，Windows 首次运行时可能提示「已保护你的电脑」，点「更多信息 → 仍要运行」即可。每个 Release 都附 `SHA256.txt`，可以自行核对。

每个 Release 还附 `DLKit.exe.bundle` 和 `DLKit-enhanced.exe.bundle`：GitHub Actions 里的 Sigstore 无密钥签名，证书中记录了仓库、工作流和 tag。装了 [cosign](https://docs.sigstore.dev/cosign/installation/) 后可以验证：

```powershell
cosign verify-blob --bundle DLKit.exe.bundle `
  --certificate-identity-regexp "https://github.com/ikunshare988/DLKit/.github/workflows/release.yml@refs/tags/.*" `
  --certificate-oidc-issuer https://token.actions.githubusercontent.com `
  DLKit.exe
```

## 运行

最省事的用法：把 `DLKit.exe` 放到任意可写目录（桌面、`D:\DLKit` 都行），双击就能用。第一次启动会自动创建 `config/`、`logs/` 和 `aria2.conf`（`rpc-secret` 随机生成），下载默认保存到系统的「下载」文件夹。

直链下载只依赖程序自带的 curl，不需要额外组件。BT / 磁力 / 视频站需要的命令行走「设置 → 依赖」或首次使用时的「一键补齐」，程序会自己下载到 `bin/` 下，不需要装任何运行库：

```
DLKit.exe
bin/
  aria2/aria2c.exe
  yt-dlp/yt-dlp.exe
  ffmpeg/bin/ffmpeg.exe
  ffmpeg/bin/ffprobe.exe
  openlist/openlist.exe      # 可选，只有网盘需要
config/
  aria2.conf                 # 首次启动自动生成
  openlist.json              # 可选
logs/
```

- 添加磁力链接时如果还没装 aria2，会自动安装并在装好后自动启动服务，不用先看文档
- 完整包（含 `start.ps1`）会连 OpenList 一起启动；只有 exe 时程序直接拉起 aria2c
- 需要网盘时，复制 `config/openlist.json.example` 为 `config/openlist.json` 并填 OpenList 地址与账号

## 功能

### 直链下载

- 文件切成若干块并发拉取（默认 32 路，界面里可调 1–128）
- 高速链路上把并发调到 64–128 能继续叠加带宽：每条连接单独分块，固定走 HTTP/1.1，避免 HTTP/2 把几十条流挤进一条连接后被单连接拥塞窗口限制
- 常驻连接池（keep-alive）：连接反复复用，不再每块重启一次客户端，起步不用等一串握手
- 分块按字节动态切分：某条流明显慢时，只把它「还没下的那半段」交给别的连接，已写字节不重下
- 预留 25% 备用连接（如 64 路准备 80 条）：单流低于中位速度或整体速度掉到峰值的 60% 时，备用连接立即接手
- 先预分配文件空间，每块直接写到目标位置，没有临时文件和最后合并
- 断点续传：中途退出后重新添加同一个链接，会接着已完成的分块继续
- 暂停 / 继续：暂停时中断在途连接并保存分块进度，继续时只补没下完的部分
- 全局限速 0–100 MB/s（0 为不限，只作用于内置下载引擎）
- 复制链接后自动填入输入框；链接或 `.torrent` 文件可以直接拖进窗口
- 一次粘贴多行链接，逐行加入队列
- 同名文件已存在时自动改用「文件 (1).ext」，不会覆盖
- 下载前检查剩余磁盘空间，不够时直接说明需要多少、可用多少
- 目标不支持 Range 时自动退化为单连接下载
- 取得真实文件名（跟随重定向、读 `Content-Disposition`）
- 遇到 429/403 自动降低并发并退避；404/410 直接提示，不做无意义重试

### 下载页解析

- 探测走进程内连接池（keep-alive），不再每个候选启动一次 curl；本地 40 个候选约 0.15 秒
- GitHub 仓库 / Release 页直接调用官方 API，拿资产列表、大小与官方 SHA256，比解析 HTML 更快更准；API 不可用时自动回退到 HTML
- npm 包页（`npmjs.com/package/...`）走 registry（优先 npmmirror）取 tarball；PyPI 项目页走 `pypi.org/pypi/<name>/json`，列出 wheel / sdist 并带上官方 SHA256
- 同一页面 10 分钟内再次解析直接复用结果（含下载所需的 Referer / Cookie）
- 粘贴软件下载页（而不是直链），点「解析页面」
- 从 HTML 里提取链接：`a`、`video/audio/source`、`iframe`、`meta og:video`、`link rel=enclosure`，以及内联 JSON 里的地址
- 对每个候选发 HEAD 请求，跟随重定向，读取真实文件名、大小和类型
- 24 路并发探测，清单抓取与探测同时进行；40 多个候选的页面通常 1–3 秒出结果（取决于站点往返延迟）
- 同源的下载跳转页会再跟进一层
- 页面用 JS 渲染时（例如 nodejs.org，HTML 里只有源码包），会读同目录下的校验和清单（`SHASUMS256.txt` 这类）把该版本的所有文件补齐，`.zip` / `.msi` 都能列出来
- 清单里的 SHA256 会一起带下来（带 `SHA256` 标记），下载完成后自动核对，不一致会判为失败并给出期望值与实际值；清单抓取与探测并行进行，不额外拖慢解析
- 同一页有多个平台版本时（Windows / macOS / Linux），自动按本机系统选中对应安装包，点「自己选」可展开全部
- 结果按页面顺序列出，可勾选后批量加入下载队列
- 中文 GBK 编码的老站点也能正常显示标题

### 多源（镜像）

同一个文件如果镜像站上也有，程序会把分块分散到多个源同时下载。内置的镜像不需要配置：

| 来源 | 镜像 |
| --- | --- |
| HuggingFace 模型 / 数据集 | hf-mirror.com |
| npm 包 | registry.npmmirror.com |
| Node.js 官方发行包 | npmmirror.com/mirrors/node |
| PyPI 包 | pypi.tuna.tsinghua.edu.cn（清华源） |
| GitHub 下载 | ghfast.top / gh-proxy.com / ghproxy.net（三个代理互为备份） |

镜像不会影响文件正确性：

- 参与前先校验：用 Range 取开头 1KB，必须返回 206、文件总长度与主站一致，开头字节也要和主站一致，同大小不同内容的文件混不进来；2 秒内没响应的直接跳过
- 新加入的镜像先只领少量分块，确认能正常下载后才参与轮换
- 有分块却 1 秒没有数据，或吞吐明显低于最快源，镜像会被停用，已领的分块交回主站重下
- 主站本身就很快时（探测阶段已跑满）不会为了等镜像而多停

**自己加镜像**：放一个 `config/mirrors.json`（可复制 `config/mirrors.example.json`）：

```json
{
  "enabled": true,
  "add": [
    { "from": "https://example.com/files/", "to": "https://mirror.example.com/files/" }
  ]
}
```

- `add`：在内置规则之外追加
- `rules`：替换全部内置规则
- `enabled: false`：完全关闭镜像
- 环境变量 `DLKIT_NO_MIRROR=1` 也能临时关闭

规则的含义是：下载地址以 `from` 开头时，额外尝试把前缀换成 `to` 的镜像地址。镜像站必须支持 Range 请求，否则校验通不过。镜像会把一部分流量交给第三方站点，请只保留你信任的镜像。

### 其它来源

- BT / 磁力：交给 aria2
- 视频站（B站、YouTube、抖音等）：交给 yt-dlp，音视频用 ffmpeg 合并
- 网盘：通过 OpenList 取直链（需要自己部署 OpenList 并挂载）

### 账号

- 「账号」页管理 B 站登录：可以直接读取 Edge / Chrome / Firefox 里已登录的 Cookie（交给 yt-dlp），也可以手动粘贴
- 登录后可下载 1080P、番剧和大会员内容（清晰度仍取决于账号权限）
- 手动粘贴的 Cookie 用 Windows DPAPI 加密后保存在 `config/accounts.json`，拷走配置也读不出明文
- 网盘：在同一页填 OpenList 地址和账号，点「保存并测试」；百度 / 阿里 / 夸克等先在 OpenList 后台登录一次，DLKit 只负责取直链

### 后台与更新

- 界面分「下载」「跟踪」「账号」「设置」四页：下载页只管链接与任务，并发 / 限速 / 目录 / 引擎和依赖都在设置页
- 点窗口的 × 不退出：程序收进系统托盘，正在下载的任务继续跑
- 托盘图标悬停显示任务数与当前速度；单击恢复主窗口，右键菜单里是「显示主窗口」「退出 DLKit」
- 启动时自动检查 GitHub Releases，发现新版本会在窗口顶部提示；点「下载并更新」会下载、核对 SHA256、重启替换，也可以选「稍后」
- 顶部「检查更新」可以手动触发

### 首次使用与自助排查

- 首次启动检测缺哪些组件，一次点「一键补齐」按顺序装好；直链下载不需要任何组件
- 下载目录默认是系统的「下载」文件夹，不用去程序目录里找
- 任务失败时不只是报错：403 / 401 给「去账号页」，磁盘不足给「去设置换目录」，aria2 未运行给「启动服务」，还可以「复制错误」
- 「设置 → 环境检查」检查程序目录可写、配置、下载目录与剩余空间、三个组件、aria2 服务、网络连通，结论用一句话说清，报告可一键复制
- 程序放在 `Program Files` 这类不可写目录时，顶部会直接提示并给出「打开目录」
- 任何崩溃都会写 `logs\crash-*.log`，下次启动提示「上次没有正常退出」，可以查看或忽略

### 策略与资源调配

复制 `config/policy.example.json` 为 `config/policy.json` 后生效，「设置 → 策略」里可以重新加载：

- `profiles`：按域名或类型（`http` / `torrent` / `media` / `netdisk` / `openlist`）覆盖并发（`conn`）、单任务限速（`limit_mbps`）和排队优先级（`priority`: `high` / `normal` / `low`）
- `budget.max_connections`：所有任务共用的连接上限，profile 里的值也会被它压住
- `budget.max_active_tasks`：同时下载的任务上限，超出的进入队列；有任务结束时空出的槽位按 `priority` 分配
- `schedule`：时段限速，支持跨零点（`23:00`–`07:00`）；命中时覆盖「设置」里的全局限速，离开时段后恢复
- 格式错误不会导致启动失败，设置页会显示出错行号并沿用默认值

### 安全（增强版）

- 下载完成后自动检查可执行文件、压缩包和文档；视频 / 图片 / 音频直接跳过
- 三层检查：文件名与文件头启发式（双扩展名、RLO 伪装字符、内容与扩展名不符、宏文档）、内嵌 Authenticode 签名、Windows Defender 单文件扫描
- 可选云端信誉：VirusTotal v3 按 SHA256 查询，只发哈希、不上传文件；API key 存在 `config/security.json`，默认关闭
- 沙箱模式：下载先落在 `<下载目录>\.dlkit-sandbox`，检查通过后自动放行；发现风险或未检查的文件留在沙箱，可在任务卡上手动放行
- Defender 以空闲优先级运行、只检测不处置（`-DisableRemediation`），不会替你删除任何文件
- 结论分「未发现威胁 / 有可疑特征 / 发现风险 / 检查未完成」；用词避免绝对化
- 有风险时提供「隔离」和「保留」：隔离是把文件移到 `quarantine/` 并写一份来源记录，可人工恢复
- 超过设定大小（默认 4GB）跳过 Defender，只做启发式与签名检查
- Defender 被组策略或第三方杀软禁用时如实报告「检查未完成」，不会假装安全
- 手动检查单个文件：`DLKit.exe --av-scan "D:\path\file.exe"`

### 链接跟踪

- 直链可以加入「跟踪」：程序停在托盘里时后台定时检查，发现更新就弹 Windows 通知
- 默认每 2 分钟检查一次；每次打开软件会先把所有条目扫一遍
- 只提示，不自动下载——点通知或到「跟踪」页点「下载」才会开始
- 检查用条件请求（`If-None-Match` / `If-Modified-Since`），服务端返回 304 时不传正文，几乎不耗流量
- 判定顺序：`ETag` → 文件大小 → 重定向后的最终地址（`releases/latest` 这类能看出目录里的版本号）；只有 `Last-Modified` 或文件名变化的算弱信号，连续两次都不同才通知，避免 CDN 抖动误报
- 预签名链接的 query 每次都会变，比较时会剥掉，不会因此误报
- 链接 404/410 标为「已失效」，降到每小时检查一次；检查失败按 1→2→4…分钟退避，最长 32 分钟
- 跟踪列表存在 `config/watches.json`，可以直接编辑或备份

### 下载过程

- 同时显示网络速度和磁盘写入速度
- 日志面板记录每次重试与换源的原因
- 域名被 hosts 指向 127.0.0.1 时会给出明确提示，并说明可能是屏蔽或加速器未生效

## 命令行

不方便开界面时，可以直接用命令行：

```powershell
# 下载单个链接
DLKit.exe --headless "https://example.com/file.zip" -c 32 -d "D:\Downloads"

# 只解析下载页，列出候选文件
DLKit.exe --scan "https://www.7-zip.org/download.html"
```

## 两个版本

同一套代码，两个构建：

| | 轻量版（默认） | 增强版（`--features enhanced`） |
| --- | --- | --- |
| 直链 / 磁力 / 视频站 / 网盘 | 有 | 有 |
| 连接池、字节级切分、备用连接 | 有 | 有 |
| 解析加速、GitHub API、缓存 | 有 | 有 |
| 按实测吞吐加权选源 | 有 | 有 |
| 链接跟踪 | 有 | 有 |
| 策略配置（按站点并发、预算、时段限速） | — | 有 |
| 下载后自动安全检查（启发式 / 签名 / Defender / 隔离） | — | 有 |

```powershell
cd ui
cargo build --release                    # 轻量版
cargo build --release --features enhanced  # 增强版
```

## 基准测试

判断改动是否真的更快，用内置的基准命令，而不是凭感觉：

```powershell
# 下载：输出每轮的时间、平均/峰值速度、首字节、尾部耗时
DLKit.exe --bench "https://example.com/file.zip" -c 32 --repeat 3

# 解析：输出耗时与候选数量
DLKit.exe --bench-scan "https://www.7-zip.org/download.html" --repeat 3
```

本地可重复的限速环境（单连接限速、支持 Range/ETag、`/bump` 改版本）：

```powershell
python tools/bench_server.py --port 8907 --rate-mbps 4 --latency-ms 20
```

## MCP（给 AI 客户端用）

同一个 exe 也能作为 MCP 服务器运行，把下载能力直接交给 AI 客户端（Trae、Claude Desktop、Cursor 等都支持）：

```powershell
DLKit.exe --mcp
```

走 stdin/stdout 的 JSON-RPC，一行一条消息。不需要 Python、Node 或任何额外服务，也不影响双击启动的图形界面。

在客户端的 MCP 配置里加一段（路径换成实际的 exe 位置）：

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

配好之后可以直接说「把这个页面的 Windows 版下下来，下完核对 SHA256」，客户端会按下面的工具自行编排：

| 工具 | 作用 |
| --- | --- |
| `scan_page` | 解析下载页，返回文件清单、大小、类型、官方 SHA256，并标出本机推荐项 |
| `download` | 下载直链 / 磁力 / 媒体页 / `openlist:` 路径，立即返回 `task_id` |
| `download_from_page` | 一步完成：解析 → 按 `index` 或推荐项挑一个 → 开始下载 |
| `task_status` | 查询单个任务的状态、百分比、速度、保存路径与近期日志 |
| `list_tasks` | 列出本进程内的所有任务 |
| `cancel_task` | 取消任务（已写入的部分保留，可续传） |
| `verify_file` | 计算本地文件 SHA256，可与期望值比对 |

几点约定：

- 任务跑在 MCP 进程里，进程退出任务就停；要长期后台下载就用图形界面
- 保存目录和默认并发沿用「设置」页的配置（`config/ui.json`）
- 工具执行失败以 `isError: true` 返回，不会打断会话
- 大文件不要一次设很长的 `wait_seconds`，先拿 `task_id` 再轮询更稳

## 构建

需要 Windows、Rust stable，以及系统自带的 curl（Windows 10 1803 及以上）。

```powershell
cd ui
cargo build --release
```

产物在 `ui/target/release/dlkit.exe`，复制到程序目录并改名为 `DLKit.exe` 即可。

如果拉取依赖很慢，可以按自己的网络环境配置 crates.io 镜像，例如在 `ui/.cargo/config.toml` 里指定 rsproxy。

打 `v*` 标签推送时，`.github/workflows/release.yml` 会自动编译、生成 `SHA256.txt`、用 Sigstore 无密钥签名产出 `DLKit.exe.bundle`，然后创建 Release。

## 项目结构

```
ui/src/main.rs      界面、配置、任务调度
ui/src/engine.rs    下载引擎（连接池分片、多源、aria2、yt-dlp、OpenList）
ui/src/scanner.rs   下载页解析（抓页面、提取链接、探测候选）
ui/src/platform.rs  按本机平台推荐下载项
ui/src/accounts.rs  账号（B站 Cookie 加密保存、OpenList 凭据）
ui/src/policy.rs    策略（按站点覆盖并发、预算、时段限速）
ui/src/deps.rs      依赖检测与一键安装
ui/src/mcp.rs       MCP 服务器（AI 客户端调用下载能力）
ui/src/watch.rs     链接跟踪（定时检查直链更新并通知）
ui/src/setup.rs     首次运行准备（目录、配置、崩溃日志）
ui/src/diag.rs      环境检查与报告
ui/src/tray.rs      系统托盘（后台常驻）
ui/src/update.rs    自动更新（检查、下载、校验、重启替换）
start.ps1           启动 aria2 与 OpenList
tools/bench_server.py  可重复的限速测试服务器
config/             配置文件与示例
```

## 已知限制

- 只支持 Windows（依赖 Windows 的定位写入、隐藏子进程窗口等接口）
- 页面解析不执行 JavaScript。纯前端渲染的下载页（例如 VS Code 官网），如果同目录也没有校验和清单，就只能列出 HTML 里直接写出的链接
- 速度取决于站点对单个 IP 的限制。站点给满单连接带宽、或对总量限速时，多连接不一定比浏览器快
- 需要登录的资源目前只支持在「账号」页配置 B 站 Cookie；其它站点、验证码或特殊请求头还没有填写入口
- 网盘功能依赖 OpenList 自身支持的网盘类型

## 免责声明

本项目只提供下载工具，不提供任何资源。请只下载你有权获取的内容，并遵守目标站点的服务条款和当地法律。

## 许可证

[MIT](LICENSE)
