use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct StatusSnapshot {
    pub rssi: i32,
    pub score: f32,
    pub band: String,
    pub state: String,
    pub ssid: String,
    pub power_state: String,
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
        }
    }
}

