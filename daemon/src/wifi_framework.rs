//! 经 Android 框架切网 / 枚举已保存网络
//! 小米等机型 wpa LIST_NETWORKS 往往只含当前网；真源在 WifiConfigStore + `cmd wifi list-networks`

use std::fs;
use std::process::Command;

const STORE_PATHS: &[&str] = &[
    "/data/misc/apexdata/com.android.wifi/WifiConfigStore.xml",
    "/data/misc/wifi/WifiConfigStore.xml",
];

/// `cmd wifi list-networks` 解析出的 SSID 列表（去重）
pub fn ssids_from_cmd_list() -> Vec<String> {
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
    let mut ssids = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Network") {
            continue;
        }
        // "7            MERCURY_5G_C8B5                  wpa2-psk"
        let rest = line
            .split_whitespace()
            .skip(1)
            .collect::<Vec<_>>();
        if rest.is_empty() {
            continue;
        }
        // 末尾 security 可能是 wpa2-psk / open / wpa3-sae^
        let mut parts = rest;
        if parts.len() >= 2 {
            let last = parts.last().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            if last.contains("wpa") || last == "open" || last == "owe" || last == "owe^" || last.ends_with('^') {
                parts.pop();
            }
        }
        let ssid = parts.join(" ");
        if ssid.is_empty() {
            continue;
        }
        if !ssids.iter().any(|s| s == &ssid) {
            ssids.push(ssid);
        }
    }
    ssids
}

/// 从 WifiConfigStore.xml 抽 SSID（不读密码到日志）
pub fn ssids_from_config_store() -> Vec<String> {
    let raw = read_store_raw();
    let Some(raw) = raw else {
        return Vec::new();
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
    // <string name="SSID">&quot;MERCURY_C8B5&quot;</string>
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
    // 内容形如 &quot;NAME&quot; 或 "NAME"
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

fn psk_from_store(ssid: &str) -> Option<String> {
    let raw = read_store_raw()?;
    // 在对应 Network 块内找 PreSharedKey；简化：按 SSID 行后若干行内找
    let needle = format!("&quot;{ssid}&quot;");
    let needle2 = format!("\"{ssid}\"");
    let lines: Vec<&str> = raw.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !(line.contains("name=\"SSID\"") || line.contains("name='SSID'")) {
            continue;
        }
        let is_match = line.contains(&needle) || line.contains(&needle2);
        if !is_match {
            // 也允许已解码比较
            if extract_quoted_xml_string(line).as_deref() != Some(ssid) {
                continue;
            }
        }
        for j in i..lines.len().min(i + 40) {
            let l = lines[j];
            if l.contains("name=\"PreSharedKey\"") || l.contains("name='PreSharedKey'") {
                if let Some(p) = extract_quoted_xml_string(l) {
                    if !p.is_empty() {
                        return Some(p);
                    }
                }
            }
            // 下一个 Network 开始则停
            if j > i && l.contains("<WifiConfiguration>") {
                break;
            }
        }
    }
    None
}

/// 框架切网：`cmd wifi connect-network <ssid> wpa2 <psk> [-b bssid]`
/// 成功条件由调用方读 wpa status 验证；这里只看命令是否报错
pub fn framework_connect(ssid: &str, bssid: Option<&str>) -> Result<(), String> {
    let psk = psk_from_store(ssid).ok_or_else(|| {
        format!("WifiConfigStore 无「{ssid}」密码（请用系统设置保存过该网）")
    })?;
    // 不把 psk 写入日志
    let mut args = vec![
        "wifi".into(),
        "connect-network".into(),
        ssid.to_string(),
        "wpa2".into(),
        psk,
    ];
    if let Some(b) = bssid {
        if !b.is_empty() {
            args.push("-b".into());
            args.push(b.to_string());
        }
    }
    let out = Command::new("cmd")
        .args(&args)
        .output()
        .map_err(|e| format!("cmd wifi: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let msg = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        // 脱敏：若消息含密码则截断
        return Err(format!("framework connect fail: {msg}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ssid_line() {
        let line = r#"<string name="SSID">&quot;MERCURY_C8B5&quot;</string>"#;
        assert_eq!(extract_quoted_xml_string(line).as_deref(), Some("MERCURY_C8B5"));
    }
}
