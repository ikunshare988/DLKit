# DLKit

[English](README.en.md)

多功能下载器：直链分片下载、下载页解析、内置镜像多源加速。Rust + egui 写的单文件 exe，不需要浏览器内核，也不需要 Python 运行时。

[下载最新版](https://github.com/ikunshare988/DLKit/releases) · [命令行用法](#命令行) · [已知限制](#已知限制)

它主要解决三件事：

- **单站限速**：一个文件切成多块并发拉取；有镜像时把分块分散到多个源，总速度可以叠加
- **下载页没有直链**：粘贴下载页，程序把页面里的文件列出来，本机平台的版本会被自动选中
- **来源太杂**：直链、BT/磁力、视频站、网盘在同一个界面里排队下载

## 下载

从 [Releases](https://github.com/ikunshare988/DLKit/releases) 下载 `DLKit.exe`，解压到任意目录。

exe 未做代码签名，Windows 首次运行时可能提示「已保护你的电脑」，点「更多信息 → 仍要运行」即可。每个 Release 都附 `SHA256.txt`，可以自行核对。

## 运行

程序本身是一个 exe，但下载能力依赖几个命令行工具。按下面的结构放在程序目录下：

```
DLKit.exe
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

- 只下直链或解析页面：直接双击 `DLKit.exe`
- 需要 BT、网盘或 aria2 兜底：先运行 `start.ps1`，再打开程序

## 功能

### 直链下载

- 文件切成若干块并发拉取（默认 32 路，界面里可调 1–64）
- 先预分配文件空间，每块直接写到目标位置，没有临时文件和最后合并
- 断点续传：中途退出后重新添加同一个链接，会接着已完成的分块继续
- 目标不支持 Range 时自动退化为单连接下载
- 取得真实文件名（跟随重定向、读 `Content-Disposition`）
- 遇到 429/403 自动降低并发并退避；404/410 直接提示，不做无意义重试

### 下载页解析

- 粘贴软件下载页（而不是直链），点「解析页面」
- 从 HTML 里提取链接：`a`、`video/audio/source`、`iframe`、`meta og:video`、`link rel=enclosure`，以及内联 JSON 里的地址
- 对每个候选发 HEAD 请求，跟随重定向，读取真实文件名、大小和类型
- 同源的下载跳转页会再跟进一层
- 页面用 JS 渲染时（例如 nodejs.org，HTML 里只有源码包），会读同目录下的校验和清单（`SHASUMS256.txt` 这类）把该版本的所有文件补齐，`.zip` / `.msi` 都能列出来
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

## 构建

需要 Windows、Rust stable，以及系统自带的 curl（Windows 10 1803 及以上）。

```powershell
cd ui
cargo build --release
```

产物在 `ui/target/release/dlkit.exe`，复制到程序目录并改名为 `DLKit.exe` 即可。

如果拉取依赖很慢，可以按自己的网络环境配置 crates.io 镜像，例如在 `ui/.cargo/config.toml` 里指定 rsproxy。

打 `v*` 标签推送时，`.github/workflows/release.yml` 会自动编译、生成 `SHA256.txt` 并创建 Release。

## 项目结构

```
ui/src/main.rs      界面、配置、任务调度
ui/src/engine.rs    下载引擎（分片、多源、aria2、yt-dlp、OpenList）
ui/src/scanner.rs   下载页解析（抓页面、提取链接、探测候选）
ui/src/platform.rs  按本机平台推荐下载项
start.ps1           启动 aria2 与 OpenList
config/             配置文件与示例
```

## 已知限制

- 只支持 Windows（依赖 Windows 的定位写入、隐藏子进程窗口等接口）
- 页面解析不执行 JavaScript。纯前端渲染的下载页（例如 VS Code 官网），如果同目录也没有校验和清单，就只能列出 HTML 里直接写出的链接
- 速度取决于站点对单个 IP 的限制。站点给满单连接带宽、或对总量限速时，多连接不一定比浏览器快
- 需要登录、验证码或特殊请求头的资源，目前没有在界面里填 Cookie 的入口
- 网盘功能依赖 OpenList 自身支持的网盘类型

## 免责声明

本项目只提供下载工具，不提供任何资源。请只下载你有权获取的内容，并遵守目标站点的服务条款和当地法律。

## 许可证

[MIT](LICENSE)
