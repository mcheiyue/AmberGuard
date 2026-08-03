# AmberGuard 构建说明

交叉编译 Android ARM64 守护进程，并装入 Magisk 模块布局。

## 环境

| 项 | 要求 |
|---|---|
| 主机 | Windows / Linux / macOS（下文以 PowerShell 为主） |
| Rust | stable，`rustup target add aarch64-linux-android` |
| NDK | r26+ 建议；设 `ANDROID_NDK_HOME` |
| 工具 | `cargo install cargo-ndk` |
| 目标 | `aarch64-linux-android`（abi：`arm64-v8a`） |
| API | `ANDROID_PLATFORM=android-24`（Android 7.0 起 Bionic 向后兼容，覆盖 13–15） |

动态链接 Bionic `libc.so`，**不要**静态链 libc。

## 一键交叉编译

```powershell
cd D:\OpenCode\AmberGuard\daemon   # 或仓库内 Cargo 工作区根

$env:ANDROID_NDK_HOME = "C:\path\to\Android\ndk\26.x.x"
$env:ANDROID_PLATFORM = "android-24"

cargo ndk -t arm64-v8a -o ..\out\jniLibs build --release
```

产物常见路径：

```
out/jniLibs/arm64-v8a/amberguard
# 或 target/aarch64-linux-android/release/amberguard
```

## Strip 与体积

```powershell
# 使用 NDK llvm-strip
& "$env:ANDROID_NDK_HOME\toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-strip.exe" `
  --strip-all .\out\jniLibs\arm64-v8a\amberguard
```

| 目标 | 上限 |
|---|---|
| daemon（release + strip） | **≤ 800KB** |
| daemon + 内嵌 web | **≤ 1.2MB** |
| debug | 约 3–5MB（不入模块） |

Phase 1 **不内嵌 Chart.js**，便于压体积。超标先砍功能/依赖，再考虑压缩资源。

## Web 内嵌

- **真源**：`daemon/src/web/static/index.html`（`include_bytes!` 打进 binary）
- 镜像：`web/index.html` 应与 static **hash 一致**（改面板后 `Copy-Item` 同步）
- KSU 壳：`webroot/index.html`（跳转 8080，勿与面板合并）
- MIME：`text/html`；仅服务本机 `127.0.0.1:8080`

## 装入 Magisk 模块

模块 zip 内路径：

```
system/bin/amberguard    # 可执行、strip 后的二进制
module.prop
post-fs-data.sh          # SELinux
service.sh               # setsid 启动 daemon
sepolicy.rule
customize.sh             # 可选
web/…                    # 可选：仅调试；正式靠 binary 内嵌
```

复制示例：

```powershell
New-Item -ItemType Directory -Force -Path .\module_staging\system\bin | Out-Null
Copy-Item .\out\jniLibs\arm64-v8a\amberguard .\module_staging\system\bin\amberguard
# 再拷 module.prop、*.sh、sepolicy.rule 等
```

或直接：

```powershell
.\scripts\pack.ps1 -BinaryPath .\out\jniLibs\arm64-v8a\amberguard
```

`pack.ps1` **不强制**二进制存在：无 binary 仍可打布局 zip，便于先测脚本与 sepolicy。

## 真机快速验

```text
adb push amberguard /data/local/tmp/
adb shell chmod 755 /data/local/tmp/amberguard
adb shell su -c /data/local/tmp/amberguard
adb forward tcp:8080 tcp:8080
# 浏览器 http://127.0.0.1:8080
```

正式环境由 Magisk `service.sh` 用模块绝对路径 + `setsid` 拉起，连续崩溃有上限（见 PLAN）。

## 注意

- Phase 1 **不实现自动切 WiFi**；编译通过 + `/api/status` 可读即里程碑。
- 远程调试用 `adb forward` 或 SSH `-L`，勿把 HTTP 绑到 `0.0.0.0`。
- 包体实测值请在 Phase 1 编译后写回本文件或 PLAN 勾选处。
