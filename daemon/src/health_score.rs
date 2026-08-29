/// 健康度 0–100。retry_rate: 0.0–1.0（Δtx_retries/Δtx_packets）；无流量时传 None → retry 中位 50。
/// RTT 推 Phase 2.5，当前忽略。
pub fn health_score(rssi: i32, retry_rate: Option<f32>, tx_delta: u64, _rtt_ms: Option<u32>) -> f32 {
    let rssi_score = rssi_to_score(rssi);
    // 无流量时 tx_retries 不增长 → 假健康，retry 置中
    const MIN_TX: u64 = 8;
    let retry_score = match retry_rate {
        Some(rate) if tx_delta > MIN_TX => (100.0 - rate * 100.0).max(0.0),
        _ => 50.0,
    };
    // Phase 2 先 RSSI+Retry，无 RTT
    let mut composite = rssi_score * 0.5 + retry_score * 0.5;
    // 静默/雪崩区：无流量时 retry 钉 50 中点会抵消 rssi 断崖，剥夺托底以可靠下切
    if rssi_score <= 25.0 {
        composite = composite.min(rssi_score * 1.2);
    }
    composite
}

/// 分段非线性 RSSI→分数（锚点插值）：满速区钝、雪崩区陡，让状态机在核心体验区与死亡边缘区灵敏度不同。
pub fn rssi_to_score(rssi: i32) -> f32 {
    let r = rssi as f32;
    let pts: [(f32, f32); 7] = [
        (-90.0, 0.0),
        (-80.0, 5.0),
        (-75.0, 25.0),
        (-67.0, 65.0),
        (-55.0, 90.0),
        (-50.0, 100.0),
        (-40.0, 100.0),
    ];
    if r <= pts[0].0 {
        return pts[0].1;
    }
    if r >= pts[6].0 {
        return pts[6].1;
    }
    for i in 0..pts.len() - 1 {
        let (r0, s0) = pts[i];
        let (r1, s1) = pts[i + 1];
        if r >= r0 && r <= r1 {
            let t = (r - r0) / (r1 - r0);
            return s0 + t * (s1 - s0);
        }
    }
    0.0
}

/// 同频追优 margin 随当前 RSSI 动态收窄：≥-55 用 base（=roam_margin_db），逼近 -75 收到硬底 5dB。
pub fn calc_dynamic_roam_margin(cur_rssi: i32, base: i32) -> i32 {
    let margin = (5.0 + (cur_rssi + 75) as f32 / 20.0 * (base as f32 - 5.0)).clamp(5.0, base as f32) as i32;
    margin
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rssi_to_score_anchors() {
        assert_eq!(rssi_to_score(-50), 100.0);
        assert_eq!(rssi_to_score(-55), 90.0);
        assert_eq!(rssi_to_score(-67), 65.0);
        assert_eq!(rssi_to_score(-75), 25.0);
        assert_eq!(rssi_to_score(-80), 5.0);
        assert_eq!(rssi_to_score(-90), 0.0);
    }

    #[test]
    fn rssi_to_score_monotonic() {
        let mut prev = -1.0f32;
        for r in (-95..=-40).step_by(5) {
            let s = rssi_to_score(r);
            assert!(s >= prev, "rssi {} score {} 应单调不降（前 {})", r, s, prev);
            prev = s;
        }
    }

    #[test]
    fn dynamic_margin_bounds() {
        assert_eq!(calc_dynamic_roam_margin(-55, 12), 12);
        assert_eq!(calc_dynamic_roam_margin(-50, 12), 12);
        assert_eq!(calc_dynamic_roam_margin(-75, 12), 5);
        assert_eq!(calc_dynamic_roam_margin(-90, 12), 5);
        let m = calc_dynamic_roam_margin(-65, 12);
        assert!(m >= 5 && m <= 12, "中段 margin {} 应在 [5,12]", m);
    }

    #[test]
    fn silent_avalanche_reaches_downswitch() {
        // 静默 -75：retry 钉 50 本会让复合 37.5 > 30，剥夺托底后落到下切阈值 30
        assert!(health_score(-75, None, 0, None) <= 30.0, "静默 -75 复合应 ≤ 下切阈值 30");
        // 更差则严格低于 30
        assert!(health_score(-77, None, 0, None) < 30.0, "静默 -77 复合应 < 下切阈值 30");
    }
}
