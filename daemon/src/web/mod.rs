use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct StatusSnapshot {
    pub rssi: i32,
    pub score: f32,
    pub band: String,
    pub state: String,
    pub ssid: String,
    /// wpa 连接态或 PAUSE；sm 态也写这里便于面板观察
    pub power_state: String,
    /// 最近一次切换/配置错误（空=无）
    pub last_error: String,
    /// 当前生效的阈值摘要（便于面板展示，无需再拉 /api/config）
    pub thresholds: ThresholdsView,
    /// 手动切网保护剩余秒数（0=未在保护）
    pub hold_remaining_secs: u64,
    /// 配置的保护时长（便于设置页展示）
    pub user_hold_secs: u64,
    /// 家网 AP 数量（0=未配置，走启发式）
    pub home_ap_count: usize,
    /// 当前链路是否在家网内（未配置家网时恒 true）
    pub in_home: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ThresholdsView {
    pub score_detect_threshold: f32,
    pub score_switch_threshold: f32,
    pub upswitch_rssi_min_dbm: i32,
    pub mode: String,
}

impl StatusSnapshot {
    pub fn new() -> Self {
        let d = crate::config::Config::default();
        Self {
            rssi: -65,
            score: 50.0,
            band: "2.4".into(),
            state: "DISCONNECTED".into(),
            ssid: String::new(),
            power_state: "ON".into(),
            last_error: String::new(),
            thresholds: ThresholdsView {
                score_detect_threshold: d.score_detect_threshold,
                score_switch_threshold: d.score_switch_threshold,
                upswitch_rssi_min_dbm: d.upswitch_rssi_min_dbm,
                mode: d.mode,
            },
            hold_remaining_secs: 0,
            user_hold_secs: d.user_hold_secs,
            home_ap_count: 0,
            in_home: true,
        }
    }
}
