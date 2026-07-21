use chrono::{Local, SecondsFormat};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
const RETAINED_LOGS: usize = 5;

static LOGGER: OnceLock<Mutex<DiagnosticWriter>> = OnceLock::new();

struct DiagnosticWriter {
    directory: PathBuf,
    current_path: PathBuf,
    file: File,
    bytes_written: u64,
}

impl DiagnosticWriter {
    fn open(directory: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        let current_path = directory.join("echo.log");
        let bytes_written = fs::metadata(&current_path).map_or(0, |metadata| metadata.len());
        let file = open_append(&current_path)?;
        Ok(Self {
            directory,
            current_path,
            file,
            bytes_written,
        })
    }

    fn append(&mut self, line: &str) -> io::Result<()> {
        let additional = line.len() as u64 + 2;
        if self.bytes_written.saturating_add(additional) > MAX_LOG_BYTES {
            self.rotate()?;
        }
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\r\n")?;
        self.file.flush()?;
        self.bytes_written = self.bytes_written.saturating_add(additional);
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;

        let oldest = rotated_path(&self.directory, RETAINED_LOGS);
        if oldest.exists() {
            fs::remove_file(&oldest)?;
        }
        for index in (1..RETAINED_LOGS).rev() {
            let source = rotated_path(&self.directory, index);
            if source.exists() {
                fs::rename(source, rotated_path(&self.directory, index + 1))?;
            }
        }
        if self.current_path.exists() {
            fs::rename(&self.current_path, rotated_path(&self.directory, 1))?;
        }
        self.file = open_append(&self.current_path)?;
        self.bytes_written = 0;
        Ok(())
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn rotated_path(directory: &Path, index: usize) -> PathBuf {
    directory.join(format!("echo.{index}.log"))
}

pub fn log_directory() -> PathBuf {
    dirs_next::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("11th_echo")
        .join("logs")
}

pub fn init() -> Result<PathBuf, String> {
    let directory = log_directory();
    let writer = DiagnosticWriter::open(directory.clone()).map_err(|err| err.to_string())?;
    LOGGER
        .set(Mutex::new(writer))
        .map_err(|_| "diagnostic logging was initialized more than once".to_string())?;
    Ok(directory.join("echo.log"))
}

pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let location = panic_info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_else(|| "unknown".to_string());
        let payload = panic_info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| {
                panic_info
                    .payload()
                    .downcast_ref::<String>()
                    .map(String::as_str)
            })
            .unwrap_or("non-string panic payload");
        record(
            "ERROR",
            "panic",
            format_args!("location={location} message={payload}"),
        );
        previous(panic_info);
    }));
}

pub fn record(level: &str, component: &str, arguments: fmt::Arguments<'_>) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    let message = arguments
        .to_string()
        .replace('\r', "\\r")
        .replace('\n', "\\n");
    let timestamp = Local::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let thread_id = format!("{:?}", std::thread::current().id());
    let line = format!(
        "{timestamp} level={level} component={component} pid={} thread={} {message}",
        std::process::id(),
        thread_id
    );
    if let Ok(mut writer) = logger.lock() {
        if let Err(err) = writer.append(&line) {
            eprintln!("Failed to write Echo diagnostic log: {err}");
        }
    }
}

#[macro_export]
macro_rules! echo_info {
    ($component:expr, $($arg:tt)*) => {{
        let message = format!($($arg)*);
        println!("{}", message);
        $crate::diagnostics::record("INFO", $component, format_args!("{}", message));
    }};
}

#[macro_export]
macro_rules! echo_warn {
    ($component:expr, $($arg:tt)*) => {{
        let message = format!($($arg)*);
        eprintln!("{}", message);
        $crate::diagnostics::record("WARN", $component, format_args!("{}", message));
    }};
}

#[macro_export]
macro_rules! echo_error {
    ($component:expr, $($arg:tt)*) => {{
        let message = format!($($arg)*);
        eprintln!("{}", message);
        $crate::diagnostics::record("ERROR", $component, format_args!("{}", message));
    }};
}

#[cfg(test)]
mod tests {
    use super::{rotated_path, DiagnosticWriter};

    #[test]
    fn diagnostic_lines_are_written_to_the_current_log() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "echo-diagnostics-test-{}-{}",
            std::process::id(),
            unique
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let mut writer = DiagnosticWriter::open(directory.clone()).unwrap();
        writer.append("test diagnostic line").unwrap();
        let content = std::fs::read_to_string(directory.join("echo.log")).unwrap();
        assert!(content.contains("test diagnostic line"));
        drop(writer);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rotation_moves_the_previous_log_and_continues_writing() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "echo-diagnostics-rotation-test-{}-{}",
            std::process::id(),
            unique
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let mut writer = DiagnosticWriter::open(directory.clone()).unwrap();
        writer.append("before rotation").unwrap();
        writer.rotate().unwrap();
        writer.append("after rotation").unwrap();

        let previous = std::fs::read_to_string(directory.join("echo.1.log")).unwrap();
        let current = std::fs::read_to_string(directory.join("echo.log")).unwrap();
        assert!(previous.contains("before rotation"));
        assert!(!previous.contains("after rotation"));
        assert!(current.contains("after rotation"));

        drop(writer);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rotated_log_names_are_stable() {
        let directory = std::path::Path::new("logs");
        assert_eq!(rotated_path(directory, 3), directory.join("echo.3.log"));
    }
}
