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

/// wpa SCAN_RESULTS 常把中文等非 ASCII 编成 `\xNN` 字节转义
pub fn decode_wpa_ssid(raw: &str) -> String {
    if !raw.as_bytes().contains(&b'\\') {
        return raw.to_string();
    }
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'x' | b'X' if i + 3 < bytes.len() => {
                    if let Ok(h) = std::str::from_utf8(&bytes[i + 2..i + 4]) {
                        if let Ok(v) = u8::from_str_radix(h, 16) {
                            out.push(v);
                            i += 4;
                            continue;
                        }
                    }
                }
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                    continue;
                }
                b'"' => {
                    out.push(b'"');
                    i += 2;
                    continue;
                }
                b'\'' => {
                    out.push(b'\'');
                    i += 2;
                    continue;
                }
                b'e' => {
                    out.push(0x1b);
                    i += 2;
                    continue;
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                    continue;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    match String::from_utf8(out) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(&e.into_bytes()).into_owned(),
    }
}

/// `cmd wifi list-scan-results`（UTF-8 中文 SSID，列对齐）
pub fn parse_cmd_scan_results(raw: &str) -> Vec<ScanAp> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("BSSID") || line.starts_with("Wifi") {
            continue;
        }
        // flags 以 [ 开头；之前：bssid freq rssi(复杂) age ssid…
        let flags_at = match line.find('[') {
            Some(i) => i,
            None => continue,
        };
        let head = line[..flags_at].trim_end();
        let mut toks = head.split_whitespace();
        let Some(bssid) = toks.next() else { continue };
        if bssid.len() < 11 || !bssid.contains(':') {
            continue;
        }
        let Some(freq_s) = toks.next() else { continue };
        let Some(rssi_tok) = toks.next() else { continue };
        let _age = toks.next(); // 可能没有
        // 剩余为 SSID（可含空格）；若 age 被当成 ssid 一部分——rssi 形如 -59 或 -59(0:-61/1:-65)
        let rssi_s = rssi_tok.split('(').next().unwrap_or(rssi_tok);
        let Ok(freq) = freq_s.parse::<u32>() else { continue };
        let Ok(signal) = rssi_s.parse::<i32>() else { continue };
        // 若第四段是纯数字/小数（Age），SSID 从第五段起；否则第四段起都是 SSID
        let rest: Vec<&str> = toks.collect();
        let ssid = if rest.is_empty() {
            String::new()
        } else if rest[0].chars().all(|c| c.is_ascii_digit() || c == '.') {
            rest.get(1..).map(|s| s.join(" ")).unwrap_or_default()
        } else {
            rest.join(" ")
        };
        out.push(ScanAp {
            bssid: bssid.to_string(),
            freq,
            signal,
            ssid,
        });
    }
    out
}

/// 合并扫描：同 BSSID 保留信号更好、SSID 非空者
pub fn merge_scan_aps(a: Vec<ScanAp>, b: Vec<ScanAp>) -> Vec<ScanAp> {
    let mut map: std::collections::HashMap<String, ScanAp> = std::collections::HashMap::new();
    for ap in a.into_iter().chain(b) {
        let key = ap.bssid.to_lowercase();
        match map.get_mut(&key) {
            None => {
                map.insert(key, ap);
            }
            Some(old) => {
                if old.ssid.is_empty() && !ap.ssid.is_empty() {
                    old.ssid = ap.ssid.clone();
                }
                // 若旧的是 \x 未解码残留而新的是可读中文
                if ap.ssid.chars().any(|c| c > '\u{7f}') && !old.ssid.chars().any(|c| c > '\u{7f}')
                {
                    old.ssid = ap.ssid.clone();
                }
                if ap.signal > old.signal {
                    old.signal = ap.signal;
                    old.freq = ap.freq;
                }
            }
        }
    }
    let mut v: Vec<_> = map.into_values().collect();
    v.sort_by(|x, y| y.signal.cmp(&x.signal));
    v
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
        let ssid_raw = parts.next().unwrap_or("");
        let ssid = decode_wpa_ssid(ssid_raw);
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
    /// 同 stem 且同时可见 2.4+5 → 建议纳入家网对
    #[serde(default)]
    pub suggested: bool,
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
            suggested: false,
        })
        .collect();
    mark_suggested_stem_pairs(&mut v);
    v.sort_by(|a, b| b.signal.cmp(&a.signal));
    v
}

/// 同 ssid_stem 下同时有 2.4 与 5 → **每 stem 只标最强 2.4 + 最强 5**（最多 2 个，避免 12 个全亮）
pub fn mark_suggested_stem_pairs(views: &mut [ScanApView]) {
    use std::collections::HashMap;
    // stem -> (best_24_idx, best_24_sig, best_5_idx, best_5_sig)
    let mut best: HashMap<String, (Option<(usize, i32)>, Option<(usize, i32)>)> = HashMap::new();
    for (i, a) in views.iter().enumerate() {
        if a.ssid.is_empty() {
            continue;
        }
        let stem = ssid_stem(&a.ssid);
        // 过短 stem 易把无关网扫成一对（如单字母）
        if stem.len() < 3 {
            continue;
        }
        let is5 = a.band == "5" || a.freq > 5000;
        let e = best.entry(stem).or_insert((None, None));
        if is5 {
            match e.1 {
                Some((_, sig)) if a.signal <= sig => {}
                _ => e.1 = Some((i, a.signal)),
            }
        } else {
            match e.0 {
                Some((_, sig)) if a.signal <= sig => {}
                _ => e.0 = Some((i, a.signal)),
            }
        }
    }
    for (_stem, (b24, b5)) in best {
        let (Some((i24, _)), Some((i5, _))) = (b24, b5) else {
            continue;
        };
        if let Some(v) = views.get_mut(i24) {
            v.suggested = true;
        }
        if let Some(v) = views.get_mut(i5) {
            v.suggested = true;
        }
    }
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
            // SSID 字段也可能是 \x 转义
            let ssid = decode_wpa_ssid(parts[1].trim_matches('"'));
            out.push((id, ssid));
        }
    }
    out
}

pub fn network_id_for_ssid(list_raw: &str, ssid: &str) -> Option<u32> {
    let list = parse_list_networks(list_raw);
    // 必须精确 SSID：stem 回退会把 5G 误绑到 2.4 的 id（小米 LIST 残缺时必炸）
    list.iter()
        .find(|(_, s)| s == ssid)
        .map(|(id, _)| *id)
}

#[cfg(test)]
mod ssid_decode_tests {
    use super::{decode_wpa_ssid, mark_suggested_stem_pairs, ScanApView};

    #[test]
    fn decodes_utf8_hex_escape() {
        let raw = r"\xe6\xb5\xb7\xe5\xba\xb7";
        let s = decode_wpa_ssid(raw);
        assert!(s.contains('海'), "got {s:?}");
    }

    #[test]
    fn plain_ascii_unchanged() {
        assert_eq!(decode_wpa_ssid("MERCURY_C8B5"), "MERCURY_C8B5");
    }

    #[test]
    fn suggests_stem_dual_band_pair() {
        let mut v = vec![
            ScanApView {
                bssid: "aa:aa:aa:aa:aa:01".into(),
                ssid: "MERCURY_C8B5".into(),
                freq: 2412,
                signal: -50,
                band: "2.4".into(),
                in_home: false,
                suggested: false,
            },
            ScanApView {
                bssid: "aa:aa:aa:aa:aa:02".into(),
                ssid: "MERCURY_5G_C8B5".into(),
                freq: 5180,
                signal: -55,
                band: "5".into(),
                in_home: false,
                suggested: false,
            },
            ScanApView {
                bssid: "bb:bb:bb:bb:bb:01".into(),
                ssid: "OTHER".into(),
                freq: 2412,
                signal: -40,
                band: "2.4".into(),
                in_home: false,
                suggested: false,
            },
        ];
        mark_suggested_stem_pairs(&mut v);
        assert!(v[0].suggested && v[1].suggested);
        assert!(!v[2].suggested);
    }

    #[test]
    fn suggests_only_strongest_pair_per_stem() {
        let mut v = vec![
            ScanApView {
                bssid: "a1".into(),
                ssid: "HOME".into(),
                freq: 2412,
                signal: -70,
                band: "2.4".into(),
                in_home: false,
                suggested: false,
            },
            ScanApView {
                bssid: "a2".into(),
                ssid: "HOME".into(),
                freq: 2412,
                signal: -40,
                band: "2.4".into(),
                in_home: false,
                suggested: false,
            },
            ScanApView {
                bssid: "b1".into(),
                ssid: "HOME_5G".into(),
                freq: 5180,
                signal: -60,
                band: "5".into(),
                in_home: false,
                suggested: false,
            },
            ScanApView {
                bssid: "b2".into(),
                ssid: "HOME_5G".into(),
                freq: 5180,
                signal: -45,
                band: "5".into(),
                in_home: false,
                suggested: false,
            },
        ];
        mark_suggested_stem_pairs(&mut v);
        // 每 stem 仅最强 2.4 + 最强 5
        assert!(!v[0].suggested);
        assert!(v[1].suggested);
        assert!(!v[2].suggested);
        assert!(v[3].suggested);
    }
}
