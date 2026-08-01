use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct StatusSnapshot {
    pub rssi: i32,
    pub score: f32,
    pub band: &'static str,
    pub state: String,
    pub power_state: &'static str,
}

impl StatusSnapshot {
    pub fn new() -> Self {
        Self {
            rssi: -65,
            score: 50.0,
            band: "2.4",
            state: "DISCONNECTED".to_string(),
            power_state: "ON",
        }
    }
}

