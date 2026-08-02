//! 从 SCAN_RESULTS 解析同 SSID 双频 BSSID，供 ROAM 使用

#[derive(Debug, Clone)]
pub struct ScanAp {
    pub bssid: String,
    pub freq: u32,
    pub signal: i32,
    pub ssid: String,
}

impl ScanAp {
    pub fn is_5g(&self) -> bool {
        self.freq > 5000
    }
}

/// 解析 wpa SCAN_RESULTS 文本
pub fn parse_scan_results(raw: &str) -> Vec<ScanAp> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("bssid") || line.starts_with("Using ") {
            continue;
        }
        // bssid / frequency / signal level / flags / ssid
        let mut parts = line.splitn(5, char::is_whitespace).filter(|p| !p.is_empty());
        let Some(bssid) = parts.next() else { continue };
        let Some(freq_s) = parts.next() else { continue };
        let Some(sig_s) = parts.next() else { continue };
        let _flags = parts.next();
        let ssid = parts.next().unwrap_or("").to_string();
        let Ok(freq) = freq_s.parse::<u32>() else { continue };
        let Ok(signal) = sig_s.parse::<i32>() else { continue };
        if bssid.len() < 11 {
            continue;
        }
        out.push(ScanAp {
            bssid: bssid.to_string(),
            freq,
            signal,
            ssid,
        });
    }
    out
}

/// 同 SSID 下，找目标频段上信号最好的 AP（排除当前 bssid）
pub fn best_peer(
    scans: &[ScanAp],
    ssid: &str,
    want_5g: bool,
    current_bssid: &str,
    min_rssi: i32,
) -> Option<ScanAp> {
    scans
        .iter()
        .filter(|a| a.ssid == ssid)
        .filter(|a| a.is_5g() == want_5g)
        .filter(|a| !a.bssid.eq_ignore_ascii_case(current_bssid))
        .filter(|a| a.signal >= min_rssi)
        .max_by_key(|a| a.signal)
        .cloned()
}

/// 若只要换频且同 BSSID 列表里仅有对侧，也可选任意对侧（含唯一 AP 时不选自己）
pub fn best_on_band(scans: &[ScanAp], ssid: &str, want_5g: bool, min_rssi: i32) -> Option<ScanAp> {
    scans
        .iter()
        .filter(|a| a.ssid == ssid)
        .filter(|a| a.is_5g() == want_5g)
        .filter(|a| a.signal >= min_rssi)
        .max_by_key(|a| a.signal)
        .cloned()
}
