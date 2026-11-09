//! Live text mode: log calls stay binary and deferred, and the drain thread
//! decodes each frame and prints it, so `cargo run` reads like `env_logger`
//! (spec section 8b, "debug stdout").
//!
//! ```no_run
//! let _log = rtlog_decode::stdout::init(); // RTLOG=debug to change the level
//! rtlog::info!("motor speed={} rpm", 1200u16);
//! // dropping the guard drains and prints what is left
//! ```
//!
//! The call site cost is unchanged: formatting happens on the drain thread.
//! The contract comes from the running binary, so nothing else is needed.

use std::io::{IsTerminal, Write};
use std::time::Duration;

use crate::stream::{Event, Session};
use crate::{Contract, Item, TextRenderer};

/// Where the text goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// A transport that decodes frames as they arrive and writes text lines.
pub struct TextTransport<W: Write> {
    contract: &'static Contract,
    session: Option<Session<'static>>,
    renderer: TextRenderer,
    out: W,
}

impl<W: Write> TextTransport<W> {
    /// Decode with `contract`, write to `out`.
    pub fn new(contract: &'static Contract, out: W, color: bool) -> Self {
        TextTransport {
            contract,
            session: None,
            renderer: TextRenderer::new(color, true, false, None),
            out,
        }
    }

    /// The writer.
    pub fn get_ref(&self) -> &W {
        &self.out
    }
}

impl<W: Write> rtlog::Transport for TextTransport<W> {
    fn write(&mut self, frame: &[u8]) -> Result<(), rtlog::Full> {
        let Some(session) = self.session.as_mut() else {
            // The first write is the stream header.
            match Session::new(self.contract, frame, true) {
                Ok(s) => self.session = Some(s),
                Err(e) => {
                    let _ = writeln!(self.out, "rtlog: {e}");
                }
            }
            return Ok(());
        };
        let item = match session.decode_frame(frame) {
            Ok(Event::Record(r)) => Item::Record(r),
            Ok(Event::SpanOpen {
                ts_ns,
                ctx,
                id,
                parent,
                site,
            }) => Item::SpanOpen {
                ts_ns,
                ctx,
                id,
                parent,
                name: &site.fmt,
            },
            Ok(Event::SpanClose { ts_ns, ctx, id }) => Item::SpanClose { ts_ns, ctx, id },
            Ok(Event::Dropped { ctx, count }) => Item::Dropped {
                ts_ns: 0,
                ctx,
                count,
            },
            Ok(Event::CtxRegister { ctx, name }) => {
                self.renderer.names.insert(ctx, name);
                return Ok(());
            }
            Ok(Event::Crash { ts_ns, ctx, crash }) => Item::Crash { ts_ns, ctx, crash },
            Ok(Event::CtxClose { .. } | Event::CtxTime { .. } | Event::Resync) => return Ok(()),
            Err(e) => {
                let _ = writeln!(self.out, "rtlog: {e}");
                return Ok(());
            }
        };
        if let Some(line) = self.renderer.render(&item) {
            // A closed pipe must not stop the program being observed.
            let _ = writeln!(self.out, "{line}");
        }
        Ok(())
    }

    fn flush(&mut self) {
        let _ = self.out.flush();
    }
}

/// Settings for [`init_with`].
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Where the text goes; defaults to [`Target::Stdout`].
    pub target: Target,
    /// Lowest level shown; `None` reads `RTLOG` (trace, debug, info, warn,
    /// error, off), defaulting to info.
    pub level: Option<rtlog::Level>,
    /// ANSI colours; `None` colours when the target is a terminal and `NO_COLOR` is unset.
    pub color: Option<bool>,
    /// How often the drain thread prints.
    pub period: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            target: Target::Stdout,
            level: None,
            color: None,
            period: Duration::from_millis(5),
        }
    }
}

enum Out {
    Stdout(std::io::Stdout),
    Stderr(std::io::Stderr),
}

impl Write for Out {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Out::Stdout(o) => o.write(b),
            Out::Stderr(e) => e.write(b),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Out::Stdout(o) => o.flush(),
            Out::Stderr(e) => e.flush(),
        }
    }
}

/// Keeps the printing drain alive; dropping it prints whatever is left.
#[must_use = "logging stops printing when this guard is dropped"]
pub struct Guard {
    handle: Option<rtlog::DrainHandle<TextTransport<Out>>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let mut t = h.stop();
            rtlog::Transport::flush(&mut t);
        }
    }
}

/// This binary's own contract, built once (a static, not a leak).
pub fn own_contract() -> &'static Contract {
    static CONTRACT: std::sync::OnceLock<Contract> = std::sync::OnceLock::new();
    CONTRACT.get_or_init(|| {
        Contract::from_jsonl(&rtlog::contract::to_jsonl())
            .expect("the binary's own contract is valid")
    })
}

/// Print every log line to stdout as text, at the level in `RTLOG`.
pub fn init() -> Guard {
    init_with(Config::default())
}

/// [`init`] with explicit settings.
pub fn init_with(cfg: Config) -> Guard {
    let level = cfg
        .level
        .or_else(|| match std::env::var("RTLOG").ok().as_deref() {
            Some("off") => None,
            Some(s) => Some(rtlog::Level::parse(s).unwrap_or(rtlog::Level::Info)),
            None => Some(rtlog::Level::Info),
        });
    match level {
        Some(l) => rtlog::set_level(l),
        None => rtlog::set_off(),
    }
    let (out, tty) = match cfg.target {
        Target::Stdout => (
            Out::Stdout(std::io::stdout()),
            std::io::stdout().is_terminal(),
        ),
        Target::Stderr => (
            Out::Stderr(std::io::stderr()),
            std::io::stderr().is_terminal(),
        ),
    };
    let color = cfg
        .color
        .unwrap_or(tty && std::env::var_os("NO_COLOR").is_none());
    let contract = own_contract();
    let transport = TextTransport::new(contract, out, color);
    Guard {
        handle: Some(rtlog::spawn_drain(transport, cfg.period)),
    }
}
