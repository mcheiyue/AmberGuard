//! 扫描解析 + 双频羁绊（同名 / 配置 / 启发式异名）

use serde::{Deserialize, Serialize};

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

/// 配置的双频对（异名 SSID 必填）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SsidBond {
    pub ssid_5g: String,
    pub ssid_24g: String,
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

/// 归一化：去 5G/2.4G/CLONE 等后缀，便于启发式配对
pub fn ssid_stem(ssid: &str) -> String {
    let mut s = ssid.to_string();
    for pat in [
        "_5G_CLONE",
        "-5G-CLONE",
        "_5G",
        "-5G",
        "5G_",
        "5G-",
        "_2.4G",
        "-2.4G",
        "_2G",
        "-2G",
        "_24G",
        "_CLONE",
        "-CLONE",
    ] {
        s = s.replace(pat, "");
    }
    // 连续下划线收束
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    s.trim_matches(|c| c == '_' || c == '-').to_string()
}

fn ssid_matches(current: &str, candidate: &str, bonds: &[SsidBond]) -> bool {
    if current == candidate {
        return true;
    }
    for b in bonds {
        if (current == b.ssid_5g && candidate == b.ssid_24g)
            || (current == b.ssid_24g && candidate == b.ssid_5g)
        {
            return true;
        }
    }
    // 启发式：stem 相同且非空
    let a = ssid_stem(current);
    let b = ssid_stem(candidate);
    !a.is_empty() && a.eq_ignore_ascii_case(&b)
}

/// 在目标频段上，找与 current_ssid 羁绊匹配、信号最好的 AP
pub fn best_bonded_on_band(
    scans: &[ScanAp],
    current_ssid: &str,
    want_5g: bool,
    min_rssi: i32,
    bonds: &[SsidBond],
) -> Option<ScanAp> {
    scans
        .iter()
        .filter(|a| a.is_5g() == want_5g)
        .filter(|a| a.signal >= min_rssi)
        .filter(|a| ssid_matches(current_ssid, &a.ssid, bonds))
        .max_by_key(|a| a.signal)
        .cloned()
}

/// 解析 LIST_NETWORKS，返回 (id, ssid)
pub fn parse_list_networks(raw: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("network") || line.starts_with("Using ") {
            continue;
        }
        // id \t ssid \t bssid \t flags
        let mut parts = line.split('\t');
        let Some(id_s) = parts.next() else { continue };
        let Some(ssid) = parts.next() else { continue };
        if let Ok(id) = id_s.parse::<u32>() {
            out.push((id, ssid.to_string()));
        }
    }
    out
}

pub fn network_id_for_ssid(list_raw: &str, ssid: &str) -> Option<u32> {
    parse_list_networks(list_raw)
        .into_iter()
        .find(|(_, s)| s == ssid)
        .map(|(id, _)| id)
}
