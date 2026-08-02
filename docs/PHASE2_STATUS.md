# Phase 2 状态（最小日用骨架）

**设备**：小米 22081212C / Android 16  
**二进制**：commit `8000821`  
**部署**：adb 覆盖 `/data/adb/modules/AmberGuard/system/bin/amberguard`

## 实机已确认

| 项 | 结果 |
|---|---|
| Phase 2 进程启动 | ✅ `AmberGuard Phase 2 daemon started` |
| wpa 连接 | ✅ `/data/vendor/wifi/wpa/sockets/wlan0` |
| STATUS + SIGNAL_POLL | ✅ |
| health_score | ✅ API `score≈70–71`（RSSI≈-44，retry 中位） |
| 状态机 | ✅ `power_state":"Idle"`（健康、首选 5G） |
| HTTP | ✅ `127.0.0.1:8080/api/status` |
| `/api/mode/pause\|daily` | 已实现，待面板点选用 |

### 样例 API

```json
{"rssi":-44,"score":71.0,"band":"5","state":"COMPLETED","ssid":"MERCURY_5G_C8B5_CLONE","power_state":"Idle"}
```

## 自动 ROAM 闭环

逻辑已接：

`score` → 防抖 → `SCAN` → `best_on_band` → `ROAM` → 等 COMPLETED / 惩罚锁

**实机全链路切换**需满足：

- **下切**：score 持续 &lt; `score_switch_threshold`（默认 30）约 4s，且扫描到同 SSID 的 2.4G AP  
- **上切**：驻留 2.4G 且 score 高 + 对侧 5G RSSI ≥ `upswitch_rssi_min_dbm`（-65）

当前环境 5G 信号好（RSSI≈-44，score≈70），**不会触发下切**——属预期。  
验证切换：远离路由或临时把 `score_switch_threshold` 调到 95 做破坏性测试。

## 相对 PLAN 的缺口

| 项 | 状态 |
|---|---|
| SELECT 异 SSID | 未接（仅 ROAM） |
| retry 真实采样 | 未接（iw/neli） |
| ATTACH 事件线程 | 未接 |
| 完整 Web 面板 UI | 仅 API + 旧 HTML |
| 息屏 Frozen | 未接 |

## 结论

**Phase 2 骨架实机可用**（观测 + 决策状态机空闲路径）。  
**日用自动切网**待弱网/双频对侧场景或调阈值后补测一次即可收口。
