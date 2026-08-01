# AmberGuard 规划文档

> WiFi 5G/2.4G 智能切换 Magisk 模块
> 状态：规划中，未开工

---

## 一、项目定位

**一句话**：Android 13-15 上，低占用的 WiFi 双频智能切换守护进程，Magisk 模块形态，底层直连 wpa_supplicant，综合健康度决策，Web 面板可视化。

**核心痛点**：
- 5G 信号衰减时不切 2.4G，视频/网页加载卡顿
- 回到路由器附近不切回 5G，带宽浪费
- "信号满格但卡成狗"——微波炉/蓝牙干扰下 RSSI 满格但丢包率 50%
- 现有方案：无障碍亮屏耗电 / App 轮询扫网卡耗电 / 已废弃 Root App

**差异化**（相对 `wifi-roaming-enhancer` 等竞品）：
1. wpa_supplicant Unix Socket 直连（非 wpa_cli 子进程）→ 低延迟
2. **综合健康度决策**（RSSI + 重传率，可选 RTT）→ 不只看信号强度
3. 梯度靶向扫描（非全频段轮询）→ 省电
4. **统一 ROAM 切换路径**（wpa_supplicant 内部根据 AP 能力选 Fast Transition 或 4-way handshake）→ 老路由器不被踢下线
5. **空间记忆预测切换**（BSSID 射频指纹）→ 走到死角前就切，不等掉线
6. Web 面板（状态/曲线/参数微调）→ 可视化
7. Rust 守护进程 → 低 runtime 开销

> **产品文案（S14，日用场景定调）**：日用场景下"稳"优先于"快"——切换本身会短暂打断视频/网页，故下切也走严格防抖（不抢占），上切更稳（更长防抖窗+更低频嗅探）。宣传口径：**下切稳、上切更稳**。非对称策略仍成立：下切防抖窗（3-5s）短于上切（5-10s），因衰减持续恶化需比恢复更早响应；但两者都不抢带宽。

---

## 二、技术栈

| 层 | 选型 | 理由 |
|---|---|---|
| 守护进程语言 | Rust | cargo-ndk 交叉编译，release strip 后 ~400KB-1.5MB（实测量级），低 runtime，Thrawl 等真实案例 |
| 目标 triple | `aarch64-linux-android` | Android ARM64 主流，兼容 Android 13-15 |
| wpa_supplicant 交互 | 直连 Unix Socket（路径探测，见 §4.1） | 最低延迟，绕过子进程开销 |
| nl80211 | `neli` crate | **只读** `NL80211_CMD_GET_STATION` 拿硬件级 rx_packets/tx_retries/signal。**绝不用于建连**——越过 wpa_supplicant 会引发状态机失同步，触发系统 Deauth 踢断 |
| HTTP 服务器 | `tiny_http` | 零依赖轻量，符合 Rust 纯血定位。**不支持 WebSocket**，实时刷新走前端轮询 `/api/status` |
| Web 资源内嵌 | `include_bytes!` + `cfg!` | stdlib 宏，编译期嵌入单 HTML 文件到 `.rodata`。不引入 rust-embed——单文件场景 MIME 写死 `text/html`，热更新用 `cfg!(debug_assertions)` 切本地文件读取。Chart.js 同样 inline 嵌入（离线/captive 场景不可依赖 CDN） |
| 序列化 | `serde` + `serde_json` + `toml` | 配置 TOML（人读）+ API JSON（机读） |
| Unix Socket | `nix` crate | 类型安全的 syscall 封装 |
| Quick Settings Tile | **方案待定** | 纯 shell + `cmd statusbar add-tile` **不可行**——该命令需要已有 TileService 的 ComponentName（来自 APK），Magisk 脚本无法凭空创建。可选：① 最小 APK 提供 TileService；② 砍掉 QS 只留 Web；③ 用现成系统设置项。**Phase 3 前定方案，当前文档保留为待定** |

**交叉编译**：`cargo-ndk -t arm64-v8a`，`ANDROID_PLATFORM=android-24`（Android 7.0），动态链接 Bionic libc（默认）。Bionic ABI 自 API 21 起向后兼容，选 android-24 作为最小 API level 确保二进制在 Android 7.0+（含 13/14/15）无差别运行。不引用 Android 13+ 才有的新 API 符号。**包体（H7 修订 + 六轮再降 + 七轮 S13 修正）**：daemon 逻辑 release strip 后目标 ≤800KB（Phase 1 编译后记录实测值，超标则砍 Chart.js 压缩/功能组合）；含 web 资源（Chart.js inline ~200KB）总包 ≤1.2MB；debug 包体 3-5MB。

**明确不做**：
- eBPF（Android 内核 CONFIG_BPF 支持率低，落地风险高，wpa_supplicant 事件驱动已够）
- 双 WLAN 并发 Dual STA（依赖 SoC+驱动+HAL 三重支持，多数设备不可用）

---

## 三、项目结构

```
AmberGuard/
├── module.prop              # Magisk 模块元数据
├── post-fs-data.sh          # **阻塞阶段**（Zygote 前）跑 magiskpolicy --live 注入 SELinux 权限，杜绝时序竞态
├── service.sh               # **late_start 非阻塞阶段**只负责 setsid 启动 daemon，不碰 SELinux
├── sepolicy.rule             # SELinux 策略静态打底（具体 allow 规则见 §7.3）
├── customize.sh              # 安装时配置（检测设备/SoC/wpa 路径）
├── daemon/                   # Rust 守护进程
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs           # 入口，启动各模块
│       ├── wpa_ctrl.rs       # wpa_supplicant Unix Socket 控制
│       ├── scanner.rs        # 梯度扫描 + 靶向嗅探
│       ├── band_bond.rs      # 双频羁绊（BSSID 配对 + 跨 SSID 切换分支）
│       ├── health_score.rs   # 综合健康度 + 防抖（合并 anti_jitter，滑动窗口 30 行内）
│       ├── state_machine.rs  # 切换状态机 + 失败惩罚锁（合并 penalty）
│       ├── power_state.rs    # **电源状态感知**（sysfs/屏状态/timer 降频，Phase 1 PoC）
│       ├── config.rs         # 配置读写（/data/adb/amberguard/config.toml）
│       └── web/
│           ├── mod.rs        # HTTP 路由 + include_bytes! 内嵌 index.html
│           └── static/
│               └── index.html # 单文件前端（内嵌 CSS+JS+Chart.js）
├── web/                      # Web 面板源文件（开发用，构建时内嵌）
│   └── index.html
├── docs/
│   └── PLAN.md               # 本文件
└── README.md
```

**模块合并理由**：13 文件 MVP 过度拆分。anti_jitter 是滑动窗口中位数 + 不对称策略，30 行内可并入 health_score；penalty 是结构体 + 指数退避逻辑，并入 state_machine 减少跨文件跳转。Phase 3 视复杂度再拆。`captive_portal.rs` / `spatial_memory.rs` / `beacon_parser.rs` 留到对应 Phase 才进 src/。

---

## 四、核心模块设计

### 4.1 wpa_ctrl.rs — wpa_supplicant 控制接口

**职责**：连 Unix Socket，收发 wpa_supplicant 控制命令。

**核心命令**：
```
STATUS              # 当前连接状态
SIGNAL_POLL          # 当前信号 RSSI（命令本身不持 WakeLock，但 daemon 定时器会唤醒 CPU）
SCAN freq=5180,5200  # 靶向扫描（指定信道）
SCAN_RESULTS         # 扫描结果
ROAM <bssid>         # 触发漫游（同 SSID 内 BSSID 切换，wpa_supplicant 内部按 AP 能力选 FT 或 4-way）
SELECT_NETWORK <id>  # 跨 SSID 切换（异名双频如 Home_5G/Home_2G 时需此命令 + bssid 约束）
LIST_NETWORKS        # 枚举已配置 network 列表（H9 修订：异 SSID 切换前快照/恢复 network 状态必需）
SET_NETWORK <id> <param> <value>  # 改 network 参数（bssid 锁定/解锁、disabled 状态）
GET_NETWORK <id>     # 读 network 当前参数（快照用）
BSS <bssid>          # 查 scan cache 单 BSS（bssid_in_cache 的实现）
REASSOCIATE          # 可选（H9 修订）：若 SELECT_NETWORK 与已连接同 nid 需重连时用，实机验证
```

**事件订阅**：attach 模式接收异步事件（与 `wpa_ctrl.h` 对齐）：
- `CTRL-EVENT-SCAN-RESULTS`
- `CTRL-EVENT-CONNECTED`（含 `bssid=`，BSSID 变化由此感知）
- `CTRL-EVENT-DISCONNECTED`
- `CTRL-EVENT-SIGNAL-CHANGE`（由 bgscan 路径触发，非任意 RSSI 抖动都推送，**Phase 1 实机验证触发频率**）
- `CTRL-EVENT-BEACON-LOSS`（间接信号：AP 失联前兆）
- `CTRL-EVENT-SUBNET-STATUS-UPDATE`（漫游后 DHCP 子网变化，可作切换成功辅助判据）

> **澄清**：`CTRL-EVENT-SLEEP` / `CTRL-EVENT-WAKEUP` **不存在**于 wpa_ctrl.h 事件宏。Framework 向 wpa_supplicant 发的是 `SUSPEND` / `RESUME` **控制命令**（非事件），`wpas_notify_suspend` 只做内部停扫，**不向 attach 的客户端广播**。电源感知入口见 §4.7 power_state。

**关键决策**：
- **套接字路径探测（厂商魔改盲盒，H7 修订）**：启动时按序尝试——① 解析 wpa 配置/prop/cmdline 的 `ctrl_interface`（`DIR=` 目录语义）→ ② 目录下按 client 协议连接（`@wpa_wlan0` abstract namespace 与 `/data/misc/wifi/sockets/wpa_ctrl_*` 等文件路径都要试）→ ③ 常见路径 `/dev/socket/wpa_wlan0` → ④ 配置项覆盖。**"活跃"唯一判定：`STATUS` 命令往返成功**。注意 `/data/misc/wifi/wpa_supplicant` 在部分机型是 **conf/目录语义**，只用于解析 `ctrl_interface`，**不当作 socket 连接目标**
- 客户端 socket **建在解析得到的 `ctrl_interface` 目录内**（abstract 则按 abstract 规则），`/data/misc/wifi/sockets/` 仅为常见候选之一（H8 修订：DIR= 模式下客户端 socket 必须与服务端同目录，写死路径会在 abstract/非标准 DIR 机型上 connect 失败），需正确目录权限与 SELinux allow（见 §7.3）
- **重连机制**：断线后指数退避重连
- **并发模型（H2 修订，见 §5.1）**：wpa 事件由**专用 I/O 线程**阻塞读，命令经**命令队列**发送，**状态机/定时器路径禁止长阻塞 `wait_event`**——SCAN 完成靠 `CTRL-EVENT-SCAN-RESULTS` 回调推进状态，不在函数内睡 3 秒
- **绝不常态轮询 SIGNAL_POLL**：定时器会唤醒 CPU 阻止深度休眠，与低耗电定位相悖。亮屏常态依赖 `CTRL-EVENT-SIGNAL-CHANGE` 事件推送（零开销）。仅防抖期内以 1s 间隔主动 `SIGNAL_POLL`（与降级链对齐），持续防抖窗长度（3-5s 下切 / 5-10s 上切），窗满后恢复事件模式。**注意**：SIGNAL-CHANGE 依赖 monitor/bgscan 配置，非"attach 就稳有"——L2"纯事件驱动"可能在某些厂商 wpa 上饿死，需低频 SIGNAL_POLL 兜底（**Phase 1 实机验证**）

### 4.2 health_score.rs — 综合健康度 + 防抖（核心创新）

**痛点**：RSSI 满格但微波炉/蓝牙干扰下丢包率 50%，"信号满格死活不切"。

**数据源降级链**（现代 SoC 把 MAC 层统计放固件 DSP，`/proc` 和 nl80211 都可能不可靠，逐个尝试取可用值）：
1. **优先**：nl80211 `NL80211_CMD_GET_STATION` 穿透内核驱动拿 `rx_packets`/`tx_retries`/`signal`（驱动用 `sinfo->filled` 位图决定是否填充；FullMAC 设备 Qualc/MTK 常缺 `TX_RETRIES`，则降级）
2. **降级**：`/proc/net/wireless` **按表头命名字段解析**（`Quality` 的 link/level/noise 与 `Discarded` 的 retry 等，**不按固定列号**——WE 版本/OEM 改格式会错位；解析失败或字段缺失则整源作废，不做半对半错）
3. **兜底**：`SIGNAL_POLL` 命令纯 RSSI（最可靠但只有信号强度）

**健康度算法**：
```rust
// retry_rate 定义：时间窗内 Δtx_retries / Δtx_packets，比值 0.0–1.0（非累计值）
// 量纲校准：0.5 重传率 → retry_score 50（满格失效预警），乘 100 后可再乘灵敏度系数
// MIN_TRAFFIC_THRESHOLD：时间窗内 tx_packets 增量下限（H6 修订：比值是 tx 统计，门控必须用 tx_delta），低于此视为无流量（Phase 2 默认 8 packets/窗）；tx_delta == 0 时 retry 项显式走中性分
// 局限（如实声明）：tx_retries 对"纯下行卡顿"不敏感——下行劣化时 tx 很少，retry 项不增长，健康度由 RSSI 主导
fn health_score(rssi: i32, retry_rate: Option<f32>, tx_delta: u64, rtt_ms: Option<u32>) -> f32 {
    let rssi_score = ((rssi + 90) as f32 / 50.0 * 100.0).clamp(0.0, 100.0);
    // 无流量（tx_packets 停滞）时 tx_retries 不增长 → "假健康"，降权 retry 项到中位
    let retry_score = match retry_rate {
        Some(rate) if tx_delta > MIN_TRAFFIC_THRESHOLD => (100.0 - rate * 100.0).max(0.0),
        _ => 50.0,  // 无流量（tx_delta 未过门控）时 retry 项置中（不加分也不扣分），由 RSSI 主导
    };
    // 权重归一：RTT 缺省时 RSSI/Retry 各 50%
    match rtt_ms {
        Some(rtt) => {
            let rtt_score = (100.0 - (rtt as f32 - 10.0) / 2.9).max(0.0).min(100.0);
            rssi_score * 0.4 + retry_score * 0.4 + rtt_score * 0.2
        }
        None => rssi_score * 0.5 + retry_score * 0.5,
    }
}
```

**决策**：三档监控阈值 + 上切条件——`score < 70` 触发梯度检测（L1 靶向扫描，>75 退出检测），`score < 30` 触发切换（日用更保守，容忍更低信号），`score > 70` 且驻留非首选频段时按 N1 策略低频嗅探对侧后上切。**上切谓词（H4+H6 修订）**：对侧 RSSI ≥ `upswitch_rssi_min_dbm`（默认 -65dBm，config 键名 `upswitch_rssi_min_dbm`，Phase 2 验收项），**禁止对未关联 BSSID 调用完整 health_score**（未关联时无 retry/RTT 时间窗统计，只能诚实用 RSSI）。与 §4.4 scanner 触发逻辑对齐。权重配置文件可调，灵敏度系数可调。

**RTT 实现待定**：Android 上 ICMP 受限，TCP connect 延迟或 HTTP HEAD 均有副作用。**Phase 2 先 RSSI+Retry，Phase 2.5 验证 RTT 测量稳定性再加权重**。

**防抖（合并自原 anti_jitter）**：
- **下切要稳**（5G→2.4G，健康度持续衰减）：**日用场景不追求"狠"**——切换本身会短暂打断视频/网页，抢占式快切反而破坏体验。常态由 L2 事件驱动（见下方**采样/轮询矩阵**），健康度跌破阈值后走 **3-5s 严格防抖**（滑动窗口取中位数，抗极值），确认仍低 + 对侧够强才进 SWITCH。**删除紧急抢占模式**：原"500ms-1s 紧急确认窗"在日用下无必要，且短窗达不到 MIN_TRAFFIC_THRESHOLD 会让 retry 项失效；改为与上切同规格的严格防抖，靠 tx_delta 长窗保证 retry 参与确认。
- **上切更稳**（2.4G→5G，信号恢复）：**5-10s 严格防抖**（日用不急切回 5G，容忍更长延迟换取稳定性），滑动窗口取中位数，防止来回跳跃。

**采样/轮询矩阵（H3 修订，写死"常态 5s 采样"的含糊表述）**：

| 模式 | 周期 | 数据源 | 是否 SIGNAL_POLL |
|---|---|---|---|---|
| L2 健康监控 | 无周期，仅事件驱动 | `CTRL-EVENT-SIGNAL-CHANGE` + 可选缓存 | 否 |
| L2 饿死兜底 | ≥30s | `SIGNAL_POLL`（SIGNAL-CHANGE 几乎不触发时启用，Phase 1 验证） | 是 |
| 防抖期主动采样 | 1s（仅防抖窗内开启） | `SIGNAL_POLL` + 降级链（完整健康度），窗内样本≥N（3-5s 窗 N≥3，5-10s 窗 N≥5） | 是 |
| 决策 tick | 1s 空转，仅检查定时器到期 | **不采网**（数据来自事件/防抖期采样） | 否 |

### 4.3 band_bond.rs — 双频羁绊 + 跨 SSID 切换分支

**职责**：识别同一路由器的 2.4G/5G BSSID 对，并决定切网命令分支。

```rust
struct BondedAP {
    ssid_5g: String,        // 同名时与 ssid_24g 相等；异名时各存各
    ssid_24g: String,
    bssid_5g: [u8; 6],
    bssid_24g: [u8; 6],
    channel_5g: u16,        // 如 36/40/44/48（优先用 SCAN_RESULTS/BSS 的 freq 字段 MHz；channel_to_mhz() 公式仅做回退：2.4G: 2412+(n-1)*5，5G: 5000+n*5；6G 公式 Phase 6 再补）
    channel_24g: u16,      // 如 1/6/11
    rssi_5g: i32,
    rssi_24g: i32,
    last_seen: Instant,
}
```

**配对策略**：
- 自动：扫描结果中 SSID 相同 + 一个在 5G 信道 + 一个在 2.4G 信道
- 同 SSID 多 AP 时，优先选 RSSI 较强的 AP 作为主配对（避免 mesh 爆炸）
- 手动：配置文件指定 BSSID 对（处理 SSID 不同但实际同路由的情况，如 `Home_5G` / `Home_2G`）

**切换命令分支**（**关键，原 PLAN 漏掉**）：
```rust
// H5+H8 修订：switch_to 是异步状态机步进，不设 wait_event，靠事件推进
// 状态机收到 TRIGGER_SWITCH → 先确保 scan cache 有对侧 BSSID
//   ├─ cache 命中 → 直接发 ROAM/SELECT（发前做 wpa_state 门禁检查）
//   └─ cache 未命中 → NEED_SCAN → 发 SCAN freq=... → 等 CTRL-EVENT-SCAN-RESULTS → re-check cache → 再发 ROAM/SELECT
// 切换事务启动时（Phase 1 重启后）也执行清理，防止残留 bssid 锁
//   ① LIST_NETWORKS 快照当前所有 network 的 enabled/disabled 状态
//   ② 异 SSID 时 SET_NETWORK <id> bssid <bssid> 锁定目标 AP
// 切换事务出口（成功/失败都执行）：
//   ③ 异 SSID 时 SET_NETWORK <id> bssid "" 解除 bssid 锁定
//   ④ 按快照恢复被 SELECT_NETWORK disable 的其它 network（SET_NETWORK <id> disabled 0）
// ConnectedAP：当前连接的最小结构 { ssid: String, bssid: [u8;6], band: Band }
// H5+H8 修订：switch_to 是异步状态机步进，不设 wait_event，靠事件推进
// 状态机收到 TRIGGER_SWITCH → 先确保 scan cache 有对侧 BSSID
//   ├─ cache 命中 → 直接发 ROAM/SELECT（发前做 wpa_state 门禁检查）
//   └─ cache 未命中 → NEED_SCAN → 发 SCAN freq=... → 等 CTRL-EVENT-SCAN-RESULTS → re-check cache → 再发 ROAM/SELECT
// 切换事务启动时（Phase 1 重启后）也执行清理，防止残留 bssid 锁
//   ① LIST_NETWORKS 快照当前所有 network 的 enabled/disabled 状态
//   ② 异 SSID 时 SET_NETWORK <id> bssid <bssid> 锁定目标 AP
// 切换事务出口（成功/失败都执行，finally 保证执行）：
//   ③ 异 SSID 时 SET_NETWORK <id> bssid "" 解除 bssid 锁定
//   ④ 按快照恢复被 SELECT_NETWORK disable 的其它 network（SET_NETWORK <id> disabled 0）
// ConnectedAP：当前连接的最小结构 { ssid: String, bssid: [u8;6], band: Band }
fn switch_to(&self, current: &ConnectedAP, target_band: Band, wpa: &mut WpaCtrl) -> SwitchResult {
    // 返回值：Ok(CommandSent) | NeedScan(freq) | Error
    let target_bssid = self.bssid_for_band(target_band);
    let target_ssid = self.ssid_for_band(target_band);
    // 发命令前做 wpa_state 门禁（4WAY_HANDSHAKE/ASSOCIATING 窗口禁止抢发）
    let state = wpa.get_state()?;
    if state == "4WAY_HANDSHAKE" || state == "ASSOCIATING" {
        return Err(Error::Busy(state));
    }
    if !wpa.bssid_in_cache(target_bssid) {
        wpa.send(&format!("SCAN freq={}", self.freq_for_band(target_band)))?;
        // 不 wait——返回 NeedScan，等 CTRL-EVENT-SCAN-RESULTS 后回调 re-check cache
        // 状态机在 GRADIENT_DETECT 等 cache 就绪后再发 ROAM/SELECT
        return SwitchResult::NeedScan(self.freq_for_band(target_band));
    }
    // cache 命中：发切换命令
    if current.ssid == target_ssid {
        // 同 SSID：ROAM <bssid>，wpa_supplicant 内部按 AP 能力选 FT 或 4-way
        wpa.send(&format!("ROAM {}", mac_to_str(target_bssid)))
    } else {
        let nid = wpa.network_id_for_ssid(target_ssid)?;
        // 切换事务入口操作（快照 + 锁 bssid）由调用方（SWITCHING 状态）负责
        // 此处只发命令，finally 由状态机出口统一处理
        wpa.send(&format!("SET_NETWORK {} bssid {}", nid, mac_to_str(target_bssid)))?;
        wpa.send(&format!("SELECT_NETWORK {}", nid))
    }
    SwitchResult::CommandSent
}
```

> **降级策略**：统一以 wpa_supplicant 为建连唯一入口。同 SSID `ROAM <bssid>`，异 SSID `SELECT_NETWORK <id>`（+ `SET_NETWORK <id> bssid` 锁定 AP）。**不做 netlink 建连**——越过 wpa_supplicant 会触发 Deauth。**异 SSID 切换细节以实机行为为准**——若 SELECT_NETWORK 与已连接同 nid 可能需 REASSOCIATE，Phase 2 实现时实测定。

### 4.4 scanner.rs — 梯度扫描 + 靶向嗅探

**梯度策略**（核心创新）：
1. **L0 全频段扫描**：只在初始化或羁绊丢失时触发（3000ms）
2. **L1 靶向扫描**：只扫已知羁绊 AP 所在信道（~100ms，但不能假设一定成功——vendor HAL/固件侧仍可能限频，**Phase 1 实机验证**）
3. **L2 被动监控**：依赖 `CTRL-EVENT-SIGNAL-CHANGE` 事件推送（零开销，不轮询）。健康度跌破阈值后走 §4.2 严格防抖（3-5s 滑动窗口），**不再紧急抢占**。若实机发现 SIGNAL-CHANGE 几乎不触发，则降级为低频 SIGNAL_POLL 兜底（30s 间隔，权衡耗电）

**触发逻辑**（基于健康度，非纯 RSSI）：
- 健康度 > 70 且 current_band == preferred：L2 被动监控（事件驱动，不轮询）
- 健康度 > 70 且 current_band ≠ preferred：**L1 对侧信道嗅探（60-120s 间隔，N1 上切感知，日用场景拉长间隔省电）**——否则永远不知道对侧 5G 已恢复
- 健康度 30-70：L1 靶向扫描（15-20s 间隔，日用拉长省电）
- 健康度 < 30：L1 靶向扫描 + 准备切换
- **Doze/Suspend**：全部冻结，不轮询不持 WakeLock（由 power_state.rs 控制）

### 4.5 state_machine.rs — 切换状态机 + 失败惩罚锁

```
IDLE
  ↓ score < 70（不经防抖，直接驱动 L1 扫描）
GRADIENT_DETECT（仅驱动 scanner L1 靶向扫描，**禁止进 SWITCH**）
  ↓ 若 score 持续 < 30 且防抖通过（下切 3-5s 严格防抖） + 对侧 RSSI ≥ upswitch_rssi_min_dbm
TRIGGER_SWITCH
  ↓ 检查羁绊对侧信号
SWITCH_READY
  ↓ 对侧够强
  ├─ NeedScan → GRADIENT_DETECT（等结果）
  SWITCHING（含 captive 风险提示，**真检测在切后并行**）
  ├─ 同 SSID → ROAM <bssid>（wpa 内部选 FT 或 4-way）
  └─ 异 SSID → SET_NETWORK <id> bssid <bssid> + SELECT_NETWORK <id>（细节以实机行为为准）
  ↓ 收到 CTRL-EVENT-CONNECTED → L3 就绪确认（1-5s 指数重试网关探测，四态 L2Connected/L3Ready/Portal/L3Timeout）→ 成功
  └─ **成功/失败都执行切换事务清理（H3/H4）**：SET_NETWORK <id> bssid "" 解除锁定；按切前快照恢复被 SELECT_NETWORK disable 的其它 network
IDLE（已切到对侧频段）
  ↓ 失败（超时或 DISCONNECTED 未恢复）
PENALTY（挂起惩罚锁，指数退避：30 → 60 → 120 → 300s 上限 5min）
  ↓ 冷却结束
IDLE
  ↓ score > 70 且驻留非首选频段时按 N1 嗅探
  ↓ 对侧 RSSI 达标
  上切分支（不进 SWITCHING）
  ↓ 息屏/Doze（任意态可进，power_state 触发）
FROZEN（冻结所有计时器，**取消 1s 决策 tick，仅保留 ≤1 个 5min 心跳 timer 或纯事件阻塞 epoll**，不持 WakeLock，避免对抗深度休眠）
  ↓ 亮屏（恢复 1s 决策 tick + restore_pre_frozen 恢复惩罚锁与健康基线）
IDLE
```

**反向**（2.4G 切回 5G）：
- **对侧 5G RSSI ≥ `upswitch_rssi_min_dbm`（默认 -65dBm，config 键名可配）**（N1 嗅探提供，防抖通过）→ 触发切回。**禁止用"5G 健康度 >70"做上切判据**——驻留 2.4G 时对侧 5G 只有扫描 RSSI，无该 BSSID 的 retry/RTT 时间窗，算不出完整 health_score
- **上切感知（N1）**：驻留 2.4G 且自身健康时只走 L2 被动监控，**不会主动嗅探 5G**——可能永远不知道 5G 已恢复。策略：驻留非首选频段时，即使 score > 70 仍以 **60-120s 低频 L1 靶向嗅探对侧信道**（日用场景拉长间隔省电），确认对侧 RSSI 达标后触发上切。Phase 2 开工前落实。

**惩罚锁**（合并自原 penalty.rs）：
```rust
struct PenaltyLock {
    ap_key: String,        // bond_id：5G-BSSID + 2.4G-BSSID pair（S12 修订：惩罚锁按羁绊粒度而非单 AP）
    failed_at: Instant,
    cooldown_secs: u64,    // 30 → 60 → 120 → 300（上限 5min）
    retry_count: u8,
}
```
- 每次切换失败 cooldown 翻倍，达 5min 上限后保持；**重置条件**：对侧稳定驻留 ≥2min 或连续 3 次健康采样 >70，证明对侧已稳定才清零，避免刚切回又波动就重置

**状态机边界**（待补完，Phase 2 实现时枚举所有事件）：
- **wpa_state 门禁（S2）**：发 SCAN/ROAM/SELECT_NETWORK 前先读 `STATUS` 的 `wpa_state`，`4WAY_HANDSHAKE`/`ASSOCIATING` 等过渡窗口**禁止抢发**——Framework 可能正在建连，此时发切换命令会打架
- 切换中用户手动连其他网 → 中止当前切换回 IDLE，**先执行切换事务清理**（解 bssid 锁 + 恢复 network 快照）
- 对侧 AP 消失 / 扫描失败 → PENALTY 短冷却（同样先清理事务）
- 飞机模式 / WiFi 关闭 → 全状态冻结（与 FROZEN 同态，冻结所有计时器，不持 WakeLock）
- Frozen 中收到 CONNECTED/DISCONNECTED → 仅记录不触发决策
- captive 用户拒绝认证 → 当前网保持，标记该 AP 不可切

**切换成功判据（H10 修订）**：不只看 ROAM/SELECT_NETWORK 命令返回 OK，要等 `CTRL-EVENT-CONNECTED` 事件 + 网关可达（**默认开启网关探测，config.toml 提供开关**——默认为可达性 HTTP 探测 generate_204，可关）。`CTRL-EVENT-SUBNET-STATUS-UPDATE` 可作辅助判据。**L3 重试窗**：CONNECTED 后 1-5s 指数重试网关探测（1s→2s→5s，最多 3 次），窗口内区分四态——L2Connected（L2 好 L3 未就绪，继续重试）/ L3Ready（成功）/ Portal（被劫持，走 §4.6）/ L3Timeout（超时，按失败走 PENALTY）。避免 DHCP/路由未就绪时误判失败或误判成功。

### 4.6 captive_portal.rs — Captive Portal 检测（Phase 3）

**场景**：机场/酒店 5G 切 2.4G 后需重新网页认证，网页/视频中断。

**职责**：**切网后并行检测**是否进入 Captive Portal 环境（不阻塞切换流程，原 PLAN 状态机放在 SWITCHING 前是错的）。

**检测逻辑**：
1. 切网前记录当前网关 MAC（风险提示用）
2. 切网后等待 CONNECTED 事件 + **网关可达确认（§4.5 L3 重试窗的 L3Ready 态）再发 HTTP**——避免 DHCP 未就绪时误判 Portal
3. 发 HTTP HEAD 到 `http://connectivitycheck.gstatic.com/generate_204`
4. 若返回非 204 或被劫持 → 标记该 AP 进入 Portal 状态 + **用户提示出口（H11 修订，三选一，Phase 3 开工前定）**：① `cmd notification post` 发系统通知（需 shell 权限，Android 10+ 部分受限，实机验证）；② Web 面板轮询到 Portal 状态后前端提示（零权限，最稳）；③ 最小 APK 提供通知 + 与 QS Tile 合并（最完整，引入 APK 决策点）。**不做悬空的"高优通知"承诺**

```rust
fn check_portal_after_switch() -> PortalStatus {
    let resp = http_head("http://connectivitycheck.gstatic.com/generate_204")?;
    match resp.status {
        204 => PortalStatus::Clean,
        302 | 301 => PortalStatus::CaptiveRedirect(resp.location),
        _ => PortalStatus::Unknown,
    }
}
```

> **注意（H13 修订）**：Android 12+ **每网随机 MAC 随机的是 STA 本机 MAC，不是扫描结果中的 AP BSSID**——射频指纹用的是周围 AP 的 BSSID+RSSI 向量，AP BSSID 仍相对稳定。指纹鲁棒性主风险是：扫描节流（拿不到完整 BSSID 列表）、AP 开关/mesh 多 BSSID、环境多径，Phase 4 实现时以**多 AP 聚类 + 扫描完整性检查**增强。

### 4.7 power_state.rs — 电源状态感知（Doze 黑洞对策，Phase 1 PoC）

**痛点**：息屏进 Deep Doze 后，Vendor HAL 物理切断 WiFi 后台扫描，内核设为挂起。Daemon 若仍跑定时器轮询会唤醒 CPU 阻止深度休眠，待机耗电尿崩，与低耗电定位相悖。

**职责**：感知系统电源状态，Doze/Suspend 时冻结状态机，Resume 后快速恢复。

**入口候选**（**原 PLAN 用 CTRL-EVENT-SLEEP/WAKEUP 是错的，这两个事件不存在**，见 §4.1 澄清）：

| 方案 | 实现 | 优劣 |
|---|---|---|
| A. sysfs 屏状态节点 | poll `/sys/class/backlight/*/brightness`（或 `/sys/class/graphics/fb0/blank`，厂商相关） | 简单但 poll 仍需 timer；**不要用 `/sys/power/wake_lock`——那是持锁列表不是屏状态**；PoC 枚举多个候选 |
| B. Input device 监听 | epoll `/dev/input/event*` 找 powerkey 事件 | 需 root + SELinux allow，准确度高 |
| C. 长周期 timer 降频 | 息屏后把所有 timer 放宽到 5min，亮屏由首次 SIGNAL-CHANGE 或 SCAN-RESULTS 唤醒 | 不依赖外部事件，但亮屏恢复慢 |
| D. 综合 | A + C：sysfs 检测 + 长周期心跳 | 推荐方向，**Phase 1 PoC 验证可用性** |

**Phase 1 必须做的事**：实机验证以上入口哪个稳定可用，再写 power_state.rs 主体。**不要相信任何未实机验证的电源事件源**。

**状态机交互**（按方案 D 写，方案 PoC 后可能调整）：
```rust
fn on_screen_off(&mut self) {
    self.scanner.freeze();
    self.health_score.freeze();
    self.state_machine.transition(State::Frozen);
    // 启动 5min 心跳 timer，期间不持 WakeLock
    self.heartbeat_timer = Some(Instant::now() + Duration::from_secs(300));
}

fn on_screen_on(&mut self) {
    // 驱动从 suspend 恢复需要时间，等 500ms-1s 再扫
    self.scanner.resume();
    self.scanner.trigger_targeted_scan();
    self.health_score.resume();
    // 恢复 Frozen 前状态，不无脑回 Idle——否则丢惩罚锁
    self.state_machine.restore_pre_frozen();
}
```

**关键约束**：
- Frozen 状态下**绝不主动唤醒**——不持 WakeLock，不对抗系统休眠策略
- **"息屏零耗电"改为可测指标（H12 修订）**：① 无 WakeLock/alarm 滥用（`dumpsys power | grep amberguard` 无持锁）；② 息屏 30min 同机对比相对基线额外耗电 <x%/h（实测后定 x）；③ log 证明未在 Deep Doze 中高频唤醒（时间戳间隔无密集唤醒）。**"零耗电"不可测，不写入里程碑**
- WoWLAN（Wake on Wireless LAN）硬件唤醒拦截留 Phase 6 实验
- 亮屏恢复后的快速扫描目标 500ms-1s（不是 200ms，驱动 resume 需要时间）
- **Doze 下 daemon 可能被 App Standby 冻结**（非系统进程）——Magisk daemon 通常豁免，但 **Phase 1 实机验证**

### 4.8 spatial_memory.rs — 空间记忆/射频指纹（Phase 4）

> **改名**：原"磁场指纹"误导（无磁力计），实际是 BSSID+RSSI 向量指纹，改名"射频指纹"。

**痛点**：等信号衰减再扫信道太慢，走到死角才切，视频/网页已卡。

**职责**：记录移动轨迹，预测性提前切换。

**数据结构**：
```rust
struct SpatialFingerprint {
    bssid_signature: Vec<(Bssid, Rssi)>, // 当前可见的所有 BSSID + 信号
    location_label: String,              // 自动聚类的"位置"（如"走廊"）
    preferred_band: Band,                // 该位置历史最优频段
    sample_count: u32,
}
```

**算法**：
1. 后台记录（BSSID 组合, RSSI 向量, 当前频段, 健康度）采样
2. 相似 RSSI 向量聚类成"位置"
3. 每个位置统计各频段历史健康度
4. 实时采样匹配最近位置 → 若该位置对侧频段历史更优 → 提前 1s 切换

**存储**：`/data/adb/amberguard/fingerprints.bin`，二进制序列化。

> **注意（H13 修订）**：Android 12+ 每网随机 MAC 影响的是 **STA 本机 MAC**，扫描到的 **AP BSSID 仍相对稳定**——指纹稳定性下降的说法不准确。Phase 4 主风险是扫描节流（BSSID 列表不全）与 AP 变化（mesh 多 BSSID/AP 开关），按多 AP 聚类 + 扫描完整性检查处理。

### 4.9 web/ — Web 面板

**API**：
| 路径 | 方法 | 功能 |
|---|---|---|
| `/api/status` | GET | 当前连接、健康度、绑定 AP、状态机当前态 |
| `/api/history` | GET | 信号/健康度曲线数据（最近 N 采样点） |
| `/api/config` | GET/POST | 读取/修改配置（阈值、扫描间隔、权重） |
| `/api/scan` | POST | 手动触发全频段扫描 |
| `/api/switch` | POST | 手动触发切换 |
| `/api/fingerprints` | GET | 射频指纹数据（Phase 4） |

> **删除 `/api/ws`**：tiny_http 不支持 WebSocket。前端用 1s 轮询 `/api/status` 拿实时数据，足够用。

**前端**：单 HTML 文件，内嵌 CSS+JS+Chart.js（Chart.js inline 嵌入，不依赖 CDN，离线/captive 场景可用）。1s 轮询刷新状态。
**监听**：`127.0.0.1:8080`，仅本机访问。
**鉴权威胁模型（S6）**：`/api/config` POST 与 `/api/switch` POST 可改配置/触发切换。本机绑定下威胁面 = 本机任意进程（root 下本无隔离）；明确接受此模型，文档声明即可，**不加 token**（防的是局域网访问，非本机恶意进程）。若后续要求更强隔离再加。

### 4.10 qs_tile — Quick Settings Tile（Phase 3，方案待定）

**职责**：注入下拉快捷开关，切换工作模式。

**模式**：

| 模式 | score <30 切？ | 防抖窗 | L1 间隔 | N1 间隔 | 阈值 | 行为 |
|---|---|---|---|---|---|---|
| 日用 | 是 | 下切 3-5s / 上切 5-10s | 15-20s | 60-120s | 标准 30/70 | 视频/网页稳为先，严格防抖不抢占 |
| 省电 | 是 | 下切 5-8s / 上切 10-15s | 20-30s | 120-180s | 换 <25 | 延长所有间隔减少扫描耗电 |
| 暂停 | 否（完全停用） | — | — | — | — | 不触发任何扫描或切换 |

**方案**（Phase 3 前定）：
- ❌ 纯 shell + `cmd statusbar add-tile`：**不可行**，需 APK 的 ComponentName
- 选项 1：最小 APK 提供 TileService（与 Magisk 模块一起安装）
- 选项 2：砍掉 QS，只留 Web 面板
- 选项 3：用现成系统设置项（功能受限）

长按打开浏览器 `http://127.0.0.1:8080`。

### 4.11 beacon 解析（降级为可选日志，非独立模块）

**原 PLAN 立独立 beacon_parser.rs 模块**，但既然 §4.3 统一用 ROAM/SELECT_NETWORK，wpa_supplicant 内部会根据 AP 能力自动选 FT 或 4-way handshake，**不需要客户端解析 11r/k/v 能力来决定切换命令**。

**降级处理**：
- 切换日志里记一行 `SCAN_RESULTS` flags 列的 `[FT]` / `[WPA2-FT-...]` 标记即可
- 字段 `BSS_MEMBERSHIP_SELECTOR` 与 HE/EHT 成员选择相关，**不是** 11r FT 能力，原 PLAN 字段名错
- 11r 真正能力看 Mobility Domain IE，但既然不用于决策，不解析
- 若后期发现某些厂商 wpa 不自动选 FT 路径需手动干预，再立模块

---

## 五、数据流

```
[wpa_supplicant] ←Unix Socket→ [wpa_ctrl.rs]
                                    ↓ CTRL-EVENT-* / 降级采样
[nl80211 GET_STATION] ─────────→ [health_score.rs] ←RSSI+Retry（降级链，RTT Phase 2.5）
[/proc/net/wireless] ──────────→ ┃ + 防抖
[SIGNAL_POLL 兜底] ────────────→ ┃
                                    ↓ 稳定健康度
                              [scanner.rs] ←梯度策略（L0/L1/L2）
                              [power_state.rs] ←sysfs/屏状态/timer（Phase 1 PoC）
                                      ↓ 冻结/恢复
                              [state_machine.rs] + 惩罚锁
                                    ↓ 切换决策
                              [band_bond.rs] ←羁绊查询 + 跨 SSID 分支
                                    ↓ BSSID 对 + 切换命令
                              [wpa_ctrl.rs] → 同 SSID: ROAM / 异 SSID: SELECT_NETWORK
                                    ↓ CTRL-EVENT-CONNECTED + 可选网关可达 → 成功
                                    ↓ 失败 → PENALTY

[captive_portal.rs] ←切后并行 HTTP 检测（Phase 3）
[spatial_memory.rs] ←后台采样→ /data/adb/amberguard/fingerprints.bin（Phase 4）
        ↓ 预测匹配
  提前注入 state_machine 决策

[web/mod.rs] ←HTTP→ 浏览器 (127.0.0.1:8080)
      ↑
  读所有模块状态（1s 轮询，无 WebSocket）

[qs_tile] ←用户点击→ 改 config.toml 工作模式（Phase 3，方案待定）
```

### 5.1 并发模型（H2 修订，Phase 1 骨架必须按此实现）

```
┌─────────────────── 主线程（状态机/定时器/决策） ───────────────────┐
│  health_score ← scanner ← state_machine → band_bond → 命令队列       │
│  定时器：决策 tick（1s 空转不采网，见 §4.2 采样矩阵）/ 严格防抖窗（3-5s 下切 / 5-10s 上切）/ N1 上切嗅探 / Frozen 5min 心跳 │
└──────────────┬───────────────────────────────────────────────┘
               │ 命令入队 / 事件回调出队
┌──────────────▼───────────────────────────────────────────────┐
│  wpa I/O 线程：阻塞读 wpa socket 事件（attach）+ 执行命令队列    │
│  SCAN 完成 → 发 CTRL-EVENT-SCAN-RESULTS 回调 → 推进状态机        │
└──────────────┬───────────────────────────────────────────────┘
               │ 只读快照
┌──────────────▼───────────────────────────────────────────────┐
│  HTTP 线程（tiny_http）：/api/* 读模块状态快照，写配置走命令队列  │
└───────────────────────────────────────────────────────────────┘
```

**铁律**：
- 状态机/定时器路径**禁止长阻塞 `wait_event`**（如等 SCAN 结果 3s）——SCAN 完成靠事件回调推进；**事件所有权钉死（S1）**：wpa I/O 线程只把原始事件入队，**主线程是唯一推进状态机的实体**
- 共享 `WpaCtrl`/状态机数据用 Mutex 或消息传递，明确唯一所有者；HTTP 线程只读快照
- 切换事务（SET_NETWORK bssid 锁/network 快照）在 SWITCHING 出口统一清理，成功/失败路径都执行（H3/H4）

---

## 六、实施阶段

### Phase 1 — 骨架跑通 + 关键 PoC（先验证可行性）
**目标**：装上模块，浏览器能看到当前 WiFi 状态；**同时钉死关键数据源和命令**。

- [ ] Magisk 模块骨架（module.prop / post-fs-data.sh / service.sh / sepolicy.rule / customize.sh）
- [ ] **post-fs-data.sh**（阻塞阶段，Zygote 前）：跑 `magiskpolicy --live` 注入 SELinux 权限（具体 allow 规则见 §7.3），杜绝时序竞态
- [ ] **service.sh**（late_start 非阻塞阶段）：只启动 daemon，不碰 SELinux。**用模块根绝对路径 + setsid daemon & + 崩溃重启策略**（简单循环或 `while true; do daemon; sleep 5; done`，**连续崩溃 5 次后停止并打日志——backoff 上限（S13），防死循环耗电**），不用相对路径
- [ ] Rust daemon 启动 + **Socket 路径扫描器（H7 修订：解析 ctrl_interface → DIR 目录 → abstract → 常见路径，`STATUS` 往返定活跃，conf 目录只解析不连接）** + 连上 wpa_supplicant + 读 STATUS
- [ ] 配置文件读写（config.toml）
- [ ] Web 面板 `/api/status` 返回原始数据
- [ ] **实机验收清单**（Phase 1 必做，否则 Phase 2 返工）：
  - [ ] attach 后到底收到哪些 `CTRL-EVENT-*`（列全清单，验证 SIGNAL-CHANGE 触发频率）
  - [ ] 三源数据可用性：`GET_STATION` 哪些属性被填、`/proc/net/wireless` 是否存在、`SIGNAL_POLL` 是否返回
  - [ ] `ROAM <对侧 bssid>` 同 SSID 切换行为（成功/失败/断流时长）
  - [ ] `SELECT_NETWORK <id>` 异 SSID 切换行为
  - [ ] 电源感知入口验证：sysfs 节点存在性、input event 可读性、Doze 下 daemon 是否被冻结
  - [ ] 目标 BSSID 不在 scan cache 时 ROAM 行为（是否需要先 SCAN freq=）
  - [ ] **本机 wpa ctrl socket 实路径**确认（遍历候选目录命中哪个）
  - [ ] `dmesg/logcat -b all | grep avc` 抓 SELinux denied，对照 §7.3 规则补缺
  - [ ] 息屏 5min 后 daemon 是否仍在 / 是否被 cgroup/App Standby 冻结（logcat 时间戳验证）
  - [ ] `LIST_NETWORKS` 可解析出全部 network id + `SET_NETWORK <id> bssid` 可写可清（H9 收尾）
  - [ ] 手发 `SCAN` 后观察系统 WiFi 是否异常重连（验证与 Framework 抢状态风险）
- [ ] **里程碑**：装模块，浏览器 127.0.0.1:8080 看到当前 WiFi 状态 + 实机验收报告归档（**Phase 1 用最小 wpa_ctrl 发命令 + 打日志，不实现自动切换**）
- [ ] **Phase 1 退出准则（书面化，任一失败则缩 scope 而非硬上自动切换）**：
  - [ ] ① wpa ctrl socket 可 attach + `STATUS` 往返成功（全项目 go/no-go）
  - [ ] ② `ROAM <bssid>` 同 SSID 切换实机行为明确（成功/失败/断流时长）
  - [ ] ③ 健康度数据源至少一条非瞎猜可用（GET_STATION 有属性被填，或 /proc/net/wireless 可解析，或 SIGNAL_POLL 返回）
  - [ ] ④ 若 ①②③ 任一失败：收缩 Phase 2 范围（如仅同 SSID 切换、或仅 RSSI 决策、或仅 Web 观测不自动切），重新评审后继续
  - [ ] ⑤ 电源感知入口方案选定（六轮 Oracle 要求写入退出准则）：sysfs 屏状态节点可用 或 input event 可读，5min 心跳 timer 兜底（表 D 方案经 PoC 验证）
- [ ] **KernelSU/APatch 差异一页备注（S11 修订）**：Phase 1 末记录两平台 sepolicy 注入机制差异（kernel 内置规则 vs magiskpolicy），避免 Phase 5 才爆炸

### Phase 2 — 核心切换（核心价值）
**目标**：自动切换生效，健康度决策，统一 ROAM/SELECT_NETWORK 路径。

- [ ] health_score.rs：RSSI + Retry 综合健康度 + 防抖（合并模块）
- [ ] band_bond.rs：双频羁绊自动配对 + 跨 SSID 切换命令分支
- [ ] scanner.rs：L1 靶向信道扫描 + L2 事件驱动（或低频 SIGNAL_POLL 兜底）
- [ ] state_machine.rs：切换状态机 + 惩罚锁（合并模块）+ 边界事件枚举
- [ ] power_state.rs：按 Phase 1 PoC 选定的入口实现冻结/恢复
- [ ] **里程碑**：5G 衰减/干扰自动切 2.4G，老路由器不被踢下线，息屏耗电达标（H12 可测指标：无 WakeLock 滥用 + 30min 基线对比 + 无高频唤醒日志）

### Phase 2.5 — RTT 加入（可选）
- [ ] 验证 RTT 测量方式（TCP connect / HTTP HEAD）稳定性
- [ ] 若稳定则加 20% 权重，权重归一；若不稳定则保持纯 RSSI+Retry

### Phase 3 — Web 面板 + 实用功能
**目标**：可视化 + 体验完善。

- [ ] 信号/健康度双曲线（Chart.js inline 内嵌）
- [ ] 配置参数微调界面
- [ ] 手动切网按钮
- [ ] 历史数据持久化
- [ ] captive_portal.rs：切网后并行检测（**通知出口先定：cmd notification / Web 提示 / 最小 APK，与 QS Tile 的 APK-or-not 合并为单点决策，Phase 3 开工前定**）
- [ ] qs_tile：定方案并实现（最小 APK / 砍掉 / 系统设置项）
- [ ] **里程碑**：浏览器看曲线 + 改参数 + QS 切模式（若实现）

### Phase 4 — 预测性切换
**目标**：走到死角前就切，不等掉线。

- [ ] spatial_memory.rs：后台采样 BSSID 射频指纹
- [ ] 位置聚类算法
- [ ] 预测匹配 + 提前切换
- [ ] 指纹数据 Web 可视化
- [ ] **里程碑**：走到走廊预切 2.4G，不卡

### Phase 5 — 打磨发布
**目标**：可发布 Magisk 模块 zip。

- [ ] 交叉编译 CI（GitHub Actions + cargo-ndk）
- [ ] README + 安装说明
- [ ] 多机型测试矩阵（Android 13/14/15，Qualcomm/MediaTek，验证 ROAM/SIGNAL_POLL/事件列表厂商差异）
- [ ] KernelSU/APatch 兼容（**注意**：sepolicy 机制与 Magisk 不同，需准备两套策略文件，不是简单"测一下"）
- [ ] **里程碑**：可发布 Magisk 模块 zip

### Phase 6 — 实验性（YAGNI，验证后再说）
- [ ] eBPF 内核态监控（需内核 CONFIG_BPF，多数设备不支持，先验证）
- [ ] 双 WLAN 并发 Dual STA（需 SoC+驱动+HAL 支持，先验证）
- [ ] 6GHz 支持（Android 15 + 设备稀少）
- [ ] WoWLAN 硬件唤醒拦截

---

## 七、风险点与待确认项

### 7.1 已知风险

1. **SELinux + Magisk 启动时序竞态（最大坑）**：Android 14/15 vendor 分离机制极严。`service.sh` 运行在 `late_start` 非阻塞阶段，与 Zygote 并行——若在此处才 `magiskpolicy --live` 注入权限，daemon 启动时 SELinux 规则可能尚未加载完，被 auditd denied 击杀。**正解**：`post-fs-data.sh`（阻塞阶段，Zygote 前）注入 SELinux 权限，`service.sh` 只负责启动 daemon。静态 `sepolicy.rule` 打底 + `post-fs-data.sh` 里 `magiskpolicy --live` 动态补充，两层结合。具体 allow 规则见 §7.3。

2. **Doze Mode 事件黑洞**：息屏进 Deep Doze 后，Vendor HAL 物理切断 WiFi 后台扫描，内核挂起。Daemon 定时器轮询会唤醒 CPU 阻止深度休眠，待机耗电尿崩。**正解**：电源感知入口**不用** `CTRL-EVENT-SLEEP`/`WAKEUP`（**这两个事件不存在**，Framework 向 wpa 发的是 SUSPEND/RESUME 控制命令，不广播给 attach 客户端），改用 sysfs 屏状态节点 + 长周期 timer 降频（5min 心跳）+ 亮屏由首次事件唤醒。Phase 1 PoC 验证入口稳定性。

3. **wpa_supplicant 套接字路径（厂商盲盒）**：不要迷信 `/dev/socket/wpa_wlan0`。很多厂商藏在 `/data/misc/wifi/sockets/` 或 abstract namespace（`@wpa_wlan0`）。**探测顺序（H7 修订）**：解析 `ctrl_interface` 配置 → `DIR=` 目录 → abstract → 常见路径；**`STATUS` 往返成功是唯一"活跃"判定**；`/data/misc/wifi/wpa_supplicant` 是 conf/目录语义，只解析不连接。客户端 socket 也要落在正确目录并有 SELinux allow。

4. **扫描节流**：Android 9+ 框架级限制，息屏后高频扫描会被 vendor HAL 拦截。wpa_supplicant 直连可绕过 App 框架节流，但 **vendor HAL/固件侧仍可能限频**——不能假设 L1 15-20s（或配置值）靶向扫描一定成功，Phase 1 实机验证。

5. **ROAM 依赖与跨 SSID**：非 802.11r 环境仍用 `ROAM <bssid>`，wpa_supplicant 内部走完整 4-way handshake（1-2s 断流但状态机同步，不会被踢断）。不做 netlink 建连。**目标 BSSID 必须已在 wpa scan cache**，否则先 `SCAN freq=`。**异 SSID 切换**（Home_5G/Home_2G）需 `SELECT_NETWORK <id>`，不能用 ROAM。

6. **Magisk + Zygisk 在 Android 15 兼容性**：需最新 Magisk 或 Zygisk Next。

7. **Health Score 数据源降级链**：nl80211 `GET_STATION` → `/proc/net/wireless` → 纯 RSSI（SIGNAL_POLL）。FullMAC 设备（多数 Android 手机）属性填充极不一致，Qualcomm/MTK 常缺 `TX_RETRIES`。`tx_retries` 是累计值，健康度需 `Δretries/Δpackets` 时间窗比值。**门控必须用 tx_delta（H6 修订）**——比值是 tx 统计，用 rx_delta 门控会方向不一致；`tx_delta == 0` 时显式走中性分。**如实承认产品局限**：`tx_retries` 对纯下行劣化不敏感（下行卡顿时 tx 少），该场景健康度由 RSSI 主导。三个数据源都可能部分不可靠，逐级降级取可用值，不一刀切。

8. **Beacon 能力解析（弱化）**：既然统一 ROAM，wpa_supplicant 内部自动按 AP 能力选 FT 或 4-way，**客户端不需要解析 11r/k/v 能力来决策**。原 PLAN 立 beacon_parser 独立模块 + 用 `BSS_MEMBERSHIP_SELECTOR` 字段是错的（该字段与 HE/EHT 相关，非 11r FT 能力）。降级为日志记录 `SCAN_RESULTS` flags 列的 `[FT]` 标记即可。

9. **空间记忆隐私**：指纹数据仅本地，不上传，但需在 README 明确说明。**（H6 修订）** Android 12+ 每网随机 MAC 随机的是 **STA 本机 MAC**，扫描到的 **AP BSSID 仍相对稳定**——"随机 MAC 使 BSSID 指纹稳定性下降"的说法不准确。指纹主风险是扫描节流（BSSID 列表不全）与 AP 变化（mesh 多 BSSID/AP 开关），Phase 4 按多 AP 聚类 + 扫描完整性检查处理。

10. **浏览器 Private Network Access 限制**：Android 14+ Chrome 对从公网页面 fetch 到 127.0.0.1 的请求有 Private Network Access 限制。但**直接在地址栏输入 127.0.0.1:8080 打开面板是同源访问，不触发 CORS**。若用户从其他网页跳转访问遇到限制，用 `chrome://flags` 关 `Block insecure private network requests`。**不要**在 HTML 加 `upgrade-insecure-requests` CSP meta——这会强制升级 HTTPS，tiny_http 不支持 HTTPS 反而打不开。

11. **Bionic libc 符号兼容**：`cargo-ndk` 选 `ANDROID_PLATFORM=android-24` 作为最小 API level。Bionic ABI 自 API 21 起向后兼容，动态链接 libc.so，不引用 Android 13+ 新符号，二进制在 Android 7.0+ 无差别运行。**注意**：Android 上静态 link libc 基本不现实，文档不应出现"静态编译守护进程"措辞。

12. **厂商 wpa 裁剪**：部分 OEM 关掉 ROAM/SIGNAL_POLL/部分事件 → Phase 1 必须多机矩阵，验证目标机支持哪些命令。

13. **Framework 与 daemon 共用 wpa socket**：Android Framework 也在用同一 socket 体系（ConnectivityService）。乱发 SCAN/ROAM 可能与 Framework 抢状态。**礼貌策略（S9）**：发命令前看 `wpa_state`（避开 connecting/completed 瞬间）、SCAN 最小间隔（如 ≥5s）、ROAM 只在状态机 TRIGGER_SWITCH 且非 Frozen 时发、避开 Framework 正在重连的窗口。

14. **neli crate + Android netlink SELinux**：nl80211 走 **NETLINK_GENERIC**，sepolicy 需 `netlink_generic_socket` 类（H8 修订），仅 `netlink_socket` 可能类型名不匹配。**Phase 1 用 avc denied + 成功 `GET_STATION` 双证据定类型与 perm**；降级链加"SELinux/驱动双失败"日志计数，避免假性"算法在工作"。

15. **Doze 下 daemon 被冻结**：即使不持 WakeLock，App Standby 可能冻非系统进程。Magisk daemon 通常豁免，但 **Phase 1 实机验证**。

16. **tiny_http 无 WebSocket**：原 PLAN API 表写 `/api/ws` 不可行，删。前端用 1s 轮询 `/api/status` 兜底。

17. **Chart.js CDN 依赖**：离线/captive 场景 CDN 不可用，Chart.js 必须 inline 嵌入 HTML。

18. **切换后 DHCP/无网**：ROAM 成功但 L3 未好。切换成功判据不只是命令返回 OK，要等 `CTRL-EVENT-CONNECTED` + 可选网关可达。`CTRL-EVENT-SUBNET-STATUS-UPDATE` 可作辅助。

19. **双频 band steering 冲突**：路由器强制 5G 时客户端 ROAM 到 2.4 可能被踢回。Phase 5 多机测试验证。

20. **MAC 随机化（H13 修订）**：Android 12+ 每网随机 MAC 随机的是 **STA 本机 MAC**，不影响扫描到的 **AP BSSID**。captive_portal 的网关 MAC 比对会受影响（主要靠 HTTP 检测）；spatial_memory 指纹主风险是扫描节流与 AP 变化，非 MAC 随机。

21. **KernelSU/APatch sepolicy 差异**：与 Magisk 机制不同，Phase 5 需准备两套策略文件，不是简单兼容测试。

### 7.2 已确认决策
- [x] **包体目标（六轮 Oracle 修订）**：daemon 逻辑 release strip 后目标 ≤800KB（Phase 1 编译后填实测锁定）；含 web 资源（Chart.js inline ~200KB）总包 ≤1.2MB；debug 3-5MB；不用 no_std
- [x] **配置格式**：TOML（人读）+ API 层转 JSON（机读）
- [x] **Web 访问**：仅 127.0.0.1 本机，需远程用 SSH 隧道
- [x] **息屏行为**：Doze/Suspend 时冻结状态机（停所有计时器，不持 WakeLock，不对抗系统休眠），电源感知入口用 sysfs 屏状态 + 长周期 timer（5min 心跳），亮屏由首次事件唤醒后 500ms-1s 内完成 L1 靶向扫描+健康度确认。**不用** `CTRL-EVENT-SLEEP`/`WAKEUP`（不存在）。Phase 1 PoC 验证入口。
- [x] **多 WiFi**：Phase 1 只 wlan0，配置项指定 interface 名，架构预留
- [x] **KernelSU/APatch**：Phase 1 只 Magisk，Phase 5 加兼容（需两套 sepolicy 策略文件）
- [x] **切换命令**：同 SSID `ROAM <bssid>`，异 SSID `SELECT_NETWORK <id>`。不做 netlink 建连，不伪造重连
- [x] **模块拆分**：MVP 7 核心模块 + web（wpa_ctrl/band_bond/health_score/scanner/state_machine/power_state/config + web/），anti_jitter 并入 health_score，penalty 并入 state_machine，beacon_parser 降级为日志。band_bond 独立保留——切换命令分支是主路径，不并入 state_machine

### 7.3 SELinux allow 规则草稿

**post-fs-data.sh 里 `magiskpolicy --live` 注入**（**H1/H8 修订：magiskpolicy/sepolicy.rule 不展开 AOSP `.te` 宏**，必须写具体 perm 集合；类型名以目标机 `ls -Z`/avc denied 实测为准，以下为起点草稿，**不可当成品粘贴**）：

```
# daemon 域（假设 magisk 域继承）
# wpa 控制 socket：sock_file 读写 + 目录 search（类型名以实机为准：wpa_socket / wifi_data_file 等）
allow magisk wpa_socket:sock_file { read write getattr open };
allow magisk wpa_socket:dir { search getattr };
allow magisk wifi_data_file:dir { search getattr };
allow magisk wifi_data_file:sock_file { create read write getattr open unlink };
# netlink（nl80211 走 NETLINK_GENERIC）
allow magisk netlink_generic_socket:socket { create bind read write getattr setsockopt };
allow magisk netlink_socket:socket { create bind read write getattr setsockopt };
# HTTP 监听（127.0.0.1:8080）
allow magisk port_t:tcp_socket { name_bind read write getattr setopt };
allow magisk node_t:tcp_socket { node_bind read write };
# sysfs 屏状态（类型名以实机为准：sysfs_leds / sysfs / sysfs_drv 等）
allow magisk sysfs_leds:dir { search getattr };
allow magisk sysfs_leds:file { read getattr open };
# input event（电源键，可选）
allow magisk input_device:dir { search getattr };
allow magisk input_device:chr_file { read getattr open };
# /proc/net/wireless
allow magisk proc_net:file { read getattr open };
```

**静态 sepolicy.rule 打底**（同名规则）。

> **Phase 1 实机用 `dmesg | grep avc` 或 `logcat -b all | grep avc` 抓 denied，按缺什么补什么原则完善**。

---

## 八、不做的事（YAGNI）

- 不做 AP 端漫游协议实现（那是路由器的事）
- 不做 WiFi 密码管理（wpa_supplicant 已管）
- 不做流量统计/限速（与切换无关）
- 不做 GUI App（Web 面板 + QS Tile 足够，避免重复造轮子）
- 不做 eBPF（Phase 6 实验性，内核支持率低）
- 不做双 WLAN 并发 Dual STA（Phase 6 实验性，设备支持率低）
- 不做 6GHz（Phase 6 实验性，设备稀少）
- 不做云同步/远程控制（安全风险，YAGNI）
- 不做 beacon_parser 独立模块（统一 ROAM 后 wpa 自动按能力选路径，客户端不解析）
- 不做 `/api/ws` WebSocket（tiny_http 不支持，前端轮询够用）
- 不做 RTT Phase 2（Phase 2.5 验证稳定性后再加）
- 不做"伪造重连 200ms"（已否决路径，统一走 wpa_supplicant）
- 不做静态编译 libc（Android 不现实，动态链接 Bionic）
