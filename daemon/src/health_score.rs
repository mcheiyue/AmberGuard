use std::time::Instant;

/// Phase 1 stub - 完整健康度逻辑在 Phase 2
pub fn health_score(rssi: i32, retry_rate: Option<f32>, _tx_delta: u64, rtt_ms: Option<u32>) -> f32 {
    let rssi_score = ((rssi + 90) as f32 / 50.0 * 100.0).clamp(0.0, 100.0);
    let retry_score = retry_rate.map_or(50.0, |r| (100.0 - r * 100.0).max(0.0));
    match rtt_ms {
        Some(_) => rssi_score * 0.4 + retry_score * 0.4 + 20.0 * 0.2,
        None => rssi_score * 0.5 + retry_score * 0.5,
    }
}
