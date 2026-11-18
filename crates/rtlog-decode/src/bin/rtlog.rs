//! `rtlog` command-line tool.
//!
//! ```text
//! rtlog decode --contract app.contract.jsonl [--out text|json|perfetto] [-o FILE]
//!              [--color auto|always|never] [--no-spans] [--force] app.rtlog
//! rtlog check  app.contract.jsonl
//! rtlog contract --elf firmware.elf -o firmware.contract.jsonl
//! rtlog verify --contract firmware.contract.jsonl firmware.bin
//! ```

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use rtlog_decode::{decode_with, journald, otlp, syslog, Contract, Options, Output};

const USAGE: &str = "usage:
  rtlog decode --contract <contract.jsonl> [-o <file>]
               [--out text|json|perfetto|syslog|otlp-json|journald]
               [--color auto|always|never] [--no-spans] [--force]
               [--start <time> | --epoch <time>] [--host <name>] [--app <name>]
               [--syslog-facility <name|0-23>] [--syslog-sd-id <name>]
               [--journald-socket <path>]
               <stream.rtlog>
  rtlog check <contract.jsonl>
  rtlog contract --elf <binary> [-o <contract.jsonl>]
  rtlog verify --contract <contract.jsonl> <image>...

  --json and --perfetto are short for --out json and --out perfetto.
  Perfetto output opens in https://ui.perfetto.dev (Open trace file).
  Syslog output is RFC 5424 lines; otlp-json is OpenTelemetry log records,
  one OTLP JSON object per line, for a collector. For these, --start is the
  wall-clock time of the capture's first event and --epoch that of the
  target's time zero, as Unix seconds or 2026-11-17T09:30:00Z (without
  either the records carry no time of day); --host and --app name the
  machine and the program. journald output is systemd journal entries in
  the Journal Export Format, for systemd-journal-remote; with
  --journald-socket /run/systemd/journal/socket the entries go to the
  running journal instead of stdout.
  `contract --elf` reads the binary that was built: ELF (Linux, firmware),
  Mach-O (macOS) or PE (Windows); `--binary` is the same option. A Rust
  program can also write its contract with rtlog::contract::write(path).
  `verify` fails if an image that ships (a flashed .bin, a stripped binary)
  still holds a format string of the contract; see docs/strip.md.";

/// Give journal entries to systemd-journald on its socket; returns how many.
#[cfg(unix)]
fn send_to_journal(export: &str, socket: &str) -> std::io::Result<usize> {
    journald::send(export, socket)
}

/// There is no systemd journal to send to on this system.
#[cfg(not(unix))]
fn send_to_journal(_export: &str, _socket: &str) -> std::io::Result<usize> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "--journald-socket needs a Unix system with systemd",
    ))
}

/// Write to stdout; a closed pipe (`| head`) ends output quietly instead of panicking.
fn write_stdout(s: &str) {
    let _ = std::io::stdout().lock().write_all(s.as_bytes());
}

fn load_contract(path: &str) -> Result<Contract, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    Contract::from_jsonl(&text).map_err(|e| format!("{path}: {e}"))
}

fn run(args: &[String]) -> Result<(), String> {
    // `rtlog <subcommand> --help` prints the usage, like `rtlog --help`.
    if args.iter().skip(1).any(|a| a == "-h" || a == "--help") {
        write_stdout(&format!("{USAGE}\n"));
        return Ok(());
    }
    match args.first().map(String::as_str) {
        Some("decode") => {
            let mut contract = None;
            let mut stream = None;
            let mut out_file: Option<String> = None;
            let mut color = "auto".to_owned();
            let mut opts = Options::default();
            let mut sys = syslog::Config::default();
            let mut otel = otlp::Config::default();
            let mut journal = journald::Config::default();
            let mut journal_socket: Option<String> = None;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                let mut value =
                    |flag: &str| it.next().cloned().ok_or(format!("{flag} needs a value"));
                match a.as_str() {
                    "--contract" => contract = Some(value("--contract")?),
                    "--json" => opts.output = Output::Json,
                    "--perfetto" => opts.output = Output::Perfetto,
                    "--out" => {
                        opts.output = match value("--out")?.as_str() {
                            "text" => Output::Text,
                            "json" => Output::Json,
                            "perfetto" | "chrome" => Output::Perfetto,
                            "syslog" => Output::Syslog,
                            "otlp-json" => Output::OtlpJson,
                            "journald" => Output::Journald,
                            o => return Err(format!("unknown output {o:?}\n{USAGE}")),
                        }
                    }
                    "-o" | "--output" => out_file = Some(value("-o")?),
                    "--color" => color = value("--color")?,
                    f @ ("--epoch" | "--start") => {
                        let v = value(f)?;
                        let t = Some(syslog::parse_epoch(&v).ok_or(format!(
                            "{f} {v:?}: expected Unix seconds or a time like 2026-11-17T09:30:00Z"
                        ))?);
                        if f == "--epoch" {
                            (sys.epoch_ns, otel.epoch_ns, journal.epoch_ns) = (t, t, t);
                        } else {
                            (sys.start_ns, otel.start_ns, journal.start_ns) = (t, t, t);
                        }
                    }
                    "--host" => {
                        sys.hostname = value("--host")?;
                        otel.host_name = sys.hostname.clone();
                        journal.hostname = sys.hostname.clone();
                    }
                    "--app" => {
                        sys.app_name = value("--app")?;
                        otel.service_name = sys.app_name.clone();
                        journal.identifier = sys.app_name.clone();
                    }
                    "--syslog-facility" => {
                        let v = value("--syslog-facility")?;
                        sys.facility = syslog::facility(&v).ok_or(format!(
                            "--syslog-facility {v:?}: expected 0 to 23 or a name such as user, daemon, local0"
                        ))?;
                    }
                    "--syslog-sd-id" => sys.sd_id = value("--syslog-sd-id")?,
                    "--journald-socket" => {
                        opts.output = Output::Journald;
                        journal_socket = Some(value("--journald-socket")?);
                    }
                    "--no-spans" => opts.spans = false,
                    "--force" => opts.force = true,
                    s if s.starts_with('-') => return Err(format!("unknown option {s}\n{USAGE}")),
                    s => stream = Some(s.to_owned()),
                }
            }
            opts.color = match color.as_str() {
                "always" => true,
                "never" => false,
                "auto" => {
                    out_file.is_none()
                        && std::io::stdout().is_terminal()
                        && std::env::var_os("NO_COLOR").is_none()
                }
                c => return Err(format!("unknown --color {c:?}")),
            };
            let contract = load_contract(&contract.ok_or(USAGE)?)?;
            let path = stream.ok_or(USAGE)?;
            let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
            let d = match opts.output {
                Output::Syslog => syslog::decode(&contract, &bytes, &opts, &sys),
                Output::OtlpJson => otlp::decode(&contract, &bytes, &opts, &otel),
                Output::Journald => journald::decode(&contract, &bytes, &opts, &journal),
                _ => decode_with(&contract, &bytes, &opts),
            }
            .map_err(|e| format!("{path}: {e}"))?;
            let mut sent = None;
            match (&journal_socket, &out_file) {
                (Some(socket), _) => {
                    let n =
                        send_to_journal(&d.text, socket).map_err(|e| format!("{socket}: {e}"))?;
                    sent = Some(format!(" -> {n} entries to {socket}"));
                }
                (None, Some(f)) => std::fs::write(f, &d.text).map_err(|e| format!("{f}: {e}"))?,
                (None, None) => {
                    let mut o = std::io::stdout().lock();
                    // A closed pipe (`| head`) is not an error worth reporting.
                    let _ = o.write_all(d.text.as_bytes());
                }
            }
            eprintln!(
                "{} records, {} spans, {} contexts, {} dropped{}{}",
                d.records,
                d.spans,
                d.contexts,
                d.dropped,
                if d.gaps > 0 {
                    format!(
                        ", {} stream gap(s) ({} bytes skipped, resumed at resync frames)",
                        d.gaps, d.skipped_bytes
                    )
                } else {
                    String::new()
                },
                sent.or(out_file.map(|f| format!(" -> {f}")))
                    .unwrap_or_default()
            );
            match d.error {
                Some(e) => Err(format!("{path}: stopped early: {e}")),
                None => Ok(()),
            }
        }
        Some("check") => {
            let path = args.get(1).ok_or(USAGE)?;
            let c = load_contract(path)?;
            write_stdout(&format!(
                "{path}: {} sites, contract hash {:#010x}\n",
                c.len(),
                c.hash()
            ));
            Ok(())
        }
        Some("contract") => {
            let mut elf = None;
            let mut out_file = None;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--elf" | "--binary" => elf = it.next().cloned(),
                    "-o" | "--output" => out_file = it.next().cloned(),
                    s => return Err(format!("unknown argument {s}\n{USAGE}")),
                }
            }
            let path = elf.ok_or(USAGE)?;
            let data = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
            let jsonl = rtlog_decode::elf::contract_from_binary(&data)
                .map_err(|e| format!("{path}: {e}"))?;
            let c = Contract::from_jsonl(&jsonl)
                .map_err(|e| format!("{path}: extracted contract is invalid: {e}"))?;
            match out_file {
                Some(o) => std::fs::write(&o, &jsonl).map_err(|e| format!("{o}: {e}"))?,
                None => write_stdout(&jsonl),
            }
            eprintln!(
                "{path}: {} sites, contract hash {:#010x}",
                c.len(),
                c.hash()
            );
            Ok(())
        }
        Some("verify") => {
            let mut contract = None;
            let mut images = Vec::new();
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--contract" => contract = it.next().cloned(),
                    s if s.starts_with('-') => return Err(format!("unknown option {s}\n{USAGE}")),
                    s => images.push(s.to_owned()),
                }
            }
            let contract = load_contract(&contract.ok_or(USAGE)?)?;
            if images.is_empty() {
                return Err(USAGE.to_owned());
            }
            let mut leaks = 0;
            for path in &images {
                let image = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
                let v = contract.verify_stripped(&image);
                for site in &v.found {
                    eprintln!(
                        "{path}: contains the format string of site {} ({}:{}): {:?}",
                        site.id, site.file, site.line, site.fmt
                    );
                }
                leaks += v.found.len();
                write_stdout(&format!(
                    "{path}: {} of {} format strings found in the image ({} shorter than {} bytes not searched for)\n",
                    v.found.len(),
                    v.found.len() + v.absent,
                    v.skipped,
                    rtlog_decode::VERIFY_MIN_LEN
                ));
            }
            match leaks {
                0 => Ok(()),
                n => Err(format!(
                    "{n} format string(s) ship in the image; it is not stripped"
                )),
            }
        }
        Some("-h" | "--help" | "help") => {
            write_stdout(&format!("{USAGE}\n"));
            Ok(())
        }
        _ => Err(USAGE.to_owned()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
