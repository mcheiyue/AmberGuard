# AmberGuard Magisk 模块打包（与 CI Stage 对齐：bin/ + webroot，无 probe 杂物）
# 用法: .\scripts\pack.ps1 [-BinaryPath path] [-OutDir path]
param(
    [string]$BinaryPath = "",
    [string]$OutDir = ""
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if (-not $OutDir) { $OutDir = Join-Path $Root "dist" }

$Staging = Join-Path $OutDir "module_staging"
if (Test-Path -LiteralPath $Staging) {
    Remove-Item -LiteralPath $Staging -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $Staging | Out-Null

# 根级 Magisk 文件（正式树；不拷 system.prop / system/）
foreach ($name in @(
    "module.prop", "sepolicy.rule", "post-fs-data.sh", "service.sh",
    "customize.sh", "uninstall.sh", "action.sh"
)) {
    $src = Join-Path $Root $name
    if (Test-Path -LiteralPath $src) {
        Copy-Item -LiteralPath $src -Destination (Join-Path $Staging $name)
        if ($name -match '\.sh$') {
            # Git on Windows 可能丢执行位；安装脚本里也会 chmod
        }
    }
}

# KSU/面具 WebUI 跳转壳
$webroot = Join-Path $Root "webroot"
if (Test-Path -LiteralPath $webroot) {
    Copy-Item -LiteralPath $webroot -Destination (Join-Path $Staging "webroot") -Recurse
}

# META-INF（若有）
$meta = Join-Path $Root "META-INF"
if (Test-Path -LiteralPath $meta) {
    Copy-Item -LiteralPath $meta -Destination (Join-Path $Staging "META-INF") -Recurse
}

# daemon → 模块私有 bin/（禁止默认 system/bin，避免挂到 /system）
if ($BinaryPath -and (Test-Path -LiteralPath $BinaryPath)) {
    $binDir = Join-Path $Staging "bin"
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    Copy-Item -LiteralPath $BinaryPath -Destination (Join-Path $binDir "amberguard")
    Write-Host "已放入 bin/amberguard"
} else {
    Write-Host "未提供 -BinaryPath：zip 仅含脚本/布局（安装后无 daemon）"
}

# 版本号
$ver = "0.1.0"
$prop = Join-Path $Root "module.prop"
if (Test-Path -LiteralPath $prop) {
    $m = Select-String -Path $prop -Pattern "^version=(.+)$" | Select-Object -First 1
    if ($m) { $ver = ($m.Matches.Groups[1].Value -replace "^v", "") }
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$zip = Join-Path $OutDir "AmberGuard-v$ver.zip"
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }

Push-Location $Staging
try {
    Compress-Archive -Path * -DestinationPath $zip -CompressionLevel Optimal -Force
} finally {
    Pop-Location
}

Write-Host "完成: $zip"
Write-Host "说明: 正式包不应含 dist/probe*；面板 HTML 已内嵌 binary，无需再打 web/"
Get-Item -LiteralPath $zip | Format-List FullName, Length
