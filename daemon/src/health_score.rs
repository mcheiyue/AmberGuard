/// 健康度 0–100。retry_rate: 0.0–1.0（Δtx_retries/Δtx_packets）；无流量时传 None → retry 中位 50。
/// RTT 推 Phase 2.5，当前忽略。
pub fn health_score(rssi: i32, retry_rate: Option<f32>, tx_delta: u64, _rtt_ms: Option<u32>) -> f32 {
    let rssi_score = ((rssi + 90) as f32 / 50.0 * 100.0).clamp(0.0, 100.0);
    // 无流量时 tx_retries 不增长 → 假健康，retry 置中
    const MIN_TX: u64 = 8;
    let retry_score = match retry_rate {
        Some(rate) if tx_delta > MIN_TX => (100.0 - rate * 100.0).max(0.0),
        _ => 50.0,
    };
    // Phase 2 先 RSSI+Retry，无 RTT
    rssi_score * 0.5 + retry_score * 0.5
}
