// Log file tee for the existing env_logger backend.
//
// The app already installs env_logger (writes to stderr). A packaged build
// gives the user no console to read, so the same records are also appended to
// %LOCALAPPDATA%\Fluence\fluence.log - which is how a runtime failure gets
// diagnosed after the fact.
//
// This does NOT install a logger of its own: doing so makes the later
// env_logger::init() panic with SetLoggerError. It only supplies a Write
// adapter that tees env_logger's output to stderr and the file.
//
// Truncates once past MAX_BYTES so a long session cannot grow it unbounded.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::sync::Mutex;

/// Cap the on-disk log at ~2 MB.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

struct Tee {
    file: Option<Mutex<File>>,
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // stderr first: the sidecars inherit it, so spawn failures show live.
        let _ = io::stderr().write_all(buf);
        if let Some(file) = &self.file {
            if let Ok(mut f) = file.lock() {
                let _ = f.write_all(buf);
                let _ = f.flush();
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = io::stderr().flush();
        if let Some(file) = &self.file {
            if let Ok(mut f) = file.lock() {
                let _ = f.flush();
            }
        }
        Ok(())
    }
}

fn open_log_file() -> Option<File> {
    let dir = dirs::data_local_dir()?.join("Fluence");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("fluence.log");
    // Truncate rather than append once oversized, keeping the log bounded.
    let oversize = std::fs::metadata(&path)
        .map(|m| m.len() > MAX_BYTES)
        .unwrap_or(false);
    OpenOptions::new()
        .create(true)
        .write(true)
        .append(!oversize)
        .truncate(oversize)
        .open(&path)
        .ok()
}

/// Install env_logger so it writes to stderr AND fluence.log.
/// `FLUENCE_LOG` overrides the level, e.g. `FLUENCE_LOG=debug`.
pub fn init() {
    let level = std::env::var("FLUENCE_LOG")
        .ok()
        .and_then(|v| v.parse::<log::LevelFilter>().ok())
        .unwrap_or(log::LevelFilter::Info);

    // A failure here means a logger is already installed; keep the existing
    // one rather than panicking, which is what env_logger::init() does.
    let _ = env_logger::Builder::new()
        .filter_level(level)
        .target(env_logger::Target::Pipe(Box::new(Tee {
            file: open_log_file().map(Mutex::new),
        })))
        .try_init();
}
