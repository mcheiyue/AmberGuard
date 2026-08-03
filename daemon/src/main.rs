use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tiny_http::{Header, Method, Response, Server, StatusCode};

use crate::band_bond::{
    best_on_band, dual_band_pair_saved, home_contains, link_in_home, merge_scan_aps,
    network_id_for_ssid, parse_cmd_scan_results, parse_list_networks, parse_scan_results,
    scan_views,
};
use crate::config::{Config, ConfigPatch};
use crate::health_score::health_score;
use crate::power_state::{PowerState, PowerStateManager};
use crate::state_machine::{StateMachine, SwitchHint};
use crate::station_info::{iw_station_dump, parse_iw_station, retry_rate, StationSample};
use crate::web::{Readiness, ReadyStep, StatusSnapshot, SwitchEvent, ThresholdsView};
use crate::wpa_ctrl::WpaCtrl;

const HISTORY_CAP: usize = 10;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 运行模式中文
fn mode_zh(mode: &str) -> &'static str {
    match mode {
        "eco" => "省电",
        "pause" => "暂停",
        _ => "日用",
    }
}

/// 守护状态中文（与面板 labelSm 对齐）
fn power_zh(ps: &str) -> String {
    let p = ps.trim();
    if p.starts_with("USER_HOLD") {
        return "手动保护中".into();
    }
    match p {
        "Idle" | "idle" => "守护中".into(),
        "GradientDetect" => "观察中".into(),
        "Switching" => "切换中".into(),
        "Penalty" => "冷却中".into(),
        "Frozen" => "冻结".into(),
        "PAUSE" | "Pause" => "已暂停".into(),
        "OUT_OF_HOME" => "非家网".into(),
        "WEAK_OFF" => "弱信号已断".into(),
        "SCREEN_OFF" => "息屏降频".into(),
        "" => "—".into(),
        other => other.to_string(),
    }
}

fn band_zh(band: &str) -> &'static str {
    match band {
        "5" => "5G",
        "2.4" => "2.4G",
        _ => "未知频段",
    }
}

/// 状态页「阈值对照」人话：调三个阈值后应能直接读到差异
fn threshold_hint_zh(
    on_preferred: bool,
    score: f32,
    rssi: i32,
    switch_th: f32,
    detect_th: f32,
    up_rssi: i32,
    best_pref: Option<i32>,
) -> String {
    if on_preferred {
        if score < switch_th {
            format!(
                "偏好下切带：分 {score:.0} < 下切线 {switch_th:.0}（{rssi} dBm）→ 防抖后切后备频段"
            )
        } else if score < detect_th {
            format!(
                "偏好观察带：下切线 {switch_th:.0} ≤ 分 {score:.0} < 观察线 {detect_th:.0} → 加勤扫描"
            )
        } else {
            format!(
                "偏好稳定带：分 {score:.0} ≥ 观察线 {detect_th:.0}（{rssi} dBm）→ 守护中"
            )
        }
    } else {
        match best_pref {
            Some(b) if b >= up_rssi => format!(
                "后备频段·上切就绪：偏好侧最强 {b} dBm ≥ 上切线 {up_rssi} → 防抖后回偏好"
            ),
            Some(b) => format!(
                "后备频段·等待：偏好侧最强 {b} dBm < 上切线 {up_rssi}"
            ),
            None => format!(
                "后备频段·寻找偏好 AP（需 ≥ {up_rssi} dBm）"
            ),
        }
    }
}

/// 面具列表 / status.txt 用的中文一行（模块实时信息）
fn status_line_zh(
    mode: &str,
    ssid: &str,
    band: &str,
    score: f32,
    power_state: &str,
    block_reason: &str,
) -> String {
    let ssid_show = if ssid.is_empty() { "未连接" } else { ssid };
    let st = if !block_reason.is_empty() {
        block_reason.to_string()
    } else {
        power_zh(power_state)
    };
    format!(
        "AmberGuard · {} · {} · {} · 分{:.0} · {}",
        mode_zh(mode),
        ssid_show,
        band_zh(band),
        score,
        st
    )
}

/// 把一行状态写入模块 module.prop 的 description=（面具列表展示）
fn update_module_description(line: &str) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = unix_now();
    let prev = LAST.load(Ordering::Relaxed);
    if now.saturating_sub(prev) < 15 {
        return;
    }
    LAST.store(now, Ordering::Relaxed);

    let prop = module_prop_path();
    let Some(prop) = prop else {
        return;
    };
    let Ok(raw) = std::fs::read_to_string(&prop) else {
        return;
    };
    // 单行、去掉 description 非法字符
    let desc: String = line
        .chars()
        .map(|c| match c {
            '\n' | '\r' | '#' | '=' => ' ',
            c => c,
        })
        .take(96)
        .collect::<String>()
        .trim()
        .to_string();
    if desc.is_empty() {
        return;
    }
    let mut out = String::with_capacity(raw.len() + 32);
    let mut replaced = false;
    for l in raw.lines() {
        if l.starts_with("description=") {
            out.push_str("description=");
            out.push_str(&desc);
            out.push('\n');
            replaced = true;
        } else {
            out.push_str(l);
            out.push('\n');
        }
    }
    if !replaced {
        out.push_str("description=");
        out.push_str(&desc);
        out.push('\n');
    }
    let _ = std::fs::write(prop, out);
}

fn module_prop_path() -> Option<std::path::PathBuf> {
    // /data/adb/modules/AmberGuard/bin/amberguard → ../module.prop
    let exe = std::fs::read_link("/proc/self/exe").ok()?;
    let mod_dir = exe.parent()?.parent()?;
    let p = mod_dir.join("module.prop");
    if p.is_file() {
        Some(p)
    } else {
        let fallback = std::path::PathBuf::from("/data/adb/modules/AmberGuard/module.prop");
        if fallback.is_file() {
            Some(fallback)
        } else {
            None
        }
    }
}

fn push_history(hist: &Arc<Mutex<VecDeque<SwitchEvent>>>, ev: SwitchEvent) {
    if let Ok(mut h) = hist.lock() {
        h.push_front(ev);
        while h.len() > HISTORY_CAP {
            h.pop_back();
        }
    }
}

/// 切后是否真到了目标。
/// 异名双频：SSID 一致即可（家网常有多 BSSID，钉死 BSSID 会误判失败并狂重试→系统停用网络）。
/// 同 SSID 漫游：有目标 BSSID 时再比 BSSID。
fn link_reached_peer(
    st: &crate::wpa_ctrl::WpaStatus,
    peer_ssid: &str,
    peer_bssid: &str,
    require_bssid: bool,
) -> bool {
    if st.wpa_state != "COMPLETED" {
        return false;
    }
    let got_ssid = st.ssid.as_deref().unwrap_or("");
    if got_ssid.is_empty() || got_ssid != peer_ssid {
        return false;
    }
    if !require_bssid {
        return true;
    }
    let want_b = peer_bssid.trim();
    if want_b.is_empty() {
        return true;
    }
    st.bssid
        .as_deref()
        .map(|b| b.eq_ignore_ascii_case(want_b))
        .unwrap_or(false)
}

/// L3：多终点探测。国内 gstatic 常 DNS 失败，不能因此判切网失败。
/// 成功：任一 HTTP 204/200/30x，或 TCP 能通公共 DNS 端口（有出网能力）。
fn l3_probe(timeout: Duration) -> Result<(), String> {
    // (host_or_none_for_ip, port, http_path_or_empty, host_header)
    let http_targets: &[(&str, u16, &str, &str)] = &[
        (
            "connectivitycheck.gstatic.com",
            80,
            "/generate_204",
            "connectivitycheck.gstatic.com",
        ),
        (
            "connect.rom.miui.com",
            80,
            "/generate_204",
            "connect.rom.miui.com",
        ),
        ("www.msftconnecttest.com", 80, "/connecttest.txt", "www.msftconnecttest.com"),
    ];
    let mut last_err = String::from("无终点");
    for (host, port, path, hdr) in http_targets {
        match l3_http_one(host, *port, path, hdr, timeout) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e,
        }
    }
    // DNS 全挂时：TCP 探测有出网即可（223.5.5.5 / 1.1.1.1）
    for ip in &["223.5.5.5:53", "1.1.1.1:80", "8.8.8.8:53"] {
        if let Ok(addr) = ip.parse() {
            if TcpStream::connect_timeout(&addr, timeout).is_ok() {
                return Ok(());
            }
        }
    }
    Err(last_err)
}

fn l3_http_one(
    host: &str,
    port: u16,
    path: &str,
    host_hdr: &str,
    timeout: Duration,
) -> Result<(), String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("DNS {host}: {e}"))?
        .next()
        .ok_or_else(|| format!("DNS {host}: 无结果"))?;
    let mut stream =
        TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("connect {host}: {e}"))?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let req = format!(
        "GET {path} HTTP/1.0\r\nHost: {host_hdr}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut buf = [0u8; 128];
    let n = stream.read(&mut buf).map_err(|e| format!("read: {e}"))?;
    if n == 0 {
        return Err("空响应".into());
    }
    let head = String::from_utf8_lossy(&buf[..n]);
    let line = head.lines().next().unwrap_or("");
    if line.contains(" 204") || line.contains(" 200") || line.contains(" 30") {
        Ok(())
    } else if line.contains("HTTP/") {
        Err(format!("状态行: {line}"))
    } else {
        Err(format!("非 HTTP: {line}"))
    }
}

mod band_bond;
mod config;
mod file_log;
mod health_score;
mod power_state;
mod scanner;
mod state_machine;
mod station_info;
mod web;
mod wifi_framework;
mod wpa_ctrl;

fn json_resp(body: String, code: StatusCode) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut r = Response::from_string(body).with_status_code(code);
    for kv in &[
        (&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]),
        (&b"Access-Control-Allow-Origin"[..], &b"*"[..]),
    ] {
        if let Ok(h) = Header::from_bytes(kv.0, kv.1) {
            r.add_header(h);
        }
    }
    r
}

fn read_body(req: &mut tiny_http::Request) -> String {
    let mut buf = String::new();
    let _ = req.as_reader().read_to_string(&mut buf);
    buf
}

fn main() {
    let config = Arc::new(Mutex::new(Config::load().expect("config load")));
    {
        let c = config.lock().unwrap();
        file_log::init(&c.log_level);
        log::info!("AmberGuard Phase 4 daemon started");
        log::info!("log file: {}", file_log::log_path_display());
        log::info!(
            "Interface: {}, Listen: {}, mode={}, detect={}, switch={}, up_rssi={}, log={}",
            c.interface,
            c.listen,
            c.mode,
            c.score_detect_threshold,
            c.score_switch_threshold,
            c.upswitch_rssi_min_dbm,
            c.log_level
        );
    }

    let wpa = match WpaCtrl::auto_connect() {
        Ok(w) => {
            log::info!("wpa_supplicant connected");
            w
        }
        Err(e) => {
            log::error!("wpa_supplicant connect failed: {e} — offline mode");
            offline_loop();
        }
    };
    let wpa = Arc::new(Mutex::new(wpa));
    let snapshot = Arc::new(Mutex::new(StatusSnapshot::new()));
    // 手动切网保护截止时间；HTTP 可 clear / soft-pause
    let hold_until: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let history: Arc<Mutex<VecDeque<SwitchEvent>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(HISTORY_CAP)));
    let mut sm = StateMachine::new();
    let mut power = PowerStateManager::new();

    let listen = config.lock().unwrap().listen.clone();
    let addr: SocketAddr = listen.parse().expect("bad listen address");
    let server = Server::http(addr).expect("listen");
    log::info!("HTTP server listening on {addr}");

    let snapshot_http = Arc::clone(&snapshot);
    let config_http = Arc::clone(&config);
    let hold_http = Arc::clone(&hold_until);
    let wpa_http = Arc::clone(&wpa);
    let history_http = Arc::clone(&history);
    thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let url = req.url().to_string();
            let method = req.method().clone();
            // 去掉 query
            let path = url.split('?').next().unwrap_or(&url);

            let resp = match (method, path) {
                (Method::Get, "/api/status") => {
                    let s = snapshot_http.lock().unwrap();
                    let json = serde_json::to_string(&*s).unwrap_or_else(|_| "{}".into());
                    json_resp(json, StatusCode(200))
                }
                (Method::Get, "/api/history") => {
                    let h = history_http.lock().unwrap();
                    let list: Vec<&SwitchEvent> = h.iter().collect();
                    let body = serde_json::json!({ "ok": true, "events": list });
                    json_resp(body.to_string(), StatusCode(200))
                }
                (Method::Get, "/api/readiness") => {
                    let c = config_http.lock().unwrap();
                    let persisted = Config::is_persisted();
                    let home_n = c.home_aps.len();
                    let saved = {
                        let raw = wpa_http
                            .lock()
                            .ok()
                            .and_then(|w| w.list_networks().ok())
                            .unwrap_or_default();
                        let wpa_only: Vec<String> = parse_list_networks(&raw)
                            .into_iter()
                            .map(|(_, s)| s)
                            .filter(|s| !s.is_empty())
                            .collect();
                        // 小米 wpa LIST 常只有当前网；合并 cmd/WifiConfigStore
                        wifi_framework::merge_saved_ssids(&wpa_only)
                    };
                    let (dual_ok, dual_hint) = dual_band_pair_saved(&saved);
                    let snap = snapshot_http.lock().unwrap();
                    let steps = vec![
                        ReadyStep {
                            id: "persist".into(),
                            ok: persisted,
                            title: "保存配置".into(),
                            hint: if persisted {
                                "config.toml 已存在".into()
                            } else {
                                "点下方「初始化」写入默认并落盘".into()
                            },
                        },
                        ReadyStep {
                            id: "system_dual".into(),
                            ok: dual_ok,
                            title: "系统已保存双频网络".into(),
                            hint: dual_hint,
                        },
                        ReadyStep {
                            id: "home".into(),
                            ok: home_n > 0,
                            title: "勾选家网 AP（建议）".into(),
                            hint: if home_n > 0 {
                                format!("已选 {home_n} 个")
                            } else {
                                "可选；多路由强烈建议。空=不限制".into()
                            },
                        },
                    ];
                    let body = Readiness {
                        persisted,
                        home_configured: home_n > 0,
                        home_ap_count: home_n,
                        saved_ssids: saved,
                        steps,
                        block_reason: snap.block_reason.clone(),
                    };
                    json_resp(
                        serde_json::to_string(&body).unwrap_or_else(|_| "{}".into()),
                        StatusCode(200),
                    )
                }
                (Method::Get, "/api/config") => {
                    let c = config_http.lock().unwrap();
                    let body = serde_json::to_string(&Config::api_response(&c))
                        .unwrap_or_else(|_| "{}".into());
                    json_resp(body, StatusCode(200))
                }
                (Method::Post, "/api/config") | (Method::Put, "/api/config") => {
                    let body = read_body(&mut req);
                    match serde_json::from_str::<ConfigPatch>(&body) {
                        Ok(patch) => {
                            let mut c = config_http.lock().unwrap();
                            match c.apply_patch(patch) {
                                Ok(()) => match c.save() {
                                    Ok(()) => {
                                        file_log::set_level(&c.log_level);
                                        log::info!(
                                            "config updated: detect={} switch={} up_rssi={} mode={} log={}",
                                            c.score_detect_threshold,
                                            c.score_switch_threshold,
                                            c.upswitch_rssi_min_dbm,
                                            c.mode,
                                            c.log_level
                                        );
                                        // 同步 mode 到 snapshot
                                        if let Ok(mut s) = snapshot_http.lock() {
                                            if c.mode == "pause" {
                                                s.power_state = "PAUSE".into();
                                            } else if s.power_state == "PAUSE" {
                                                s.power_state = "ON".into();
                                            }
                                            s.thresholds = ThresholdsView {
                                                score_detect_threshold: c.score_detect_threshold,
                                                score_switch_threshold: c.score_switch_threshold,
                                                upswitch_rssi_min_dbm: c.upswitch_rssi_min_dbm,
                                                mode: c.mode.clone(),
                                            };
                                            s.last_error.clear();
                                        }
                                        let body = serde_json::to_string(&Config::api_response(&c))
                                            .unwrap_or_else(|_| "{}".into());
                                        json_resp(body, StatusCode(200))
                                    }
                                    Err(e) => json_resp(
                                        format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
                                        StatusCode(500),
                                    ),
                                },
                                Err(e) => json_resp(
                                    format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
                                    StatusCode(400),
                                ),
                            }
                        }
                        Err(e) => json_resp(
                            format!("{{\"ok\":false,\"error\":\"JSON: {e}\"}}"),
                            StatusCode(400),
                        ),
                    }
                }
                (Method::Post, "/api/config/preset/daily")
                | (Method::Post, "/api/config/preset/stable")
                | (Method::Post, "/api/config/preset/sensitive")
                | (Method::Get, "/api/config/preset/daily")
                | (Method::Get, "/api/config/preset/stable")
                | (Method::Get, "/api/config/preset/sensitive") => {
                    let id = path.rsplit('/').next().unwrap_or("daily");
                    let mut c = config_http.lock().unwrap();
                    match c.apply_preset(id).and_then(|_| c.save()) {
                        Ok(()) => {
                            if let Ok(mut s) = snapshot_http.lock() {
                                s.thresholds = ThresholdsView {
                                    score_detect_threshold: c.score_detect_threshold,
                                    score_switch_threshold: c.score_switch_threshold,
                                    upswitch_rssi_min_dbm: c.upswitch_rssi_min_dbm,
                                    mode: c.mode.clone(),
                                };
                            }
                            let body = serde_json::to_string(&Config::api_response(&c))
                                .unwrap_or_else(|_| "{}".into());
                            json_resp(body, StatusCode(200))
                        }
                        Err(e) => json_resp(
                            format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
                            StatusCode(400),
                        ),
                    }
                }
                (Method::Get, "/api/mode/pause") | (Method::Post, "/api/mode/pause") => {
                    let mut c = config_http.lock().unwrap();
                    c.mode = "pause".into();
                    let _ = c.save();
                    if let Ok(mut s) = snapshot_http.lock() {
                        s.power_state = "PAUSE".into();
                        s.thresholds.mode = "pause".into();
                    }
                    json_resp("{\"ok\":true,\"mode\":\"pause\"}".into(), StatusCode(200))
                }
                (Method::Get, "/api/mode/daily") | (Method::Post, "/api/mode/daily") => {
                    let mut c = config_http.lock().unwrap();
                    c.mode = "daily".into();
                    let _ = c.save();
                    if let Ok(mut s) = snapshot_http.lock() {
                        s.power_state = "ON".into();
                        s.thresholds.mode = "daily".into();
                    }
                    json_resp("{\"ok\":true,\"mode\":\"daily\"}".into(), StatusCode(200))
                }
                (Method::Get, "/api/mode/eco") | (Method::Post, "/api/mode/eco") => {
                    let mut c = config_http.lock().unwrap();
                    c.mode = "eco".into();
                    let _ = c.save();
                    if let Ok(mut s) = snapshot_http.lock() {
                        s.power_state = "ON".into();
                        s.thresholds.mode = "eco".into();
                    }
                    json_resp("{\"ok\":true,\"mode\":\"eco\"}".into(), StatusCode(200))
                }
                (Method::Get, "/api/hold/clear") | (Method::Post, "/api/hold/clear") => {
                    if let Ok(mut h) = hold_http.lock() {
                        *h = None;
                    }
                    if let Ok(mut s) = snapshot_http.lock() {
                        s.hold_remaining_secs = 0;
                    }
                    log::info!("user_hold cleared via API");
                    json_resp("{\"ok\":true,\"hold_remaining_secs\":0}".into(), StatusCode(200))
                }
                (Method::Get, "/api/soft-pause") | (Method::Post, "/api/soft-pause") => {
                    // ?mins=20 默认 20
                    let mins = url
                        .split('?')
                        .nth(1)
                        .and_then(|q| {
                            q.split('&').find_map(|p| {
                                let mut kv = p.splitn(2, '=');
                                match (kv.next(), kv.next()) {
                                    (Some("mins"), Some(v)) => v.parse::<u64>().ok(),
                                    _ => None,
                                }
                            })
                        })
                        .unwrap_or(20)
                        .clamp(1, 180);
                    let until = Instant::now() + Duration::from_secs(mins * 60);
                    *hold_http.lock().unwrap() = Some(until);
                    log::info!("soft-pause {mins} min via API");
                    json_resp(
                        format!("{{\"ok\":true,\"mins\":{mins},\"hold_remaining_secs\":{}}}", mins * 60),
                        StatusCode(200),
                    )
                }
                (Method::Get, "/api/scan") | (Method::Post, "/api/scan") => {
                    let home = config_http.lock().unwrap().home_aps.clone();
                    let scan_res = (|| -> Result<Vec<crate::band_bond::ScanApView>, String> {
                        {
                            let w = wpa_http.lock().map_err(|e| e.to_string())?;
                            let _ = w.command("SCAN");
                        }
                        // 框架扫描：中文 SSID 更完整
                        let _ = std::process::Command::new("cmd")
                            .args(["wifi", "start-scan"])
                            .output();
                        thread::sleep(Duration::from_millis(1500));
                        let wpa_aps = {
                            let w = wpa_http.lock().map_err(|e| e.to_string())?;
                            let raw = w.scan_results().unwrap_or_default();
                            parse_scan_results(&raw)
                        };
                        let cmd_aps = {
                            let out = std::process::Command::new("cmd")
                                .args(["wifi", "list-scan-results"])
                                .output()
                                .ok();
                            out.and_then(|o| {
                                if o.status.success() {
                                    Some(parse_cmd_scan_results(&String::from_utf8_lossy(
                                        &o.stdout,
                                    )))
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_default()
                        };
                        let aps = merge_scan_aps(wpa_aps, cmd_aps);
                        Ok(scan_views(&aps, &home))
                    })();
                    match scan_res {
                        Ok(list) => {
                            let body = serde_json::json!({
                                "ok": true,
                                "count": list.len(),
                                "aps": list,
                                "home_ap_count": home.len(),
                            });
                            json_resp(body.to_string(), StatusCode(200))
                        }
                        Err(e) => json_resp(
                            format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
                            StatusCode(500),
                        ),
                    }
                }
                (Method::Get, "/api/init-config") | (Method::Post, "/api/init-config") => {
                    match Config::init_if_missing() {
                        Ok(written) => {
                            if written {
                                if let Ok(mut c) = config_http.lock() {
                                    let _ = c.reload();
                                    // 同步快照阈值
                                    if let Ok(mut s) = snapshot_http.lock() {
                                        s.thresholds = ThresholdsView {
                                            score_detect_threshold: c.score_detect_threshold,
                                            score_switch_threshold: c.score_switch_threshold,
                                            upswitch_rssi_min_dbm: c.upswitch_rssi_min_dbm,
                                            mode: c.mode.clone(),
                                        };
                                        s.user_hold_secs = c.user_hold_secs;
                                    }
                                }
                                log::info!("配置初始化完成（已落盘日用默认）");
                            }
                            let body = serde_json::json!({
                                "ok": true,
                                "initialized": written,
                                "persisted": Config::is_persisted(),
                            });
                            json_resp(body.to_string(), StatusCode(200))
                        }
                        Err(e) => json_resp(
                            format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
                            StatusCode(500),
                        ),
                    }
                }
                (Method::Get, "/api/logs") => {
                    // ?lines=200
                    let lines = url
                        .split('?')
                        .nth(1)
                        .and_then(|q| {
                            q.split('&').find_map(|p| {
                                let mut kv = p.splitn(2, '=');
                                match (kv.next(), kv.next()) {
                                    (Some("lines"), Some(v)) => v.parse::<usize>().ok(),
                                    _ => None,
                                }
                            })
                        })
                        .unwrap_or(200)
                        .clamp(20, 2000);
                    let body = file_log::tail(lines);
                    let json = serde_json::json!({
                        "ok": true,
                        "path": file_log::log_path_display(),
                        "lines": lines,
                        "content": body,
                    });
                    json_resp(json.to_string(), StatusCode(200))
                }
                (Method::Post, "/api/logs/clear") | (Method::Get, "/api/logs/clear") => {
                    match file_log::clear() {
                        Ok(()) => {
                            log::info!("log cleared via API");
                            json_resp(
                                "{\"ok\":true,\"cleared\":true}".into(),
                                StatusCode(200),
                            )
                        }
                        Err(e) => json_resp(
                            format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
                            StatusCode(500),
                        ),
                    }
                }
                (Method::Get, "/") | (Method::Get, "/index.html") => {
                    let html = include_bytes!("web/static/index.html");
                    let mut r = Response::from_data(html.to_vec());
                    for kv in &[
                        (&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]),
                        (&b"Access-Control-Allow-Origin"[..], &b"*"[..]),
                    ] {
                        if let Ok(h) = Header::from_bytes(kv.0, kv.1) {
                            r.add_header(h);
                        }
                    }
                    r
                }
                (Method::Options, _) => {
                    let mut r = Response::from_string("").with_status_code(StatusCode(204));
                    for kv in &[
                        (&b"Access-Control-Allow-Origin"[..], &b"*"[..]),
                        (&b"Access-Control-Allow-Methods"[..], &b"GET, POST, PUT, OPTIONS"[..]),
                        (&b"Access-Control-Allow-Headers"[..], &b"Content-Type"[..]),
                    ] {
                        if let Ok(h) = Header::from_bytes(kv.0, kv.1) {
                            r.add_header(h);
                        }
                    }
                    r
                }
                _ => {
                    if path.starts_with("/api/") {
                        json_resp(
                            "{\"ok\":false,\"error\":\"not found\"}".into(),
                            StatusCode(404),
                        )
                    } else {
                        let html = include_bytes!("web/static/index.html");
                        Response::from_data(html.to_vec())
                    }
                }
            };
            let _ = req.respond(resp);
        }
    });

    let mut last_scan = Instant::now() - Duration::from_secs(60);
    let mut prev_station = StationSample::default();
                // 链路键 ssid|bssid；daemon 自切后短暂忽略变更（含失败尝试后的回弹）
    let mut prev_link_key = String::new();
    let mut suppress_link_change_until: Option<Instant> = None;
    // 切后短锁 BSSID，防 band-steering 踢回
    let mut lock_bssid_until: Option<Instant> = None;
    let mut screen_on_grace_until: Option<Instant> = None;
    let mut last_screen = PowerState::On;
    // 启动时同步 mode
    let mut last_eco = {
        let c = config.lock().unwrap();
        sm.apply_eco(c.mode == "eco");
        c.mode == "eco"
    };
    {
        let c = config.lock().unwrap();
        if let Ok(mut s) = snapshot.lock() {
            if c.mode == "pause" {
                s.power_state = "PAUSE".into();
            }
            s.user_hold_secs = c.user_hold_secs;
            s.thresholds = ThresholdsView {
                score_detect_threshold: c.score_detect_threshold,
                score_switch_threshold: c.score_switch_threshold,
                upswitch_rssi_min_dbm: c.upswitch_rssi_min_dbm,
                mode: c.mode.clone(),
            };
        }
    }

    let mut weak_bad_since: Option<Instant> = None;
    let mut weak_disconnected = false;
    /// 最近扫描到的家网 5G 最强 RSSI（供阈值对照）
    let mut cached_best_5g: Option<i32> = None;

    loop {
        let (
            paused,
            switch_th,
            detect_th,
            up_rssi,
            bonds,
            home_aps,
            iface,
            mode,
            hold_secs,
            weak_action,
            rssi_disc,
            weak_hold,
            auto_rec,
            l3_on,
            eco,
            preferred_is_5g,
        ) = {
            let c = config.lock().unwrap();
            (
                c.mode == "pause",
                c.score_switch_threshold,
                c.score_detect_threshold,
                c.upswitch_rssi_min_dbm,
                c.bonds.clone(),
                c.home_aps.clone(),
                c.interface.clone(),
                c.mode.clone(),
                c.user_hold_secs,
                c.weak_action.clone(),
                c.rssi_disconnect_dbm,
                c.weak_hold_secs,
                c.auto_reconnect,
                c.l3_probe_enable,
                c.mode == "eco",
                c.preferred_band == "5" || c.preferred_band.is_empty(),
            )
        };

        if eco != last_eco {
            sm.apply_eco(eco);
            last_eco = eco;
            log::info!("mode debounce: eco={eco}");
        }

        let screen = power.current_state();
        if screen == PowerState::On && last_screen == PowerState::Off {
            screen_on_grace_until = Some(Instant::now() + Duration::from_secs(3));
        }
        last_screen = screen;
        let screen_off = screen == PowerState::Off;
        let in_grace = screen_on_grace_until
            .map(|t| Instant::now() < t)
            .unwrap_or(false);
        let bssid_locked = lock_bssid_until
            .map(|t| Instant::now() < t)
            .unwrap_or(false);
        // 扫描间隔：息屏最稀；观察带（分<观察线）更勤 —— 用户调「观察线」应能感到扫/反应变化
        let prev_score = snapshot
            .lock()
            .ok()
            .map(|s| s.score)
            .unwrap_or(50.0);
        let observing = prev_score < detect_th;
        let scan_gap: u64 = if screen_off {
            60
        } else if eco {
            if observing {
                12
            } else {
                30
            }
        } else if observing {
            8
        } else {
            15
        };

        // hold 剩余
        let hold_rem = {
            let mut h = hold_until.lock().unwrap();
            match *h {
                Some(until) if Instant::now() < until => {
                    until.saturating_duration_since(Instant::now()).as_secs()
                }
                Some(_) => {
                    *h = None;
                    0
                }
                None => 0,
            }
        };

        // 快照侧暂停标记与 config 对齐
        if paused {
            if let Ok(mut s) = snapshot.lock() {
                s.power_state = "PAUSE".into();
            }
        }

        let st = {
            let w = wpa.lock().unwrap();
            w.status_with_signal().ok()
        };

        if let Some(ref st) = st {
            let rssi = st.signal_dbm.unwrap_or(-100);
            let ssid_now = st.ssid.clone().unwrap_or_default();
            let bssid_now = st.bssid.clone().unwrap_or_default();
            let link_key = format!(
                "{}|{}",
                ssid_now.to_lowercase(),
                bssid_now.to_lowercase()
            );

            // 检测用户手动切网（非 daemon 发起）
            if !prev_link_key.is_empty()
                && link_key != prev_link_key
                && st.wpa_state == "COMPLETED"
                && !ssid_now.is_empty()
            {
                let our = suppress_link_change_until
                    .map(|t| Instant::now() < t)
                    .unwrap_or(false);
                if !our && hold_secs > 0 && !paused {
                    let until = Instant::now() + Duration::from_secs(hold_secs);
                    *hold_until.lock().unwrap() = Some(until);
                    sm.reset_soft();
                    log::info!(
                        "manual switch detected {} -> {} ; user_hold {}s",
                        prev_link_key,
                        link_key,
                        hold_secs
                    );
                }
            }
            if st.wpa_state == "COMPLETED" && !link_key.ends_with('|') {
                prev_link_key = link_key;
            }

            let (retry_opt, tx_delta) = match iw_station_dump(&iface) {
                Ok(raw) => {
                    let cur = parse_iw_station(&raw);
                    let rate = retry_rate(&prev_station, &cur);
                    let delta = cur.tx_packets.saturating_sub(prev_station.tx_packets);
                    prev_station = cur;
                    (rate, delta)
                }
                Err(e) => {
                    log::debug!("iw station dump: {e}");
                    (None, 0)
                }
            };

            let score = health_score(rssi, retry_opt, tx_delta, None);
            let band = match st.freq {
                Some(f) if f > 5000 => "5",
                Some(_) => "2.4",
                None => "?",
            };
            let on_preferred = if preferred_is_5g {
                band == "5"
            } else {
                band == "2.4"
            };

            let hold_rem_now = {
                let h = hold_until.lock().unwrap();
                match *h {
                    Some(until) if Instant::now() < until => {
                        until.saturating_duration_since(Instant::now()).as_secs()
                    }
                    _ => 0,
                }
            };

            let in_home_now = link_in_home(&home_aps, &bssid_now, &ssid_now);
            let pen_rem = sm.penalty_remaining_secs();

            let th_hint = threshold_hint_zh(
                on_preferred,
                score,
                rssi,
                switch_th,
                detect_th,
                up_rssi,
                cached_best_5g,
            );

            // 中文原因条（阻塞 > 阈值对照提示）
            let block_reason = if paused {
                "已暂停守护".to_string()
            } else if hold_rem_now > 0 {
                format!("手动/观影保护中（剩 {hold_rem_now}s）")
            } else if pen_rem > 0 {
                format!("切换冷却中（剩 {pen_rem}s）")
            } else if screen_off {
                "息屏降频，不主动切网".to_string()
            } else if in_grace {
                "亮屏冷静窗，暂不切网".to_string()
            } else if bssid_locked {
                "短时锁定当前 AP（防踢回）".to_string()
            } else if !home_aps.is_empty() && !in_home_now {
                "当前不在家网".to_string()
            } else if weak_disconnected {
                "弱信号已断开".to_string()
            } else if !Config::is_persisted() {
                "配置未落盘（可用默认运行）".to_string()
            } else {
                // 无硬阻塞：原因条留空，阈值说明只走 threshold_hint（避免黄/灰双条重复）
                String::new()
            };

            {
                let mut s = snapshot.lock().unwrap();
                s.state = st.wpa_state.clone();
                s.rssi = rssi;
                s.ssid = ssid_now.clone();
                s.bssid = bssid_now.clone();
                s.band = band.into();
                s.score = score;
                s.hold_remaining_secs = hold_rem_now;
                s.user_hold_secs = hold_secs;
                s.home_ap_count = home_aps.len();
                s.in_home = in_home_now;
                s.block_reason = block_reason.clone();
                s.threshold_hint = th_hint.clone();
                s.best_5g_rssi = cached_best_5g;
                s.penalty_remaining_secs = pen_rem;
                s.screen = if screen_off { "OFF" } else { "ON" }.into();
                s.thresholds = ThresholdsView {
                    score_detect_threshold: detect_th,
                    score_switch_threshold: switch_th,
                    upswitch_rssi_min_dbm: up_rssi,
                    mode: mode.clone(),
                };
                if s.power_state != "PAUSE" {
                    if weak_disconnected {
                        s.power_state = "WEAK_OFF".into();
                    } else if hold_rem_now > 0 {
                        s.power_state = format!("USER_HOLD({hold_rem_now}s)");
                    } else if !in_home_now && !home_aps.is_empty() {
                        s.power_state = "OUT_OF_HOME".into();
                    } else if screen_off {
                        s.power_state = "SCREEN_OFF".into();
                    } else {
                        s.power_state = format!("{:?}", sm.state);
                    }
                }
                // 面具列表 description + status.txt：中文实时模块状态
                let line = status_line_zh(
                    &mode,
                    &ssid_now,
                    band,
                    score,
                    &s.power_state,
                    &block_reason,
                );
                let _ = std::fs::write(
                    "/data/adb/amberguard/status.txt",
                    format!("{line}\n"),
                );
                update_module_description(&line);
            }

            // 息屏：只更新状态，不 SCAN/不切换
            if screen_off {
                thread::sleep(Duration::from_secs(5));
                continue;
            }

            if paused {
                thread::sleep(Duration::from_secs(1));
                continue;
            }

            // 手动保护期内：只观测，不切网
            if hold_rem_now > 0 {
                thread::sleep(Duration::from_secs(1));
                continue;
            }

            // 亮屏冷静 / BSSID 短锁：不上切（下切弱信号救援仍允许？ponytail：锁期间全跳过自动切）
            if in_grace || bssid_locked {
                thread::sleep(Duration::from_secs(1));
                continue;
            }

            // 断后自动重连
            if weak_disconnected && auto_rec {
                if st.wpa_state != "COMPLETED" {
                    if last_scan.elapsed() > Duration::from_secs(10) {
                        log::info!("weak reconnect: RECONNECT");
                        let _ = wpa.lock().unwrap().command("RECONNECT");
                        last_scan = Instant::now();
                    }
                } else {
                    weak_disconnected = false;
                    if let Ok(mut snap) = snapshot.lock() {
                        if snap.last_error.starts_with("弱信号") {
                            snap.last_error.clear();
                        }
                    }
                }
            } else if st.wpa_state == "COMPLETED" {
                weak_disconnected = false;
            }

            // 已配置家网且当前不在家网：不自动切、不弱信号断
            if !home_aps.is_empty() && !in_home_now {
                weak_bad_since = None;
                thread::sleep(Duration::from_secs(1));
                continue;
            }

            // 弱信号：先给切换机会，满时限仍差才考虑断（默认 off）
            // ponytail: OUT_OF_HOME/hold/pause 已在上方跳过
            let mut weak_rescue = false;
            if weak_action == "disconnect" && st.wpa_state == "COMPLETED" {
                if rssi < rssi_disc {
                    if weak_bad_since.is_none() {
                        weak_bad_since = Some(Instant::now());
                    }
                    let elapsed = weak_bad_since
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(0);
                    // 满时限后每 ≥8s 救援一次，避免每秒 SCAN
                    if elapsed >= weak_hold && last_scan.elapsed() >= Duration::from_secs(8) {
                        weak_rescue = true;
                        log::info!(
                            "weak rescue window: rssi={rssi} < {rssi_disc} for {elapsed}s — try switch before disconnect"
                        );
                    }
                } else {
                    weak_bad_since = None;
                }
            } else {
                weak_bad_since = None;
            }

            let hint = sm.on_score(score, switch_th, detect_th, on_preferred);

            if hint != SwitchHint::None || weak_rescue {
                if last_scan.elapsed() > Duration::from_secs(scan_gap) || weak_rescue {
                    let _ = wpa.lock().unwrap().command("SCAN");
                    let _ = std::process::Command::new("cmd")
                        .args(["wifi", "start-scan"])
                        .output();
                    thread::sleep(Duration::from_secs(2));
                    last_scan = Instant::now();
                }
                // wpa 结果常残缺；合并 cmd list-scan-results（家网 BSSID / 中文 SSID）
                let wpa_scans = {
                    let w = wpa.lock().unwrap();
                    w.scan_results()
                        .ok()
                        .map(|r| parse_scan_results(&r))
                        .unwrap_or_default()
                };
                let cmd_scans = std::process::Command::new("cmd")
                    .args(["wifi", "list-scan-results"])
                    .output()
                    .ok()
                    .map(|o| parse_cmd_scan_results(&String::from_utf8_lossy(&o.stdout)))
                    .unwrap_or_default();
                let scans = merge_scan_aps(wpa_scans, cmd_scans);
                // 更新家网 5G 最强（不论是否达标），供面板对照上切线
                // 偏好频段上最强家网 AP（上切对照）；字段名历史原因仍叫 best_5g
                cached_best_5g = scans
                    .iter()
                    .filter(|a| {
                        a.is_5g() == preferred_is_5g
                            && (home_aps.is_empty() || home_contains(&home_aps, &a.bssid))
                    })
                    .map(|a| a.signal)
                    .max();
                let ssid = ssid_now;
                let cur_bssid = bssid_now;
                let cur_is_5g = band == "5";

                let target = match hint {
                    // 下切=离开偏好 → 非偏好频段；上切=回到偏好
                    SwitchHint::Downswitch => {
                        best_on_band(
                            &scans,
                            &ssid,
                            !preferred_is_5g,
                            -80,
                            &bonds,
                            &home_aps,
                        )
                    }
                    SwitchHint::Upswitch => {
                        best_on_band(
                            &scans,
                            &ssid,
                            preferred_is_5g,
                            up_rssi,
                            &bonds,
                            &home_aps,
                        )
                    }
                    SwitchHint::None if weak_rescue => {
                        // 救援：① 同频更强(+5dB) ② 否则 2.4 且明显高于断开阈值
                        let better_same = best_on_band(
                            &scans,
                            &ssid,
                            cur_is_5g,
                            rssi + 5,
                            &bonds,
                            &home_aps,
                        )
                        .filter(|a| !a.bssid.eq_ignore_ascii_case(&cur_bssid));
                        better_same.or_else(|| {
                            if cur_is_5g {
                                best_on_band(
                                    &scans,
                                    &ssid,
                                    false,
                                    rssi_disc + 10,
                                    &bonds,
                                    &home_aps,
                                )
                                .filter(|a| !a.bssid.eq_ignore_ascii_case(&cur_bssid))
                            } else {
                                None
                            }
                        })
                    }
                    SwitchHint::None => None,
                };

                if let Some(peer) = target {
                    if peer.bssid.eq_ignore_ascii_case(&cur_bssid) {
                        sm.finish_switch_ok();
                        if weak_rescue {
                            // 没有更好目标 → 断
                            log::warn!(
                                "weak disconnect: no better peer (still on {})",
                                cur_bssid
                            );
                            let _ = wpa.lock().unwrap().command("DISCONNECT");
                            weak_disconnected = true;
                            weak_bad_since = None;
                            if let Ok(mut snap) = snapshot.lock() {
                                snap.last_error = format!(
                                    "弱信号已断开（{rssi} dBm，无更优 AP）"
                                );
                                snap.power_state = "WEAK_OFF".into();
                            }
                        }
                        continue;
                    }
                    let bond_key = format!("{ssid}->{}/{}", peer.ssid, peer.bssid);
                    let same_ssid = peer.ssid == ssid;
                    let reason = if weak_rescue && hint == SwitchHint::None {
                        "WeakRescue"
                    } else {
                        "Score"
                    };
                    let from_band = band.to_string();
                    let to_band = if peer.freq > 5000 { "5" } else { "2.4" }.to_string();
                    let switch_t0 = Instant::now();
                    log::info!(
                        "switch {reason}/{:?}: {} {} ssid={} freq={} sig={} score={score:.1}",
                        hint,
                        if same_ssid { "ROAM" } else { "SELECT" },
                        peer.bssid,
                        peer.ssid,
                        peer.freq,
                        peer.signal
                    );

                    // 标记：随后链路变化视为 daemon 自切
                    suppress_link_change_until =
                        Some(Instant::now() + Duration::from_secs(12));

                    // 尝试前尽量 enable 所有 wpa 网络，减轻「管理员停用」残留
                    {
                        let _ = wpa.lock().unwrap().command("ENABLE_NETWORK all");
                    }
                    let mut selected_nid: Option<u32> = None;
                    let switch_res = if same_ssid {
                        match wpa.lock().unwrap().roam(&peer.bssid) {
                            Ok(()) => Ok(()),
                            Err(e) => {
                                log::info!("ROAM fail ({e}), try framework connect");
                                wifi_framework::framework_connect(
                                    &peer.ssid,
                                    Some(&peer.bssid),
                                )
                                .map_err(wpa_ctrl::WpaError::Parse)
                            }
                        }
                    } else {
                        match wifi_framework::framework_connect(
                            &peer.ssid,
                            Some(&peer.bssid),
                        ) {
                            Ok(()) => {
                                log::info!("framework connect issued -> {}", peer.ssid);
                                Ok(())
                            }
                            Err(e_fw) => {
                                log::warn!("framework connect: {e_fw}; fallback wpa SELECT");
                                let list =
                                    wpa.lock().unwrap().list_networks().unwrap_or_default();
                                match network_id_for_ssid(&list, &peer.ssid) {
                                    Some(nid) => {
                                        selected_nid = Some(nid);
                                        let w = wpa.lock().unwrap();
                                        let _ = w.enable_network(nid);
                                        // 异名双频不钉 BSSID，避免选错/失败
                                        let _ = w.set_network_bssid(nid, "\"\"");
                                        w.select_network(nid)
                                    }
                                    None => {
                                        let msg = format!(
                                            "无法切换到「{}」：{e_fw}；请确认系统已保存该网且未被停用",
                                            peer.ssid
                                        );
                                        log::warn!("{msg}");
                                        if let Ok(mut snap) = snapshot.lock() {
                                            snap.last_error = msg.clone();
                                            snap.block_reason = "对侧网络无法选中（或已停用）".into();
                                        }
                                        Err(wpa_ctrl::WpaError::Parse(msg))
                                    }
                                }
                            }
                        }
                    };

                    match switch_res {
                        Ok(()) => {
                            // 异名：只验 SSID；同名漫游：验 BSSID
                            let mut ok = false;
                            let mut last_got = String::new();
                            for i in 0..48 {
                                thread::sleep(Duration::from_millis(250));
                                if let Ok(w) = wpa.lock() {
                                    if let Ok(s2) = w.status() {
                                        let gs = s2.ssid.clone().unwrap_or_default();
                                        let gb = s2.bssid.clone().unwrap_or_default();
                                        last_got = format!(
                                            "{}|{}|{}",
                                            s2.wpa_state, gs, gb
                                        );
                                        if link_reached_peer(
                                            &s2,
                                            &peer.ssid,
                                            &peer.bssid,
                                            same_ssid,
                                        ) {
                                            ok = true;
                                            log::info!(
                                                "switch verified at {}ms -> {} {}",
                                                (i + 1) * 250,
                                                gs,
                                                gb
                                            );
                                            break;
                                        }
                                    }
                                }
                            }
                            // 未落地且异名：再试一次不带 BSSID 的框架连接（部分机 -b 空转）
                            if !ok && !same_ssid {
                                log::info!("retry framework connect without bssid -> {}", peer.ssid);
                                if wifi_framework::framework_connect(&peer.ssid, None).is_ok() {
                                    for i in 0..32 {
                                        thread::sleep(Duration::from_millis(250));
                                        if let Ok(w) = wpa.lock() {
                                            if let Ok(s2) = w.status() {
                                                last_got = format!(
                                                    "{}|{}|{}",
                                                    s2.wpa_state,
                                                    s2.ssid.clone().unwrap_or_default(),
                                                    s2.bssid.clone().unwrap_or_default()
                                                );
                                                if link_reached_peer(
                                                    &s2,
                                                    &peer.ssid,
                                                    &peer.bssid,
                                                    false,
                                                ) {
                                                    ok = true;
                                                    log::info!(
                                                        "switch verified (retry) at {}ms",
                                                        (i + 1) * 250
                                                    );
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            // 清目标网 bssid 钉（无论成败，避免长期锁死）
                            if let Some(nid) = selected_nid {
                                if let Ok(w) = wpa.lock() {
                                    let _ = w.set_network_bssid(nid, "\"\"");
                                }
                            }
                            let mut result = if ok { "Ok" } else { "Fail" }.to_string();
                            if !ok {
                                log::warn!(
                                    "switch not landed: want {}/{} got {last_got}",
                                    peer.ssid,
                                    peer.bssid
                                );
                                if let Ok(mut snap) = snapshot.lock() {
                                    snap.last_error = format!(
                                        "切换未生效（目标 {}，仍为 {last_got}）",
                                        peer.ssid
                                    );
                                }
                            }
                            // L3：链路已落到目标后只作连通性标注，失败不撤销成功、不进惩罚
                            // （国内 gstatic DNS 失败曾导致「已切上却 L3Timeout+冷却」）
                            if ok && l3_on {
                                match l3_probe(Duration::from_secs(2)) {
                                    Ok(()) => {
                                        if let Ok(mut snap) = snapshot.lock() {
                                            snap.l3_last = "ok".into();
                                        }
                                        result = "Ok".into();
                                        log::info!("L3 probe ok");
                                    }
                                    Err(e) => {
                                        log::warn!("L3 probe soft-fail (仍计切换成功): {e}");
                                        result = "OkL3Warn".into();
                                        if let Ok(mut snap) = snapshot.lock() {
                                            snap.l3_last = format!("warn:{e}");
                                            // 不写 last_error 抢主状态；仅 l3_last
                                        }
                                    }
                                }
                            } else if ok {
                                if let Ok(mut snap) = snapshot.lock() {
                                    snap.l3_last = "skip".into();
                                }
                            }
                            let dur = switch_t0.elapsed().as_millis() as u64;
                            push_history(
                                &history,
                                SwitchEvent {
                                    ts_unix: unix_now(),
                                    from_ssid: ssid.clone(),
                                    to_ssid: peer.ssid.clone(),
                                    from_band: from_band.clone(),
                                    to_band: to_band.clone(),
                                    reason: reason.into(),
                                    result: result.clone(),
                                    duration_ms: dur,
                                },
                            );
                            // 无论成败：短时忽略链路键变化，避免「代连失败回弹」被当成手动切网
                            suppress_link_change_until =
                                Some(Instant::now() + Duration::from_secs(15));
                            if let Ok(w) = wpa.lock() {
                                if let Ok(s2) = w.status() {
                                    let ns = s2.ssid.clone().unwrap_or_default();
                                    let nb = s2.bssid.clone().unwrap_or_default();
                                    if !ns.is_empty() {
                                        prev_link_key = format!(
                                            "{}|{}",
                                            ns.to_lowercase(),
                                            nb.to_lowercase()
                                        );
                                    }
                                }
                            }
                            if ok {
                                sm.finish_switch_ok();
                                lock_bssid_until =
                                    Some(Instant::now() + Duration::from_secs(45));
                                if weak_rescue {
                                    weak_bad_since = None;
                                }
                                if let Ok(mut snap) = snapshot.lock() {
                                    if result.starts_with("Ok") {
                                        snap.last_error.clear();
                                    }
                                }
                                log::info!("switch OK -> {} ({})", peer.ssid, result);
                            } else {
                                sm.enter_penalty(&bond_key);
                                if weak_rescue {
                                    log::warn!("weak disconnect after failed rescue switch");
                                    let _ = wpa.lock().unwrap().command("DISCONNECT");
                                    weak_disconnected = true;
                                    weak_bad_since = None;
                                    if let Ok(mut snap) = snapshot.lock() {
                                        snap.last_error =
                                            format!("弱信号已断开（切换失败，{rssi} dBm）");
                                        snap.power_state = "WEAK_OFF".into();
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("switch failed: {e}");
                            let msg = e.to_string();
                            if let Some(nid) = selected_nid {
                                if let Ok(w) = wpa.lock() {
                                    let _ = w.set_network_bssid(nid, "\"\"");
                                }
                            }
                            push_history(
                                &history,
                                SwitchEvent {
                                    ts_unix: unix_now(),
                                    from_ssid: ssid.clone(),
                                    to_ssid: peer.ssid.clone(),
                                    from_band: from_band.clone(),
                                    to_band: to_band.clone(),
                                    reason: reason.into(),
                                    result: "Fail".into(),
                                    duration_ms: switch_t0.elapsed().as_millis() as u64,
                                },
                            );
                            sm.enter_penalty(&bond_key);
                            if msg.contains("请先在系统设置") || msg.contains("no network id") {
                                if let Some(p) = sm.penalty.as_mut() {
                                    p.cooldown_secs = 15;
                                    p.until = Instant::now() + Duration::from_secs(15);
                                }
                            }
                            if weak_rescue {
                                log::warn!("weak disconnect after rescue error: {msg}");
                                let _ = wpa.lock().unwrap().command("DISCONNECT");
                                weak_disconnected = true;
                                weak_bad_since = None;
                                if let Ok(mut snap) = snapshot.lock() {
                                    snap.last_error =
                                        format!("弱信号已断开（无可用网络，{rssi} dBm）");
                                    snap.power_state = "WEAK_OFF".into();
                                }
                            }
                        }
                    }
                } else {
                    if weak_rescue {
                        log::warn!(
                            "weak disconnect: no better peer in set (ssid={ssid}, home={}, bonds={})",
                            home_aps.len(),
                            bonds.len()
                        );
                        let _ = wpa.lock().unwrap().command("DISCONNECT");
                        weak_disconnected = true;
                        weak_bad_since = None;
                        if let Ok(mut snap) = snapshot.lock() {
                            snap.last_error =
                                format!("弱信号已断开（无更优 AP，{rssi} dBm < {rssi_disc}）");
                            snap.power_state = "WEAK_OFF".into();
                        }
                    } else {
                        let why = match hint {
                            SwitchHint::Upswitch => match cached_best_5g {
                                Some(b) => format!(
                                    "上切未执行：家网 5G 最强 {b} dBm < 上切线 {up_rssi} dBm"
                                ),
                                None => format!(
                                    "上切未执行：未扫到家网 5G（上切线 {up_rssi} dBm）"
                                ),
                            },
                            SwitchHint::Downswitch => {
                                "下切未执行：未扫到可用 2.4G 家网 AP".into()
                            }
                            _ => format!("无可用对端 AP（bonds={}）", bonds.len()),
                        };
                        log::info!("switch {:?}: {why}", hint);
                        if let Ok(mut snap) = snapshot.lock() {
                            snap.last_error = why.clone();
                            snap.block_reason = why;
                            snap.best_5g_rssi = cached_best_5g;
                            snap.threshold_hint = threshold_hint_zh(
                                on_preferred,
                                score,
                                rssi,
                                switch_th,
                                detect_th,
                                up_rssi,
                                cached_best_5g,
                            );
                        }
                        sm.finish_switch_ok();
                    }
                }
            }
        } else if let Ok(mut s) = snapshot.lock() {
            s.hold_remaining_secs = hold_rem;
            s.user_hold_secs = hold_secs;
            s.home_ap_count = home_aps.len();
            s.in_home = true;
        }

        thread::sleep(Duration::from_secs(1));
    }
}

fn offline_loop() -> ! {
    let snapshot = Arc::new(Mutex::new(StatusSnapshot::new()));
    let snapshot2 = Arc::clone(&snapshot);
    let addr: SocketAddr = "127.0.0.1:8080".parse().expect("addr");
    if let Ok(server) = Server::http(addr) {
        thread::spawn(move || {
            for req in server.incoming_requests() {
                let s = snapshot2.lock().unwrap();
                let json = serde_json::to_string(&*s).unwrap_or_else(|_| "{}".into());
                let _ = req.respond(Response::from_string(json));
            }
        });
    }
    let mut c = 0u32;
    loop {
        if let Ok(mut s) = snapshot.lock() {
            s.rssi = -55 - (c % 20) as i32;
            s.score = 42.0;
            s.state = "OFFLINE".into();
            s.band = if c % 2 == 0 { "2.4" } else { "5" }.into();
        }
        c += 1;
        thread::sleep(Duration::from_secs(1));
    }
}
