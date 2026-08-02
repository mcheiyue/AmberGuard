// wpa_ctrl.rs — 对齐 hostap wpa_ctrl.c：
// SOCK_DGRAM + 客户端 bind 本地路径 + connect 服务端
// 小米等机型：ctrl_interface=/data/vendor/wifi/wpa/sockets，服务端名为 wlan0

use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum WpaError {
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse: {0}")]
    Parse(String),
    #[error("Not connected")]
    NotConnected,
}

#[derive(Debug, Clone, Default)]
pub struct WpaStatus {
    pub wpa_state: String,
    pub ssid: Option<String>,
    pub bssid: Option<String>,
    pub freq: Option<u32>,
    pub signal_dbm: Option<i32>,
    pub ip_address: Option<String>,
    pub disabled: Option<u32>,
    pub raw: HashMap<String, String>,
}

impl WpaStatus {
    pub fn parse(text: &str) -> Self {
        let mut s = WpaStatus::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some(eq) = line.find('=') else { continue };
            let key = line[..eq].trim();
            let val = line[eq + 1..].trim().to_string();
            match key {
                "wpa_state" => s.wpa_state = val,
                "ssid" => s.ssid = Some(val),
                "bssid" => s.bssid = Some(val),
                "freq" => s.freq = val.parse().ok(),
                // Android / 各厂商字段名不一
                "signal_dbm" | "signal" | "rssi" | "RSSI" => {
                    if s.signal_dbm.is_none() {
                        s.signal_dbm = val.parse().ok();
                    }
                }
                "ip_address" => s.ip_address = Some(val),
                "disabled" => s.disabled = val.parse().ok(),
                _ => {
                    s.raw.insert(key.to_string(), val);
                }
            }
        }
        s
    }
}

#[cfg(unix)]
mod platform {
    use std::os::unix::io::{AsRawFd, BorrowedFd, OwnedFd};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use nix::sys::socket::{
        bind, connect, socket, SockFlag, SockType, UnixAddr,
    };
    use nix::unistd::{read, write};

    use super::*;

    const BUF_SIZE: usize = 4096;

    fn fd_to_borrowed(fd: &OwnedFd) -> BorrowedFd<'_> {
        unsafe { BorrowedFd::borrow_raw(fd.as_raw_fd()) }
    }

    fn io_err(e: nix::Error) -> WpaError {
        WpaError::Io(std::io::Error::from_raw_os_error(e as i32))
    }

    pub struct WpaCtrlImpl {
        fd: Option<OwnedFd>,
        /// 服务端路径（日志用）
        ctrl_path: String,
        /// 客户端本地 bind 路径（文件系统 socket 需在 Drop 时 unlink）
        local_path: Option<PathBuf>,
    }

    impl WpaCtrlImpl {
        pub fn new(ctrl_path: String) -> Self {
            Self {
                fd: None,
                ctrl_path,
                local_path: None,
            }
        }

        pub fn connect(&mut self) -> Result<(), WpaError> {
            // hostap wpa_ctrl：SOCK_DGRAM，不是 SEQPACKET
            let fd = socket(
                nix::sys::socket::AddressFamily::Unix,
                SockType::Datagram,
                SockFlag::empty(),
                None,
            )
            .map_err(io_err)?;

            if self.ctrl_path.starts_with('@') {
                // abstract：客户端也 bind 一个 abstract 名，再 connect 服务端
                let client_name = format!("wpa_ctrl_{}", std::process::id());
                let local = UnixAddr::new_abstract(client_name.as_bytes()).map_err(io_err)?;
                bind(fd.as_raw_fd(), &local).map_err(io_err)?;
                let remote =
                    UnixAddr::new_abstract(self.ctrl_path[1..].as_bytes()).map_err(io_err)?;
                connect(fd.as_raw_fd(), &remote).map_err(io_err)?;
            } else {
                // 文件系统 socket：客户端必须 bind 在**同一目录**（wpa 才回得了包）
                // 禁止落到 /dev/socket 或 /data/local/tmp——会 connect 成功但 STATUS 超时
                let server = Path::new(&self.ctrl_path);
                let dir = server
                    .parent()
                    .ok_or_else(|| WpaError::Parse("ctrl path has no parent dir".into()))?;

                let local_path = dir.join(format!(
                    "wpa_ctrl_{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|t| t.as_nanos() % 1_000_000)
                        .unwrap_or(0)
                ));
                if local_path.as_os_str().len() >= 108 {
                    return Err(WpaError::Parse(format!(
                        "client path too long: {}",
                        local_path.display()
                    )));
                }
                let _ = std::fs::remove_file(&local_path);
                let local_addr = UnixAddr::new(local_path.as_os_str()).map_err(io_err)?;
                bind(fd.as_raw_fd(), &local_addr).map_err(|e| {
                    WpaError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "bind client in {} failed: {e} (need sepolicy wpa_data_file sock_file create)",
                            dir.display()
                        ),
                    ))
                })?;
                // wpa 进程 uid=wifi(1010)：客户端 socket 必须 wifi 组可写，否则 STATUS 必超时
                // 实机历史 socket 为 1021:1010 mode 660
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let path_str = local_path.to_string_lossy().to_string();
                    // chown wifi:wifi；失败再试 system:wifi(1021 在部分 OEM 上是 radio/system 变体)
                    let _ = std::process::Command::new("chown")
                        .args(["1010:1010", &path_str])
                        .status();
                    let _ = std::fs::set_permissions(
                        &local_path,
                        std::fs::Permissions::from_mode(0o660),
                    );
                    // 仍 root 独占则放宽到 666（保底）
                    if let Ok(meta) = std::fs::metadata(&local_path) {
                        use std::os::unix::fs::MetadataExt;
                        if meta.uid() == 0 {
                            let _ = std::fs::set_permissions(
                                &local_path,
                                std::fs::Permissions::from_mode(0o666),
                            );
                        }
                    }
                    let _ = std::process::Command::new("restorecon")
                        .arg(&path_str)
                        .status();
                    log::info!(
                        "wpa_ctrl: client sock {} meta={:?}",
                        path_str,
                        std::fs::metadata(&local_path).ok().map(|m| {
                            use std::os::unix::fs::MetadataExt;
                            format!("uid={} gid={} mode={:o}", m.uid(), m.gid(), m.mode() & 0o777)
                        })
                    );
                }
                self.local_path = Some(local_path);

                let remote = UnixAddr::new(server.as_os_str()).map_err(io_err)?;
                connect(fd.as_raw_fd(), &remote).map_err(io_err)?;
            }

            self.fd = Some(fd);
            log::info!("wpa_ctrl: connected to {}", self.ctrl_path);
            Ok(())
        }

        pub fn send_command(&self, cmd: &str) -> Result<(), WpaError> {
            let fd = self.fd.as_ref().ok_or(WpaError::NotConnected)?;
            write(fd_to_borrowed(fd), cmd.as_bytes()).map_err(io_err)?;
            Ok(())
        }

        pub fn receive_reply(&self, timeout: Duration) -> Result<String, WpaError> {
            let fd = self.fd.as_ref().ok_or(WpaError::NotConnected)?;
            let mut buf = vec![0u8; BUF_SIZE];
            let poll_timeout =
                nix::poll::PollTimeout::from(timeout.as_millis().try_into().unwrap_or(u16::MAX));
            let nready = nix::poll::poll(
                &mut [nix::poll::PollFd::new(
                    fd_to_borrowed(fd),
                    nix::poll::PollFlags::POLLIN,
                )],
                poll_timeout,
            )
            .map_err(io_err)?;
            if nready == 0 {
                return Err(WpaError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "wpa reply timeout",
                )));
            }
            let n = read(fd.as_raw_fd(), &mut buf).map_err(io_err)?;
            buf.truncate(n);
            String::from_utf8(buf).map_err(|_| WpaError::Parse("non-utf8 reply from wpa".into()))
        }

        pub fn command(&self, cmd: &str, timeout: Duration) -> Result<String, WpaError> {
            self.send_command(cmd)?;
            self.receive_reply(timeout)
        }

        pub fn status(&self) -> Result<WpaStatus, WpaError> {
            let raw = self.command("STATUS", Duration::from_secs(3))?;
            log::debug!("wpa STATUS raw:\n{raw}");
            Ok(WpaStatus::parse(&raw))
        }

        /// SIGNAL_POLL：小米 STATUS 无 RSSI，用此补 signal_dbm
        pub fn signal_poll(&self) -> Result<WpaStatus, WpaError> {
            let raw = self.command("SIGNAL_POLL", Duration::from_secs(3))?;
            log::debug!("wpa SIGNAL_POLL raw:\n{raw}");
            Ok(WpaStatus::parse(&raw))
        }
    }

    impl Drop for WpaCtrlImpl {
        fn drop(&mut self) {
            self.fd.take();
            if let Some(p) = self.local_path.take() {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    /// 是否像「服务端」控制口（不要连历史客户端 wpa_ctrl_* 残留）
    fn looks_like_server_socket(name: &str) -> bool {
        if name.starts_with("wpa_ctrl_") {
            return false;
        }
        // 接口名：wlan0 / p2p0 / wlan1 ...
        name.starts_with("wlan")
            || name.starts_with("p2p")
            || name.starts_with("wifi")
            || name == "wpa_wlan0"
            || name.starts_with("vendor_wpa")
    }

    pub fn discover() -> Vec<String> {
        let mut candidates = Vec::new();

        // 1) conf 明示的 DIR（小米实测）
        let conf_dirs = [
            "/data/vendor/wifi/wpa/sockets",
            "/data/misc/wifi/sockets",
            "/data/misc/wifi/wpa_supplicant",
            "/data/misc/wifi/mainline_supplicant/sockets",
            "/data/vendor/wifi/sockets",
        ];
        for dir in conf_dirs {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if looks_like_server_socket(&name) {
                        candidates.push(entry.path().to_string_lossy().to_string());
                    }
                }
            }
        }

        // 2) /dev/socket 下 vendor 控制口（小米：vendor_wpa_wlan0）
        if let Ok(entries) = std::fs::read_dir("/dev/socket") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.contains("wpa") || name.contains("wifi") {
                    candidates.push(entry.path().to_string_lossy().to_string());
                }
            }
        }

        // 3) abstract 常见名
        candidates.push("@wpa_wlan0".into());
        candidates.push("@wpa_wifi0".into());

        // 去重保序
        let mut seen = std::collections::HashSet::new();
        candidates.retain(|c| seen.insert(c.clone()));
        log::info!("wpa_ctrl: discover candidates: {candidates:?}");
        candidates
    }
}

#[cfg(not(unix))]
mod platform {
    use super::*;
    use std::time::Duration;

    pub struct WpaCtrlImpl {
        ctrl_path: String,
    }

    impl WpaCtrlImpl {
        pub fn new(ctrl_path: String) -> Self {
            Self { ctrl_path }
        }

        pub fn connect(&mut self) -> Result<(), WpaError> {
            log::warn!(
                "wpa_ctrl: mock connect (not unix), path={}",
                self.ctrl_path
            );
            Ok(())
        }

        pub fn command(&self, _cmd: &str, _timeout: Duration) -> Result<String, WpaError> {
            Ok("wpa_state=COMPLETED\nssid=MockWiFi\nsignal_dbm=-65\n".into())
        }

        pub fn status(&self) -> Result<WpaStatus, WpaError> {
            let raw = self.command("STATUS", Duration::from_secs(3))?;
            Ok(WpaStatus::parse(&raw))
        }

        pub fn signal_poll(&self) -> Result<WpaStatus, WpaError> {
            Ok(WpaStatus::parse("RSSI=-65\nFREQUENCY=5180\n"))
        }
    }

    pub fn discover() -> Vec<String> {
        vec!["/mock/socket/wpa_ctrl".into()]
    }
}

pub struct WpaCtrl {
    inner: platform::WpaCtrlImpl,
    connected: bool,
}

impl WpaCtrl {
    pub fn auto_connect() -> Result<Self, WpaError> {
        let candidates = platform::discover();
        log::info!("wpa_ctrl: probing {} candidates", candidates.len());
        let mut last_err = WpaError::NotConnected;
        for path in &candidates {
            let mut wpa = WpaCtrl {
                inner: platform::WpaCtrlImpl::new(path.clone()),
                connected: false,
            };
            match wpa.inner.connect() {
                Ok(()) => {
                    // 连通后再发 STATUS 确认不是僵尸 socket
                    match wpa.inner.status() {
                        Ok(st) => {
                            log::info!(
                                "wpa_ctrl: OK via {path} state={} ssid={:?}",
                                st.wpa_state,
                                st.ssid
                            );
                            wpa.connected = true;
                            return Ok(wpa);
                        }
                        Err(e) => {
                            log::warn!("wpa_ctrl: {path} connected but STATUS failed: {e}");
                            last_err = e;
                        }
                    }
                }
                Err(e) => {
                    log::info!("wpa_ctrl: {path} failed: {e}");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    pub fn connect_path(path: &str) -> Result<Self, WpaError> {
        let mut wpa = WpaCtrl {
            inner: platform::WpaCtrlImpl::new(path.into()),
            connected: false,
        };
        wpa.inner.connect()?;
        wpa.connected = true;
        Ok(wpa)
    }

    pub fn status(&self) -> Result<WpaStatus, WpaError> {
        self.inner.status()
    }

    pub fn signal_poll(&self) -> Result<WpaStatus, WpaError> {
        self.inner.signal_poll()
    }

    /// STATUS + 必要时 SIGNAL_POLL 补 RSSI（小米实测 STATUS 无信号字段）
    pub fn status_with_signal(&self) -> Result<WpaStatus, WpaError> {
        let mut st = self.status()?;
        if st.signal_dbm.is_none() {
            if let Ok(sig) = self.signal_poll() {
                if let Some(rssi) = sig.signal_dbm {
                    st.signal_dbm = Some(rssi);
                }
                // 合并 raw 便于调试
                for (k, v) in sig.raw {
                    st.raw.entry(k).or_insert(v);
                }
            }
        }
        Ok(st)
    }

    pub fn command(&self, cmd: &str) -> Result<String, WpaError> {
        self.inner.command(cmd, std::time::Duration::from_secs(3))
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// 期望回复含 OK（ROAM/SELECT 等）
    fn expect_ok(&self, cmd: &str) -> Result<(), WpaError> {
        let rep = self.command(cmd)?;
        if rep.contains("OK") || rep.trim() == "OK" {
            Ok(())
        } else if rep.contains("FAIL") {
            Err(WpaError::Parse(format!("{cmd} => {rep}")))
        } else {
            // 部分实现只回空/其它；仍返回原文错误便于排查
            Err(WpaError::Parse(format!("{cmd} unexpected: {rep}")))
        }
    }

    pub fn ping(&self) -> Result<bool, WpaError> {
        let r = self.command("PING")?;
        Ok(r.contains("PONG"))
    }

    pub fn roam(&self, bssid: &str) -> Result<(), WpaError> {
        self.expect_ok(&format!("ROAM {bssid}"))
    }

    pub fn list_networks(&self) -> Result<String, WpaError> {
        self.command("LIST_NETWORKS")
    }

    pub fn scan_results(&self) -> Result<String, WpaError> {
        self.command("SCAN_RESULTS")
    }

    pub fn select_network(&self, id: u32) -> Result<(), WpaError> {
        self.expect_ok(&format!("SELECT_NETWORK {id}"))
    }

    pub fn set_network_bssid(&self, id: u32, bssid: &str) -> Result<(), WpaError> {
        // bssid "" 清除锁定
        self.expect_ok(&format!("SET_NETWORK {id} bssid {bssid}"))
    }

    pub fn disable_network(&self, id: u32) -> Result<(), WpaError> {
        self.expect_ok(&format!("DISABLE_NETWORK {id}"))
    }

    pub fn enable_network(&self, id: u32) -> Result<(), WpaError> {
        // wpa 用 disabled 0，无 enabled=1
        self.expect_ok(&format!("SET_NETWORK {id} disabled 0"))
    }
}
