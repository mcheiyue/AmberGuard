# AmberGuard

WiFi 5G/2.4G 智能切换 **Magisk 模块**（Android 13–15）。Phase 1 为骨架：守护进程读 wpa 状态 + 本机 Web 面板，**不自动切换 WiFi**。

> 当前仅支持 **Magisk**。KernelSU / APatch 留到 Phase 5。

## 功能边界（Phase 1）

| 有 | 无 |
|---|---|
| 连接 wpa_supplicant、读 `STATUS` | 自动 ROAM / 切频 |
| `127.0.0.1:8080` 状态面板 | Chart.js 曲线（Phase 3） |
| 配置文件骨架 | QS Tile、Captive、空间记忆 |

## 目录

```
AmberGuard/
├── module.prop / sepolicy.rule / …   # Magisk 模块文件
├── daemon/                           # Rust 守护进程（另路实现）
├── web/index.html                    # Web 源（构建时内嵌到 binary）
├── docs/PLAN.md / docs/BUILD.md
├── scripts/pack.ps1                  # 打 Magisk zip
└── README.md
```

`web/index.html` 是前端**源文件**。若 `daemon/src/web/static/index.html` 存在，应与 `web/` 保持一致；发布构建以 `web/` 为准，由 daemon 用 `include_bytes!`（或构建脚本复制）嵌入。

## 依赖

- Magisk（最新稳定版）
- 交叉编译：见 [docs/BUILD.md](docs/BUILD.md)（NDK、`cargo-ndk`、`aarch64-linux-android`）
- 真机 root + 可写模块分区

## 构建（摘要）

```powershell
# 详见 docs/BUILD.md
$env:ANDROID_PLATFORM = "android-24"
cargo ndk -t arm64-v8a -o ./jniLibs build --release -p amberguard
# strip 后拷到模块：system/bin/amberguard
```

包体目标：daemon strip 后 ≤800KB；含 web 总包 ≤1.2MB。

## 打包模块 zip

```powershell
.\scripts\pack.ps1
# 可选：.\scripts\pack.ps1 -BinaryPath path\to\amberguard
```

未提供二进制时仍会打 zip（安装后无 daemon 则无法起服务，仅便于先测模块布局）。

## 安装与运行

1. Magisk → 模块 → 从本地安装生成的 `AmberGuard-*.zip` → 重启  
2. 确认 `service.sh` 已拉起 daemon（`ps -A | grep amberguard`）  
3. 本机浏览器打开：**http://127.0.0.1:8080**  
4. 面板每 2 秒轮询 `/api/status`

### 远程访问（SSH 隧道）

Web **只绑定** `127.0.0.1:8080`，不监听局域网。电脑上：

```bash
adb forward tcp:8080 tcp:8080
# 或 SSH：ssh -L 8080:127.0.0.1:8080 user@phone
```

然后在电脑浏览器访问 `http://127.0.0.1:8080`。

> Android 14+ 从公网页 fetch 本机可能触发 Private Network Access；**地址栏直接打开** 127.0.0.1 为同源，一般无妨。不要给面板加强制 HTTPS 的 CSP（tiny_http 无 TLS）。

## 配置

默认路径：`/data/adb/amberguard/config.toml`（TOML）。Phase 1 以 daemon 实装为准。

## 隐私

射频指纹等数据（Phase 4）仅本地，不上传。

## 许可

以仓库内 LICENSE 为准（若无则待补）。
