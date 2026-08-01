use std::sync::Arc;
use std::thread;

#[derive(Debug, thiserror::Error)]
pub enum WpaError {
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse: {0}")]
    Parse(String),
}

pub struct WpaCtrl {
    connected: bool,
}

impl WpaCtrl {
    pub fn new() -> Result<Self, WpaError> {
        Ok(Self { connected: false })
    }

    pub fn connect(&mut self) -> Result<(), WpaError> {
        log::info!("wpa_ctrl: stub connect (Phase 1)");
        self.connected = true;
        Ok(())
    }

    pub fn status(&self) -> Result<String, WpaError> {
        if !self.connected {
            return Err(WpaError::Parse("not connected".into()));
        }
        Ok("wpa_state=COMPLETED\nssid=Home\nsignal=-65\ndisabled=0".to_string())
    }

    pub fn send_command(&self, cmd: &str) -> Result<(), WpaError> {
        log::debug!("wpa_ctrl: send_command stub: {}", cmd);
        Ok(())
    }

    pub fn attach_event_loop(&mut self) {
        log::info!("wpa_ctrl: stub attach (no real thread in Phase 1)");
    }
}

pub fn discover_ctrl_interface() -> Vec<String> {
    vec![
        "/data/misc/wifi/sockets/wpa_ctrl".to_string(),
        "/dev/socket/wpa_wlan0".to_string(),
    ]
}
