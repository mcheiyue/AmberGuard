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

/// 家网 AP（BSSID 主键，通用多路由/Mesh）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeAp {
    pub bssid: String,
    #[serde(default)]
    pub ssid: String,
    /// "5" | "2.4" | "auto"
    #[serde(default = "default_home_band")]
    pub band: String,
}

fn default_home_band() -> String {
    "auto".into()
}

impl HomeAp {
    pub fn bssid_norm(&self) -> String {
        self.bssid.to_lowercase()
    }

    pub fn is_5g_hint(&self) -> Option<bool> {
        match self.band.as_str() {
            "5" | "5g" | "5G" => Some(true),
            "2.4" | "24" | "2.4g" | "2G" | "2g" => Some(false),
            _ => None,
        }
    }
}

/// 规范化 BSSID 比较
pub fn bssid_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

pub fn home_contains(home: &[HomeAp], bssid: &str) -> bool {
    home.iter().any(|h| bssid_eq(&h.bssid, bssid))
}

/// 当前连接是否属于家网（空家网=未配置，视为「未限制」由调用方解释）
pub fn link_in_home(home: &[HomeAp], bssid: &str, ssid: &str) -> bool {
    if home.is_empty() {
        return true;
    }
    if !bssid.is_empty() && home_contains(home, bssid) {
        return true;
    }
    // 回退：仅 SSID 命中（无 BSSID 时）
    !ssid.is_empty()
        && home
            .iter()
            .any(|h| !h.ssid.is_empty() && h.ssid == ssid)
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

/// 系统已保存「可双频切换」的网络对：同 stem 不同 SSID，或至少 2 个不同 SSID
pub fn dual_band_pair_saved(ssids: &[String]) -> (bool, String) {
    let mut uniq = Vec::new();
    for s in ssids {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        if !uniq.iter().any(|u: &String| u == t) {
            uniq.push(t.to_string());
        }
    }
    if uniq.len() < 2 {
        return (
            false,
            if uniq.is_empty() {
                "未读到已保存网络（请先在系统连过 WiFi）".into()
            } else {
                format!("仅 1 个：{}；请再保存对端频段", uniq[0])
            },
        );
    }
    for i in 0..uniq.len() {
        for j in (i + 1)..uniq.len() {
            let a = &uniq[i];
            let b = &uniq[j];
            let sa = ssid_stem(a);
            let sb = ssid_stem(b);
            if a != b && !sa.is_empty() && sa == sb {
                return (true, format!("配对 {a} ↔ {b}"));
            }
        }
    }
    // 弱通过：有 ≥2 个不同名（非标准命名仍可 SELECT）
    (
        true,
        format!("已保存 {} 个网络（未识别同名双频后缀）", uniq.len()),
    )
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

/// 在目标频段上选目标 AP。
/// 优先级：① 家网组内（BSSID）② bonds/stem 启发式（无家网或家网内无可见目标时）
pub fn best_bonded_on_band(
    scans: &[ScanAp],
    current_ssid: &str,
    want_5g: bool,
    min_rssi: i32,
    bonds: &[SsidBond],
) -> Option<ScanAp> {
    best_on_band(scans, current_ssid, want_5g, min_rssi, bonds, &[])
}

pub fn best_on_band(
    scans: &[ScanAp],
    current_ssid: &str,
    want_5g: bool,
    min_rssi: i32,
    bonds: &[SsidBond],
    home: &[HomeAp],
) -> Option<ScanAp> {
    let band_ok = |a: &ScanAp| a.is_5g() == want_5g && a.signal >= min_rssi;

    if !home.is_empty() {
        // 家网内：钉 BSSID，取组内目标频段最强
        let in_home: Vec<&ScanAp> = scans
            .iter()
            .filter(|a| band_ok(a) && home_contains(home, &a.bssid))
            .collect();
        if let Some(best) = in_home.into_iter().max_by_key(|a| a.signal) {
            return Some(best.clone());
        }
        // 家网已配置但目标频段无可见成员 → 不跨出家网乱切
        return None;
    }

    scans
        .iter()
        .filter(|a| band_ok(a))
        .filter(|a| ssid_matches(current_ssid, &a.ssid, bonds))
        .max_by_key(|a| a.signal)
        .cloned()
}

/// 扫描结果转 API JSON 友好结构
#[derive(Debug, Clone, Serialize)]
pub struct ScanApView {
    pub bssid: String,
    pub ssid: String,
    pub freq: u32,
    pub signal: i32,
    pub band: String,
    pub in_home: bool,
}

pub fn scan_views(scans: &[ScanAp], home: &[HomeAp]) -> Vec<ScanApView> {
    let mut v: Vec<ScanApView> = scans
        .iter()
        .map(|a| ScanApView {
            bssid: a.bssid.clone(),
            ssid: a.ssid.clone(),
            freq: a.freq,
            signal: a.signal,
            band: if a.is_5g() { "5" } else { "2.4" }.into(),
            in_home: home_contains(home, &a.bssid),
        })
        .collect();
    v.sort_by(|a, b| b.signal.cmp(&a.signal));
    v
}

/// 解析 LIST_NETWORKS，返回 (id, ssid)（兼容 tab / 多空格）
pub fn parse_list_networks(raw: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("network") || line.starts_with("Using ") {
            continue;
        }
        // id \t ssid \t bssid \t flags  或空格分隔
        let parts: Vec<&str> = if line.contains('\t') {
            line.split('\t').collect()
        } else {
            line.split_whitespace().collect()
        };
        if parts.len() < 2 {
            continue;
        }
        if let Ok(id) = parts[0].parse::<u32>() {
            out.push((id, parts[1].to_string()));
        }
    }
    out
}

pub fn network_id_for_ssid(list_raw: &str, ssid: &str) -> Option<u32> {
    let list = parse_list_networks(list_raw);
    list.iter()
        .find(|(_, s)| s == ssid)
        .map(|(id, _)| *id)
        // 启发式：已保存网络 stem 匹配
        .or_else(|| {
            let want = ssid_stem(ssid);
            list.into_iter()
                .find(|(_, s)| {
                    let st = ssid_stem(s);
                    !want.is_empty() && st.eq_ignore_ascii_case(&want)
                })
                .map(|(id, _)| id)
        })
}
