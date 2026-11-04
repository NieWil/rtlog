//! Log levels and the runtime level filter.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Severity of a log site, lowest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Level {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl Level {
    /// Lower-case name, as written in the contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    /// Parse a lower-case level name.
    pub fn parse(s: &str) -> Option<Level> {
        Some(match s {
            "trace" => Level::Trace,
            "debug" => Level::Debug,
            "info" => Level::Info,
            "warn" => Level::Warn,
            "error" => Level::Error,
            _ => return None,
        })
    }

    const fn from_u8(v: u8) -> Option<Level> {
        Some(match v {
            0 => Level::Trace,
            1 => Level::Debug,
            2 => Level::Info,
            3 => Level::Warn,
            4 => Level::Error,
            _ => return None,
        })
    }
}

/// Value meaning "everything disabled" in the runtime filter.
const OFF: u8 = 5;

/// Compile-time floor, from the `RTLOG_LEVEL` environment variable at build time.
///
/// Sites below this level are compiled out of the call path.
pub const STATIC_MAX_OFF: bool = matches!(option_env!("RTLOG_LEVEL"), Some(s) if eq(s, "off"));

/// Lowest level compiled in.
pub const STATIC_MIN: Level = match option_env!("RTLOG_LEVEL") {
    Some(s) if eq(s, "trace") => Level::Trace,
    Some(s) if eq(s, "debug") => Level::Debug,
    Some(s) if eq(s, "info") => Level::Info,
    Some(s) if eq(s, "warn") => Level::Warn,
    Some(s) if eq(s, "error") => Level::Error,
    _ => Level::Trace,
};

const fn eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

static RUNTIME_MIN: AtomicU8 = AtomicU8::new(Level::Trace as u8);

/// Set the lowest level that is recorded at runtime (one atomic store).
pub fn set_level(level: Level) {
    RUNTIME_MIN.store(level as u8, Ordering::Relaxed);
}

/// Disable every site at runtime.
pub fn set_off() {
    RUNTIME_MIN.store(OFF, Ordering::Relaxed);
}

static SPANS: AtomicBool = AtomicBool::new(true);

/// Enable or disable span recording at runtime. Spans are independent of the
/// level filter: `set_level(Level::Warn)` keeps the Perfetto timeline, and
/// `set_spans(false)` removes it while log records continue. `set_off` stops
/// both.
pub fn set_spans(on: bool) {
    SPANS.store(on, Ordering::Relaxed);
}

/// Whether spans are recorded (two relaxed loads).
#[inline(always)]
pub fn spans_enabled() -> bool {
    !STATIC_MAX_OFF && SPANS.load(Ordering::Relaxed) && RUNTIME_MIN.load(Ordering::Relaxed) != OFF
}

/// Current runtime level, or `None` when everything is off.
pub fn level() -> Option<Level> {
    Level::from_u8(RUNTIME_MIN.load(Ordering::Relaxed))
}

/// Whether a site at `level` is recorded (one relaxed load).
#[inline(always)]
pub fn enabled(level: Level) -> bool {
    !STATIC_MAX_OFF && level >= STATIC_MIN && (level as u8) >= RUNTIME_MIN.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_roundtrip() {
        for l in [
            Level::Trace,
            Level::Debug,
            Level::Info,
            Level::Warn,
            Level::Error,
        ] {
            assert_eq!(Level::parse(l.as_str()), Some(l));
        }
        assert_eq!(Level::parse("loud"), None);
    }

    #[test]
    fn ordering() {
        assert!(Level::Error > Level::Warn && Level::Warn > Level::Info);
        assert!(Level::Info > Level::Debug && Level::Debug > Level::Trace);
    }

    #[test]
    fn const_eq() {
        assert!(eq("info", "info"));
        assert!(!eq("info", "inf"));
        assert!(!eq("info", "infx"));
    }
}
