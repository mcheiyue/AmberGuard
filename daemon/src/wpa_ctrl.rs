// wpa_ctrl.rs — wpa_supplicant 控制接口
// Unix：真 socket 连接（nix::sys::socket）；其他平台：模拟 stub（用于 Windows 本地编译）

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

/// wpa STATUS 的解析结果
#[derive(Debug, Clone, Default)]
pub struct WpaStatus {
    pub wpa_state: String,
    pub ssid: Option<String>,
    pub bssid: Option<String>,
    pub freq: Option<u32>,
    pub signal_dbm: Option<i32>,
    pub ip_address: Option<String>,
    pub disabled: Option<u32>,
    /// 其他未识别的字段
    pub raw: HashMap<String, String>,
}

impl WpaStatus {
    /// 从 STATUS 命令的文本输出解析
    pub fn parse(text: &str) -> Self {
        let mut s = WpaStatus::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(eq) = line.find('=') {
                let key = line[..eq].trim().to_string();
                let val = line[eq + 1..].trim().to_string();
                match key.as_str() {
                    "wpa_state" => s.wpa_state = val,
                    "ssid" => s.ssid = Some(val),
                    "bssid" => s.bssid = Some(val),
                    "freq" => s.freq = val.parse::<u32>().ok(),
                    "signal_dbm" => s.signal_dbm = val.parse::<i32>().ok(),
                    "ip_address" => s.ip_address = Some(val),
                    "disabled" => s.disabled = val.parse::<u32>().ok(),
                    _ => { s.raw.insert(key, val); }
                }
            }
        }
        s
    }
}

// ── 平台实现 ──────────────────────────────────────────

#[cfg(unix)]
mod platform {
    use std::os::unix::io::{AsRawFd, BorrowedFd, OwnedFd};
    use std::time::Duration;

    use nix::sys::socket::{socket, connect, UnixAddr, SockFlag, SockType};
    use nix::unistd::{read, write};

    use super::*;

    const BUF_SIZE: usize = 4096;

    fn fd_to_borrowed(fd: &OwnedFd) -> BorrowedFd<'_> {
        unsafe { BorrowedFd::borrow_raw(fd.as_raw_fd()) }
    }

    pub struct WpaCtrlImpl {
        fd: Option<OwnedFd>,
        ctrl_path: String,
    }

    impl WpaCtrlImpl {
        pub fn new(ctrl_path: String) -> Self {
            Self { fd: None, ctrl_path }
        }

        pub fn connect(&mut self) -> Result<(), WpaError> {
            let fd = socket(
                nix::sys::socket::AddressFamily::Unix,
                SockType::SeqPacket,
                SockFlag::empty(),
                None,
            ).map_err(|e| WpaError::Io(std::io::Error::from_raw_os_error(e as i32)))?;

            let sock_addr = if self.ctrl_path.starts_with('@') {
                UnixAddr::new_abstract(self.ctrl_path[1..].as_bytes())
                    .map_err(|e| WpaError::Io(std::io::Error::from_raw_os_error(e as i32)))?
            } else {
                UnixAddr::new(self.ctrl_path.as_bytes())
                    .map_err(|e| WpaError::Io(std::io::Error::from_raw_os_error(e as i32)))?
            };

            connect(fd.as_raw_fd(), &sock_addr)
                .map_err(|e| WpaError::Io(std::io::Error::from_raw_os_error(e as i32)))?;

            self.fd = Some(fd);
            log::info!("wpa_ctrl: connected to {}", self.ctrl_path);
            Ok(())
        }

        pub fn send_command(&self, cmd: &str) -> Result<(), WpaError> {
            let fd = self.fd.as_ref().ok_or(WpaError::NotConnected)?;
            write(fd_to_borrowed(fd), cmd.as_bytes())
                .map_err(|e| WpaError::Io(std::io::Error::from_raw_os_error(e as i32)))?;
            Ok(())
        }

        pub fn receive_reply(&self, timeout: Duration) -> Result<String, WpaError> {
            let fd = self.fd.as_ref().ok_or(WpaError::NotConnected)?;
            let mut buf = vec![0u8; BUF_SIZE];
            let poll_timeout = nix::poll::PollTimeout::from(
                timeout.as_millis().try_into().unwrap_or(u16::MAX)
            );
            nix::poll::poll(
                &mut [nix::poll::PollFd::new(
                    fd_to_borrowed(fd),
                    nix::poll::PollFlags::POLLIN,
                )],
                poll_timeout,
            ).map_err(|e| WpaError::Io(std::io::Error::from_raw_os_error(e as i32)))?;

            let n = read(fd_to_borrowed(fd), &mut buf)
                .map_err(|e| WpaError::Io(std::io::Error::from_raw_os_error(e as i32)))?;
            buf.truncate(n);
            String::from_utf8(buf)
                .map_err(|_| WpaError::Parse("non-utf8 reply from wpa".into()))
        }

        pub fn command(&self, cmd: &str, timeout: Duration) -> Result<String, WpaError> {
            self.send_command(cmd)?;
            self.receive_reply(timeout)
        }

        pub fn status(&self) -> Result<WpaStatus, WpaError> {
            let raw = self.command("STATUS", Duration::from_secs(3))?;
            Ok(WpaStatus::parse(&raw))
        }
    }

    pub fn discover() -> Vec<String> {
        let mut candidates = Vec::new();
        candidates.push("@wpa_wlan0".into());
        for dir in &[
            "/data/misc/wifi/sockets",
            "/data/vendor/wifi/sockets",
            "/data/misc/wifi",
        ] {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                    if name.starts_with("wpa_ctrl") || name.starts_with("wpa_wlan") {
                        candidates.push(p.to_string_lossy().to_string());
                    }
                }
            }
        }
        candidates
    }
}

// ── Windows / 非 Unix mock ──────────────────────────

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
            log::warn!("wpa_ctrl: mock connect (not unix), path={}", self.ctrl_path);
            Ok(())
        }

        pub fn command(&self, _cmd: &str, _timeout: Duration) -> Result<String, WpaError> {
            Ok("wpa_state=COMPLETED\nssid=MockWiFi\nsignal_dbm=-65\n".into())
        }

        pub fn status(&self) -> Result<WpaStatus, WpaError> {
            let raw = self.command("STATUS", Duration::from_secs(3))?;
            Ok(WpaStatus::parse(&raw))
        }
    }

    pub fn discover() -> Vec<String> {
        vec!["/mock/socket/wpa_ctrl".into()]
    }
}

// ── 对外暴露的 WpaCtrl 结构体 ─────────────────────

pub struct WpaCtrl {
    inner: platform::WpaCtrlImpl,
    connected: bool,
}

impl WpaCtrl {
    /// 创建并尝试自动发现 & 连接
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
                    wpa.connected = true;
                    log::info!("wpa_ctrl: connected via {path}");
                    return Ok(wpa);
                }
                Err(e) => {
                    log::debug!("wpa_ctrl: {path} failed: {e}");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    /// 直接指定路径连接
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

    pub fn command(&self, cmd: &str) -> Result<String, WpaError> {
        self.inner.command(cmd, std::time::Duration::from_secs(3))
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }
}