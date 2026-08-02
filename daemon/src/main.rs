use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tiny_http::{Response, Server};

use crate::band_bond::{best_bonded_on_band, network_id_for_ssid, parse_scan_results};
use crate::config::Config;
use crate::health_score::health_score;
use crate::state_machine::{State, StateMachine, SwitchHint};
use crate::web::StatusSnapshot;
use crate::wpa_ctrl::WpaCtrl;

mod band_bond;
mod config;
mod health_score;
mod power_state;
mod scanner;
mod state_machine;
mod web;
mod wpa_ctrl;

fn main() {
    env_logger::init();

    let config = Config::load().expect("config load");

    log::info!("AmberGuard Phase 2 daemon started");
    log::info!("Interface: {}, Listen: {}", config.interface, config.listen);

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

    let addr: SocketAddr = config.listen.parse().expect("bad listen address");
    let server = Server::http(addr).expect("listen");
    log::info!("HTTP server listening on {addr}");

    let snapshot_http = Arc::clone(&snapshot);
    thread::spawn(move || {
        for req in server.incoming_requests() {
            match req.url() {
                "/api/status" => {
                    let s = snapshot_http.lock().unwrap();
                    let json = serde_json::to_string(&*s).unwrap_or_else(|_| "{}".into());
                    let _ = req.respond(Response::from_string(json));
                }
                "/api/mode/pause" => {
                    // 简单：写快照 power_state；完整 mode 配置后续
                    if let Ok(mut s) = snapshot_http.lock() {
                        s.power_state = "PAUSE".into();
                    }
                    let _ = req.respond(Response::from_string("{\"ok\":true}"));
                }
                "/api/mode/daily" => {
                    if let Ok(mut s) = snapshot_http.lock() {
                        s.power_state = "ON".into();
                    }
                    let _ = req.respond(Response::from_string("{\"ok\":true}"));
                }
                _ => {
                    let html = include_bytes!("web/static/index.html");
                    let _ = req.respond(Response::from_data(html.to_vec()));
                }
            }
        }
    });

    let mut last_scan = Instant::now() - Duration::from_secs(60);
    let preferred_is_5g = true; // 日用默认偏好 5G；后续读 config

    loop {
        // 暂停模式：只刷新状态，不切换
        let paused = snapshot
            .lock()
            .map(|s| s.power_state == "PAUSE")
            .unwrap_or(false);

        let st = {
            let w = wpa.lock().unwrap();
            w.status_with_signal().ok()
        };

        if let Some(ref st) = st {
            let rssi = st.signal_dbm.unwrap_or(-100);
            let score = health_score(rssi, None, 0, None);
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
                if s.power_state != "PAUSE" {
                    s.power_state = format!("{:?}", sm.state);
                }
            }

            if paused {
                thread::sleep(Duration::from_secs(1));
                continue;
            }

            let hint = sm.on_score(
                score,
                config.score_switch_threshold,
                config.score_detect_threshold,
                on_preferred,
            );

            if hint != SwitchHint::None {
                // 需要较新的扫描结果
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
                        best_bonded_on_band(&scans, &ssid, false, -80, &config.bonds)
                    }
                    SwitchHint::Upswitch => best_bonded_on_band(
                        &scans,
                        &ssid,
                        true,
                        config.upswitch_rssi_min_dbm,
                        &config.bonds,
                    ),
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
                        // 异 SSID：需已保存的 network id
                        let list = wpa.lock().unwrap().list_networks().unwrap_or_default();
                        match network_id_for_ssid(&list, &peer.ssid) {
                            Some(nid) => {
                                let w = wpa.lock().unwrap();
                                // 锁定 BSSID 后 SELECT（失败不阻塞清锁——以实机为准）
                                let _ = w.set_network_bssid(nid, &peer.bssid);
                                let r = w.select_network(nid);
                                let _ = w.set_network_bssid(nid, "\"\"");
                                r
                            }
                            None => {
                                log::warn!(
                                    "peer ssid {} not in LIST_NETWORKS — save WiFi first",
                                    peer.ssid
                                );
                                Err(wpa_ctrl::WpaError::Parse(format!(
                                    "no network id for {}",
                                    peer.ssid
                                )))
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
                                log::info!("switch OK -> {}", peer.ssid);
                            } else {
                                sm.enter_penalty(&bond_key);
                            }
                        }
                        Err(e) => {
                            log::error!("switch failed: {e}");
                            sm.enter_penalty(&bond_key);
                        }
                    }
                } else {
                    log::info!(
                        "switch {:?}: no bonded peer (ssid={ssid}, bonds={})",
                        hint,
                        config.bonds.len()
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
    // 若端口占用则空转
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
