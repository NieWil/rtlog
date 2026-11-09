//! `stdout::init` and `init_with`: the level they set (from `RTLOG` or the
//! `Config`), where the text goes, and when it is coloured. `init` changes the
//! process-wide level and starts a drain thread, so every case runs in a child
//! process (this test binary, re-run with `RTLOG_STDOUT_CHILD` set).

use std::process::{Command, Output, Stdio};

use rtlog_decode::stdout::{init, init_with, Config, Target};

const CHILD_ENV: &str = "RTLOG_STDOUT_CHILD";
/// The child prints the level `init` left behind on a line starting with this.
const LEVEL_MARK: &str = "child level: ";

/// The child half: runs only when the parent sets `RTLOG_STDOUT_CHILD`.
#[test]
fn stdout_child() {
    let Ok(mode) = std::env::var(CHILD_ENV) else {
        return;
    };
    let guard = match mode.as_str() {
        "init" => init(),
        "config_warn" => init_with(Config {
            level: Some(rtlog::Level::Warn),
            ..Config::default()
        }),
        "stderr" => init_with(Config {
            target: Target::Stderr,
            ..Config::default()
        }),
        "color" => init_with(Config {
            color: Some(true),
            ..Config::default()
        }),
        "no_color" => init_with(Config {
            color: Some(false),
            ..Config::default()
        }),
        m => panic!("unknown child mode {m}"),
    };
    rtlog::trace!("probe {}", "trace");
    rtlog::debug!("probe {}", "debug");
    rtlog::info!("probe {}", "info");
    rtlog::warn!("probe {}", "warn");
    rtlog::error!("probe {}", "error");
    // Dropping the guard stops the drain and prints what is left.
    drop(guard);
    println!("{LEVEL_MARK}{:?}", rtlog::level::level());
}

/// This test binary, launched through the target runner (QEMU) when the tests
/// themselves run under one.
fn child_cmd(mode: &str) -> Command {
    let exe = std::env::current_exe().unwrap();
    let runner = std::env::vars()
        .find(|(k, v)| {
            k.starts_with("CARGO_TARGET_") && k.ends_with("_RUNNER") && !v.trim().is_empty()
        })
        .map(|(_, v)| v);
    let mut cmd = match &runner {
        Some(r) => {
            let mut parts = r.split_whitespace();
            let mut c = Command::new(parts.next().unwrap());
            c.args(parts).arg(&exe);
            c
        }
        None => Command::new(&exe),
    };
    cmd.args(["--exact", "stdout_child", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, mode)
        .env_remove("RTLOG")
        .env_remove("NO_COLOR");
    cmd
}

fn run(cmd: &mut Command) -> Output {
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "child failed: {out:?}");
    out
}

/// The levels of the probe lines in `text`, in order.
fn probes(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.split_once("probe ").map(|(_, lvl)| lvl.to_owned()))
        .collect()
}

/// What the child printed after `LEVEL_MARK`.
fn level(text: &str) -> &str {
    text.lines()
        .find_map(|l| l.split_once(LEVEL_MARK).map(|(_, v)| v))
        .unwrap_or_else(|| panic!("no level line in {text}"))
}

#[test]
fn init_defaults_to_info_on_stdout_without_colour_on_a_pipe() {
    let out = run(&mut child_cmd("init"));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(probes(&stdout), ["info", "warn", "error"], "{stdout}");
    assert_eq!(level(&stdout), "Some(Info)");
    assert!(stdout.contains(" INFO  probe info"), "{stdout}");
    assert!(
        !stdout.contains('\x1b'),
        "a pipe gets no colour: {stdout:?}"
    );
    assert!(probes(&String::from_utf8_lossy(&out.stderr)).is_empty());
}

#[test]
fn init_takes_the_level_from_rtlog() {
    let out = run(child_cmd("init").env("RTLOG", "debug"));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        probes(&stdout),
        ["debug", "info", "warn", "error"],
        "{stdout}"
    );
    assert_eq!(level(&stdout), "Some(Debug)");

    let out = run(child_cmd("init").env("RTLOG", "trace"));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        probes(&stdout),
        ["trace", "debug", "info", "warn", "error"],
        "{stdout}"
    );
}

#[test]
fn init_with_rtlog_off_prints_nothing() {
    let out = run(child_cmd("init").env("RTLOG", "off"));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(probes(&stdout).is_empty(), "{stdout}");
    assert_eq!(level(&stdout), "None");
}

#[test]
fn init_falls_back_to_info_for_an_unknown_rtlog_value() {
    let out = run(child_cmd("init").env("RTLOG", "verbose"));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(probes(&stdout), ["info", "warn", "error"], "{stdout}");
    assert_eq!(level(&stdout), "Some(Info)");
}

#[test]
fn config_level_overrides_rtlog() {
    let out = run(child_cmd("config_warn").env("RTLOG", "trace"));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(probes(&stdout), ["warn", "error"], "{stdout}");
    assert_eq!(level(&stdout), "Some(Warn)");
}

#[test]
fn stderr_target_prints_to_stderr_only() {
    let out = run(&mut child_cmd("stderr"));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(probes(&stdout).is_empty(), "{stdout}");
    assert_eq!(probes(&stderr), ["info", "warn", "error"], "{stderr}");
    assert!(
        !stderr.contains('\x1b'),
        "a pipe gets no colour: {stderr:?}"
    );
}

#[test]
fn config_color_forces_ansi_on_or_off() {
    let out = run(&mut child_cmd("color"));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let warn = stdout.lines().find(|l| l.contains("probe warn")).unwrap();
    assert!(warn.contains("\x1b["), "colour forced on: {warn:?}");

    // Forced off even on a terminal is checked below; on a pipe it is plain.
    let out = run(&mut child_cmd("no_color"));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(probes(&stdout), ["info", "warn", "error"], "{stdout}");
    assert!(!stdout.contains('\x1b'), "{stdout:?}");
}

/// Run the child with its stdout on a pseudo-terminal; returns what the
/// terminal received.
#[cfg(target_os = "linux")]
fn run_on_terminal(cmd: &mut Command) -> String {
    use std::io::Read;
    use std::os::fd::{FromRawFd, OwnedFd};

    let (mut master, mut slave) = (0, 0);
    // SAFETY: openpty writes two new descriptors; the null pointers ask for
    // no name, default terminal settings and window size.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: both descriptors are fresh and owned here.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let mut child = cmd
        .stdout(Stdio::from(slave))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // The Command holds the parent's copy of the slave; close it so the
    // master reads end of file (EIO on Linux) once the child exits.
    drop(std::mem::replace(cmd, Command::new("true")));
    let mut master = std::fs::File::from(master);
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => panic!("reading the terminal: {e}"),
        }
    }
    assert!(child.wait().unwrap().success());
    String::from_utf8(out).unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn init_colours_a_terminal_unless_no_color_is_set() {
    let text = run_on_terminal(&mut child_cmd("init"));
    let warn = text.lines().find(|l| l.contains("probe warn")).unwrap();
    assert!(warn.contains("\x1b["), "a terminal gets colour: {text:?}");

    let text = run_on_terminal(child_cmd("init").env("NO_COLOR", "1"));
    assert_eq!(probes(&text), ["info", "warn", "error"], "{text:?}");
    assert!(!text.contains('\x1b'), "NO_COLOR turns it off: {text:?}");

    let text = run_on_terminal(&mut child_cmd("no_color"));
    assert_eq!(probes(&text), ["info", "warn", "error"], "{text:?}");
    assert!(!text.contains('\x1b'), "Config::color wins: {text:?}");
}
