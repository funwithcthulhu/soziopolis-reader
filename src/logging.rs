use crate::app_paths;
use anyhow::{Context, Result};
use regex::Regex;
use std::{
    backtrace::Backtrace,
    fs::{File, OpenOptions},
    io::Write,
    panic::PanicHookInfo,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();
static TOKEN_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();

pub fn init() -> Result<PathBuf> {
    let path = app_paths::app_log_path()?;
    if LOG_FILE.get().is_none() {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open log file {}", path.display()))?;
        let _ = LOG_FILE.set(Mutex::new(file));
        let _ = LOG_PATH.set(path.clone());
        write_line("INFO", &format!("logger initialized at {}", path.display()));
    }
    Ok(path)
}

pub fn log_path() -> Option<PathBuf> {
    LOG_PATH
        .get()
        .cloned()
        .or_else(|| app_paths::app_log_path().ok())
}

pub fn install_panic_hook() {
    PANIC_HOOK_INSTALLED.get_or_init(|| {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let message = describe_panic(panic_info);
            let backtrace = Backtrace::force_capture();
            write_line("PANIC", &format!("{message}\nBacktrace:\n{backtrace}"));
            previous_hook(panic_info);
        }));
    });
}

pub fn info(message: impl AsRef<str>) {
    write_line("INFO", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    write_line("WARN", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    write_line("ERROR", message.as_ref());
}

fn write_line(level: &str, message: &str) {
    if LOG_FILE.get().is_none() {
        let _ = init();
    }

    let timestamp = timestamp_string();
    let line = format!("[{timestamp}] [{level}] {}\n", sanitize_message(message));

    if let Some(file) = LOG_FILE.get()
        && let Ok(mut guard) = file.lock()
    {
        let _ = guard.write_all(line.as_bytes());
        let _ = guard.flush();
    }
}

pub(crate) fn sanitize_message(message: &str) -> String {
    let mut sanitized = message.to_owned();
    for pattern in redaction_patterns() {
        sanitized = pattern
            .replace_all(&sanitized, "${prefix}[REDACTED]")
            .into_owned();
    }
    sanitized
}

fn redaction_patterns() -> &'static [Regex] {
    TOKEN_PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r#"(?i)(?P<prefix>authorization\s*[:=]\s*token\s+)[A-Za-z0-9._-]+"#)
                .expect("authorization token redaction regex"),
            Regex::new(
                r#"(?i)(?P<prefix>["']?(?:api[_ -]?key|[\w.-]*token|[\w.-]*password|credential)["']?\s*[:=]\s*["']?)[^"',\s]+"#,
            )
            .expect("generic secret redaction regex"),
        ]
    })
}

fn describe_panic(panic_info: &PanicHookInfo<'_>) -> String {
    let payload = if let Some(message) = panic_info.payload().downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = panic_info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    };

    if let Some(location) = panic_info.location() {
        format!(
            "panic at {}:{}:{}: {}",
            location.file(),
            location.line(),
            location.column(),
            payload
        )
    } else {
        format!("panic: {payload}")
    }
}

fn timestamp_string() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("{}.{:03}", duration.as_secs(), duration.subsec_millis()),
        Err(_) => "time-error".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_message;

    #[test]
    fn sanitize_message_redacts_common_secret_patterns() {
        let message = "Authorization: Token abc123 password=secret api_key:\"xyz\" token=mytoken credential=stored";
        let sanitized = sanitize_message(message);
        assert!(!sanitized.contains("abc123"));
        assert!(!sanitized.contains("secret"));
        assert!(!sanitized.contains("xyz"));
        assert!(!sanitized.contains("mytoken"));
        assert!(!sanitized.contains("stored"));
        assert!(sanitized.contains("[REDACTED]"));
    }
}
