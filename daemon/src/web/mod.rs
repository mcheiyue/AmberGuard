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
}

impl StatusSnapshot {
    pub fn new() -> Self {
        Self {
            rssi: -65,
            score: 50.0,
            band: "2.4".into(),
            state: "DISCONNECTED".into(),
            ssid: String::new(),
            power_state: "ON".into(),
            last_error: String::new(),
        }
    }
}

