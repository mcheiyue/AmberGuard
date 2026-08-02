use std::sync::{Arc, Mutex};
use std::thread;
use std::net::SocketAddr;
use tiny_http::{Response, Server};

use crate::config::Config;
use crate::web::StatusSnapshot;

mod config;
mod wpa_ctrl;
mod health_score;
mod band_bond;
mod state_machine;
mod scanner;
mod power_state;
mod web;

fn main() {
    env_logger::init();

    let config = Config::load().expect("config load");

    log::info!("AmberGuard Phase 1 daemon started");
    log::info!("Interface: {}, Listen: {}", config.interface, config.listen);

    // 连接 wpa_supplicant
    let wpa = match wpa_ctrl::WpaCtrl::auto_connect() {
        Ok(w) => {
            log::info!("wpa_supplicant connected");
            w
        }
        Err(e) => {
            log::error!("wpa_supplicant connect failed: {e} — running in offline mode");
            // 降级：保留 mock 数据
            let mut s = StatusSnapshot::new();
            s.state = "OFFLINE".to_string();
            offline_loop(config);
        }
    };
    let wpa = Arc::new(Mutex::new(wpa));

    let snapshot = Arc::new(Mutex::new(StatusSnapshot::new()));

    let addr: SocketAddr = config.listen.parse().expect("bad listen address");
    let server = Server::http(addr).expect("listen");
    log::info!("HTTP server listening on {}", addr);

    let snapshot2 = Arc::clone(&snapshot);
    let _wpa2 = Arc::clone(&wpa);
    thread::spawn(move || {
        for req in server.incoming_requests() {
            match req.url() {
                "/api/status" => {
                    let s = snapshot2.lock().unwrap();
                    let json = serde_json::to_string(&*s).unwrap();
                    let resp = Response::from_string(json);
                    let _ = req.respond(resp);
                }
                _ => {
                    let html = include_bytes!("web/static/index.html");
                    let resp = Response::from_data(html.to_vec());
                    let _ = req.respond(resp);
                }
            }
        }
    });

    // 主循环：每秒 STATUS + SIGNAL_POLL（补 RSSI）→ 更新 snapshot
    loop {
        let status = {
            let w = wpa.lock().unwrap();
            w.status_with_signal().ok()
        };
        if let Some(st) = status {
            let mut s = snapshot.lock().unwrap();
            s.state = st.wpa_state.clone();
            s.rssi = st.signal_dbm.unwrap_or(-100);
            s.ssid = st.ssid.clone().unwrap_or_default();
            s.band = match st.freq {
                Some(f) if f > 5000 => "5".into(),
                Some(_) => "2.4".into(),
                None => s.band.clone(),
            };
            // Phase 1：RSSI 线性占位分；Phase 2 换 health_score
            s.score = ((s.rssi + 90) as f32 / 50.0 * 100.0).clamp(0.0, 100.0);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn offline_loop(_config: Config) -> ! {
    let snapshot = Arc::new(Mutex::new(StatusSnapshot::new()));
    let snapshot2 = Arc::clone(&snapshot);
    let addr = "127.0.0.1:8080".parse::<SocketAddr>().expect("addr");
    let server = Server::http(addr).expect("listen");
    thread::spawn(move || {
        for req in server.incoming_requests() {
            let s = snapshot2.lock().unwrap();
            let json = serde_json::to_string(&*s).unwrap();
            let _ = req.respond(Response::from_string(json));
        }
    });
    let mut counter = 0u32;
    loop {
        let mut s = snapshot.lock().unwrap();
        s.rssi = -55 - (counter % 20) as i32;
        s.score = 42.0;
        s.band = if counter % 2 == 0 { "2.4".to_string() } else { "5".to_string() };
        counter += 1;
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
