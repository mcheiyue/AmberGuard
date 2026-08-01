# AmberGuard Magisk 模块打包（可不含 binary）
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

# 根级 Magisk 文件
foreach ($name in @("module.prop", "sepolicy.rule", "post-fs-data.sh", "service.sh", "customize.sh", "uninstall.sh")) {
    $src = Join-Path $Root $name
    if (Test-Path -LiteralPath $src) {
        Copy-Item -LiteralPath $src -Destination (Join-Path $Staging $name)
    }
}

# 可选 web 源（调试用；正式依赖 binary 内嵌）
$webSrc = Join-Path $Root "web"
if (Test-Path -LiteralPath $webSrc) {
    Copy-Item -LiteralPath $webSrc -Destination (Join-Path $Staging "web") -Recurse
}

# 可选 daemon 二进制 → system/bin/amberguard
if ($BinaryPath -and (Test-Path -LiteralPath $BinaryPath)) {
    $binDir = Join-Path $Staging "system\bin"
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    Copy-Item -LiteralPath $BinaryPath -Destination (Join-Path $binDir "amberguard")
    Write-Host "已放入 system/bin/amberguard"
} else {
    Write-Host "未提供 binary（-BinaryPath），zip 仅含模块脚本/布局"
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

# Compress-Archive 需要条目相对路径：先 cd staging
Push-Location $Staging
try {
    Compress-Archive -Path * -DestinationPath $zip -CompressionLevel Optimal -Force
} finally {
    Pop-Location
}

Write-Host "完成: $zip"
Get-Item -LiteralPath $zip | Format-List FullName, Length
