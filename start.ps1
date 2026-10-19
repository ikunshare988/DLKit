# OST 下载器 一键启动：aria2 (RPC:6800) + OpenList (Web:5244)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$log  = Join-Path $root 'logs'
if (-not (Test-Path $log)) { New-Item -ItemType Directory -Path $log | Out-Null }

# 让 yt-dlp / ffmpeg 可互相找到
$env:PATH = "$root\bin\ffmpeg\bin;$root\bin\yt-dlp;$root\bin\aria2;$env:PATH"

function Test-Port($port) {
    (Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue) -ne $null
}

if (-not (Test-Port 6800)) {
    Start-Process -FilePath "$root\bin\aria2\aria2c.exe" `
        -ArgumentList "--conf-path=$root\config\aria2.conf" `
        -WindowStyle Hidden
    Write-Host "[ok] aria2 已启动  RPC=http://127.0.0.1:6800"
} else {
    Write-Host "[skip] aria2 已在运行"
}

if (-not (Test-Port 5244)) {
    Start-Process -FilePath "$root\bin\openlist\openlist.exe" `
        -ArgumentList "server","--data","$root\data" `
        -WorkingDirectory "$root\bin\openlist" `
        -RedirectStandardOutput "$log\openlist.out.log" `
        -RedirectStandardError "$log\openlist.err.log" `
        -WindowStyle Hidden
    Start-Sleep -Seconds 2
    Write-Host "[ok] OpenList 已启动  http://127.0.0.1:5244（账号见 config/openlist.json）"
} else {
    Write-Host "[skip] OpenList 已在运行"
}

Write-Host ""
Write-Host "下载目录: $root\downloads"
Write-Host "停止服务: Get-Process aria2c,openlist -ErrorAction SilentlyContinue | Stop-Process"
