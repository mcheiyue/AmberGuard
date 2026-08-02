//! 配置读写：/data/adb/amberguard/config.toml
//! 宿主开发时回退到工作目录下的 amberguard_config.toml

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Android 正式路径
pub const ANDROID_CONFIG_PATH: &str = "/data/adb/amberguard/config.toml";
/// 宿主开发回退路径（相对 cwd）
const DEV_CONFIG_PATH: &str = "amberguard_config.toml";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML 解析: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("TOML 序列化: {0}")]
    TomlSer(#[from] toml::ser::Error),
}

/// 守护进程配置（Phase 1 最小字段）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 网卡名，默认 wlan0
    #[serde(default = "default_interface")]
    pub interface: String,
    /// HTTP 监听，仅本机
    #[serde(default = "default_listen")]
    pub listen: String,
    /// 上切对侧 RSSI 下限（dBm）
    #[serde(default = "default_upswitch_rssi")]
    pub upswitch_rssi_min_dbm: i32,
    /// 进入梯度检测阈值
    #[serde(default = "default_score_detect")]
    pub score_detect_threshold: f32,
    /// 触发切换阈值
    #[serde(default = "default_score_switch")]
    pub score_switch_threshold: f32,
    /// 可选：强制 wpa ctrl 路径（覆盖自动探测）
    #[serde(default)]
    pub wpa_ctrl_path: Option<String>,
    /// 异名双频羁绊（如 5G SSID 与 2.4G SSID 不同）
    #[serde(default)]
    pub bonds: Vec<crate::band_bond::SsidBond>,
    /// 工作模式：daily / pause
    #[serde(default = "default_mode")]
    pub mode: String,
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
            mode: default_mode(),
        }
    }
}

impl Config {
    /// 解析实际读写路径：Android 路径可写则用之，否则开发回退
    pub fn resolve_path() -> PathBuf {
        let android = Path::new(ANDROID_CONFIG_PATH);
        if android.parent().is_some_and(|p| p.is_dir()) || android.exists() {
            return android.to_path_buf();
        }
        PathBuf::from(DEV_CONFIG_PATH)
    }

    /// 加载配置；文件不存在则写默认并返回
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::resolve_path();
        if !path.exists() {
            let cfg = Self::default();
            // 父目录可能不存在（如 /data/adb 未挂载）——写失败仅打日志由调用方处理
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(e) = cfg.save_to(&path) {
                log::warn!("写入默认配置失败 {}: {e}，使用内存默认", path.display());
            }
            return Ok(cfg);
        }
        let text = fs::read_to_string(&path)?;
        let cfg: Config = toml::from_str(&text)?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(&Self::resolve_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        fs::write(path, text)?;
        Ok(())
    }
}
