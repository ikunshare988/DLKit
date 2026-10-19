# OST 下载器

Windows 上的桌面下载工具。把链接粘进去，它会用多个连接分段下载；如果给的是软件下载页，它会先把页面里的文件列出来，勾选后批量下载。

界面用 Rust + egui 写，不需要浏览器内核，也不需要 Python 运行时。

## 功能

**直链下载**
- 把文件切成若干块，多连接并发拉取（默认 32 路，可在界面里调 1–64）
- 先预分配文件空间，每块直接写到目标位置，没有临时文件和最后合并
- 断点续传：中途退出后重新添加同一个链接，会接着已完成的分块继续
- 目标不支持 Range 时自动退化为单连接下载

**下载页解析**
- 粘贴软件下载页（而不是直链），点「解析页面」
- 从 HTML 里提取链接：`a`、`video/audio/source`、`iframe`、`meta og:video`、`link rel=enclosure`，以及内联 JSON 里的地址
- 对每个候选发 HEAD 请求，跟随重定向，读取真实文件名、大小和类型
- 同源的下载跳转页会再跟进一层
- 结果按页面顺序列出，可勾选后批量加入下载队列
- 中文 GBK 编码的老站点也能正常显示标题

**其它来源**
- BT / 磁力：交给 aria2
- 视频站（B站、YouTube、抖音等）：交给 yt-dlp，音视频用 ffmpeg 合并
- 网盘：通过 OpenList 取直链（需要自己部署 OpenList 并挂载）

**下载过程**
- 同时显示网络速度和磁盘写入速度
- 遇到 429/403 自动降低并发并退避重试；404/410 直接提示，不无意义重试
- 日志面板会记录每次重试的原因
- 域名被 hosts 指向 127.0.0.1 时会给出明确提示

## 下载

仓库里附带了一份已编译好的程序：[dist/OSTDownloader.exe](dist/OSTDownloader.exe)。

它是未签名的 exe，Windows 首次运行时可能提示“已保护你的电脑”，点“更多信息 → 仍要运行”即可。也可以按下面的说明自行编译。

## 运行

程序本身是一个 exe，但下载能力依赖几个命令行工具。把它们放到程序目录下：

```
OSTDownloader.exe
bin/
  aria2/aria2c.exe
  yt-dlp/yt-dlp.exe
  ffmpeg/bin/ffmpeg.exe
  ffmpeg/bin/ffprobe.exe
  openlist/openlist.exe      # 可选，只有网盘需要
config/
  aria2.conf
  openlist.json              # 可选
downloads/
```

准备配置：

1. 复制 `config/aria2.conf.example` 为 `config/aria2.conf`，把里面的路径和 `rpc-secret` 改成自己的
2. 需要网盘时，复制 `config/openlist.json.example` 为 `config/openlist.json` 并填 OpenList 地址与账号

启动：

- 只下直链或解析页面：直接双击 `OSTDownloader.exe`
- 需要 BT、网盘或 aria2 兜底：先运行 `start.ps1`，再打开程序

## 构建

需要 Windows、Rust stable，以及系统自带的 curl（Windows 10 1803 及以上）。

```powershell
cd ui
cargo build --release
```

产物在 `ui/target/release/ost-downloader.exe`，复制到程序目录并改名为 `OSTDownloader.exe` 即可。

如果拉取依赖很慢，可以按自己的网络环境配置 crates.io 镜像，例如在 `ui/.cargo/config.toml` 里指定 rsproxy。

## 命令行

不方便开界面时，可以直接用命令行：

```powershell
# 下载单个链接
OSTDownloader.exe --headless "https://example.com/file.zip" -c 32 -d "D:\Downloads"

# 只解析下载页，列出候选文件
OSTDownloader.exe --scan "https://www.7-zip.org/download.html"
```

## 项目结构

```
ui/src/main.rs      界面、配置、任务调度
ui/src/engine.rs    下载引擎（分片、aria2、yt-dlp、OpenList）
ui/src/scanner.rs   下载页解析（抓页面、提取链接、探测候选）
start.ps1           启动 aria2 与 OpenList
config/             配置文件与示例
```

## 已知限制

- 只支持 Windows（依赖 Windows 的定位写入、隐藏子进程窗口等接口）
- 页面解析不执行 JavaScript。纯前端渲染的下载页（例如 VS Code 官网）解析不到文件，会提示“可能是 JS 动态加载”
- 速度取决于站点对单个 IP 的限制。站点给满单连接带宽、或对总量限速时，多连接不一定比浏览器快
- 需要登录、验证码或特殊请求头的资源，目前没有在界面里填 Cookie 的入口
- 网盘功能依赖 OpenList 自身支持的网盘类型

## 免责声明

本项目只提供下载工具，不提供任何资源。请只下载你有权获取的内容，并遵守目标站点的服务条款和当地法律。

## 许可证

[MIT](LICENSE)
