//! 扫描解析 + 家网匹配 + 启发式双频组

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
    if !bssid.is_empty() {
        return home_contains(home, bssid);
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

/// 在目标频段上选目标 AP。
/// 优先级：① 家网组内（BSSID）② stem 启发式（无家网时）
pub fn best_bonded_on_band(
    scans: &[ScanAp],
    current_ssid: &str,
    want_5g: bool,
    min_rssi: i32,
) -> Option<ScanAp> {
    best_on_band(scans, current_ssid, want_5g, min_rssi, &[])
}

pub fn best_on_band(
    scans: &[ScanAp],
    current_ssid: &str,
    want_5g: bool,
    min_rssi: i32,
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

    // 无家网：stem 启发式匹配（同名 / 同 stem 异名 SSID）
    let stem = ssid_stem(current_ssid);
    scans
        .iter()
        .filter(|a| band_ok(a))
        .filter(|a| {
            a.ssid == current_ssid
                || (!stem.is_empty() && ssid_stem(&a.ssid).eq_ignore_ascii_case(&stem))
        })
        .max_by_key(|a| a.signal)
        .cloned()
}

/// 带降权过滤的 best_on_band：剔除处于降权期的 BSSID
pub fn best_on_band_with(
    scans: &[ScanAp],
    current_ssid: &str,
    want_5g: bool,
    min_rssi: i32,
    home: &[HomeAp],
    demoted: &[String],
) -> Option<ScanAp> {
    let eligible: Vec<ScanAp> = scans
        .iter()
        .filter(|ap| !demoted.iter().any(|d| bssid_eq(d, &ap.bssid)))
        .cloned()
        .collect();
    best_on_band(&eligible, current_ssid, want_5g, min_rssi, home)
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
    scan_views_filtered(scans, home, None)
}

/// `allowed_stems`：仅这些 stem 可标双频候选（已保存/当前连接）；None=旧行为不推荐，当作空
pub fn scan_views_filtered(
    scans: &[ScanAp],
    home: &[HomeAp],
    allowed_stems: Option<&std::collections::HashSet<String>>,
) -> Vec<ScanApView> {
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
    mark_suggested_stem_pairs(&mut v, allowed_stems);
    v.sort_by(|a, b| b.signal.cmp(&a.signal));
    v
}

/// 从 SSID 列表生成允许的 stem 集合（小写比较用原 stem 字符串）
pub fn stems_from_ssids(ssids: &[String]) -> std::collections::HashSet<String> {
    ssids
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| ssid_stem(s))
        .filter(|s| s.len() >= 3)
        .collect()
}

/// 仅 `allowed_stems` 内、且同 stem 同时有 2.4+5 时：
/// **该 stem 下所有可见 BSSID 都标 suggested**（主+副路由可 4 个，不限最强一对）
pub fn mark_suggested_stem_pairs(
    views: &mut [ScanApView],
    allowed_stems: Option<&std::collections::HashSet<String>>,
) {
    use std::collections::HashMap;
    let Some(allow) = allowed_stems else {
        return; // 无白名单 → 不标，避免邻居刷屏
    };
    if allow.is_empty() {
        return;
    }
    // stem -> (has24, has5, indices)
    let mut groups: HashMap<String, (bool, bool, Vec<usize>)> = HashMap::new();
    for (i, a) in views.iter().enumerate() {
        if a.ssid.is_empty() {
            continue;
        }
        let stem = ssid_stem(&a.ssid);
        if stem.len() < 3 || !allow.contains(&stem) {
            continue;
        }
        let is5 = a.band == "5" || a.freq > 5000;
        let e = groups.entry(stem).or_insert((false, false, Vec::new()));
        if is5 {
            e.1 = true;
        } else {
            e.0 = true;
        }
        e.2.push(i);
    }
    for (_stem, (has24, has5, idxs)) in groups {
        if !(has24 && has5) {
            continue;
        }
        for i in idxs {
            if let Some(v) = views.get_mut(i) {
                v.suggested = true;
            }
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
    use super::{
        best_on_band_with, decode_wpa_ssid, link_in_home, mark_suggested_stem_pairs, HomeAp,
        ScanAp, ScanApView,
    };

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
        use std::collections::HashSet;
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
        let allow: HashSet<String> = ["MERCURY_C8B5".into()].into_iter().collect();
        mark_suggested_stem_pairs(&mut v, Some(&allow));
        assert!(v[0].suggested && v[1].suggested);
        assert!(!v[2].suggested);
    }

    #[test]
    fn suggests_all_bssids_on_allowed_stem_not_neighbors() {
        use std::collections::HashSet;
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
            ScanApView {
                bssid: "n1".into(),
                ssid: "NEIGHBOR".into(),
                freq: 2412,
                signal: -30,
                band: "2.4".into(),
                in_home: false,
                suggested: false,
            },
            ScanApView {
                bssid: "n2".into(),
                ssid: "NEIGHBOR_5G".into(),
                freq: 5180,
                signal: -35,
                band: "5".into(),
                in_home: false,
                suggested: false,
            },
        ];
        let allow: HashSet<String> = ["HOME".into()].into_iter().collect();
        mark_suggested_stem_pairs(&mut v, Some(&allow));
        // 自家 stem 4 个全标；邻居不标
        assert!(v[0].suggested && v[1].suggested && v[2].suggested && v[3].suggested);
        assert!(!v[4].suggested && !v[5].suggested);
        // 无白名单不标
        for x in v.iter_mut() {
            x.suggested = false;
        }
        mark_suggested_stem_pairs(&mut v, None);
        assert!(v.iter().all(|x| !x.suggested));
    }

    #[test]
    fn skips_demoted_strongest_and_uses_next_candidate() {
        let scans = vec![
            ScanAp {
                bssid: "aa:aa:aa:aa:aa:01".into(),
                freq: 5180,
                signal: -40,
                ssid: "HOME_5G".into(),
            },
            ScanAp {
                bssid: "aa:aa:aa:aa:aa:02".into(),
                freq: 5180,
                signal: -55,
                ssid: "HOME_5G".into(),
            },
        ];
        let home = vec![
            HomeAp {
                bssid: scans[0].bssid.clone(),
                ssid: scans[0].ssid.clone(),
                band: "5".into(),
            },
            HomeAp {
                bssid: scans[1].bssid.clone(),
                ssid: scans[1].ssid.clone(),
                band: "5".into(),
            },
        ];

        let selected = best_on_band_with(
            &scans,
            "HOME",
            true,
            -80,
            &home,
            &[scans[0].bssid.clone()],
        )
        .expect("second candidate should remain selectable");

        assert_eq!(selected.bssid, scans[1].bssid);
    }

    #[test]
    fn returns_none_when_all_candidates_are_demoted() {
        let scans = vec![
            ScanAp {
                bssid: "aa:aa:aa:aa:aa:01".into(),
                freq: 5180,
                signal: -40,
                ssid: "HOME_5G".into(),
            },
            ScanAp {
                bssid: "aa:aa:aa:aa:aa:02".into(),
                freq: 5180,
                signal: -55,
                ssid: "HOME_5G".into(),
            },
        ];
        let home = scans
            .iter()
            .map(|scan| HomeAp {
                bssid: scan.bssid.clone(),
                ssid: scan.ssid.clone(),
                band: "5".into(),
            })
            .collect::<Vec<_>>();
        let demoted = scans.iter().map(|scan| scan.bssid.clone()).collect::<Vec<_>>();

        assert!(best_on_band_with(&scans, "HOME", true, -80, &home, &demoted).is_none());
    }

    #[test]
    fn known_nonmatching_bssid_is_not_home() {
        let home = vec![HomeAp {
            bssid: "aa:aa:aa:aa:aa:01".into(),
            ssid: "HOME".into(),
            band: "auto".into(),
        }];

        assert!(!link_in_home(
            &home,
            "aa:aa:aa:aa:aa:02",
            "HOME"
        ));
        assert!(link_in_home(&home, "", "HOME"));
    }
}
