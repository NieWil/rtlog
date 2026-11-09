//! rtlog: deferred, lock-free, allocation-free logging.
//!
//! A log call writes a site ID, a timestamp delta, a context ID and raw
//! argument bytes into the calling context's own ring. Format strings never
//! reach the wire: they live in the contract, which the host decoder
//! ([`rtlog-decode`](https://docs.rs/rtlog-decode)) joins with the stream to
//! render text, JSON lines or a Perfetto trace.
//!
//! Nothing leaves the rings until a **drain** moves frames to a transport: with
//! `std`, `spawn_drain` runs one on a background thread, or call
//! `Drain::poll` yourself; on `no_std`, call [`drain::drain_ring`] from the
//! idle loop. The contract comes from `contract::write` (any OS) or from the
//! binary (`rtlog contract --elf`, ELF only).
//!
//! - Default feature `std`: per-thread rings created on first use, the drain
//!   thread, `contract`, and on Unix the `crash` handler.
//! - `default-features = false`: `no_std`, no allocation; static [`Ring`]s and
//!   explicit producers (`info!(to: &mut producer, ...)`).
//!
//! Guides: [getting started](https://github.com/NieWil/rtlog#try-it),
//! [configuration and CLI](https://github.com/NieWil/rtlog/blob/main/docs/reference.md),
//! [embedded](https://github.com/NieWil/rtlog/blob/main/docs/embedded.md),
//! [crash dumps](https://github.com/NieWil/rtlog/blob/main/docs/crash.md).
//!
//! ```
//! rtlog::info!("pid out={} err={}", 0.42f32, -3i16);
//! let mut drain = rtlog::Drain::new(Vec::new());
//! drain.poll().unwrap();
//! assert!(drain.transport().len() > rtlog::wire::HEADER_LEN);
//! ```
//!
//! On `no_std`, give each task a static ring and pass its producer explicitly:
//!
//! ```
//! static RING: rtlog::Ring<1024> = rtlog::Ring::new();
//! let (mut p, mut c) = RING.split(1).unwrap();
//! rtlog::warn!(to: &mut p, "temp={}", 81u8);
//! let mut out = [0u8; rtlog::FRAME_MAX];
//! assert!(c.pop(&mut out).is_some());
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

#[cfg(feature = "std")]
extern crate std;

pub mod arg;
pub mod clock;
pub mod drain;
pub mod hash;
pub mod level;
pub mod ring;
pub mod site;
pub mod varint;
pub mod wire;

#[cfg(feature = "std")]
pub mod contract;
#[cfg(all(feature = "std", unix))]
pub mod crash;
#[cfg(feature = "std")]
mod runtime;

pub use arg::{Log, Tag, Writer};
pub use clock::set_clock;
pub use drain::{Full, SliceTransport, Transport};
pub use level::{enabled, set_level, set_off, set_spans, spans_enabled, Level};
pub use ring::{Consumer, Producer, Ring, SpanId, ARGS_CAP, FRAME_MAX};
pub use site::{Site, SiteKind};

#[cfg(feature = "std")]
pub use drain::IoTransport;
#[cfg(feature = "std")]
pub use runtime::{
    __context_names, set_context_name, spawn_drain, stats, Drain, DrainHandle, SpanGuard, Stats,
    KEEP_WITHOUT_DRAIN, THREAD_RING,
};

#[doc(hidden)]
pub mod __private {
    use core::mem::MaybeUninit;

    pub use linkme;

    #[cfg(feature = "std")]
    pub use crate::runtime::__span_enter as span_enter;

    /// Uninitialised argument buffer for one log call.
    #[inline(always)]
    pub fn args_buf() -> [MaybeUninit<u8>; crate::ARGS_CAP] {
        // SAFETY: an array of `MaybeUninit` needs no initialisation.
        unsafe { MaybeUninit::<[MaybeUninit<u8>; crate::ARGS_CAP]>::uninit().assume_init() }
    }

    /// Emit a frame through a producer; `encode` writes the arguments.
    #[inline(always)]
    pub fn emit_to<const N: usize, F: FnOnce(&mut crate::Writer<'_>)>(
        p: &mut crate::Producer<'_, N>,
        site: u32,
        encode: F,
    ) {
        p.emit(site, encode);
    }

    /// Emit a frame into the calling thread's ring; `encode` writes the arguments.
    #[cfg(feature = "std")]
    #[inline(always)]
    pub fn emit_local<F: FnOnce(&mut crate::Writer<'_>)>(site: u32, encode: F) {
        crate::runtime::__local_emit(site, encode)
    }

    pub const fn nargs(n: usize) -> u8 {
        assert!(n <= 32, "rtlog: at most 32 arguments per call");
        n as u8
    }
}

#[doc(hidden)]
#[macro_export]
macro_rules! __unit {
    ($e:expr) => {
        ()
    };
}

/// Log at an explicit level.
///
/// `rtlog::log!(Level::Info, "x={}", x)` writes to the calling thread's ring
/// (`std`); `rtlog::log!(to: &mut producer, Level::Info, "x={}", x)` writes
/// through an explicit [`Producer`] (all targets).
///
/// The number of `{}` placeholders must match the arguments, at compile time:
///
/// ```compile_fail
/// rtlog::info!("x={} y={}", 1u8);
/// ```
///
/// Inline captures (`{x}`) are not supported, because the format string must
/// be a plain literal that goes into the contract; write `"x={}", x`:
///
/// ```compile_fail
/// let x = 1u8;
/// rtlog::info!("x={x}");
/// ```
///
/// Arguments must implement [`Log`]:
///
/// ```compile_fail
/// struct NotLog;
/// rtlog::info!("v={}", NotLog);
/// ```
#[macro_export]
macro_rules! log {
    (@site $lvl:expr, $fmt:literal, $n:expr) => {{
        const __N: usize = $n;
        const _: () = assert!(
            !$crate::site::has_inline_capture($fmt),
            "rtlog: inline captures like {{x}} are not supported; write \"x={{}}\", x"
        );
        const _: () = assert!(
            $crate::site::count_placeholders($fmt) == __N,
            "rtlog: placeholder count does not match the number of arguments"
        );
        // The ID is a compile-time constant: the call never reads its
        // descriptor, which on bare metal may live in a non-loaded section.
        const __SITE: $crate::Site = $crate::Site::new(
            $lvl,
            ::core::module_path!(),
            ::core::file!(),
            ::core::line!(),
            ::core::column!(),
            $fmt,
            $crate::__private::nargs(__N),
        );
        #[$crate::__private::linkme::distributed_slice($crate::site::SITES)]
        #[linkme(crate = $crate::__private::linkme)]
        static __RTLOG_SITE: $crate::Site = __SITE;
        __SITE.id
    }};
    (to: $p:expr, $lvl:expr, $fmt:literal $(, $arg:expr)* $(,)?) => {{
        let __id: u32 =
            $crate::log!(@site $lvl, $fmt, <[()]>::len(&[$($crate::__unit!($arg)),*]));
        if $crate::enabled($lvl) {
            $crate::__private::emit_to($p, __id, |__w: &mut $crate::Writer<'_>| {
                $( $crate::Log::write(&$arg, __w); )*
            });
        }
    }};
    ($lvl:expr, $fmt:literal $(, $arg:expr)* $(,)?) => {{
        let __id: u32 =
            $crate::log!(@site $lvl, $fmt, <[()]>::len(&[$($crate::__unit!($arg)),*]));
        if $crate::enabled($lvl) {
            $crate::__private::emit_local(__id, |__w: &mut $crate::Writer<'_>| {
                $( $crate::Log::write(&$arg, __w); )*
            });
        }
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __span_site {
    ($name:literal) => {{
        const _: () = assert!(
            $crate::site::count_placeholders($name) == 0,
            "rtlog: a span name takes no placeholders"
        );
        const __SITE: $crate::Site = $crate::Site::span(
            ::core::module_path!(),
            ::core::file!(),
            ::core::line!(),
            ::core::column!(),
            $name,
        );
        #[$crate::__private::linkme::distributed_slice($crate::site::SITES)]
        #[linkme(crate = $crate::__private::linkme)]
        static __RTLOG_SPAN: $crate::Site = __SITE;
        __SITE.id
    }};
}

/// Open a span: a named, timed region that the host shows as a slice (for
/// example in Perfetto), nested under the span that was open when it started.
///
/// On `std`, `let _g = rtlog::span!("parse");` returns a guard that closes the
/// span when dropped. With an explicit producer,
/// `let s = rtlog::span!(to: &mut p, "parse");` returns a [`SpanId`] that you
/// close with `p.span_exit(s)`.
///
/// ```
/// fn handle() {
///     let _req = rtlog::span!("request");
///     {
///         let _db = rtlog::span!("db.query");
///         rtlog::debug!("rows={}", 3u32);
///     }
/// }
/// handle();
/// ```
#[macro_export]
macro_rules! span {
    (to: $p:expr, $name:literal) => {{
        let __id: u32 = $crate::__span_site!($name);
        $crate::Producer::span_enter($p, __id)
    }};
    ($name:literal) => {{
        let __id: u32 = $crate::__span_site!($name);
        $crate::__private::span_enter(__id)
    }};
}

/// Log at `Trace` level; see [`log!`].
#[macro_export]
macro_rules! trace {
    (to: $p:expr, $($t:tt)+) => { $crate::log!(to: $p, $crate::Level::Trace, $($t)+) };
    ($($t:tt)+) => { $crate::log!($crate::Level::Trace, $($t)+) };
}

/// Log at `Debug` level; see [`log!`].
#[macro_export]
macro_rules! debug {
    (to: $p:expr, $($t:tt)+) => { $crate::log!(to: $p, $crate::Level::Debug, $($t)+) };
    ($($t:tt)+) => { $crate::log!($crate::Level::Debug, $($t)+) };
}

/// Log at `Info` level; see [`log!`].
#[macro_export]
macro_rules! info {
    (to: $p:expr, $($t:tt)+) => { $crate::log!(to: $p, $crate::Level::Info, $($t)+) };
    ($($t:tt)+) => { $crate::log!($crate::Level::Info, $($t)+) };
}

/// Log at `Warn` level; see [`log!`].
#[macro_export]
macro_rules! warn {
    (to: $p:expr, $($t:tt)+) => { $crate::log!(to: $p, $crate::Level::Warn, $($t)+) };
    ($($t:tt)+) => { $crate::log!($crate::Level::Warn, $($t)+) };
}

/// Log at `Error` level; see [`log!`].
#[macro_export]
macro_rules! error {
    (to: $p:expr, $($t:tt)+) => { $crate::log!(to: $p, $crate::Level::Error, $($t)+) };
    ($($t:tt)+) => { $crate::log!($crate::Level::Error, $($t)+) };
}

#[cfg(test)]
mod tests {
    use crate::{Ring, FRAME_MAX};

    #[test]
    fn argument_overflow_is_a_counted_drop() {
        static RING: Ring<1024> = Ring::new();
        let (mut p, mut c) = RING.split(1).unwrap();
        let s = "y".repeat(127);
        // Three capped strings need 3 × 129 B > ARGS_CAP.
        crate::info!(to: &mut p, "{} {} {}", s.as_str(), s.as_str(), s.as_str());
        assert_eq!(RING.drops(), 1);
        let mut out = [0u8; FRAME_MAX];
        assert!(c.pop(&mut out).is_none());

        crate::info!(to: &mut p, "{}", s.as_str());
        assert!(c.pop(&mut out).is_some());
    }

    #[test]
    fn every_level_macro_records_through_a_producer() {
        static RING: Ring<1024> = Ring::new();
        let (mut p, mut c) = RING.split(2).unwrap();
        crate::trace!(to: &mut p, "t");
        crate::debug!(to: &mut p, "d {}", 1u8);
        crate::info!(to: &mut p, "i");
        crate::warn!(to: &mut p, "w");
        crate::error!(to: &mut p, "e");
        crate::log!(to: &mut p, crate::Level::Info, "l {}", 2u8,);
        let mut out = [0u8; FRAME_MAX];
        let mut n = 0;
        while c.pop(&mut out).is_some() {
            n += 1;
        }
        assert_eq!(n, 6);
    }

    #[test]
    fn thread_local_overflow_counts_without_panicking() {
        std::thread::spawn(|| {
            let s = "z".repeat(127);
            crate::warn!("{} {} {}", s.as_str(), s.as_str(), s.as_str());
            crate::warn!("ok");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn producer_spans_nest_and_restore_parent() {
        static RING: Ring<1024> = Ring::new();
        let (mut p, mut c) = RING.split(3).unwrap();
        let outer = crate::span!(to: &mut p, "outer");
        let inner = crate::span!(to: &mut p, "inner");
        assert_eq!(inner.parent, outer.id);
        assert_eq!(outer.id >> 32, 3);
        p.span_exit(inner);
        p.span_exit(outer);
        let again = crate::span!(to: &mut p, "outer");
        assert_eq!(again.parent, 0);
        p.span_exit(again);
        let mut out = [0u8; FRAME_MAX];
        let mut n = 0;
        while c.pop(&mut out).is_some() {
            n += 1;
        }
        assert_eq!(n, 6);
        assert!(crate::site::sites()
            .iter()
            .any(|s| s.kind == crate::SiteKind::Span && s.fmt == "inner"));
    }

    #[test]
    fn sites_are_collected_with_unique_ids() {
        assert!(crate::site::sites().len() >= 9);
        assert!(crate::site::find_collision().is_none());
        assert!(crate::site::sites()
            .iter()
            .any(|s| s.fmt == "{} {} {}" && s.nargs == 3));
    }
}
