# AmberGuard

手切优先的 **WiFi 双频调度 Magisk 模块**（Android 13–16）。  
守护进程读 wpa / 框架状态，按健康分与对端 RSSI **自动下切 / 上切**，本机 Web 面板可配家网与阈值。

> 当前主推 **Magisk**；KernelSU 可用 `webroot` 进面板。无 root / 云同步不在范围内。

**版本**：见 `module.prop`（当前 v0.5.x）。

## 有什么

| 有 | 不做（现阶段） |
|----|----------------|
| 异名双频 ROAM/SELECT + 框架 `connect-network` 真连 | QS Tile / 独立 APK |
| 家网 BSSID、一键当前、扫描勾选 | Chart 全家桶 |
| 日用 / 省电 / 暂停 + 观影 soft-pause | neli 替换 iw（可选后续） |
| 手切 hold、失败退避、切后短锁 AP | ATTACH 全事件驱动 |
| L3 多终点 soft-fail、切换历史 | 多机兼容矩阵（机型不足暂缓） |
| 息屏降频、面板 `127.0.0.1:8080`、面具「操作」状态 | 空间记忆 / eBPF / Dual STA |

## 安装（4 步）

1. Magisk → 模块 → 安装 `AmberGuard-magisk.zip`（或 CI Artifact）→ 重启  
2. 升级时音量键：**+ 保留配置** / **− 重置配置（像新装）**；超时默认保留  
3. 确认进程：`ps -A | grep amberguard`；面具点「操作」可看一行中文状态  
4. 本机浏览器：**http://127.0.0.1:8080**（或 KSU 模块 WebUI）

电脑调试：

```bash
adb forward tcp:8080 tcp:8080
```

## 上手注意

1. 系统设置里先连过并**保存** 2.4 与 5G（可异名，如 `FOO` / `FOO_5G`）  
2. 面板建议配置**家网**（扫描勾选或「当前加入」；有同 stem 双频时可用「采纳建议对」）  
3. **手切优先**：系统里手动换网后默认约 60s 不自动抢；可点「清除保护」  
4. 模块是**调度器**不是强拉器：连不上会退避，避免系统把网络标成「管理员停用」

## 目录与真源

```
AmberGuard/
├── module.prop / service.sh / customize.sh / action.sh …
├── daemon/          # Rust 守护进程
│   └── src/web/static/index.html   # 面板真源（include_bytes! 进 binary）
├── web/index.html   # 与 static 应对齐的副本
├── webroot/         # 模块管理器跳转壳 → :8080
├── docs/BUILD.md
└── scripts/pack.ps1
```

改面板请改 **`daemon/src/web/static/index.html`**，再同步到 `web/index.html`。

## 构建与打包

详见 [docs/BUILD.md](docs/BUILD.md)。

```powershell
# 交叉编译（示例）
$env:ANDROID_PLATFORM = "android-24"
cd daemon
cargo ndk -t arm64-v8a build --release --bin amberguard

# 本地打模块树/zip（binary 放 bin/，与 CI 一致）
.\scripts\pack.ps1 -BinaryPath path\to\amberguard
```

CI：push `main` 产出 Artifact `AmberGuard-magisk-zip`。  
正式包**不含** `dist/probe*` 等调试脚本。

## 配置

- 路径：`/data/adb/amberguard/config.toml`  
- 日志：`/data/adb/amberguard/log/amberguard.log`  
- 切换历史（落盘）：`/data/adb/amberguard/history.json`  
- 首次可不落盘用内存默认；面板「初始化」写入日用默认

## 隐私

仅本机读写配置与日志，无上传。

## 许可

以仓库内 LICENSE 为准。
