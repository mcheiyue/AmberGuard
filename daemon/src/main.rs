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

    let snapshot = Arc::new(Mutex::new(StatusSnapshot::new()));

    let addr: SocketAddr = config.listen.parse().expect("bad listen address");
    let server = Server::http(addr).expect("listen");
    log::info!("HTTP server listening on {}", addr);

    let snapshot2 = Arc::clone(&snapshot);
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

    let mut counter = 0u32;
    loop {
        {
            let mut s = snapshot.lock().unwrap();
            s.rssi = -55 - (counter % 20) as i32;
            s.score = 42.0;
            s.band = if counter % 2 == 0 { "2.4" } else { "5" };
            s.state = "CONNECTED".to_string();
            counter += 1;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
