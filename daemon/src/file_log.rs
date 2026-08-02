//! 文件日志：双写 stderr + 文件，按大小轮转，供 Web 读取
//! 路径：/data/adb/amberguard/log/amberguard.log（开发机：./amberguard.log）

use log::{LevelFilter, Log, Metadata, Record};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const ANDROID_LOG_DIR: &str = "/data/adb/amberguard/log";
const LOG_NAME: &str = "amberguard.log";
const MAX_BYTES: u64 = 1_048_576; // 1MB

pub fn resolve_log_path() -> PathBuf {
    let dir = Path::new(ANDROID_LOG_DIR);
    if dir.is_dir() || dir.parent().is_some_and(|p| p.is_dir()) {
        let _ = fs::create_dir_all(dir);
        return dir.join(LOG_NAME);
    }
    PathBuf::from(LOG_NAME)
}

fn level_from_str(s: &str) -> LevelFilter {
    match s.to_ascii_lowercase().as_str() {
        "error" => LevelFilter::Error,
        "warn" | "warning" => LevelFilter::Warn,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        _ => LevelFilter::Info,
    }
}


struct FileLogger {
    path: PathBuf,
    file: Mutex<Option<File>>,
}

impl FileLogger {
    fn open(path: &Path) -> std::io::Result<File> {
        if let Some(p) = path.parent() {
            let _ = fs::create_dir_all(p);
        }
        OpenOptions::new().create(true).append(true).open(path)
    }

    fn rotate_if_needed(&self, slot: &mut Option<File>) {
        let Some(f) = slot.as_mut() else { return };
        let Ok(meta) = f.metadata() else { return };
        if meta.len() < MAX_BYTES {
            return;
        }
        let _ = f.flush();
        *slot = None;
        let old = self.path.with_extension("log.1");
        let _ = fs::rename(&self.path, &old);
        *slot = Self::open(&self.path).ok();
    }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        // 级别由 log::set_max_level 全局控制
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:>5} [{}] {}\n",
            now_stamp(),
            record.level(),
            record.target(),
            record.args()
        );
        let _ = std::io::stderr().write_all(line.as_bytes());
        if let Ok(mut guard) = self.file.lock() {
            self.rotate_if_needed(&mut guard);
            if let Some(f) = guard.as_mut() {
                let _ = f.write_all(line.as_bytes());
                let _ = f.flush();
            }
        }
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
        if let Ok(mut guard) = self.file.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = f.flush();
            }
        }
    }
}

fn now_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

/// 初始化全局日志。`level` 如 "info"/"debug"。
pub fn init(level: &str) {
    let path = resolve_log_path();
    let filter = level_from_str(level);
    let file = match FileLogger::open(&path) {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("amberguard: open log failed: {e}; stderr-only via env_logger");
            let _ = env_logger::Builder::from_env(
                env_logger::Env::default().default_filter_or(level),
            )
            .try_init();
            return;
        }
    };
    let logger = FileLogger {
        path,
        file: Mutex::new(file),
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(filter);
    }
}

/// 热更新日志级别（依赖 log 全局 max_level）
pub fn set_level(level: &str) {
    log::set_max_level(level_from_str(level));
}

/// 读末尾最多 `max_lines` 行
pub fn tail(max_lines: usize) -> String {
    let path = resolve_log_path();
    if let Ok(text) = fs::read_to_string(&path) {
        return last_lines(&text, max_lines);
    }
    let alt = Path::new("/data/adb/amberguard/service.log");
    if let Ok(t) = fs::read_to_string(alt) {
        return last_lines(&t, max_lines);
    }
    String::new()
}

fn last_lines(text: &str, max_lines: usize) -> String {
    if max_lines == 0 {
        return String::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.to_string();
    }
    lines[lines.len() - max_lines..].join("\n") + "\n"
}

pub fn clear() -> Result<(), String> {
    let path = resolve_log_path();
    if let Some(p) = path.parent() {
        let _ = fs::create_dir_all(p);
    }
    fs::write(&path, "").map_err(|e| e.to_string())
}

pub fn log_path_display() -> String {
    resolve_log_path().display().to_string()
}



