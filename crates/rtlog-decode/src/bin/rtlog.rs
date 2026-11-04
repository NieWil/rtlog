//! `rtlog` command-line tool.
//!
//! ```text
//! rtlog decode --contract app.contract.jsonl [--json] [--force] app.rtlog
//! rtlog check  app.contract.jsonl
//! ```

use std::process::ExitCode;

use rtlog_decode::{decode, Contract, Output};

const USAGE: &str = "usage:
  rtlog decode --contract <contract.jsonl> [--json] [--force] <stream.rtlog>
  rtlog check <contract.jsonl>";

fn load_contract(path: &str) -> Result<Contract, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    Contract::from_jsonl(&text).map_err(|e| format!("{path}: {e}"))
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("decode") => {
            let mut contract = None;
            let mut stream = None;
            let mut out = Output::Text;
            let mut force = false;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--contract" => {
                        contract = Some(it.next().ok_or("--contract needs a path")?.clone())
                    }
                    "--json" => out = Output::Json,
                    "--force" => force = true,
                    s if s.starts_with('-') => return Err(format!("unknown option {s}\n{USAGE}")),
                    s => stream = Some(s.to_owned()),
                }
            }
            let contract = load_contract(&contract.ok_or(USAGE)?)?;
            let path = stream.ok_or(USAGE)?;
            let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
            let d = decode(&contract, &bytes, out, force).map_err(|e| format!("{path}: {e}"))?;
            print!("{}", d.text);
            eprintln!(
                "{} records, {} contexts, {} dropped",
                d.records, d.contexts, d.dropped
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
