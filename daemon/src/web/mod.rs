pub struct StatusSnapshot {
    pub rssi: i32,
    pub score: f32,
    pub band: String,
    pub state: String,
    pub ssid: String,
    /// wpa 连接态或 PAUSE；sm 态也写这里便于面板观察
    pub power_state: String,
    /// 最近一次切换/配置错误（空=无）
    pub last_error: String,
    /// 当前生效的阈值摘要（便于面板展示，无需再拉 /api/config）
    pub thresholds: ThresholdsView,
    /// 保护剩余秒数（0=未在保护）；手切或观影
    pub hold_remaining_secs: u64,
    /// 保护种类："" | "manual" | "soft_pause"
    pub hold_kind: String,
    /// 配置的手切保护时长（便于设置页展示）
    pub user_hold_secs: u64,
    /// wpa 链路控制：ok / reconnect / fail
    pub link_ctrl: String,
    /// 家网 AP 数量（0=未配置，走启发式）
    pub home_ap_count: usize,
    /// 当前链路是否在家网内（未配置家网时恒 true）
    pub in_home: bool,
    /// 中文原因条：为何不切 / 当前阻塞
    pub block_reason: String,
    /// 惩罚冷却剩余秒
    pub penalty_remaining_secs: u64,
    /// 屏幕：ON / OFF
    pub screen: String,
    /// 最近 L3 结果：ok / fail / skip / ""
    pub l3_last: String,
    /// 当前 BSSID（一键入家网用）
    pub bssid: String,
    /// 阈值对照人话：调阈值后应能在这里感到变化
    pub threshold_hint: String,
    /// 最近一次扫描到的家网 5G 最强 RSSI（无则 null）
    pub best_5g_rssi: Option<i32>,
    /// 状态页一句人话
    pub summary: String,
    /// 切后 BSSID 短锁剩余秒（0=未锁）
    pub bssid_lock_remaining_secs: u64,
    /// 守护进程版本号（编译时注入）
    pub version: String,
}

#[derive(Debug, Clone, Default)]
pub struct ThresholdsView {
    pub score_detect_threshold: f32,
    pub score_switch_threshold: f32,
    pub upswitch_rssi_min_dbm: i32,
    pub mode: String,
}

/// 切换历史一条
#[derive(Debug, Clone)]
pub struct SwitchEvent {
    pub ts_unix: u64,
    pub from_ssid: String,
    pub to_ssid: String,
    pub from_band: String,
    pub to_band: String,
    pub reason: String,
    pub result: String,
    pub duration_ms: u64,
}

impl SwitchEvent {
    /// 单条 JSON 序列化
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"ts_unix":{},"from_ssid":"{}","to_ssid":"{}","from_band":"{}","to_band":"{}","reason":"{}","result":"{}","duration_ms":{}}}"#,
            self.ts_unix, json_esc(&self.from_ssid), json_esc(&self.to_ssid),
            json_esc(&self.from_band), json_esc(&self.to_band),
            json_esc(&self.reason), json_esc(&self.result), self.duration_ms,
        )
    }
}

/// 将 Vec<SwitchEvent> 序列化为 JSON 数组字符串
pub fn events_to_json(events: &[SwitchEvent]) -> String {
    let mut out = String::with_capacity(events.len() * 120);
    out.push('[');
    for (i, ev) in events.iter().enumerate() {
        if i > 0 { out.push(','); }
        out.push_str(&ev.to_json());
    }
    out.push(']');
    out
}

/// 从 JSON 数组字符串解析 Vec<SwitchEvent>（简易解析，不依赖 serde_json）
pub fn events_from_json(raw: &str) -> Vec<SwitchEvent> {
    let mut events = Vec::new();
    let mut pos = 0;
    let bytes = raw.as_bytes();
    // 跳过开头的 [ 和空白
    while pos < bytes.len() && bytes[pos] != b'[' { pos += 1; }
    pos += 1; // skip [
    loop {
        // 跳过空白
        while pos < bytes.len() && (bytes[pos] == b' ' || bytes[pos] == b'\n' || bytes[pos] == b'\r' || bytes[pos] == b'\t') { pos += 1; }
        if pos >= bytes.len() || bytes[pos] == b']' { break; }
        // 跳过 {
        while pos < bytes.len() && bytes[pos] != b'{' { pos += 1; }
        pos += 1; // skip {
        let mut ev = SwitchEvent {
            ts_unix: 0, from_ssid: String::new(), to_ssid: String::new(),
            from_band: String::new(), to_band: String::new(),
            reason: String::new(), result: String::new(), duration_ms: 0,
        };
        // 解析键值对
        loop {
            while pos < bytes.len() && bytes[pos] != b'"' && bytes[pos] != b'}' { pos += 1; }
            if pos >= bytes.len() || bytes[pos] == b'}' { pos += 1; break; }
            // 读 key
            pos += 1; // skip opening "
            let key_start = pos;
            while pos < bytes.len() && bytes[pos] != b'"' { pos += 1; }
            let key = &raw[key_start..pos];
            pos += 1; // skip closing "
            // 跳过 :
            while pos < bytes.len() && bytes[pos] != b':' { pos += 1; }
            pos += 1; // skip :
            // 跳过空白
            while pos < bytes.len() && (bytes[pos] == b' ' || bytes[pos] == b'\n' || bytes[pos] == b'\r' || bytes[pos] == b'\t') { pos += 1; }
            // 读 value
            if pos < bytes.len() && bytes[pos] == b'"' {
                // 字符串
                pos += 1;
                let val_start = pos;
                while pos < bytes.len() && bytes[pos] != b'"' {
                    if bytes[pos] == b'\\' { pos += 1; } // skip escaped char
                    pos += 1;
                }
                let val = raw[val_start..pos].replace("\\\"", "\"").replace("\\\\", "\\");
                pos += 1; // skip closing "
                match key {
                    "from_ssid" => ev.from_ssid = val,
                    "to_ssid" => ev.to_ssid = val,
                    "from_band" => ev.from_band = val,
                    "to_band" => ev.to_band = val,
                    "reason" => ev.reason = val,
                    "result" => ev.result = val,
                    _ => {}
                }
            } else {
                // 数字
                let val_start = pos;
                while pos < bytes.len() && bytes[pos] != b',' && bytes[pos] != b'}' && bytes[pos] != b' ' { pos += 1; }
                let val_str = &raw[val_start..pos];
                match key {
                    "ts_unix" => ev.ts_unix = val_str.parse().unwrap_or(0),
                    "duration_ms" => ev.duration_ms = val_str.parse().unwrap_or(0),
                    _ => {}
                }
            }
            // 跳过 ,
            while pos < bytes.len() && bytes[pos] != b',' && bytes[pos] != b'}' { pos += 1; }
            if pos < bytes.len() && bytes[pos] == b',' { pos += 1; }
        }
        events.push(ev);
    }
    events
}

/// 就绪检查一步
#[derive(Debug, Clone)]
pub struct ReadyStep {
    pub id: String,
    pub ok: bool,
    pub title: String,
    pub hint: String,
}

impl ReadyStep {
    fn to_json(&self) -> String {
        format!(
            r#"{{"id":"{}","ok":{},"title":"{}","hint":"{}"}}"#,
            json_esc(&self.id), self.ok, json_esc(&self.title), json_esc(&self.hint),
        )
    }
}

#[derive(Debug, Clone)]
pub struct Readiness {
    pub persisted: bool,
    pub home_configured: bool,
    pub home_ap_count: usize,
    pub saved_ssids: Vec<String>,
    pub steps: Vec<ReadyStep>,
    pub block_reason: String,
}

impl Readiness {
    pub fn to_json(&self) -> String {
        let mut steps = String::from('[');
        for (i, step) in self.steps.iter().enumerate() {
            if i > 0 { steps.push(','); }
            steps.push_str(&step.to_json());
        }
        steps.push(']');
        let mut ssids = String::from('[');
        for (i, s) in self.saved_ssids.iter().enumerate() {
            if i > 0 { ssids.push(','); }
            ssids.push('"');
            ssids.push_str(&json_esc(s));
            ssids.push('"');
        }
        ssids.push(']');
        format!(
            r#"{{"persisted":{},"home_configured":{},"home_ap_count":{},"saved_ssids":{},"steps":{},"block_reason":"{}"}}"#,
            self.persisted, self.home_configured, self.home_ap_count, ssids, steps,
            json_esc(&self.block_reason),
        )
    }
}

impl StatusSnapshot {
    pub fn new() -> Self {
        let d = crate::config::Config::default();
        Self {
            rssi: -65,
            score: 50.0,
            band: "2.4".into(),
            state: "DISCONNECTED".into(),
            ssid: String::new(),
            power_state: "ON".into(),
            last_error: String::new(),
            thresholds: ThresholdsView {
                score_detect_threshold: d.score_detect_threshold,
                score_switch_threshold: d.score_switch_threshold,
                upswitch_rssi_min_dbm: d.upswitch_rssi_min_dbm,
                mode: d.mode,
            },
            hold_remaining_secs: 0,
            hold_kind: String::new(),
            user_hold_secs: d.user_hold_secs,
            link_ctrl: "ok".into(),
            home_ap_count: 0,
            in_home: true,
            block_reason: String::new(),
            penalty_remaining_secs: 0,
            screen: "ON".into(),
            l3_last: String::new(),
            bssid: String::new(),
            threshold_hint: String::new(),
            best_5g_rssi: None,
            summary: String::new(),
            bssid_lock_remaining_secs: 0,
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    /// 手动拼 JSON，不依赖 serde_json
    pub fn to_json(&self) -> String {
        let th = &self.thresholds;
        // ponytail: manual JSON, ~50% less codegen than serde_json
        format!(
            r#"{{"rssi":{},"score":{},"band":"{}","state":"{}","ssid":"{}","power_state":"{}","last_error":"{}","thresholds":{{"score_detect_threshold":{},"score_switch_threshold":{},"upswitch_rssi_min_dbm":{},"mode":"{}"}},"hold_remaining_secs":{},"hold_kind":"{}","user_hold_secs":{},"link_ctrl":"{}","home_ap_count":{},"in_home":{},"block_reason":"{}","penalty_remaining_secs":{},"screen":"{}","l3_last":"{}","bssid":"{}","threshold_hint":"{}","best_5g_rssi":{},"summary":"{}","bssid_lock_remaining_secs":{},"version":"{}"}}"#,
            self.rssi, self.score, json_esc(&self.band), json_esc(&self.state),
            json_esc(&self.ssid), json_esc(&self.power_state), json_esc(&self.last_error),
            th.score_detect_threshold, th.score_switch_threshold, th.upswitch_rssi_min_dbm,
            json_esc(&th.mode),
            self.hold_remaining_secs, json_esc(&self.hold_kind), self.user_hold_secs,
            json_esc(&self.link_ctrl), self.home_ap_count, self.in_home,
            json_esc(&self.block_reason), self.penalty_remaining_secs,
            json_esc(&self.screen), json_esc(&self.l3_last), json_esc(&self.bssid),
            json_esc(&self.threshold_hint),
            match self.best_5g_rssi { Some(v) => v.to_string(), None => "null".into() },
            json_esc(&self.summary), self.bssid_lock_remaining_secs, json_esc(&self.version),
        )
    }
}

/// JSON 字符串转义（" → \"，\n → \\n，\t → \\t）
pub fn json_esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
