//! 经 Android 框架切网 / 枚举已保存网络
//! 小米等机型 wpa LIST_NETWORKS 往往只含当前网；真源在 WifiConfigStore + `cmd wifi list-networks`

use std::fs;
use std::process::Command;

const STORE_PATHS: &[&str] = &[
    "/data/misc/apexdata/com.android.wifi/WifiConfigStore.xml",
    "/data/misc/wifi/WifiConfigStore.xml",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiSecurity {
    Open,
    Wpa2,
    Wpa3,
}

impl WifiSecurity {
    fn cmd_token(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Wpa2 => "wpa2",
            Self::Wpa3 => "wpa3",
        }
    }
}

/// `cmd wifi list-networks` 解析出的 SSID 列表（去重）
pub fn ssids_from_cmd_list() -> Vec<String> {
    network_rows_from_cmd()
        .into_iter()
        .map(|(s, _)| s)
        .fold(Vec::new(), |mut acc, s| {
            if !acc.iter().any(|x| x == &s) {
                acc.push(s);
            }
            acc
        })
}

/// (ssid, security hint from list-networks line)
fn network_rows_from_cmd() -> Vec<(String, Option<WifiSecurity>)> {
    let out = Command::new("cmd")
        .args(["wifi", "list-networks"])
        .output()
        .ok();
    let Some(o) = out else {
        return Vec::new();
    };
    if !o.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&o.stdout);
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Network") {
            continue;
        }
        let mut parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        // drop id
        parts.remove(0);
        let sec = parts.last().and_then(|t| parse_sec_token(t));
        if sec.is_some() {
            parts.pop();
        }
        let ssid = parts.join(" ");
        if ssid.is_empty() {
            continue;
        }
        rows.push((ssid, sec));
    }
    rows
}

fn parse_sec_token(t: &str) -> Option<WifiSecurity> {
    let l = t.to_ascii_lowercase();
    if l.contains("sae") || l.contains("wpa3") {
        Some(WifiSecurity::Wpa3)
    } else if l.contains("wpa") || l.contains("psk") {
        Some(WifiSecurity::Wpa2)
    } else if l == "open" || l.starts_with("owe") {
        Some(WifiSecurity::Open)
    } else {
        None
    }
}

/// 从 WifiConfigStore.xml 抽 SSID
pub fn ssids_from_config_store() -> Vec<String> {
    let raw = match read_store_raw() {
        Some(s) => s,
        None => return Vec::new(),
    };
    parse_store_ssids(&raw)
}

fn read_store_raw() -> Option<String> {
    for p in STORE_PATHS {
        if let Ok(s) = fs::read_to_string(p) {
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

fn parse_store_ssids(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in raw.lines() {
        if !line.contains("name=\"SSID\"") && !line.contains("name='SSID'") {
            continue;
        }
        if let Some(s) = extract_quoted_xml_string(line) {
            if !s.is_empty() && !out.iter().any(|x| x == &s) {
                out.push(s);
            }
        }
    }
    out
}

fn extract_quoted_xml_string(line: &str) -> Option<String> {
    let start = line.find('>')? + 1;
    let end = line.rfind('<')?;
    if end <= start {
        return None;
    }
    let mut inner = line[start..end].to_string();
    inner = inner.replace("&quot;", "\"").replace("&amp;", "&");
    let inner = inner.trim().trim_matches('"').to_string();
    if inner.is_empty() {
        None
    } else {
        Some(inner)
    }
}

/// 合并 wpa / cmd / store 的已保存 SSID
pub fn merge_saved_ssids(wpa_ssids: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for s in wpa_ssids
        .iter()
        .chain(ssids_from_cmd_list().iter())
        .chain(ssids_from_config_store().iter())
    {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        if !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// 推断安全类型：list-networks > ConfigKey > 默认 wpa2
pub fn security_for_ssid(ssid: &str) -> WifiSecurity {
    for (s, sec) in network_rows_from_cmd() {
        if s == ssid {
            if let Some(sec) = sec {
                return sec;
            }
        }
    }
    if let Some(raw) = read_store_raw() {
        if let Some(sec) = security_from_store(&raw, ssid) {
            return sec;
        }
    }
    WifiSecurity::Wpa2
}

fn security_from_store(raw: &str, ssid: &str) -> Option<WifiSecurity> {
    let needle = format!("&quot;{ssid}&quot;");
    for line in raw.lines() {
        if !line.contains("ConfigKey") {
            continue;
        }
        if !line.contains(&needle) && !line.contains(ssid) {
            continue;
        }
        let l = line.to_ascii_lowercase();
        if l.contains("sae") || l.contains("wpa3") {
            return Some(WifiSecurity::Wpa3);
        }
        if l.contains("wpa_psk") || l.contains("wpa2") || l.contains("psk") {
            return Some(WifiSecurity::Wpa2);
        }
        if l.contains("none") || l.contains("open") || l.contains("owe") {
            return Some(WifiSecurity::Open);
        }
    }
    None
}

fn psk_from_store(ssid: &str) -> Result<String, String> {
    let raw = read_store_raw().ok_or_else(|| {
        format!(
            "读不到 WifiConfigStore（无 root 或路径变化）。请确认已用系统设置连接并保存「{ssid}」。"
        )
    })?;
    let needle = format!("&quot;{ssid}&quot;");
    let needle2 = format!("\"{ssid}\"");
    let lines: Vec<&str> = raw.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !(line.contains("name=\"SSID\"") || line.contains("name='SSID'")) {
            continue;
        }
        let is_match = line.contains(&needle)
            || line.contains(&needle2)
            || extract_quoted_xml_string(line).as_deref() == Some(ssid);
        if !is_match {
            continue;
        }
        for j in i..lines.len().min(i + 50) {
            let l = lines[j];
            if l.contains("name=\"PreSharedKey\"") || l.contains("name='PreSharedKey'") {
                if l.contains("<null") {
                    return Err(format!(
                        "「{ssid}」在系统里无 PreSharedKey（开放网应走 open；企业/证书网无法代连）"
                    ));
                }
                if let Some(p) = extract_quoted_xml_string(l) {
                    if p.is_empty() {
                        return Err(format!(
                            "「{ssid}」密码字段为空。请在系统设置中忘记该网后重新输入密码并保存。"
                        ));
                    }
                    // 加密占位（部分 ROM）
                    if p.starts_with('*') || p.contains("encrypted") || p.len() > 128 {
                        // 仍尝试：有的机仍是明文长 PSK
                        if p.chars().all(|c| c == '*') {
                            return Err(format!(
                                "「{ssid}」密码已被系统加密存储，模块无法代连。请保持系统已保存该网，或改用可明文保存的 ROM/设置。"
                            ));
                        }
                    }
                    return Ok(p);
                }
            }
            if j > i && l.contains("<WifiConfiguration>") {
                break;
            }
        }
        return Err(format!(
            "ConfigStore 有「{ssid}」但未找到 PreSharedKey。请用系统 WiFi 重新连接并勾选保存后重试。"
        ));
    }
    Err(format!(
        "系统未保存「{ssid}」。请先在系统设置连接该 WiFi 并保存，再让 AmberGuard 切换。"
    ))
}

fn run_connect(ssid: &str, sec: WifiSecurity, psk: Option<&str>, bssid: Option<&str>) -> Result<(), String> {
    let mut args: Vec<String> = vec![
        "wifi".into(),
        "connect-network".into(),
        ssid.to_string(),
        sec.cmd_token().into(),
    ];
    if sec != WifiSecurity::Open {
        let p = psk.ok_or_else(|| format!("「{ssid}」需要密码但未提供"))?;
        args.push(p.to_string());
    }
    if let Some(b) = bssid {
        if !b.is_empty() {
            args.push("-b".into());
            args.push(b.to_string());
        }
    }
    let out = Command::new("cmd")
        .args(&args)
        .output()
        .map_err(|e| format!("无法执行 cmd wifi：{e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let msg = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        stdout.trim().to_string()
    };
    // 不回显可能含 PSK 的整段
    let safe = if msg.len() > 160 {
        format!("{}…", &msg[..160])
    } else {
        msg
    };
    Err(format!(
        "框架连接失败（{} / {}）：{safe}",
        sec.cmd_token(),
        ssid
    ))
}

/// 框架切网：安全类型 + 是否带 BSSID 多组合尝试。
/// 注意：`cmd` 返回成功只表示「已下发」，不保证已关联；调用方必须验链路。
/// 小米上带 `-b` 常失败或空转，故**先不带 BSSID**，再带 BSSID 重试。
pub fn framework_connect(ssid: &str, bssid: Option<&str>) -> Result<(), String> {
    let primary = security_for_ssid(ssid);
    let mut secs = vec![primary, WifiSecurity::Wpa3, WifiSecurity::Wpa2];
    secs.dedup();

    log::info!(
        "framework connect try ssid={} primary={:?} bssid={}",
        ssid,
        primary,
        bssid.unwrap_or("-")
    );

    let psk = if primary == WifiSecurity::Open && secs.iter().all(|s| *s == WifiSecurity::Open) {
        None
    } else {
        match psk_from_store(ssid) {
            Ok(p) => Some(p),
            Err(e) if primary == WifiSecurity::Open => {
                log::warn!("open 网无 PSK 可读: {e}");
                None
            }
            Err(e) => return Err(e),
        }
    };

    let mut last_err = String::from("未尝试");
    // 1) 不钉 BSSID（OEM 更稳）2) 再钉 BSSID
    let bssid_opts: &[Option<&str>] = if bssid.map(|b| !b.is_empty()).unwrap_or(false) {
        &[None, bssid]
    } else {
        &[None]
    };

    for b in bssid_opts {
        for sec in &secs {
            let r = match sec {
                WifiSecurity::Open => run_connect(ssid, WifiSecurity::Open, None, *b),
                WifiSecurity::Wpa2 | WifiSecurity::Wpa3 => {
                    let Some(ref p) = psk else {
                        last_err = format!("「{ssid}」无密码，无法用 {:?}", sec);
                        continue;
                    };
                    run_connect(ssid, *sec, Some(p), *b)
                }
            };
            match r {
                Ok(()) => {
                    log::info!(
                        "framework connect issued ok sec={:?} bssid={}",
                        sec,
                        b.unwrap_or("-")
                    );
                    return Ok(());
                }
                Err(e) => {
                    log::debug!("framework connect try fail sec={:?}: {e}", sec);
                    last_err = e;
                }
            }
        }
    }
    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ssid_line() {
        let line = r#"<string name="SSID">&quot;MERCURY_C8B5&quot;</string>"#;
        assert_eq!(
            extract_quoted_xml_string(line).as_deref(),
            Some("MERCURY_C8B5")
        );
    }

    #[test]
    fn parse_sec() {
        assert_eq!(parse_sec_token("wpa2-psk"), Some(WifiSecurity::Wpa2));
        assert_eq!(parse_sec_token("wpa3-sae^"), Some(WifiSecurity::Wpa3));
        assert_eq!(parse_sec_token("open"), Some(WifiSecurity::Open));
    }
}
