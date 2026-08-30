//! per-BSSID 质量记忆：故障 AP 自动降权隔离
//! 连续失败 >= 3 次 → 降权 cooldown_until（默认 30 分钟）
//! 成功切换 → 清零计数
//! 持久化到 /data/adb/amberguard/bssid_stats.json

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const STATS_PATH: &str = "/data/adb/amberguard/bssid_stats.json";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct BssidStat {
    pub fail_count: u32,
    pub last_fail_unix: u64,
    pub cooldown_until_unix: u64,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 从磁盘加载
pub fn load() -> HashMap<String, BssidStat> {
    let Ok(raw) = std::fs::read_to_string(STATS_PATH) else {
        return HashMap::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// 持久化到磁盘
pub fn save(map: &HashMap<String, BssidStat>) {
    let _ = std::fs::create_dir_all("/data/adb/amberguard");
    if let Ok(json) = serde_json::to_string(map) {
        let _ = std::fs::write(STATS_PATH, json);
    }
}

/// 记录一次切换失败；fail_count >= 3 时设置降权截止时间
pub fn record_fail(map: &mut HashMap<String, BssidStat>, bssid: &str, demote_secs: u64) {
    let key = bssid.to_lowercase();
    let entry = map.entry(key).or_default();
    entry.fail_count += 1;
    entry.last_fail_unix = unix_now();
    if entry.fail_count >= 3 {
        entry.cooldown_until_unix = unix_now() + demote_secs;
    }
}

/// 切换成功：清零失败计数
pub fn record_success(map: &mut HashMap<String, BssidStat>, bssid: &str) {
    let key = bssid.to_lowercase();
    map.remove(&key);
}

/// 返回当前处于降权期的 BSSID 列表（用于 best_on_band 过滤）
pub fn get_demoted(map: &HashMap<String, BssidStat>) -> Vec<String> {
    let now = unix_now();
    map.iter()
        .filter(|(_, stat)| stat.cooldown_until_unix > now && stat.fail_count >= 3)
        .map(|(bssid, _)| bssid.clone())
        .collect()
}

/// 清空所有记忆
pub fn clear_all(map: &mut HashMap<String, BssidStat>) {
    map.clear();
    save(map);
}
