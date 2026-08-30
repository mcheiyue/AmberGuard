//! 解析 `iw dev wlan0 station dump` 输出，获取 tx_retries / tx_packets
//! Phase 2.5：替代 neli 直接解析，零额外依赖

use std::time::Instant;

#[derive(Debug, Clone)]
pub struct StationSample {
    pub tx_packets: u64,
    pub tx_retries: u64,
    pub tx_failed: u64,
    pub rx_packets: u64,
    pub signal: Option<i32>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub timestamp: Instant,
}

impl Default for StationSample {
    fn default() -> Self {
        Self {
            tx_packets: 0,
            tx_retries: 0,
            tx_failed: 0,
            rx_packets: 0,
            signal: None,
            rx_bytes: 0,
            tx_bytes: 0,
            timestamp: Instant::now(),
        }
    }
}

/// 解析 iw station dump 文本
pub fn parse_iw_station(text: &str) -> StationSample {
    let mut s = StationSample {
        timestamp: Instant::now(),
        ..Default::default()
    };
    for line in text.lines() {
        let t = line.trim();
        if let Some(v) = parse_kv(t, "rx packets:\t") {
            s.rx_packets = v;
        } else if let Some(v) = parse_kv(t, "tx packets:\t") {
            s.tx_packets = v;
        } else if let Some(v) = parse_kv(t, "tx retries:\t") {
            s.tx_retries = v;
        } else if let Some(v) = parse_kv(t, "tx failed:\t") {
            s.tx_failed = v;
        } else if let Some(v) = parse_signal(t, "signal:") {
            s.signal = Some(v);
        } else if let Some(v) = parse_kv(t, "rx bytes:\t") {
            s.rx_bytes = v;
        } else if let Some(v) = parse_kv(t, "tx bytes:\t") {
            s.tx_bytes = v;
        }
    }
    s
}

fn parse_kv(line: &str, key: &str) -> Option<u64> {
    if line.starts_with(key) {
        line[key.len()..].trim().split_whitespace().next()?.parse().ok()
    } else {
        None
    }
}

fn parse_signal(line: &str, key: &str) -> Option<i32> {
    if line.starts_with(key) {
        let rest = line[key.len()..].trim();
        rest.split_whitespace().next()?.parse().ok()
    } else {
        None
    }
}

/// 计算 retry_rate = Δtx_retries / Δtx_packets，避免除零
pub fn retry_rate(prev: &StationSample, cur: &StationSample) -> Option<f32> {
    let dp = cur.tx_packets.checked_sub(prev.tx_packets)?;
    let dr = cur.tx_retries.checked_sub(prev.tx_retries)?;
    if dp == 0 {
        return None;
    }
    let rate = dr as f32 / dp as f32;
    Some(rate.clamp(0.0, 1.0))
}

/// 执行 iw station dump
pub fn iw_station_dump(iface: &str) -> Result<String, String> {
    let out = std::process::Command::new("iw")
        .args(["dev", iface, "station", "dump"])
        .output()
        .map_err(|e| format!("iw exec: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("iw exit {}: {}", out.status, stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 计算双向流量速率（KB/s）：(Δrx_bytes + Δtx_bytes) / Δt
/// 采样间隔太短（<0.5s）时返回 None 避免噪声
pub fn traffic_rate_kbps(prev: &StationSample, cur: &StationSample) -> Option<f32> {
    let dbytes = cur.rx_bytes.saturating_sub(prev.rx_bytes)
        + cur.tx_bytes.saturating_sub(prev.tx_bytes);
    let dt = prev.timestamp.elapsed().as_secs_f32();
    if dt < 0.5 {
        return None;
    }
    Some(dbytes as f32 / dt / 1024.0)
}