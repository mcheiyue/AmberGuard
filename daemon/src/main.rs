use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tiny_http::{Header, Method, Response, Server, StatusCode};

use crate::band_bond::{
    best_on_band, dual_band_pair_saved, link_in_home, network_id_for_ssid, parse_list_networks,
    parse_scan_results,
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

/// 切后是否真到了目标：COMPLETED + SSID 一致，且（有目标 BSSID 时）BSSID 一致
fn link_reached_peer(
    st: &crate::wpa_ctrl::WpaStatus,
    peer_ssid: &str,
    peer_bssid: &str,
) -> bool {
    if st.wpa_state != "COMPLETED" {
        return false;
    }
    let got_ssid = st.ssid.as_deref().unwrap_or("");
    if got_ssid.is_empty() || got_ssid != peer_ssid {
        return false;
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

/// L3：对 connectivitycheck.gstatic.com/generate_204 发 HTTP/1.0，2s 超时
fn l3_probe(timeout: Duration) -> Result<(), String> {
    let addr = ("connectivitycheck.gstatic.com", 80)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or_else(|| "DNS 无结果".to_string())?;
    let mut stream =
        TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("connect: {e}"))?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    stream
        .write_all(
            b"GET /generate_204 HTTP/1.0\r\nHost: connectivitycheck.gstatic.com\r\nConnection: close\r\n\r\n",
        )
        .map_err(|e| format!("write: {e}"))?;
    let mut buf = [0u8; 128];
    let n = stream.read(&mut buf).map_err(|e| format!("read: {e}"))?;
    if n == 0 {
        return Err("空响应".into());
    }
    let head = String::from_utf8_lossy(&buf[..n]);
    let line = head.lines().next().unwrap_or("");
    if line.contains(" 204") || line.contains(" 200") || line.contains(" 204 ") {
        Ok(())
    } else if line.contains("HTTP/") {
        // 部分运营商劫持返回 302/200 也算「有网」
        if line.contains(" 30") || line.contains(" 200") {
            Ok(())
        } else {
            Err(format!("状态行: {line}"))
        }
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
                        thread::sleep(Duration::from_millis(1200));
                        let w = wpa_http.lock().map_err(|e| e.to_string())?;
                        let raw = w.scan_results().map_err(|e| e.to_string())?;
                        let aps = parse_scan_results(&raw);
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
    let preferred_is_5g = true;
    // 链路键 ssid|bssid；daemon 自切后短暂忽略变更
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
        // eco / 息屏：SCAN 更稀
        let scan_gap = if screen_off {
            60
        } else if eco {
            30
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

            // 中文原因条
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
                // 面具 action / 列表 description 用的一行状态
                let st_label = if block_reason.is_empty() {
                    s.power_state.as_str()
                } else {
                    block_reason.as_str()
                };
                let line = format!(
                    "[{mode}] {} {band} score={:.0} · {st_label}",
                    if ssid_now.is_empty() {
                        "-"
                    } else {
                        ssid_now.as_str()
                    },
                    score,
                );
                let _ = std::fs::write(
                    "/data/adb/amberguard/status.txt",
                    format!("{line}\n"),
                );
                // 限频写 module.prop description（列表可见；管理器不一定实时刷，点操作/重进会新）
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
                    thread::sleep(Duration::from_secs(2));
                    last_scan = Instant::now();
                }
                let scans = {
                    let w = wpa.lock().unwrap();
                    w.scan_results()
                        .ok()
                        .map(|r| parse_scan_results(&r))
                        .unwrap_or_default()
                };
                let ssid = ssid_now;
                let cur_bssid = bssid_now;
                let cur_is_5g = band == "5";

                let target = match hint {
                    SwitchHint::Downswitch => {
                        best_on_band(&scans, &ssid, false, -80, &bonds, &home_aps)
                    }
                    SwitchHint::Upswitch => {
                        best_on_band(&scans, &ssid, true, up_rssi, &bonds, &home_aps)
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

                    // SELECT 时记下 network id，成功/失败后再清 bssid 锁
                    let mut selected_nid: Option<u32> = None;
                    let switch_res = if same_ssid {
                        // 同 SSID：先 ROAM，失败再走框架（少见）
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
                        // 异名双频：优先 Android 框架（小米 wpa SELECT 基本无效）
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
                                        let _ = w.set_network_bssid(nid, &peer.bssid);
                                        w.select_network(nid)
                                    }
                                    None => {
                                        let msg = format!(
                                            "无法切换到「{}」：{e_fw}；wpa 列表亦无精确 id（当前「{}」）",
                                            peer.ssid, ssid
                                        );
                                        log::warn!("{msg}");
                                        if let Ok(mut snap) = snapshot.lock() {
                                            snap.last_error = msg.clone();
                                            snap.block_reason = "对侧网络无法由框架/wpa 选中".into();
                                        }
                                        Err(wpa_ctrl::WpaError::Parse(msg))
                                    }
                                }
                            }
                        }
                    };

                    match switch_res {
                        Ok(()) => {
                            // 须落到目标 SSID/BSSID，不能只看 COMPLETED（原链路本就是 COMPLETED）
                            let mut ok = false;
                            let mut last_got = String::new();
                            for i in 0..40 {
                                thread::sleep(Duration::from_millis(250));
                                if let Ok(w) = wpa.lock() {
                                    if let Ok(s2) = w.status() {
                                        let gs = s2.ssid.clone().unwrap_or_default();
                                        let gb = s2.bssid.clone().unwrap_or_default();
                                        last_got = format!(
                                            "{}|{}|{}",
                                            s2.wpa_state, gs, gb
                                        );
                                        if link_reached_peer(&s2, &peer.ssid, &peer.bssid) {
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
                            if ok && l3_on {
                                match l3_probe(Duration::from_secs(2)) {
                                    Ok(()) => {
                                        if let Ok(mut snap) = snapshot.lock() {
                                            snap.l3_last = "ok".into();
                                        }
                                        log::info!("L3 probe ok");
                                    }
                                    Err(e) => {
                                        log::warn!("L3 probe fail: {e}");
                                        result = "L3Timeout".into();
                                        ok = false;
                                        if let Ok(mut snap) = snapshot.lock() {
                                            snap.l3_last = format!("fail:{e}");
                                            snap.last_error =
                                                format!("切后 L3 失败: {e}");
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
                            if ok {
                                sm.finish_switch_ok();
                                lock_bssid_until =
                                    Some(Instant::now() + Duration::from_secs(45));
                                if weak_rescue {
                                    weak_bad_since = None;
                                }
                                if let Ok(w) = wpa.lock() {
                                    if let Ok(s2) = w.status() {
                                        let ns = s2.ssid.clone().unwrap_or_default();
                                        let nb = s2.bssid.clone().unwrap_or_default();
                                        prev_link_key = format!(
                                            "{}|{}",
                                            ns.to_lowercase(),
                                            nb.to_lowercase()
                                        );
                                    }
                                }
                                if let Ok(mut snap) = snapshot.lock() {
                                    if result == "Ok" {
                                        snap.last_error.clear();
                                    }
                                }
                                log::info!("switch OK -> {}", peer.ssid);
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
                        log::info!(
                            "switch {:?}: no bonded peer (ssid={ssid}, bonds={})",
                            hint,
                            bonds.len()
                        );
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
