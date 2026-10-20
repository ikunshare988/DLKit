# DLKit

多功能下载器。Windows 上的桌面下载工具，把链接粘进去，它会用多个连接分段下载；如果给的是软件下载页，它会先把页面里的文件列出来，勾选后批量下载。

界面用 Rust + egui 写，不需要浏览器内核，也不需要 Python 运行时。

## 功能

**直链下载**
- 把文件切成若干块，多连接并发拉取（默认 32 路，可在界面里调 1–64）
- 先预分配文件空间，每块直接写到目标位置，没有临时文件和最后合并
- 断点续传：中途退出后重新添加同一个链接，会接着已完成的分块继续
- 目标不支持 Range 时自动退化为单连接下载
- 镜像叠加：内置 HuggingFace / npm / Node.js / PyPI / GitHub 加速等镜像，分块会分散到多个源，单站限速时总速度可以叠加

**下载页解析**
- 粘贴软件下载页（而不是直链），点「解析页面」
- 从 HTML 里提取链接：`a`、`video/audio/source`、`iframe`、`meta og:video`、`link rel=enclosure`，以及内联 JSON 里的地址
- 对每个候选发 HEAD 请求，跟随重定向，读取真实文件名、大小和类型
- 同源的下载跳转页会再跟进一层
- 页面用 JS 渲染时（例如 nodejs.org，HTML 里只有源码包），会读同目录下的校验和清单（`SHASUMS256.txt` 这类）把该版本的所有文件补齐，`.zip` / `.msi` 都能列出来
- 结果按页面顺序列出，可勾选后批量加入下载队列
- 同一页有多个平台版本时（Windows / macOS / Linux），自动按本机系统选中对应安装包，点「自己选」可展开全部
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

仓库里附带了一份已编译好的程序：[dist/DLKit.exe](dist/DLKit.exe)。

它是未签名的 exe，Windows 首次运行时可能提示“已保护你的电脑”，点“更多信息 → 仍要运行”即可。也可以按下面的说明自行编译。

## 运行

程序本身是一个 exe，但下载能力依赖几个命令行工具。把它们放到程序目录下：

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

## 多源（镜像）

同一个文件如果镜像站上也有，程序会把分块分散到多个源同时下载，单站限速时总速度能叠加。内置的镜像不需要任何配置，装好就能用：

| 来源 | 镜像 |
| --- | --- |
| HuggingFace 模型 / 数据集 | hf-mirror.com |
| npm 包 | registry.npmmirror.com |
| Node.js 官方发行包 | npmmirror.com/mirrors/node |
| PyPI 包 | pypi.tuna.tsinghua.edu.cn（清华源） |
| GitHub 下载 | ghfast.top / gh-proxy.com / ghproxy.net（三个代理互为备份） |

镜像不会影响文件正确性：

- 参与前要先通过校验：用 Range 取开头 1KB，必须返回 206、文件总长度与主站一致，开头字节也要和主站一致，同大小不同内容的文件混不进来；2 秒内没响应的直接跳过
- 新加入的镜像先只领 1 个分块，确认能正常下载后才参与轮换
- 有分块却 1 秒没有数据，或吞吐低于最快源 25%，镜像会被停用，已领的分块交回主站重下
- 主站本身就很快时（探测阶段已跑满）不会为了等镜像而多停

### 自己加镜像

高阶用户可以放一个 `config/mirrors.json`，复制示例后修改：

```powershell
Copy-Item config\mirrors.example.json config\mirrors.json
```

```json
{
  "enabled": true,
  "add": [
    { "from": "https://example.com/files/", "to": "https://mirror.example.com/files/" }
  ]
}
```

- `add`：在内置规则之外追加（推荐，只加自己需要的）
- `rules`：替换全部内置规则
- `enabled: false`：完全关闭镜像，只用主站
- 环境变量 `DLKIT_NO_MIRROR=1` 也能临时关闭

规则的含义是：下载地址以 `from` 开头时，额外尝试把前缀换成 `to` 的镜像地址。镜像站必须支持 Range 请求，否则校验通不过。

镜像会把一部分流量交给第三方站点，请只保留你信任的镜像。

## 构建

需要 Windows、Rust stable，以及系统自带的 curl（Windows 10 1803 及以上）。

```powershell
cd ui
cargo build --release
```

产物在 `ui/target/release/dlkit.exe`，复制到程序目录并改名为 `DLKit.exe` 即可。

如果拉取依赖很慢，可以按自己的网络环境配置 crates.io 镜像，例如在 `ui/.cargo/config.toml` 里指定 rsproxy。

## 命令行

不方便开界面时，可以直接用命令行：

```powershell
# 下载单个链接
DLKit.exe --headless "https://example.com/file.zip" -c 32 -d "D:\Downloads"

# 只解析下载页，列出候选文件
DLKit.exe --scan "https://www.7-zip.org/download.html"
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
- 页面解析不执行 JavaScript。纯前端渲染的下载页（例如 VS Code 官网），如果同目录也没有校验和清单，就只能列出 HTML 里直接写出的链接
- 速度取决于站点对单个 IP 的限制。站点给满单连接带宽、或对总量限速时，多连接不一定比浏览器快
- 需要登录、验证码或特殊请求头的资源，目前没有在界面里填 Cookie 的入口
- 网盘功能依赖 OpenList 自身支持的网盘类型

## 免责声明

本项目只提供下载工具，不提供任何资源。请只下载你有权获取的内容，并遵守目标站点的服务条款和当地法律。

## 许可证

[MIT](LICENSE)
