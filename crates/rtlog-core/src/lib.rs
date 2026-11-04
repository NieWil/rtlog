//! rtlog core: deferred, lock-free, allocation-free logging.
//!
//! A log call writes a site ID, a timestamp delta, a context ID and raw
//! argument bytes into the calling context's own ring. Format strings never
//! reach the wire: they live in the contract, which the host decoder joins with
//! the stream to render text.
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
#[cfg(feature = "std")]
mod runtime;

pub use arg::{Log, Tag, Writer};
pub use clock::set_clock;
pub use drain::{Full, SliceTransport, Transport};
pub use level::{enabled, set_level, set_off, Level};
pub use ring::{Consumer, Producer, Ring, ARGS_CAP, FRAME_MAX};
pub use site::Site;

#[cfg(feature = "std")]
pub use drain::IoTransport;
#[cfg(feature = "std")]
pub use runtime::{set_context_name, spawn_drain, stats, Drain, DrainHandle, Stats, THREAD_RING};

#[doc(hidden)]
pub mod __private {
    use core::mem::MaybeUninit;

    pub use linkme;

    #[cfg(feature = "std")]
    pub use crate::runtime::__local_push;

    /// Uninitialised argument buffer for one log call.
    #[inline(always)]
    pub fn args_buf() -> [MaybeUninit<u8>; crate::ARGS_CAP] {
        // SAFETY: an array of `MaybeUninit` needs no initialisation.
        unsafe { MaybeUninit::<[MaybeUninit<u8>; crate::ARGS_CAP]>::uninit().assume_init() }
    }

    /// Push a finished argument writer through a producer.
    #[inline(always)]
    pub fn push_to<const N: usize>(
        p: &mut crate::Producer<'_, N>,
        site: u32,
        w: &crate::Writer<'_>,
    ) {
        if w.overflowed() {
            p.count_drop();
        } else {
            p.push(site, w.as_slice());
        }
    }

    /// Push a finished argument writer into the calling thread's ring.
    #[cfg(feature = "std")]
    #[inline(always)]
    pub fn push_local(site: u32, w: &crate::Writer<'_>) {
        if w.overflowed() {
            // Oversize argument lists are counted as drops by the ring.
            __local_push(site, &[0u8; crate::ARGS_CAP + 1]);
        } else {
            __local_push(site, w.as_slice());
        }
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
            $crate::site::count_placeholders($fmt) == __N,
            "rtlog: placeholder count does not match the number of arguments"
        );
        #[$crate::__private::linkme::distributed_slice($crate::site::SITES)]
        #[linkme(crate = $crate::__private::linkme)]
        static __RTLOG_SITE: $crate::Site = $crate::Site::new(
            $lvl, ::core::module_path!(), ::core::file!(), ::core::line!(), ::core::column!(),
            $fmt, $crate::__private::nargs(__N),
        );
        &__RTLOG_SITE
    }};
    (to: $p:expr, $lvl:expr, $fmt:literal $(, $arg:expr)* $(,)?) => {{
        let __site: &'static $crate::Site =
            $crate::log!(@site $lvl, $fmt, <[()]>::len(&[$($crate::__unit!($arg)),*]));
        if $crate::enabled($lvl) {
            let mut __buf = $crate::__private::args_buf();
            let mut __w = $crate::Writer::new_uninit(&mut __buf);
            $( $crate::Log::write(&$arg, &mut __w); )*
            $crate::__private::push_to($p, __site.id, &__w);
        }
    }};
    ($lvl:expr, $fmt:literal $(, $arg:expr)* $(,)?) => {{
        let __site: &'static $crate::Site =
            $crate::log!(@site $lvl, $fmt, <[()]>::len(&[$($crate::__unit!($arg)),*]));
        if $crate::enabled($lvl) {
            let mut __buf = $crate::__private::args_buf();
            let mut __w = $crate::Writer::new_uninit(&mut __buf);
            $( $crate::Log::write(&$arg, &mut __w); )*
            $crate::__private::push_local(__site.id, &__w);
        }
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
    fn sites_are_collected_with_unique_ids() {
        assert!(crate::site::sites().len() >= 9);
        assert!(crate::site::find_collision().is_none());
        assert!(crate::site::sites()
            .iter()
            .any(|s| s.fmt == "{} {} {}" && s.nargs == 3));
    }
}
