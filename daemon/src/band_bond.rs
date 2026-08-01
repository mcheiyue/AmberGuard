#[derive(Debug, Clone)]
pub struct BondedAP {
    pub ssid_5g: String,
    pub ssid_24g: String,
    pub bssid_5g: [u8;6],
    pub bssid_24g: [u8;6],
}

impl BondedAP {
    pub fn switch_to(&self, _current: &ConnectedAP, _target_band: Band) -> SwitchResult {
        log::info!("band_bond: stub switch_to");
        SwitchResult::CommandSent
    }
}

#[derive(Debug)]
pub struct ConnectedAP {
    pub ssid: String,
    pub bssid: [u8;6],
    pub band: Band,
}

#[derive(Debug)]
pub enum Band {
    Band2,
    Band5,
}

#[derive(Debug)]
pub enum SwitchResult {
    CommandSent,
    NeedScan(u32),
    Error(String),
}
