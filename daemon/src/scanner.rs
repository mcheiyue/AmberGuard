#[derive(Debug)]
pub struct Scanner;

impl Scanner {
    pub fn new() -> Self {
        Self
    }
    pub fn trigger_targeted_scan(&self) {
        log::info!("scanner: stub targeted scan");
    }
}
