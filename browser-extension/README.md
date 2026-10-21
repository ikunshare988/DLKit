# DLKit 浏览器接管扩展

这个目录里的文件会由 DLKit 自动复制到 `%LOCALAPPDATA%\DLKit\extension`，一般不需要手动修改。

## 安装

1. 打开 DLKit → 设置 → 浏览器接管 → 「一键启用」（会自动打开扩展页和扩展目录）
2. Chrome 打开 `chrome://extensions`，Edge 打开 `edge://extensions`
3. 打开「开发者模式」，点「加载已解压的扩展」
4. 选择 `%LOCALAPPDATA%\DLKit\extension`

扩展 ID 固定为 `ffbjgmkdclbopflkmjbmbhmaaonhlakf`（由 manifest 里的 `key` 决定），所以 native messaging 的 `allowed_origins` 可以精确绑定。

## 功能

- 右键链接 / 图片：用 DLKit 下载
- 右键页面：用 DLKit 解析此页
- 浏览器开始下载时取消，并交给 DLKit（可在扩展选项里关闭）
- 不读取、不同步 Cookie，只传链接和来源页

## 连接失败

- 确认 DLKit 正在运行，且设置里的「启用浏览器接管」是打开的
- 点一次「安装 / 修复」，会重写注册表和扩展目录
- 扩展图标出现 `!` 时，右键 → 选项，可以看到最近一次连接错误
