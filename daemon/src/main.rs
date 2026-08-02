use std::io::Read;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tiny_http::{Header, Method, Response, Server, StatusCode};

use crate::band_bond::{best_bonded_on_band, network_id_for_ssid, parse_scan_results};
use crate::config::{Config, ConfigPatch};
use crate::health_score::health_score;
use crate::state_machine::{StateMachine, SwitchHint};
use crate::station_info::{iw_station_dump, parse_iw_station, retry_rate, StationSample};
use crate::web::{StatusSnapshot, ThresholdsView};
use crate::wpa_ctrl::WpaCtrl;

mod band_bond;
mod config;
mod file_log;
mod health_score;
mod power_state;
mod scanner;
mod state_machine;
mod station_info;
mod web;
mod wpa_ctrl;

fn json_resp(body: String, code: StatusCode) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut r = Response::from_string(body).with_status_code(code);
    if let Ok(h) = Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..])
    {
        r.add_header(h);
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
    let mut sm = StateMachine::new();

    let listen = config.lock().unwrap().listen.clone();
    let addr: SocketAddr = listen.parse().expect("bad listen address");
    let server = Server::http(addr).expect("listen");
    log::info!("HTTP server listening on {addr}");

    let snapshot_http = Arc::clone(&snapshot);
    let config_http = Arc::clone(&config);
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
                (Method::Get, "/api/init-config") | (Method::Post, "/api/init-config") => {
                    match Config::init_if_missing() {
                        Ok(written) => {
                            if written {
                                // 重载配置
                                if let Ok(mut c) = config_http.lock() {
                                    let _ = c.reload();
                                }
                                log::info!("配置初始化完成");
                                json_resp("{\"ok\":true,\"initialized\":true}".into(), StatusCode(200))
                            } else {
                                json_resp("{\"ok\":true,\"initialized\":false}".into(), StatusCode(200))
                            }
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
                    if let Ok(h) =
                        Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                    {
                        r.add_header(h);
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

    // 启动时同步 mode
    {
        let c = config.lock().unwrap();
        if let Ok(mut s) = snapshot.lock() {
            if c.mode == "pause" {
                s.power_state = "PAUSE".into();
            }
            s.thresholds = ThresholdsView {
                score_detect_threshold: c.score_detect_threshold,
                score_switch_threshold: c.score_switch_threshold,
                upswitch_rssi_min_dbm: c.upswitch_rssi_min_dbm,
                mode: c.mode.clone(),
            };
        }
    }

    loop {
        let (paused, switch_th, detect_th, up_rssi, bonds, iface, mode) = {
            let c = config.lock().unwrap();
            (
                c.mode == "pause",
                c.score_switch_threshold,
                c.score_detect_threshold,
                c.upswitch_rssi_min_dbm,
                c.bonds.clone(),
                c.interface.clone(),
                c.mode.clone(),
            )
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

            {
                let mut s = snapshot.lock().unwrap();
                s.state = st.wpa_state.clone();
                s.rssi = rssi;
                s.ssid = st.ssid.clone().unwrap_or_default();
                s.band = band.into();
                s.score = score;
                s.thresholds = ThresholdsView {
                    score_detect_threshold: detect_th,
                    score_switch_threshold: switch_th,
                    upswitch_rssi_min_dbm: up_rssi,
                    mode: mode.clone(),
                };
                if s.power_state != "PAUSE" {
                    s.power_state = format!("{:?}", sm.state);
                }
            }

            if paused {
                thread::sleep(Duration::from_secs(1));
                continue;
            }

            let hint = sm.on_score(score, switch_th, detect_th, on_preferred);

            if hint != SwitchHint::None {
                if last_scan.elapsed() > Duration::from_secs(15) {
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
                let ssid = st.ssid.clone().unwrap_or_default();
                let cur_bssid = st.bssid.clone().unwrap_or_default();

                let target = match hint {
                    SwitchHint::Downswitch => {
                        best_bonded_on_band(&scans, &ssid, false, -80, &bonds)
                    }
                    SwitchHint::Upswitch => {
                        best_bonded_on_band(&scans, &ssid, true, up_rssi, &bonds)
                    }
                    SwitchHint::None => None,
                };

                if let Some(peer) = target {
                    if peer.bssid.eq_ignore_ascii_case(&cur_bssid) {
                        sm.finish_switch_ok();
                        continue;
                    }
                    let bond_key = format!("{ssid}->{}/{}", peer.ssid, peer.bssid);
                    let same_ssid = peer.ssid == ssid;
                    log::info!(
                        "switch {:?}: {} {} ssid={} freq={} sig={} score={score:.1}",
                        hint,
                        if same_ssid { "ROAM" } else { "SELECT" },
                        peer.bssid,
                        peer.ssid,
                        peer.freq,
                        peer.signal
                    );

                    let switch_res = if same_ssid {
                        wpa.lock().unwrap().roam(&peer.bssid)
                    } else {
                        let list = wpa.lock().unwrap().list_networks().unwrap_or_default();
                        match network_id_for_ssid(&list, &peer.ssid) {
                            Some(nid) => {
                                let w = wpa.lock().unwrap();
                                let _ = w.set_network_bssid(nid, &peer.bssid);
                                let r = w.select_network(nid);
                                let _ = w.set_network_bssid(nid, "\"\"");
                                r
                            }
                            None => {
                                let msg = format!(
                                    "请先在系统设置连接并保存 WiFi「{}」（与当前「{}」双频成对）",
                                    peer.ssid, ssid
                                );
                                log::warn!("{msg}");
                                if let Ok(mut snap) = snapshot.lock() {
                                    snap.last_error = msg.clone();
                                }
                                Err(wpa_ctrl::WpaError::Parse(msg))
                            }
                        }
                    };

                    match switch_res {
                        Ok(()) => {
                            let mut ok = false;
                            for _ in 0..30 {
                                thread::sleep(Duration::from_millis(200));
                                if let Ok(w) = wpa.lock() {
                                    if let Ok(s2) = w.status() {
                                        if s2.wpa_state == "COMPLETED" {
                                            ok = true;
                                            break;
                                        }
                                    }
                                }
                            }
                            if ok {
                                sm.finish_switch_ok();
                                if let Ok(mut snap) = snapshot.lock() {
                                    snap.last_error.clear();
                                }
                                log::info!("switch OK -> {}", peer.ssid);
                            } else {
                                sm.enter_penalty(&bond_key);
                            }
                        }
                        Err(e) => {
                            log::error!("switch failed: {e}");
                            let msg = e.to_string();
                            sm.enter_penalty(&bond_key);
                            if msg.contains("请先在系统设置") || msg.contains("no network id") {
                                if let Some(p) = sm.penalty.as_mut() {
                                    p.cooldown_secs = 15;
                                    p.until = Instant::now() + Duration::from_secs(15);
                                }
                            }
                        }
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
