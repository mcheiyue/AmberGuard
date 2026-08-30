# AmberGuard 实施规划：自动观影保护 + 失败退避通知 + BSSID 质量记忆

> 文档版本：v1.0  
> 对应版本：v0.5.28  
> 状态：规划中（等待用户授权实施）  
> 涉及功能项：首推项（自动观影保护）+ 次推项（失败/退避通知）+ 第四项（per-BSSID 质量记忆） + 杂项（通知格式本地改动合入）

---

## 1. 目标与背景

当前 AmberGuard（v0.5.27）已完成核心双频切换、同频追优、家网白名单、手切保护（`user_hold`）、非线性健康分、动态 margin 及热更新检测修复。但在日常真实使用中，仍存在三处体验断层：

1. **观影/大流量被打断**：目前「观影保护」仅有 WebUI 手动按钮（「观影 20 分钟」），看视频或通话时若未提前手动点击，守护进程仍会在信号波动时触发切换，导致画面卡顿甚至短时重连。
2. **切换失败与退避无感知**：当目标 AP 因框架偶发空转或不可达导致连续 3 次未落地（如日志 09:36 的 3 连败），守护进程进入 180s 退避并写 `last_error`，但用户在通知栏和桌面毫无感知，不知道守护已暂时放弃。
3. **Flaky 目标反复踩坑（无状态记忆）**：对同一不可达或频段关联困难的 BSSID，退避 180s 结束后若信号依然强，追优逻辑会立刻再次选中它并重新触发 3 连败，缺乏跨周期的 AP 质量降权记忆。

本规划基于**最小侵入、最大复用、真实场景量化**原则，将上述三项需求完整设计，并明确各参数阈值的依据与边界。

---

## 2. 功能一：自动观影/流量保护（Auto Soft-Pause）

### 2.1 真实场景与流量特征分析

| 场景 | 典型速率特征 | 守护预期行为 | 判定机制 |
|---|---|---|---|
| **在线高清视频**（抖音 / B 站 / YouTube 1080p） | 持续下行 $3 \sim 8\text{ Mbps}$（$\approx 375 \sim 1000\text{ KB/s}$） | **严禁切换**，保持当前链路 | 持续下行速率 $\ge 400\text{ KB/s}$ 达 $12\text{s}$ 触发 |
| **超高清 / 4K 流媒体** | 持续下行 $15 \sim 25\text{ Mbps}$（$\approx 2 \sim 3\text{ MB/s}$） | **严禁切换** | 同上（大幅超标触发） |
| **大文件下载 / 应用更新 / 系统 OTA** | 持续高下行（满速） | **严禁切换**（切网必致 TCP 拥塞重传或中断） | 同上触发 |
| **热点共享（Tethering）** | 持续上下行转发 | **严禁切换**（切网致下挂设备全断） | 下行或上行速率达标即保护 |
| **网页浏览 / 刷资讯** | 突发性高流量（加载 $1 \sim 2\text{s}$ 内几 MB，随后近乎 0） | **允许正常切换**（非连续高负载） | $12\text{s}$ 持续窗口自然过滤瞬时流量 |
| **在线音乐**（128 ~ 320 kbps） | 下行 $\approx 16 \sim 40\text{ KB/s}$ + 本地大缓冲 | **允许正常切换**（缓冲足、切网无感） | 低于 $400\text{ KB/s}$ 阈值，刻意不拦截 |
| **语音/视频通话**（微信语音/VoIP） | 上下行对称 $30 \sim 100\text{ kbps}$（$\approx 4 \sim 12\text{ KB/s}$） | 理想需保护，但流量极低与后台同步无异 | **限制**：纯流量法无法可靠区分语音与后台，**建议通话场景继续沿用手动「观影 20 分钟」按钮** |

### 2.2 数据源与速率计算方案

1. **数据源扩展**（`station_info.rs`）：
   - 内核 `iw dev wlan0 station dump` 文本中包含 `rx bytes:\t<u64>` 与 `tx bytes:\t<u64>` 字段。
   - `StationSample` 结构体新增 `rx_bytes: u64` 与 `tx_bytes: u64`。
   - `parse_iw_station` 新增解析 `rx bytes:\t` 与 `tx bytes:\t`。
2. **采样与速率公式**：
   - 亮屏空闲时 `iw` 采样间隔为 $3\text{s}$，观察带/切换中为 $1\text{s}$（`main.rs:1484-1517`）。
   - 计算双向流量增量 $\Delta \text{bytes} = (\text{cur.rx\_bytes} - \text{prev.rx\_bytes}) + (\text{cur.tx\_bytes} - \text{prev.tx\_bytes})$（`checked_sub` 防倒退）。
   - 速率 $R = \frac{\Delta \text{bytes}}{\Delta t} \text{ (单位: B/s $\rightarrow$ KB/s)}$。

### 2.3 状态机与保护周期设计

```
                    R >= soft_auto_on_kb (400 KB/s) 持续 12s
    [ 正常守护态 ] ───────────────────────────────────────────> [ 自动观影保护中 ]
          ^                                                            │
          │                                                            │ R >= soft_auto_off_kb (80 KB/s)
          │                                                            │ 持续刷新 until = now + 30s
          │                                                            │ (滑动窗口延长)
          │ R < soft_auto_off_kb (80 KB/s) 持续 45s                   │
          └────────────────────────────────────────────────────────────┘
                            (或保护时长达上限 soft_auto_max_mins，强制退出防死锁)
```

1. **触发条件**：
   - `soft_auto_enable == true` 且亮屏（息屏本就硬 halt）。
   - 连续采样计算的 $R \ge \text{soft\_auto\_on\_kb}$（默认 $400\text{ KB/s}$）持续时间 $\ge \text{soft\_auto\_trigger\_secs}$（默认 $12\text{s}$）。
   - 触发时进入 `HoldKind::SoftPause`，设置 `until = Instant::now() + Duration::from_secs(30)`，记录 `auto_soft_started_at = Some(Instant::now())`。
2. **维持与滑动续期**：
   - 保护中若 $R \ge \text{soft\_auto\_off\_kb}$（默认 $80\text{ KB/s}$），每次主循环将 `until` 刷新为 `Instant::now() + Duration::from_secs(30)`。
3. **解除条件（迟滞释放）**：
   - 若 $R < \text{soft\_auto\_off\_kb}$ 持续时间 $\ge \text{soft\_auto\_release\_secs}$（默认 $45\text{s}$），主动清除 `HoldState`，退出保护。
   - **防死锁上限**：若保护累计持续时间 $\ge \text{soft\_auto\_max\_mins} \times 60\text{s}$（默认 240 分钟 / 4 小时），强制退出保护。
4. **与手动保护隔离**：
   - `HoldState` 新增 `is_auto: bool` 字段。
   - 手动点击「观影 20 分钟」或手切网产生的 `HoldState`（`is_auto = false`）**绝不被自动流量下降逻辑清除**，必须倒计时走完或用户手动点「结束保护」。
   - 面板与 `status.txt` 文案做清晰区分：自动触发显示 `观影保护中（自动·剩 Xs）`，手动触发显示 `观影保护中（剩 Xs）`。

---

## 3. 功能二：切换失败 / 退避通知（Switch Backoff Notification）

### 3.1 痛点与通知时机

当前切换失败逻辑在 `main.rs:2213-2235`：
- 单次未落地：记录 `PENALTY`（冷却 30s/60s），继续正常工作。单次偶发失败无需通知，避免频繁打扰。
- 连续失败 $\ge 3$ 次：触发长退避（$180\text{s} \sim 600\text{s}$），写 `snap.last_error`。

**通知策略**：仅在**正式进入退避（`fail_streak == 3`）的边沿**触发一次系统通知。

### 3.2 规范与文案

- **依赖门控**：`notify_enable`（总通知开关开启即生效；不与 `notify_switch` 切换成功开关耦合）。
- **通知格式**（已修复标题重复）：
  - 标题（`-t`）：`AmberGuard`
  - 正文：`切换至「{ssid}」连续 3 次未落地，已暂停尝试 {back_mins} 分钟`
- **ID 与防堆叠**：使用固定 ID `amber_fail_backoff`（`notify::event_id`），多次退避原地覆盖，不污染通知栏。

---

## 4. 功能三：per-BSSID 质量记忆（Spatial Failure Demote）

### 4.1 核心机制

解决目标 AP 频段配置不匹配、系统未保存或硬件偶发拒绝关联导致反复 3 连败的问题。

1. **数据模型**：
   - 文件路径：`/data/adb/amberguard/bssid_stats.json`
   - 内存结构：`HashMap<String, BssidStat>`
   ```rust
   #[derive(Serialize, Deserialize, Clone, Debug, Default)]
   pub struct BssidStat {
       pub fail_count: u32,
       pub last_fail_unix: u64,
       pub cooldown_until_unix: u64, // 降权截止时间戳 (unix seconds)
   }
   ```
2. **记录时机**：
   - **失败累加**（`main.rs:2213` 失败分支）：目标 `peer.bssid` 的 `fail_count += 1`。当 `fail_count >= 3` 时，设置 `cooldown_until_unix = unix_now() + bssid_demote_secs`（默认 1800s / 30 分钟），并持久化写盘。
   - **成功清零**（`main.rs:2182` 成功分支）：当前关联成功的 `peer.bssid` 从 `bssid_stats` 中移除或清零其失败计数，并写盘。
3. **消费与降权调度**（两处挂点）：
   - **挂点 A**：`band_bond::best_on_band` 选对端 AP 时，增加参数 `demoted_bssids: &[String]`。处于降权期（`unix_now() < cooldown_until_unix`）的 AP 过滤剔除；若剔除后无其他候选，**返回 `None`**（不向已知不可达 AP 盲目发起切换，留在当前可用链路）。
   - **挂点 B**：`main.rs:1886-1913` 同频追优候选选择（`roam candidate`）时，过滤掉处于降权期的 BSSID，不为其建立 `roam_pending`。
4. **自愈与寿命**：
   - 降权为软隔离（默认 30 分钟），30 分钟后降权自动失效，给予一次重新探测机会。
   - 每次成功切换立即重置计数，防止历史陈旧计数误杀正常 AP。

---

## 5. 配置体系与 WebUI 交互定义

### 5.1 `config.toml` 新增字段

```toml
# 自动观影/大流量保护
soft_auto_enable = true         # 自动流量保护总开关（默认 true）
soft_auto_on_kb = 400           # 触发阈值（KB/s，默认 400，范围 100~5000）
soft_auto_off_kb = 80           # 解除阈值（KB/s，默认 80，范围 20~1000，必须 < on_kb）
soft_auto_trigger_secs = 12     # 持续高于触发阈值达此秒数才进入保护（默认 12，范围 3~60）
soft_auto_release_secs = 45     # 持续低于解除阈值达此秒数才退出保护（默认 45，范围 10~180）
soft_auto_max_mins = 240        # 连续自动保护最长分钟数上限，防死锁（默认 240，0=不设限）

# BSSID 质量记忆
bssid_memory_enable = true      # AP 质量记忆降权开关（默认 true）
bssid_demote_secs = 1800        # 连续失败后降权冷却时长（秒，默认 1800 = 30分钟，范围 300~7200）
```

### 5.2 `ConfigPatch` 与原子校验规则

- `soft_auto_on_kb` 必须严格大于 `soft_auto_off_kb`，否则 `apply_patch` 拒绝并报错回显。
- 延续 `clone -> patch -> validate -> swap` 原子更新规范（#1186），磁盘与内存绝不污染中间态。

### 5.3 WebUI 设置页呈现（符合「可见、可感、可配」）

1. **新增「自动观影保护」卡片**：
   - 状态指示：当前下行速率实时显示（如 `当前吞吐: 1.2 MB/s · 保护生效中`）。
   - 开关：`启用自动大流量保护`（Toggle）。
   - 滑块 1：`触发速率`（默认 400 KB/s，附文案「流媒体/大下载触发线，建议 300~800」）。
   - 滑块 2：`解除速率`（默认 80 KB/s，附文案「流量回落至此线后开始倒计时解除」）。
   - 滑块 3：`最长保护时长`（默认 4 小时，可选 1h / 2h / 4h / 不限）。
2. **新增「AP 质量记忆」卡片**：
   - 开关：`启用故障 AP 自动降权`（Toggle）。
   - 滑块：`故障隔离时长`（默认 30 分钟，可选 10分 / 30分 / 60分 / 120分）。
   - 状态列表：若当前有被降权的 BSSID，展示列表及剩余冷却时间，并提供「重置记忆」按钮（`POST /api/bssid-memory/clear`）。

---

## 6. 代码落地范围与文件清单

| 文件 | 变动性质 | 详细内容 |
|---|---|---|
| `daemon/src/station_info.rs` | 扩展 | `StationSample` 增加 `rx_bytes`/`tx_bytes` 解析；增加 `traffic_rate_kbps()` 计算辅助函数。 |
| `daemon/src/config.rs` | 扩展 | 增加 8 个新配置项、默认值函数、`ConfigPatch` 映射与原子校验。 |
| `daemon/src/band_bond.rs` | 扩展 | `best_on_band` 增加降权 BSSID 过滤逻辑。 |
| `daemon/src/notify.rs` | 修复+扩展 | 合入通知正文去重修复；新增 `amber_fail_backoff` 通知模板。 |
| `daemon/src/main.rs` | 核心接入 | ① 主循环增加流量速率与自动 SoftPause 状态追踪；② 失败分支接入退避通知与 BSSID 失败计数持久化；③ 追优候选与 `best_on_band` 注入降权过滤；④ HTTP 增加 `/api/bssid-memory/clear`。 |
| `daemon/src/web/mod.rs` | 扩展 | `StatusSnapshot` 暴露 `traffic_rate_kb` 与 `bssid_demoted_count`；增加内存持久化读写路由。 |
| `daemon/src/web/static/index.html` | UI 同步 | 新增两个设置卡片、实时速率显示与两份 HTML SHA256 门禁同步（`web/index.html`）。 |
| `module.prop` + `Cargo.toml` | 版本号 | 升至 `v0.5.28` / `versionCode=79`。 |

---

## 7. 边界与防御性降级设计

1. **`iw` 命令故障或无权限**：
   - 若 `iw station dump` 失败或未输出 `rx bytes`，`traffic_rate_kbps()` 返回 `None`。
   - 自动观影逻辑保持静默，不触发自动保护，不阻断手动保护与正常切换流程。
2. **息屏状态**：
   - 息屏期间守护进程硬 halt（不执行 `iw`、不扫网、不切网），自动观影状态机暂停计时，亮屏后以最新真实流量自然重新判定。
3. **降权全隔离死锁防护**：
   - 若家网内所有可用 AP 均被降权，`best_on_band` 返回 `None`，守护进程维持在当前连接链路，**绝不进行断网或随机乱切**。
   - 弱信号救援（`weak_rescue`）场景下，若已无其他选择，可配置降权 AP 兜底放行机制。

---

## 8. 验证计划（验收标准）

- [ ] **编译门禁**：GitHub Actions CI `cargo check` + `cargo test` 100% 通过，两份 HTML SHA256 一致。
- [ ] **单测覆盖**：
  - `station_info` 新增带 `rx bytes` / `tx bytes` 的 station dump 文本解析单测。
  - `config` 新增 `on_kb <= off_kb` 非法配置拦截校验单测。
  - `bssid_stats` 新增失败累加、降权截止、成功重置单测。
- [ ] **实机验证**（xaga / diting）：
  - 播放 B 站 / 抖音 1080p 视频 $15\text{s}$ 内，观察 WebUI 状态变为 `观影保护中（自动）`，且此期间信号波动不触发切换。
  - 暂停视频 $45\text{s}$ 后，观察自动退出保护，恢复正常守护。
  - 手动点击「观影 20 分钟」，停止视频后确认其**不被自动提前解除**。
  - 观察通知栏：切换成功通知为纯净 `已切换到 5G：SSID`（无重复标题）。
  - 热更新验证：新包刷入后 daemon 自动 `exit(0)` 并由 `service.sh` 重拉为 `v0.5.28`。

---

## 9. 明确不做的事（YAGNI 边界）

1. 不通过读取 `/proc/net/dev` 全局网卡做流量统计（避免受后台大文件下载但非 Wi-Fi STA 相关流量误导，直接绑定 wlan0 station dump 最准）。
2. 不做基于 App 进程名或包名探测（不 Hook 系统 `ActivityManager`，不搞高侵入性 OEM 专有适配）。
3. 不把降权做成永久黑名单（必须有时效衰减与自愈机制）。