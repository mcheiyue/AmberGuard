use std::time::Instant;

#[derive(Debug, Clone)]
pub enum State {
    IDLE,
    GRADIENT_DETECT,
    SWITCHING,
    PENALTY,
    FROZEN,
}

pub struct PenaltyLock {
    pub bond_id: String,
    pub failed_at: Instant,
    pub cooldown_secs: u64,
}

impl PenaltyLock {
    pub fn new(bond_id: &str) -> Self {
        Self {
            bond_id: bond_id.to_string(),
            failed_at: Instant::now(),
            cooldown_secs: 30,
        }
    }
}

pub struct StateMachine {
    pub state: State,
    pub penalty: Option<PenaltyLock>,
}

impl StateMachine {
    pub fn new() -> Self {
        Self { state: State::IDLE, penalty: None }
    }

    pub fn trigger_switch(&mut self) {
        log::info!("state_machine: stub trigger_switch");
        self.state = State::SWITCHING;
    }
}
