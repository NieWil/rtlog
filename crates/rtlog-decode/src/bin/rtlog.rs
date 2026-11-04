//! `rtlog` command-line tool.
//!
//! ```text
//! rtlog decode --contract app.contract.jsonl [--out text|json|perfetto] [-o FILE]
//!              [--color auto|always|never] [--no-spans] [--force] app.rtlog
//! rtlog check  app.contract.jsonl
//! ```

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use rtlog_decode::{decode_with, Contract, Options, Output};

const USAGE: &str = "usage:
  rtlog decode --contract <contract.jsonl> [--out text|json|perfetto] [-o <file>]
               [--color auto|always|never] [--no-spans] [--force] <stream.rtlog>
  rtlog check <contract.jsonl>

  --json and --perfetto are short for --out json and --out perfetto.
  Perfetto output opens in https://ui.perfetto.dev (Open trace file).";

fn load_contract(path: &str) -> Result<Contract, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    Contract::from_jsonl(&text).map_err(|e| format!("{path}: {e}"))
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("decode") => {
            let mut contract = None;
            let mut stream = None;
            let mut out_file: Option<String> = None;
            let mut color = "auto".to_owned();
            let mut opts = Options::default();
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
                            o => return Err(format!("unknown output {o:?}\n{USAGE}")),
                        }
                    }
                    "-o" | "--output" => out_file = Some(value("-o")?),
                    "--color" => color = value("--color")?,
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
            let d = decode_with(&contract, &bytes, &opts).map_err(|e| format!("{path}: {e}"))?;
            match &out_file {
                Some(f) => std::fs::write(f, &d.text).map_err(|e| format!("{f}: {e}"))?,
                None => {
                    let mut o = std::io::stdout().lock();
                    // A closed pipe (`| head`) is not an error worth reporting.
                    let _ = o.write_all(d.text.as_bytes());
                }
            }
            eprintln!(
                "{} records, {} spans, {} contexts, {} dropped{}",
                d.records,
                d.spans,
                d.contexts,
                d.dropped,
                out_file.map(|f| format!(" -> {f}")).unwrap_or_default()
            );
            match d.error {
                Some(e) => Err(format!("{path}: stopped early: {e}")),
                None => Ok(()),
            }
        }
        Some("check") => {
            let path = args.get(1).ok_or(USAGE)?;
            let c = load_contract(path)?;
            println!(
                "{path}: {} sites, contract hash {:#010x}",
                c.len(),
                c.hash()
            );
            Ok(())
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
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
