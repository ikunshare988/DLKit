# DLKit 一键启动：aria2 (RPC:6800) + OpenList (Web:5244)
# 只依赖 Windows 自带的 PowerShell，不需要任何开发环境
$ErrorActionPreference = 'Continue'
$root = $PSScriptRoot
$log  = Join-Path $root 'logs'
foreach ($d in @('logs', 'downloads', 'config')) {
    $p = Join-Path $root $d
    if (-not (Test-Path $p)) { New-Item -ItemType Directory -Path $p | Out-Null }
}
# 会话文件不存在时 aria2 会报错，先建一个空的
$session = Join-Path $root 'config\aria2.session'
if (-not (Test-Path $session)) { New-Item -ItemType File -Path $session | Out-Null }

# 让 yt-dlp / ffmpeg 可互相找到
$env:PATH = "$root\bin\ffmpeg\bin;$root\bin\yt-dlp;$root\bin\aria2;$env:PATH"

function Test-Port($port) {
    # Get-NetTCPConnection 在老系统 / 精简系统上可能不存在，退回到 .NET 探测
    if (Get-Command Get-NetTCPConnection -ErrorAction SilentlyContinue) {
        return [bool](Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue)
    }
    try {
        $c = New-Object System.Net.Sockets.TcpClient
        $ok = $c.ConnectAsync('127.0.0.1', $port).Wait(300)
        $c.Close()
        return $ok
    } catch {
        return $false
    }
}

# ---------------- aria2 ----------------
$aria2 = Join-Path $root 'bin\aria2\aria2c.exe'
$conf  = Join-Path $root 'config\aria2.conf'
if (-not (Test-Port 6800)) {
    if (-not (Test-Path $aria2)) {
        Write-Host "[error] 缺少 $aria2 —— 程序包不完整，请把 bin 目录一起复制"
    } elseif (-not (Test-Path $conf)) {
        Write-Host "[error] 缺少 $conf —— 复制 aria2.conf.example 为 aria2.conf 并填写 rpc-secret"
    } else {
        Start-Process -FilePath $aria2 `
            -ArgumentList "--conf-path=`"$conf`"" `
            -WorkingDirectory $root `
            -WindowStyle Hidden
        Start-Sleep -Milliseconds 800
        Write-Host "[ok] aria2 已启动  RPC=http://127.0.0.1:6800"
    }
} else {
    Write-Host "[skip] aria2 已在运行"
}

# ---------------- OpenList（可选） ----------------
$openlist = Join-Path $root 'bin\openlist\openlist.exe'
if (-not (Test-Port 5244)) {
    if (Test-Path $openlist) {
        Start-Process -FilePath $openlist `
            -ArgumentList "server","--data","`"$root\data`"" `
            -WorkingDirectory "$root\bin\openlist" `
            -RedirectStandardOutput "$log\openlist.out.log" `
            -RedirectStandardError "$log\openlist.err.log" `
            -WindowStyle Hidden
        Write-Host "[ok] OpenList 已启动  http://127.0.0.1:5244（账号见 config/openlist.json）"
    } else {
        Write-Host "[skip] 未安装 OpenList（只有网盘功能需要）"
    }
} else {
    Write-Host "[skip] OpenList 已在运行"
}

Write-Host ""
Write-Host "下载目录: $root\downloads"
Write-Host "停止服务: Get-Process aria2c,openlist -ErrorAction SilentlyContinue | Stop-Process"
