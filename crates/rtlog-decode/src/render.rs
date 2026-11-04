//! Host-side formatting: the format string from the contract applied to typed args.
//!
//! Supported specs: `{}` `{name}` `{:?}` `{:x}` `{:X}` `{:hex}` `{:b}` `{:bin}`
//! `{:.N}` (float precision) and `{:str}`; anything else renders as `{}`.

use std::fmt::Write;

use crate::value::Value;

fn hex(out: &mut String, v: &Value, upper: bool) {
    match v {
        Value::U(n) if upper => write!(out, "{n:X}").unwrap(),
        Value::U(n) => write!(out, "{n:x}").unwrap(),
        Value::I(n) if upper => write!(out, "{n:X}").unwrap(),
        Value::I(n) => write!(out, "{n:x}").unwrap(),
        Value::Bytes { bytes, truncated } => {
            for b in bytes {
                if upper {
                    write!(out, "{b:02X}").unwrap();
                } else {
                    write!(out, "{b:02x}").unwrap();
                }
            }
            if *truncated {
                out.push('…');
            }
        }
        other => plain(out, other, None),
    }
}

fn bin(out: &mut String, v: &Value) {
    match v {
        Value::U(n) => write!(out, "{n:b}").unwrap(),
        Value::I(n) => write!(out, "{n:b}").unwrap(),
        other => plain(out, other, None),
    }
}

fn plain(out: &mut String, v: &Value, precision: Option<usize>) {
    match v {
        Value::U(n) => write!(out, "{n}").unwrap(),
        Value::I(n) => write!(out, "{n}").unwrap(),
        Value::F(f) => match precision {
            Some(p) => write!(out, "{f:.p$}").unwrap(),
            None => write!(out, "{f}").unwrap(),
        },
        Value::Bool(b) => write!(out, "{b}").unwrap(),
        Value::Char(c) => out.push(*c),
        Value::Str { s, truncated } => {
            out.push_str(s);
            if *truncated {
                out.push('…');
            }
        }
        Value::Bytes { .. } => {
            out.push('[');
            hex(out, v, false);
            out.push(']');
        }
        Value::Option(None) => out.push_str("None"),
        Value::Option(Some(inner)) => {
            out.push_str("Some(");
            plain(out, inner, precision);
            out.push(')');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                plain(out, item, precision);
            }
            out.push(']');
        }
    }
}

fn apply(out: &mut String, spec: &str, v: &Value) {
    let spec = spec.split_once(':').map(|(_, s)| s).unwrap_or("");
    match spec {
        "x" | "hex" | "#x" => hex(out, v, false),
        "X" | "#X" => hex(out, v, true),
        "b" | "bin" | "#b" => bin(out, v),
        s if s.starts_with('.') => plain(out, v, s[1..].parse().ok()),
        _ => plain(out, v, None),
    }
}

/// Render `fmt` with `args`. Missing args render as `{?}`, extra args are ignored.
pub fn format(fmt: &str, args: &[Value]) -> String {
    let mut out = String::with_capacity(fmt.len() + 16 * args.len());
    let mut args = args.iter();
    let mut rest = fmt;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(r) = tail.strip_prefix("{{") {
            out.push('{');
            rest = r;
        } else if let Some(r) = tail.strip_prefix("}}") {
            out.push('}');
            rest = r;
        } else if tail.starts_with('{') {
            let end = tail.find('}').unwrap_or(tail.len());
            let spec = &tail[1..end];
            match args.next() {
                Some(v) => apply(&mut out, spec, v),
                None => out.push_str("{?}"),
            }
            rest = &tail[(end + 1).min(tail.len())..];
        } else {
            out.push('}');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        Value::Str {
            s: v.into(),
            truncated: false,
        }
    }

    #[test]
    fn basic() {
        assert_eq!(
            format("pid out={} err={}", &[Value::F(0.42), Value::I(-3)]),
            "pid out=0.42 err=-3"
        );
        assert_eq!(
            format("{name} is {:?}", &[s("motor"), Value::Bool(true)]),
            "motor is true"
        );
        assert_eq!(format("no args", &[]), "no args");
    }

    #[test]
    fn specs() {
        assert_eq!(
            format(
                "{:x} {:X} {:#x}",
                &[Value::U(255), Value::U(255), Value::I(16)]
            ),
            "ff FF 10"
        );
        assert_eq!(format("{:b}", &[Value::U(5)]), "101");
        assert_eq!(format("{:.2}", &[Value::F(1.23456)]), "1.23");
        assert_eq!(
            format(
                "{:hex}",
                &[Value::Bytes {
                    bytes: vec![0xde, 0xad],
                    truncated: true
                }]
            ),
            "dead…"
        );
        assert_eq!(
            format(
                "{}",
                &[Value::Bytes {
                    bytes: vec![1],
                    truncated: false
                }]
            ),
            "[01]"
        );
        assert_eq!(
            format(
                "{:str}",
                &[Value::Str {
                    s: "ab".into(),
                    truncated: true
                }]
            ),
            "ab…"
        );
    }

    #[test]
    fn escapes_and_mismatches() {
        assert_eq!(format("{{}} {}", &[Value::U(1)]), "{} 1");
        assert_eq!(format("{} {}", &[Value::U(1)]), "1 {?}");
        assert_eq!(format("a } b", &[]), "a } b");
        assert_eq!(format("unterminated {", &[Value::U(1)]), "unterminated 1");
    }

    #[test]
    fn composites() {
        let v = Value::Array(vec![
            Value::Option(Some(Box::new(Value::U(1)))),
            Value::Option(None),
        ]);
        assert_eq!(format("{}", &[v]), "[Some(1), None]");
        assert_eq!(format("{}{}", &[Value::Char('é'), Value::F(-0.5)]), "é-0.5");
    }
}
