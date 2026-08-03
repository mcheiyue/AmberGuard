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
    #[serde(default)]
    pub bonds: Vec<crate::band_bond::SsidBond>,
    /// 家网 AP 列表（BSSID 主键）。非空时自动切换仅在组内进行。
    #[serde(default)]
    pub home_aps: Vec<crate::band_bond::HomeAp>,
    /// daily=自动切换 / pause=仅观测
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

impl Default for Config {
    fn default() -> Self {
        Self {
            interface: default_interface(),
            listen: default_listen(),
            upswitch_rssi_min_dbm: default_upswitch_rssi(),
            score_detect_threshold: default_score_detect(),
            score_switch_threshold: default_score_switch(),
            wpa_ctrl_path: None,
            bonds: Vec::new(),
            home_aps: Vec::new(),
            mode: default_mode(),
            log_level: default_log_level(),
            user_hold_secs: default_user_hold_secs(),
            weak_action: default_weak_action(),
            rssi_disconnect_dbm: default_rssi_disconnect(),
            weak_hold_secs: default_weak_hold(),
            auto_reconnect: default_auto_reconnect(),
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
    pub bonds: Option<Vec<crate::band_bond::SsidBond>>,
    pub home_aps: Option<Vec<crate::band_bond::HomeAp>>,
    pub interface: Option<String>,
    pub user_hold_secs: Option<u64>,
    pub weak_action: Option<String>,
    pub rssi_disconnect_dbm: Option<i32>,
    pub weak_hold_secs: Option<u64>,
    pub auto_reconnect: Option<bool>,
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
            FieldMeta {
                key: "score_detect_threshold",
                label: "梯度检测阈值",
                unit: "健康度 0–100",
                default: 70.0,
                min: 40.0,
                max: 95.0,
                step: 1.0,
                meaning: "健康度跌破此值时，进入「观察/更积极扫描」阶段，但还不会切网。",
                guide: "调高=更早开始留意信号变差（偏敏感）；调低=更迟才进入观察（更稳、少折腾）。日用建议 65–75，默认 70。",
            },
            FieldMeta {
                key: "score_switch_threshold",
                label: "下切触发阈值",
                unit: "健康度 0–100",
                default: 30.0,
                min: 10.0,
                max: 80.0,
                step: 1.0,
                meaning: "健康度持续低于此值，且通过防抖确认后，才会从首选频段（通常 5G）切到更稳的 2.4G。",
                guide: "调高=更早下切 2.4G（卡顿少、切网多）；调低=更能扛信号差（切网少、可能先卡一阵）。必须小于「梯度检测阈值」。日用建议 25–40，默认 30。",
            },
            FieldMeta {
                key: "upswitch_rssi_min_dbm",
                label: "上切对侧 RSSI 下限",
                unit: "dBm（负数）",
                default: -65.0,
                min: -85.0,
                max: -40.0,
                step: 1.0,
                meaning: "当前在 2.4G 时，扫描到的 5G 对侧信号至少要达到此强度才允许切回 5G。RSSI 是接收信号强度，越接近 0 越好（如 -40 优于 -70）。",
                guide: "调高（如 -55）= 要求 5G 更强才切回（更稳、更久停在 2.4G）；调低（如 -75）= 5G 稍弱也切回（更爱 5G）。日用建议 -70～-60，默认 -65。",
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
            PresetMeta {
                id: "daily",
                label: "日用（默认）",
                desc: "视频/网页：下切偏稳、上切更稳。推荐起步。",
                score_detect_threshold: 70.0,
                score_switch_threshold: 30.0,
                upswitch_rssi_min_dbm: -65,
            },
            PresetMeta {
                id: "stable",
                label: "更稳（少切网）",
                desc: "能扛再切：检测与下切都更靠后，适合嫌切换打断的人。",
                score_detect_threshold: 60.0,
                score_switch_threshold: 22.0,
                upswitch_rssi_min_dbm: -58,
            },
            PresetMeta {
                id: "sensitive",
                label: "更敏（早切 2.4G）",
                desc: "信号一差就准备下切，卡顿少、切网可能略多。",
                score_detect_threshold: 78.0,
                score_switch_threshold: 42.0,
                upswitch_rssi_min_dbm: -70,
            },
        ]
    }

    pub fn tips() -> Vec<&'static str> {
        vec![
            "健康度：综合 RSSI 与重传率的 0–100 分，越高越好。不是单纯信号格。",
            "下切：5G→2.4G，优先保流畅；上切：2.4G→5G，要等对侧够强且防抖通过。",
            "异名双频（如 XXX_5G 与 XXX）须在系统 WiFi 里分别连接并保存，否则无法 SELECT 切网。",
            "改阈值后立即生效并写入 /data/adb/amberguard/config.toml；可用「恢复默认」一键还原。",
            "防抖时间固定为日用策略（下切约 4s、上切约 7s），本页不开放，避免误调导致来回跳。",
            "手动切网保护：系统里换 WiFi 后会暂停自动切换一段时间；状态页可「立即恢复」。设为 0 即关闭。",
            "家网：在设置里扫描并勾选属于你的 AP（按 BSSID）。配置后只在家网内双频切换，避免公共 WiFi / 错 AP。",
        ]
    }

    pub fn apply_patch(&mut self, p: ConfigPatch) -> Result<(), ConfigError> {
        if let Some(v) = p.score_detect_threshold {
            self.score_detect_threshold = v.clamp(40.0, 95.0);
        }
        if let Some(v) = p.score_switch_threshold {
            self.score_switch_threshold = v.clamp(10.0, 80.0);
        }
        if let Some(v) = p.upswitch_rssi_min_dbm {
            self.upswitch_rssi_min_dbm = v.clamp(-85, -40);
        }
        if let Some(m) = p.mode {
            let m = m.to_lowercase();
            if m != "daily" && m != "pause" {
                return Err(ConfigError::Validate(
                    "mode 只能是 daily 或 pause".into(),
                ));
            }
            self.mode = m;
        }
        if let Some(lv) = p.log_level {
            let lv = lv.to_ascii_lowercase();
            match lv.as_str() {
                "error" | "warn" | "info" | "debug" => self.log_level = lv,
                _ => {
                    return Err(ConfigError::Validate(
                        "log_level 只能是 error/warn/info/debug".into(),
                    ))
                }
            }
        }
        if let Some(b) = p.bonds {
            self.bonds = b;
        }
        if let Some(homes) = p.home_aps {
            self.home_aps = normalize_home_aps(homes);
        }
        if let Some(iface) = p.interface {
            if !iface.is_empty() {
                self.interface = iface;
            }
        }
        if let Some(h) = p.user_hold_secs {
            self.user_hold_secs = h.min(300);
        }
        if let Some(a) = p.weak_action {
            let a = a.to_ascii_lowercase();
            if a != "off" && a != "disconnect" {
                return Err(ConfigError::Validate(
                    "weak_action 只能是 off 或 disconnect".into(),
                ));
            }
            self.weak_action = a;
        }
        if let Some(v) = p.rssi_disconnect_dbm {
            self.rssi_disconnect_dbm = v.clamp(-95, -70);
        }
        if let Some(v) = p.weak_hold_secs {
            self.weak_hold_secs = v.clamp(5, 60);
        }
        if let Some(v) = p.auto_reconnect {
            self.auto_reconnect = v;
        }
        self.validate()
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
        self.bonds = cfg.bonds;
        self.home_aps = normalize_home_aps(cfg.home_aps);
        self.mode = cfg.mode;
        self.log_level = cfg.log_level;
        self.user_hold_secs = cfg.user_hold_secs;
        self.weak_action = cfg.weak_action;
        self.rssi_disconnect_dbm = cfg.rssi_disconnect_dbm;
        self.weak_hold_secs = cfg.weak_hold_secs;
        self.auto_reconnect = cfg.auto_reconnect;
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
