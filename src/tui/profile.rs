use std::env;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

struct ProfileConfig {
    threshold: Duration,
    file: Mutex<File>,
}

static PROFILE: LazyLock<Option<ProfileConfig>> = LazyLock::new(|| {
    let path = env::var_os("AICS_TUI_PROFILE_FILE")?;
    let threshold_ms = env::var("AICS_TUI_PROFILE_THRESHOLD_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(8);
    let file = open_profile_file(PathBuf::from(path)).ok()?;
    Some(ProfileConfig {
        threshold: Duration::from_millis(threshold_ms),
        file: Mutex::new(file),
    })
});

pub struct Scope {
    label: &'static str,
    start: Instant,
}

pub fn enabled() -> bool {
    PROFILE.is_some()
}

pub fn scope(label: &'static str) -> Option<Scope> {
    enabled().then_some(Scope {
        label,
        start: Instant::now(),
    })
}

pub fn record(label: &'static str, elapsed: Duration) {
    let Some(profile) = PROFILE.as_ref() else {
        return;
    };
    if elapsed < profile.threshold {
        return;
    }

    let millis = elapsed.as_secs_f64() * 1000.0;
    if let Ok(mut file) = profile.file.lock() {
        let _ = writeln!(file, "{millis:>8.3} ms  {label}");
    }
}

pub fn event(label: &'static str) {
    let Some(profile) = PROFILE.as_ref() else {
        return;
    };
    if let Ok(mut file) = profile.file.lock() {
        let _ = writeln!(file, "event          {label}");
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        record(self.label, self.start.elapsed());
    }
}

fn open_profile_file(path: PathBuf) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}
