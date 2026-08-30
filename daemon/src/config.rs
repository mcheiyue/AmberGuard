//! 配置读写：/data/adb/amberguard/config.toml
//! Phase 3：Web 可调阈值 + 标准化默认与字段元数据

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const ANDROID_CONFIG_PATH: &str = "/data/adb/amberguard/config.toml";
const DEV_CONFIG_PATH: &str = "amberguard_config.toml";
const MAX_HOME_APS: usize = 16;

fn normalize_home_aps(list: Vec<crate::band_bond::HomeAp>) -> Vec<crate::band_bond::HomeAp> {
    use crate::band_bond::HomeAp;
    let mut out: Vec<HomeAp> = Vec::new();
    for mut h in list {
        h.bssid = h.bssid.trim().to_lowercase();
        if h.bssid.len() < 11 {
            continue;
        }
        if out.iter().any(|x| x.bssid == h.bssid) {
            continue;
        }
        if h.band.is_empty() {
            h.band = "auto".into();
        }
        out.push(h);
        if out.len() >= MAX_HOME_APS {
            break;
        }
    }
    out
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML 解析: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("TOML 序列化: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("校验: {0}")]
    Validate(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_interface")]
    pub interface: String,
    #[serde(default = "default_listen")]
    pub listen: String,
    /// 上切对侧 RSSI 下限（dBm）。数值越大=要求对侧信号越强。默认 -65。
    #[serde(default = "default_upswitch_rssi")]
    pub upswitch_rssi_min_dbm: i32,
    /// 梯度检测阈值（健康度 0–100）。低于此值进入观察/扫描，尚未切换。默认 70。
    #[serde(default = "default_score_detect")]
    pub score_detect_threshold: f32,
    /// 切换阈值（健康度 0–100）。低于此值且防抖通过后执行下切。须小于检测阈值。默认 30。
    #[serde(default = "default_score_switch")]
    pub score_switch_threshold: f32,
    #[serde(default)]
    pub wpa_ctrl_path: Option<String>,
    /// 家网 AP 列表（BSSID 主键）。非空时自动切换仅在组内进行。
    #[serde(default)]
    pub home_aps: Vec<crate::band_bond::HomeAp>,
    /// 家网模式：当前不在家网（异地）时是否允许弱信号救援/断开。默认 false（异地不做任何自动处理，保安全）。
    #[serde(default)]
    pub allow_weak_off_home: bool,
    /// daily=自动切换 / eco=省电拉长防抖 / pause=仅观测
    #[serde(default = "default_mode")]
    pub mode: String,
    /// 日志级别：error / warn / info / debug
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// 检测到用户手动切网后，暂停自动切换的秒数。0=关闭保护。默认 60。
    #[serde(default = "default_user_hold_secs")]
    pub user_hold_secs: u64,
    /// 弱信号动作：off（默认）| disconnect
    #[serde(default = "default_weak_action")]
    pub weak_action: String,
    /// 弱信号断开阈值（dBm）。仅 weak_action=disconnect 时生效。
    #[serde(default = "default_rssi_disconnect")]
    pub rssi_disconnect_dbm: i32,
    /// 连续低于阈值多少秒才断网。
    #[serde(default = "default_weak_hold")]
    pub weak_hold_secs: u64,
    /// 断后是否自动重连。
    #[serde(default = "default_auto_reconnect")]
    pub auto_reconnect: bool,
    /// 切后 L3 探测（generate_204）。默认开。
    #[serde(default = "default_l3_probe")]
    pub l3_probe_enable: bool,
    /// 偏好频段：`"5"`（默认上 5G）或 `"2.4"`（偏好 2.4，场景少见）
    #[serde(default = "default_preferred_band")]
    pub preferred_band: String,
    /// 切成功后短锁当前 AP 秒数（防 band-steering 踢回）。0=关闭。默认 45。
    #[serde(default = "default_bssid_lock_secs")]
    pub bssid_lock_secs: u64,
    /// 偏好频段内追优漫游（同频更好 AP）。默认开。
    #[serde(default = "default_roam_enable")]
    pub roam_enable: bool,
    /// 对端须比当前强多少 dB 才追优。默认 12。
    #[serde(default = "default_roam_margin")]
    pub roam_margin_db: i32,
    /// 更好 AP 需持续可见秒数。默认 6。
    #[serde(default = "default_roam_hold")]
    pub roam_hold_secs: u64,
    /// 通知总开关。false 时全部不发。默认 true。
    #[serde(default = "default_notify_enable")]
    pub notify_enable: bool,
    /// 切换成功/失败时发事件通知。默认 true。
    #[serde(default = "default_notify_switch")]
    pub notify_switch: bool,
    /// 弱信号断开时发通知。默认 false。
    #[serde(default = "default_notify_weak")]
    pub notify_weak: bool,
    /// 常驻状态条更新间隔秒。0=关。默认 0。
    #[serde(default)]
    pub notify_ongoing_secs: u64,
    /// 自动大流量保护总开关（默认 true）
    #[serde(default = "default_soft_auto_enable")]
    pub soft_auto_enable: bool,
    /// 触发保护的流量阈值（KB/s，默认 400，范围 100~5000）
    #[serde(default = "default_soft_auto_on_kb")]
    pub soft_auto_on_kb: u64,
    /// 解除保护的流量阈值（KB/s，默认 80，范围 20~1000，须 < on_kb）
    #[serde(default = "default_soft_auto_off_kb")]
    pub soft_auto_off_kb: u64,
    /// 持续高于触发线多少秒才进入保护（默认 12，范围 3~60）
    #[serde(default = "default_soft_auto_trigger_secs")]
    pub soft_auto_trigger_secs: u64,
    /// 持续低于解除线多少秒才退出保护（默认 45，范围 10~180）
    #[serde(default = "default_soft_auto_release_secs")]
    pub soft_auto_release_secs: u64,
    /// 连续自动保护最长分钟数上限（默认 240，0=不限）
    #[serde(default = "default_soft_auto_max_mins")]
    pub soft_auto_max_mins: u64,
    /// BSSID 质量记忆降权开关（默认 true）
    #[serde(default = "default_bssid_memory_enable")]
    pub bssid_memory_enable: bool,
    /// 故障 AP 降权冷却时长（秒，默认 1800=30分钟，范围 300~7200）
    #[serde(default = "default_bssid_demote_secs")]
    pub bssid_demote_secs: u64,
}

fn default_interface() -> String {
    "wlan0".into()
}
fn default_listen() -> String {
    "127.0.0.1:8080".into()
}
fn default_upswitch_rssi() -> i32 {
    -65
}
fn default_score_detect() -> f32 {
    70.0
}
fn default_score_switch() -> f32 {
    30.0
}
fn default_mode() -> String {
    "daily".into()
}
fn default_log_level() -> String {
    "info".into()
}
fn default_user_hold_secs() -> u64 {
    60
}
fn default_weak_action() -> String {
    "off".into()
}
fn default_rssi_disconnect() -> i32 {
    -90
}
fn default_weak_hold() -> u64 {
    15
}
fn default_auto_reconnect() -> bool {
    true
}
fn default_l3_probe() -> bool {
    true
}
fn default_preferred_band() -> String {
    "5".into()
}
fn default_bssid_lock_secs() -> u64 {
    45
}
fn default_roam_enable() -> bool {
    true
}
fn default_roam_margin() -> i32 {
    12
}
fn default_roam_hold() -> u64 {
    6
}
fn default_notify_enable() -> bool {
    true
}
fn default_notify_switch() -> bool {
    true
}
fn default_notify_weak() -> bool {
    false
}

fn default_soft_auto_enable() -> bool {
    true
}
fn default_soft_auto_on_kb() -> u64 {
    400
}
fn default_soft_auto_off_kb() -> u64 {
    80
}
fn default_soft_auto_trigger_secs() -> u64 {
    12
}
fn default_soft_auto_release_secs() -> u64 {
    45
}
fn default_soft_auto_max_mins() -> u64 {
    240
}
fn default_bssid_memory_enable() -> bool {
    true
}
fn default_bssid_demote_secs() -> u64 {
    1800
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interface: default_interface(),
            listen: default_listen(),
            upswitch_rssi_min_dbm: default_upswitch_rssi(),
            score_detect_threshold: default_score_detect(),
            score_switch_threshold: default_score_switch(),
            wpa_ctrl_path: None,
            home_aps: Vec::new(),
            allow_weak_off_home: false,
            mode: default_mode(),
            log_level: default_log_level(),
            user_hold_secs: default_user_hold_secs(),
            weak_action: default_weak_action(),
            rssi_disconnect_dbm: default_rssi_disconnect(),
            weak_hold_secs: default_weak_hold(),
            auto_reconnect: default_auto_reconnect(),
            l3_probe_enable: default_l3_probe(),
            preferred_band: default_preferred_band(),
            bssid_lock_secs: default_bssid_lock_secs(),
            roam_enable: default_roam_enable(),
            roam_margin_db: default_roam_margin(),
            roam_hold_secs: default_roam_hold(),
            notify_enable: default_notify_enable(),
            notify_switch: default_notify_switch(),
            notify_weak: default_notify_weak(),
            notify_ongoing_secs: 0,
            soft_auto_enable: default_soft_auto_enable(),
            soft_auto_on_kb: default_soft_auto_on_kb(),
            soft_auto_off_kb: default_soft_auto_off_kb(),
            soft_auto_trigger_secs: default_soft_auto_trigger_secs(),
            soft_auto_release_secs: default_soft_auto_release_secs(),
            soft_auto_max_mins: default_soft_auto_max_mins(),
            bssid_memory_enable: default_bssid_memory_enable(),
            bssid_demote_secs: default_bssid_demote_secs(),
        }
    }
}

/// Web 部分更新（只改传入字段）
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ConfigPatch {
    pub score_detect_threshold: Option<f32>,
    pub score_switch_threshold: Option<f32>,
    pub upswitch_rssi_min_dbm: Option<i32>,
    pub mode: Option<String>,
    pub log_level: Option<String>,
    pub home_aps: Option<Vec<crate::band_bond::HomeAp>>,
    pub interface: Option<String>,
    pub user_hold_secs: Option<u64>,
    pub weak_action: Option<String>,
    pub rssi_disconnect_dbm: Option<i32>,
    pub weak_hold_secs: Option<u64>,
    pub auto_reconnect: Option<bool>,
    pub l3_probe_enable: Option<bool>,
    pub preferred_band: Option<String>,
    pub bssid_lock_secs: Option<u64>,
    pub roam_enable: Option<bool>,
    pub roam_margin_db: Option<i32>,
    pub roam_hold_secs: Option<u64>,
    pub notify_enable: Option<bool>,
    pub notify_switch: Option<bool>,
    pub notify_weak: Option<bool>,
    pub notify_ongoing_secs: Option<u64>,
    pub allow_weak_off_home: Option<bool>,
    pub soft_auto_enable: Option<bool>,
    pub soft_auto_on_kb: Option<u64>,
    pub soft_auto_off_kb: Option<u64>,
    pub soft_auto_trigger_secs: Option<u64>,
    pub soft_auto_release_secs: Option<u64>,
    pub soft_auto_max_mins: Option<u64>,
    pub bssid_memory_enable: Option<bool>,
    pub bssid_demote_secs: Option<u64>,
}

/// 字段说明（给面板引导用）
#[derive(Debug, Serialize)]
pub struct FieldMeta {
    pub key: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    /// 一句话：是什么
    pub meaning: &'static str,
    /// 调大/调小会怎样 + 日用建议
    pub guide: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ConfigApiResponse {
    pub config: Config,
    pub defaults: Config,
    pub fields: Vec<FieldMeta>,
    pub presets: Vec<PresetMeta>,
    pub tips: Vec<&'static str>,
    /// 是否已有 config.toml（false=新用户，应显示引导）
    pub persisted: bool,
}

#[derive(Debug, Serialize)]
pub struct PresetMeta {
    pub id: &'static str,
    pub label: &'static str,
    pub desc: &'static str,
    pub score_detect_threshold: f32,
    pub score_switch_threshold: f32,
    pub upswitch_rssi_min_dbm: i32,
}

impl Config {
    pub fn field_meta() -> Vec<FieldMeta> {
        vec![
            // 优先级：① 下切健康分 ② 上切偏好侧 RSSI ③ 观察健康分（只观察/加扫，不单独触发切）
            FieldMeta {
                key: "score_switch_threshold",
                label: "① 下切线（健康分）",
                unit: "健康分 0–100",
                default: 30.0,
                min: 10.0,
                max: 80.0,
                step: 1.0,
                meaning: "【离开偏好频段】当前健康分持续低于此值 → 防抖后切到后备频段。健康分≈半信号+半重传。与「优先 5G/2.4」联动。",
                guide: "调高=更早离开偏好；调低=更扛。必须小于观察线。默认 30。看状态页阈值对照。",
            },
            FieldMeta {
                key: "upswitch_rssi_min_dbm",
                label: "② 上切线（偏好侧最低 RSSI）",
                unit: "dBm",
                default: -65.0,
                min: -85.0,
                max: -40.0,
                step: 1.0,
                meaning: "【回到偏好频段】在后备频段时，偏好侧家网 AP 的 RSSI 须 ≥ 此值才允许上切。只看对端场强。",
                guide: "调高=偏好要更强才回去；调低=更积极回偏好。状态页「偏好侧最强」对照此线。默认 -65。",
            },
            FieldMeta {
                key: "score_detect_threshold",
                label: "③ 观察线（健康分）",
                unit: "健康分 0–100",
                default: 70.0,
                min: 40.0,
                max: 95.0,
                step: 1.0,
                meaning: "【不直接切网】在偏好上：下切线≤分<观察线→观察/加扫；分≥观察线→稳定。在后备上：分≥观察线时上切防抖加长（约25s），减少误拉回。",
                guide: "调高=更早观察、后备上更久才拉回偏好。默认 70。",
            },
            FieldMeta {
                key: "user_hold_secs",
                label: "手动切网保护",
                unit: "秒",
                default: 60.0,
                min: 0.0,
                max: 300.0,
                step: 5.0,
                meaning: "检测到你在系统里手动换了 WiFi/AP 后，自动暂停切换这么久，避免守护立刻抢回。",
                guide: "0=关闭保护；60 适合日常；想多调一会儿再自动可 120–180。",
            },
            FieldMeta {
                key: "rssi_disconnect_dbm",
                label: "弱信号断开 RSSI",
                unit: "dBm（负数）",
                default: -90.0,
                min: -95.0,
                max: -70.0,
                step: 1.0,
                meaning: "仅在开启「弱信号断开」时生效：持续低于此值后，先尝试家网/启发式换更好 AP 或下切 2.4，仍不行才断 WiFi。",
                guide: "默认 -90。更接近 0（如 -80）= 更容易进入救援/断开；更低更难断。默认建议功能保持关闭。",
            },
            FieldMeta {
                key: "weak_hold_secs",
                label: "弱信号持续秒数",
                unit: "秒",
                default: 15.0,
                min: 5.0,
                max: 60.0,
                step: 1.0,
                meaning: "连续低于断开 RSSI 多少秒后进入「先切换再断开」流程。",
                guide: "15 秒给自动下切留时间；调高更不易误断。手切外网/暂停时不生效。",
            },
        ]
    }

    pub fn presets() -> Vec<PresetMeta> {
        vec![
            // 与运行 mode（日用/省电/暂停）正交：这里只改三个阈值数字
            PresetMeta {
                id: "daily",
                label: "均衡（推荐）",
                desc: "下切30 / 观察70 / 回偏好≥-65。与运行模式无关。",
                score_detect_threshold: 70.0,
                score_switch_threshold: 30.0,
                upswitch_rssi_min_dbm: -65,
            },
            PresetMeta {
                id: "stable",
                label: "更稳（少切）",
                desc: "下切更低、回偏好要求更强 → 少折腾。",
                score_detect_threshold: 60.0,
                score_switch_threshold: 22.0,
                upswitch_rssi_min_dbm: -58,
            },
            PresetMeta {
                id: "sensitive",
                label: "更敏（早切）",
                desc: "下切更高、回偏好更松 → 更早离开偏好、更爱回偏好。",
                score_detect_threshold: 78.0,
                score_switch_threshold: 42.0,
                upswitch_rssi_min_dbm: -70,
            },
        ]
    }

    pub fn tips() -> Vec<&'static str> {
        vec![
            "健康度：综合 RSSI 与重传率的 0–100 分，越高越好。不是单纯信号格。",
            "下切=离开偏好频段；上切=回到偏好（默认偏好 5G，可在设置改）。上切看对端 RSSI 门槛。",
            "异名双频须在系统分别连接并保存；模块用框架代连时需能读到已存密码（家用 WPA2/3）。",
            "改阈值后立即生效并写入 /data/adb/amberguard/config.toml；可用「恢复默认」一键还原。",
            "日用防抖约下切 4s / 上切 7s；省电(eco)更长，少切网少扫描。",
            "手切优先：系统里换网后自动暂停调度（默认60s）；模块只做自动调度，不与你抢。清保护后才会按偏好慢慢拉回。",
            "家网：在设置里扫描并勾选属于你的 AP（按 BSSID）。配置后只在家网内双频切换，避免公共 WiFi / 错 AP。",
            "切后会做一次 L3 探测；失败只标注（门户/超时），不撤销成功、不进冷却。可关 l3_probe_enable。",
            "同频追优：偏好频段上且分<观察线时，家网内另一 AP 比当前强≥12dB 并持续约6s → 只换同频（默认开，roam_enable）。",
        ]
    }

    pub fn apply_patch(&mut self, p: ConfigPatch) -> Result<(), ConfigError> {
        // 原子应用：先在副本上改+校验，成功才写回 self，避免被拒的 patch 污染内存中的配置
        let mut candidate = self.clone();
        if let Some(v) = p.score_detect_threshold {
            candidate.score_detect_threshold = v.clamp(40.0, 95.0);
        }
        if let Some(v) = p.score_switch_threshold {
            candidate.score_switch_threshold = v.clamp(10.0, 80.0);
        }
        if let Some(v) = p.upswitch_rssi_min_dbm {
            candidate.upswitch_rssi_min_dbm = v.clamp(-85, -40);
        }
        if let Some(m) = p.mode {
            let m = m.to_lowercase();
            if m != "daily" && m != "pause" && m != "eco" {
                return Err(ConfigError::Validate(
                    "mode 只能是 daily / eco / pause".into(),
                ));
            }
            candidate.mode = m;
        }
        if let Some(lv) = p.log_level {
            let lv = lv.to_ascii_lowercase();
            match lv.as_str() {
                "error" | "warn" | "info" | "debug" => candidate.log_level = lv,
                _ => {
                    return Err(ConfigError::Validate(
                        "log_level 只能是 error/warn/info/debug".into(),
                    ))
                }
            }
        }
        if let Some(homes) = p.home_aps {
            candidate.home_aps = normalize_home_aps(homes);
        }
        if let Some(iface) = p.interface {
            if !iface.is_empty() {
                candidate.interface = iface;
            }
        }
        if let Some(h) = p.user_hold_secs {
            candidate.user_hold_secs = h.min(300);
        }
        if let Some(a) = p.weak_action {
            let a = a.to_ascii_lowercase();
            if a != "off" && a != "disconnect" {
                return Err(ConfigError::Validate(
                    "weak_action 只能是 off 或 disconnect".into(),
                ));
            }
            candidate.weak_action = a;
        }
        if let Some(v) = p.rssi_disconnect_dbm {
            candidate.rssi_disconnect_dbm = v.clamp(-95, -70);
        }
        if let Some(v) = p.weak_hold_secs {
            candidate.weak_hold_secs = v.clamp(5, 60);
        }
        if let Some(v) = p.auto_reconnect {
            candidate.auto_reconnect = v;
        }
        if let Some(v) = p.l3_probe_enable {
            candidate.l3_probe_enable = v;
        }
        if let Some(b) = p.preferred_band {
            let b = b.trim().to_ascii_lowercase();
            let norm = match b.as_str() {
                "5" | "5g" | "5ghz" => "5",
                "2.4" | "24" | "2.4g" | "2g" => "2.4",
                _ => {
                    return Err(ConfigError::Validate(
                        "preferred_band 只能是 5 或 2.4".into(),
                    ));
                }
            };
            candidate.preferred_band = norm.into();
        }
        if let Some(s) = p.bssid_lock_secs {
            candidate.bssid_lock_secs = s.min(300);
        }
        if let Some(v) = p.roam_enable {
            candidate.roam_enable = v;
        }
        if let Some(m) = p.roam_margin_db {
            candidate.roam_margin_db = m.clamp(5, 25);
        }
        if let Some(h) = p.roam_hold_secs {
            candidate.roam_hold_secs = h.clamp(2, 30);
        }
        if let Some(v) = p.notify_enable {
            candidate.notify_enable = v;
        }
        if let Some(v) = p.notify_switch {
            candidate.notify_switch = v;
        }
        if let Some(v) = p.notify_weak {
            candidate.notify_weak = v;
        }
        if let Some(v) = p.notify_ongoing_secs {
            candidate.notify_ongoing_secs = v.min(300);
        }
        if let Some(v) = p.allow_weak_off_home {
            candidate.allow_weak_off_home = v;
        }
        if let Some(v) = p.soft_auto_enable {
            candidate.soft_auto_enable = v;
        }
        if let Some(v) = p.soft_auto_on_kb {
            candidate.soft_auto_on_kb = v.clamp(100, 5000);
        }
        if let Some(v) = p.soft_auto_off_kb {
            candidate.soft_auto_off_kb = v.clamp(20, 1000);
        }
        if let Some(v) = p.soft_auto_trigger_secs {
            candidate.soft_auto_trigger_secs = v.clamp(3, 60);
        }
        if let Some(v) = p.soft_auto_release_secs {
            candidate.soft_auto_release_secs = v.clamp(10, 180);
        }
        if let Some(v) = p.soft_auto_max_mins {
            candidate.soft_auto_max_mins = v.min(1440);
        }
        if let Some(v) = p.bssid_memory_enable {
            candidate.bssid_memory_enable = v;
        }
        if let Some(v) = p.bssid_demote_secs {
            candidate.bssid_demote_secs = v.clamp(300, 7200);
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn apply_preset(&mut self, id: &str) -> Result<(), ConfigError> {
        let Some(p) = Self::presets().into_iter().find(|x| x.id == id) else {
            return Err(ConfigError::Validate(format!("未知预设: {id}")));
        };
        self.score_detect_threshold = p.score_detect_threshold;
        self.score_switch_threshold = p.score_switch_threshold;
        self.upswitch_rssi_min_dbm = p.upswitch_rssi_min_dbm;
        self.validate()
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.score_switch_threshold >= self.score_detect_threshold {
            return Err(ConfigError::Validate(format!(
                "下切阈值({:.0}) 必须小于 梯度检测阈值({:.0})",
                self.score_switch_threshold, self.score_detect_threshold
            )));
        }
        let wa = self.weak_action.to_ascii_lowercase();
        if wa != "off" && wa != "disconnect" {
            return Err(ConfigError::Validate(
                "weak_action 只能是 off 或 disconnect".into(),
            ));
        }
        if self.soft_auto_on_kb <= self.soft_auto_off_kb {
            return Err(ConfigError::Validate(format!(
                "触发速率({} KB/s) 必须大于 解除速率({} KB/s)",
                self.soft_auto_on_kb, self.soft_auto_off_kb
            )));
        }
        Ok(())
    }

    pub fn api_response(cfg: &Config) -> ConfigApiResponse {
        ConfigApiResponse {
            config: cfg.clone(),
            defaults: Config::default(),
            fields: Self::field_meta(),
            presets: Self::presets(),
            tips: Self::tips(),
            persisted: Self::is_persisted(),
        }
    }

    pub fn resolve_path() -> PathBuf {
        let android = Path::new(ANDROID_CONFIG_PATH);
        if android.parent().is_some_and(|p| p.is_dir()) || android.exists() {
            return android.to_path_buf();
        }
        PathBuf::from(DEV_CONFIG_PATH)
    }

    /// 磁盘上是否已有配置文件（引导用；load 缺省时不落盘）
    pub fn is_persisted() -> bool {
        Self::resolve_path().exists()
    }

    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::resolve_path();
        if !path.exists() {
            // ponytail: 不自动写盘，便于 Web 新用户引导；点初始化/保存再落盘
            log::info!(
                "配置文件不存在 {}，使用内存默认（未落盘）",
                path.display()
            );
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)?;
        let cfg: Config = toml::from_str(&text)?;
        if let Err(e) = cfg.validate() {
            log::warn!("配置校验警告: {e}（将尽量运行）");
        }
        Ok(cfg)
    }

    /// 配置不存在时写入默认（Web 引导用）。已存在则 false。
    pub fn init_if_missing() -> Result<bool, ConfigError> {
        let path = Self::resolve_path();
        if path.exists() {
            return Ok(false);
        }
        let cfg = Self::default();
        cfg.save_to(&path)?;
        log::info!("已写入默认配置 {}", path.display());
        Ok(true)
    }

    /// 强制把当前内存配置落盘（初始化并开始 / 保存）
    pub fn persist(&self) -> Result<(), ConfigError> {
        self.save()
    }

    /// 从 config.toml 重新加载
    pub fn reload(&mut self) -> Result<(), ConfigError> {
        let path = Self::resolve_path();
        if !path.exists() {
            return Ok(());
        }
        let text = fs::read_to_string(&path)?;
        let cfg: Config = toml::from_str(&text)?;
        self.interface = cfg.interface;
        self.listen = cfg.listen;
        self.upswitch_rssi_min_dbm = cfg.upswitch_rssi_min_dbm;
        self.score_detect_threshold = cfg.score_detect_threshold;
        self.score_switch_threshold = cfg.score_switch_threshold;
        self.wpa_ctrl_path = cfg.wpa_ctrl_path;
        self.home_aps = normalize_home_aps(cfg.home_aps);
        self.preferred_band = if cfg.preferred_band == "2.4" {
            "2.4".into()
        } else {
            "5".into()
        };
        self.mode = cfg.mode;
        self.log_level = cfg.log_level;
        self.user_hold_secs = cfg.user_hold_secs;
        self.weak_action = cfg.weak_action;
        self.rssi_disconnect_dbm = cfg.rssi_disconnect_dbm;
        self.weak_hold_secs = cfg.weak_hold_secs;
        self.auto_reconnect = cfg.auto_reconnect;
        self.l3_probe_enable = cfg.l3_probe_enable;
        self.soft_auto_enable = cfg.soft_auto_enable;
        self.soft_auto_on_kb = cfg.soft_auto_on_kb;
        self.soft_auto_off_kb = cfg.soft_auto_off_kb;
        self.soft_auto_trigger_secs = cfg.soft_auto_trigger_secs;
        self.soft_auto_release_secs = cfg.soft_auto_release_secs;
        self.soft_auto_max_mins = cfg.soft_auto_max_mins;
        self.bssid_memory_enable = cfg.bssid_memory_enable;
        self.bssid_demote_secs = cfg.bssid_demote_secs;
        Ok(())
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(&Self::resolve_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        fs::write(path, text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigPatch};

    #[test]
    fn apply_patch_updates_notification_fields() {
        let mut config = Config::default();
        config
            .apply_patch(ConfigPatch {
                notify_enable: Some(false),
                notify_switch: Some(false),
                notify_weak: Some(true),
                notify_ongoing_secs: Some(45),
                ..ConfigPatch::default()
            })
            .expect("notification patch should validate");

        assert!(!config.notify_enable);
        assert!(!config.notify_switch);
        assert!(config.notify_weak);
        assert_eq!(config.notify_ongoing_secs, 45);
    }

    #[test]
    fn preset_descriptions_are_preference_neutral() {
        assert!(Config::presets()
            .iter()
            .all(|preset| !preset.desc.contains("5G")));
    }
}
