#[derive(Debug)]
pub enum PowerState {
    On,
    Off,
}

pub struct PowerStateManager;

impl PowerStateManager {
    pub fn new() -> Self {
        Self
    }
    pub fn current_state(&self) -> PowerState {
        PowerState::On
    }
    pub fn on_screen_off(&mut self) {
        log::info!("power_state: stub screen off (Phase 1)");
    }
    pub fn on_screen_on(&mut self) {
        log::info!("power_state: stub screen on (Phase 1)");
    }
}
