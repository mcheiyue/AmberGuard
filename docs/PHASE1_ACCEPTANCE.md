# Phase 1 实机验收报告

**设备**：小米 22081212C（diting）/ Android 16  
**日期**：2026-08-02  
**二进制**：commit `8a2ef17`（SIGNAL_POLL + chown wifi client sock）

## 1. go/no-go：wpa socket + STATUS

| 项 | 结果 |
|---|---|
| 控制口路径 | `/data/vendor/wifi/wpa/sockets/wlan0`（conf: `ctrl_interface=/data/vendor/wifi/wpa/sockets`） |
| 协议 | `SOCK_DGRAM` + 同目录 client bind + `chown 1010:1010` mode 660 |
| STATUS | **通过** — `wpa_state=COMPLETED` ssid=`MERCURY_5G_C8B5_CLONE` |
| `/api/status` | `{"rssi":-44,"score":92.0,"band":"5","state":"COMPLETED","ssid":"MERCURY_5G_C8B5_CLONE",...}` |

### 踩过的坑（已修）

1. Magisk sepolicy 必须用**空格**语法：`allow src tgt class perm`，不是 `tgt:class`
2. 目录 SELinux type 为 `wpa_data_file`（不是 vendor_wifi_* 臆测名）
3. wpa 进程域 `hal_wifi_supplicant_default`，需 `unix_dgram_socket sendto` 双向
4. Client socket 必须在**同一 DIR** bind；且 owner 须 wifi(1010) 可写，否则 STATUS 超时
5. 小米 `STATUS` **不含 RSSI**，必须 `SIGNAL_POLL`（返回 `RSSI=` / `AVG_RSSI=`）

## 2. SIGNAL_POLL

```
RSSI=-45
LINKSPEED=866
NOISE=-96
FREQUENCY=5180
WIDTH=80 MHz
AVG_RSSI=-44
```

daemon 已接 `status_with_signal()`：STATUS 无 signal 时自动 SIGNAL_POLL。

## 3. 数据源结论（Phase 1）

| 源 | 可用性 | 备注 |
|---|---|---|
| STATUS | ✅ | 状态/SSID/BSSID/freq/IP，无 RSSI |
| SIGNAL_POLL | ✅ | RSSI/LINKSPEED/NOISE/FREQ；daemon 已自动补 |
| `iw dev wlan0 station dump` | ✅ | signal / tx retries / tx failed 均有值（等价 GET_STATION 观测） |
| GET_STATION (nl80211/neli) | ⏳ Phase 2 | 代码未接；实机 `iw` 证明驱动有 retry 统计 |
| /proc/net/wireless | 未测 | 降级链保留 |
| PING | ✅ | `PONG` |
| LIST_NETWORKS | ✅ | id=0 CURRENT |
| SCAN_RESULTS | ✅ | 同 SSID 可见，含 signal level |

### iw station 摘录（实机）

```
signal: -45 dBm
tx retries: 7617
tx failed: 8
tx/rx bitrate: 866.7 MBit/s
```

→ Phase 2 health_score 的 retry 项**有数据源**（优先 neli GET_STATION，或暂用解析 `iw` 不推荐；应用 neli）。

## 4. 事件列表

Phase 1 **未** ATTACH 常驻（避免与 Framework 抢事件）。  
`wpa_cli` 交互可用；事件订阅推 Phase 2 事件线程。

## 5. SELinux 基线（已验证可 live 注入）

见模块根 `sepolicy.rule`（Magisk 空格语法）。开机 `post-fs-data.sh` 逐行 `--live`。

## 6. 包体

daemon strip 后约 **1.33–1.36MB**（略超 800KB 目标，含 tiny_http/serde/nix；Phase 2 再压）。

## 7. ROAM 同 SSID（实机）

对当前 BSSID `44:f9:71:3c:c8:b7` 发 `ROAM`：

| 项 | 结果 |
|---|---|
| 命令返回 | `OK` |
| 状态序列 | `COMPLETED` → `ASSOCIATING` → `COMPLETED` |
| 恢复时长 | **~0.55s**（555ms 量级） |
| 切后 API | 仍 `COMPLETED` / 同 SSID / RSSI 正常 |

结论：**同 SSID ROAM 可用**，短中断约半秒，日用可接受；异 SSID SELECT 未测（无第二 SSID 对）。

## 8. 退出准则对照

| 准则 | 状态 |
|---|---|
| ① attach/STATUS 往返 | ✅（命令通道等价，ATTACH 推 Phase 2） |
| ② ROAM 同 SSID 行为 | ✅ OK，~0.55s 回 COMPLETED |
| ③ 健康度数据源至少一条 | ✅ SIGNAL_POLL RSSI；iw 有 tx_retries |
| ④ 失败缩 scope | 不需要 |
| ⑤ 电源入口 | ⚠️ 未做完整 5min 息屏；daemon 亮屏常驻已确认，息屏推后续补测 |

## 9. Phase 2 范围建议（**全开最小日用**）

| 做 | 缓做 |
|---|---|
| health_score(RSSI，retry 有则用 iw/neli) | RTT |
| 防抖 3–5s / 5–10s | ATTACH 全事件 |
| ROAM 同 SSID + SELECT 异 SSID | captive 完整 |
| 惩罚锁 | QS Tile |
| Web 真实状态 + 日用/暂停 | 包体压到 800KB |

**判定：Phase 1 退出准则核心项已满足，可进入 Phase 2 最小日用实现。**
