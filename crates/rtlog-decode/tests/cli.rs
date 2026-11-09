mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

/// The CLI binary, launched through the target runner (QEMU) when the tests
/// themselves run under one, as in cross-architecture CI.
fn rtlog_cmd() -> Command {
    let bin = env!("CARGO_BIN_EXE_rtlog");
    let runner = std::env::vars()
        .find(|(k, v)| {
            k.starts_with("CARGO_TARGET_") && k.ends_with("_RUNNER") && !v.trim().is_empty()
        })
        .map(|(_, v)| v);
    match runner {
        Some(r) => {
            let mut parts = r.split_whitespace();
            let mut c = Command::new(parts.next().unwrap());
            c.args(parts).arg(bin);
            c
        }
        None => Command::new(bin),
    }
}

/// This binary's capture: one span with a record inside it, then a warning.
/// Every drain consumes every thread's ring, so the tests share one capture
/// made once instead of logging and draining in parallel.
fn capture() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        {
            let _s = rtlog::span!("cli.step");
            rtlog::info!("cli speed={} ok={}", 1200u16, true);
        }
        rtlog::warn!("cli temp={}", 81u8);
        common::drain_all()
    })
}

/// A directory for one test's files, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("rtlog-cli-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// Write `bytes` to `name` in the directory; returns its path.
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// This binary's contract and capture, as files.
    fn contract_and_stream(&self) -> (PathBuf, PathBuf) {
        let contract = self.0.join("app.contract.jsonl");
        rtlog::contract::write(&contract).unwrap();
        (contract, self.file("app.rtlog", capture()))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn decode(contract: &Path, stream: &Path, extra: &[&str]) -> Output {
    rtlog_cmd()
        .arg("decode")
        .args(extra)
        .arg("--contract")
        .arg(contract)
        .arg(stream)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8(b.to_vec()).unwrap()
}

/// Asserts the command failed and returns its stderr.
fn failed(out: &Output) -> String {
    assert!(!out.status.success(), "expected failure: {out:?}");
    text(&out.stderr)
}

/// Asserts the command succeeded and returns its stdout.
fn succeeded(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    text(&out.stdout)
}

#[test]
fn cli_decodes_files_and_checks_contracts() {
    let dir = Scratch::new("basic");
    let (contract, stream) = dir.contract_and_stream();

    let out = decode(&contract, &stream, &[]);
    let stdout = succeeded(&out);
    assert!(
        stdout.contains("INFO    cli speed=1200 ok=true"),
        "{stdout}"
    );
    assert!(
        text(&out.stderr).starts_with("2 records, 1 spans, "),
        "summary on stderr: {out:?}"
    );

    let out = decode(&contract, &stream, &["--json"]);
    let v: serde_json::Value =
        serde_json::from_slice(out.stdout.split(|&b| b == b'\n').next().unwrap()).unwrap();
    assert_eq!(v["message"], "cli speed=1200 ok=true");

    let out = rtlog_cmd().arg("check").arg(&contract).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("contract hash"));

    let out = rtlog_cmd().arg("bogus").output().unwrap();
    assert!(!out.status.success());
    let out = rtlog_cmd()
        .args(["decode", "--contract"])
        .arg(&contract)
        .arg(dir.0.join("missing"))
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn every_subcommand_prints_usage_on_help() {
    for args in [
        &["--help"][..],
        &["decode", "--help"],
        &["check", "--help"],
        &["contract", "-h"],
    ] {
        let out = rtlog_cmd().args(args).output().unwrap();
        assert!(out.status.success(), "rtlog {args:?}: {out:?}");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("rtlog decode --contract"),
            "rtlog {args:?}: {text}"
        );
    }
}

#[test]
fn out_selects_text_json_or_perfetto() {
    let dir = Scratch::new("out");
    let (contract, stream) = dir.contract_and_stream();

    let plain = succeeded(&decode(&contract, &stream, &[]));
    assert_eq!(
        succeeded(&decode(&contract, &stream, &["--out", "text"])),
        plain
    );

    let json = succeeded(&decode(&contract, &stream, &["--out", "json"]));
    assert_eq!(json, succeeded(&decode(&contract, &stream, &["--json"])));
    let first: serde_json::Value = serde_json::from_str(json.lines().next().unwrap()).unwrap();
    assert!(first.is_object(), "{json}");

    let trace = succeeded(&decode(&contract, &stream, &["--out", "perfetto"]));
    let v: serde_json::Value = serde_json::from_str(&trace).unwrap();
    assert!(v["traceEvents"].is_array(), "{trace}");
    for same in [&["--out", "chrome"][..], &["--perfetto"]] {
        assert_eq!(
            succeeded(&decode(&contract, &stream, same)),
            trace,
            "{same:?}"
        );
    }
}

#[test]
fn an_unknown_output_format_is_rejected_with_usage() {
    let dir = Scratch::new("out-bad");
    let (contract, stream) = dir.contract_and_stream();
    let err = failed(&decode(&contract, &stream, &["--out", "xml"]));
    assert!(err.starts_with("unknown output \"xml\"\nusage:"), "{err}");
}

#[test]
fn color_always_paints_and_never_or_auto_on_a_pipe_does_not() {
    let dir = Scratch::new("color");
    let (contract, stream) = dir.contract_and_stream();

    let always = succeeded(&decode(&contract, &stream, &["--color", "always"]));
    let warn = always.lines().find(|l| l.contains("cli temp=81")).unwrap();
    assert!(warn.contains("\x1b["), "{warn:?}");
    for c in ["never", "auto"] {
        let out = succeeded(&decode(&contract, &stream, &["--color", c]));
        assert!(out.contains("cli temp=81"), "--color {c}: {out}");
        assert!(!out.contains('\x1b'), "--color {c}: {out:?}");
    }
}

#[test]
fn an_unknown_color_value_is_rejected() {
    let dir = Scratch::new("color-bad");
    let (contract, stream) = dir.contract_and_stream();
    let err = failed(&decode(&contract, &stream, &["--color", "rainbow"]));
    assert_eq!(err, "unknown --color \"rainbow\"\n");
}

/// Run `cmd` with its stdout on a pseudo-terminal; returns what the terminal
/// received.
#[cfg(target_os = "linux")]
fn run_on_terminal(mut cmd: Command) -> String {
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
        .stdout(std::process::Stdio::from(slave))
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // The Command holds the parent's copy of the slave; close it so the
    // master reads end of file (EIO on Linux) once the child exits.
    drop(cmd);
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
fn color_auto_paints_a_terminal_unless_no_color_is_set() {
    let dir = Scratch::new("color-tty");
    let (contract, stream) = dir.contract_and_stream();
    let cmd = |no_color: bool| {
        let mut c = rtlog_cmd();
        c.args(["decode", "--contract"]).arg(&contract).arg(&stream);
        if no_color {
            c.env("NO_COLOR", "1");
        } else {
            c.env_remove("NO_COLOR");
        }
        c
    };
    let painted = run_on_terminal(cmd(false));
    assert!(painted.contains("cli temp=81"), "{painted:?}");
    assert!(painted.contains("\x1b["), "{painted:?}");
    let plain = run_on_terminal(cmd(true));
    assert!(plain.contains("cli temp=81"), "{plain:?}");
    assert!(!plain.contains('\x1b'), "{plain:?}");
}

#[test]
fn no_spans_hides_span_lines_but_keeps_records() {
    let dir = Scratch::new("no-spans");
    let (contract, stream) = dir.contract_and_stream();
    let with = succeeded(&decode(&contract, &stream, &[]));
    assert!(with.contains("SPAN  ▶ cli.step"), "{with}");
    let without = succeeded(&decode(&contract, &stream, &["--no-spans"]));
    assert!(!without.contains("SPAN"), "{without}");
    assert!(without.contains("cli speed=1200 ok=true"), "{without}");
    assert!(without.contains("cli temp=81"), "{without}");
}

#[test]
fn a_contract_mismatch_fails_unless_forced() {
    let dir = Scratch::new("force");
    let (contract, _) = dir.contract_and_stream();
    let mut bytes = capture().to_vec();
    bytes[6] ^= 0xff; // the header's contract hash
    let stream = dir.file("other.rtlog", &bytes);

    let err = failed(&decode(&contract, &stream, &[]));
    assert!(err.contains("contract mismatch"), "{err}");
    assert!(err.contains("use --force"), "{err}");

    let forced = succeeded(&decode(&contract, &stream, &["--force"]));
    // Forced records are marked: they may not decode as written.
    assert!(forced.contains("?cli temp=81"), "{forced}");
}

#[test]
fn output_file_gets_the_text_and_stdout_stays_empty() {
    let dir = Scratch::new("out-file");
    let (contract, stream) = dir.contract_and_stream();
    let target = dir.0.join("app.txt");
    let target_s = target.to_str().unwrap();
    let out = decode(&contract, &stream, &["-o", target_s]);
    assert_eq!(succeeded(&out), "");
    let written = std::fs::read_to_string(&target).unwrap();
    assert!(written.contains("cli speed=1200 ok=true"), "{written}");
    assert!(
        text(&out.stderr)
            .trim_end()
            .ends_with(&format!(" -> {target_s}")),
        "{out:?}"
    );

    // A file that cannot be written is an error naming it.
    let bad = dir.0.join("no-such-dir").join("app.txt");
    let err = failed(&decode(
        &contract,
        &stream,
        &["--output", bad.to_str().unwrap()],
    ));
    assert!(err.starts_with(bad.to_str().unwrap()), "{err}");
}

#[test]
fn decode_without_contract_or_stream_prints_usage() {
    let dir = Scratch::new("missing-args");
    let (contract, stream) = dir.contract_and_stream();
    let out = rtlog_cmd().arg("decode").arg(&stream).output().unwrap();
    assert!(failed(&out).starts_with("usage:"));
    let out = rtlog_cmd()
        .args(["decode", "--contract"])
        .arg(&contract)
        .output()
        .unwrap();
    assert!(failed(&out).starts_with("usage:"));
    let out = rtlog_cmd().args(["decode", "--contract"]).output().unwrap();
    assert_eq!(failed(&out), "--contract needs a value\n");
    for flag in ["--out", "-o", "--color"] {
        let out = rtlog_cmd().args(["decode", flag]).output().unwrap();
        // `--output` reports itself as `-o`, its short form.
        assert_eq!(failed(&out), format!("{flag} needs a value\n"));
    }
}

#[test]
fn decode_rejects_unknown_options() {
    let out = rtlog_cmd().args(["decode", "--fast"]).output().unwrap();
    assert!(
        failed(&out).starts_with("unknown option --fast\nusage:"),
        "{out:?}"
    );
}

#[test]
fn unreadable_or_invalid_contract_files_are_errors_naming_the_file() {
    let dir = Scratch::new("bad-contract");
    let (_, stream) = dir.contract_and_stream();
    let missing = dir.0.join("missing.jsonl");
    let err = failed(&decode(&missing, &stream, &[]));
    assert!(err.starts_with(missing.to_str().unwrap()), "{err}");

    let garbage = dir.file("garbage.jsonl", b"this is not json\n");
    let err = failed(&decode(&garbage, &stream, &[]));
    assert!(
        err.starts_with(&format!("{}: ", garbage.display())),
        "{err}"
    );
    let out = rtlog_cmd().arg("check").arg(&garbage).output().unwrap();
    assert!(failed(&out).starts_with(&format!("{}: ", garbage.display())));
}

#[test]
fn check_prints_site_count_and_hash() {
    let dir = Scratch::new("check");
    let (contract, _) = dir.contract_and_stream();
    let c = common::contract();
    let out = rtlog_cmd().arg("check").arg(&contract).output().unwrap();
    assert_eq!(
        succeeded(&out),
        format!(
            "{}: {} sites, contract hash {:#010x}\n",
            contract.display(),
            c.len(),
            c.hash()
        )
    );
    let out = rtlog_cmd().arg("check").output().unwrap();
    assert!(failed(&out).starts_with("usage:"));
}

#[test]
fn a_corrupt_stretch_is_reported_as_a_gap_and_decoding_resumes() {
    let dir = Scratch::new("gap");
    let (contract, _) = dir.contract_and_stream();
    let bytes = capture();
    let header = &bytes[..rtlog::wire::HEADER_LEN];
    // header, 6 bad bytes, a resync frame, then the real frames.
    let mut damaged = header.to_vec();
    damaged.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0x01, 0x02]);
    damaged.extend_from_slice(&rtlog::wire::CTRL_RESYNC.to_le_bytes());
    damaged.extend_from_slice(header);
    damaged.extend_from_slice(&bytes[rtlog::wire::HEADER_LEN..]);
    let stream = dir.file("gap.rtlog", &damaged);

    let out = decode(&contract, &stream, &[]);
    let stdout = succeeded(&out);
    assert!(stdout.contains("GAP"), "{stdout}");
    assert!(stdout.contains("cli temp=81"), "{stdout}");
    let err = text(&out.stderr);
    assert!(
        err.contains(", 1 stream gap(s) (6 bytes skipped, resumed at resync frames)"),
        "{err}"
    );
}

#[test]
fn a_truncated_stream_prints_what_decoded_and_fails() {
    let dir = Scratch::new("truncated");
    let (contract, _) = dir.contract_and_stream();
    let bytes = capture();
    let stream = dir.file("cut.rtlog", &bytes[..bytes.len() - 1]);
    let out = decode(&contract, &stream, &[]);
    let err = failed(&out);
    assert!(text(&out.stdout).contains("cli speed=1200"), "{out:?}");
    assert!(
        err.contains(&format!("{}: stopped early: ", stream.display())),
        "{err}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn contract_extracts_the_contract_from_an_elf_binary() {
    capture(); // this binary has log and span sites
    let exe = std::env::current_exe().unwrap();
    let c = common::contract();
    let summary = format!(
        "{}: {} sites, contract hash {:#010x}\n",
        exe.display(),
        c.len(),
        c.hash()
    );

    let out = rtlog_cmd()
        .arg("contract")
        .arg("--elf")
        .arg(&exe)
        .output()
        .unwrap();
    assert_eq!(succeeded(&out), rtlog::contract::to_jsonl());
    assert_eq!(text(&out.stderr), summary);

    let dir = Scratch::new("contract");
    let target = dir.0.join("app.contract.jsonl");
    let out = rtlog_cmd()
        .arg("contract")
        .arg("--elf")
        .arg(&exe)
        .arg("-o")
        .arg(&target)
        .output()
        .unwrap();
    assert_eq!(succeeded(&out), "");
    assert_eq!(text(&out.stderr), summary);
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        rtlog::contract::to_jsonl()
    );

    // A file that cannot be written is an error naming it.
    let bad = dir.0.join("no-such-dir").join("app.contract.jsonl");
    let out = rtlog_cmd()
        .arg("contract")
        .arg("--elf")
        .arg(&exe)
        .arg("--output")
        .arg(&bad)
        .output()
        .unwrap();
    assert!(failed(&out).starts_with(&format!("{}: ", bad.display())));
}

#[test]
fn contract_rejects_bad_arguments_and_non_elf_files() {
    let dir = Scratch::new("contract-bad");
    let out = rtlog_cmd()
        .args(["contract", "--pdb", "x"])
        .output()
        .unwrap();
    assert!(failed(&out).starts_with("unknown argument --pdb\nusage:"));

    let out = rtlog_cmd().arg("contract").output().unwrap();
    assert!(failed(&out).starts_with("usage:"));

    let missing = dir.0.join("missing.elf");
    let out = rtlog_cmd()
        .arg("contract")
        .arg("--elf")
        .arg(&missing)
        .output()
        .unwrap();
    assert!(failed(&out).starts_with(&format!("{}: ", missing.display())));

    let not_elf = dir.file("notes.txt", b"not an elf");
    let out = rtlog_cmd()
        .arg("contract")
        .arg("--elf")
        .arg(&not_elf)
        .output()
        .unwrap();
    assert!(failed(&out).starts_with(&format!("{}: ", not_elf.display())));
}
