//! Timestamp source. `std` defaults to the CPU's constant-rate counter (or a
//! monotonic nanosecond clock where there is none); `no_std` targets install
//! their own with [`set_clock`] (DWT, RTOS tick, …). Set `RTLOG_CLOCK=instant`
//! to force the portable `Instant` clock on `std`.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

static CLOCK_FN: AtomicUsize = AtomicUsize::new(0);
static CLOCK_HZ: AtomicU32 = AtomicU32::new(0);

/// Install a clock returning ticks at `hz` ticks per second.
pub fn set_clock(now: fn() -> u64, hz: u32) {
    CLOCK_HZ.store(hz, Ordering::Relaxed);
    // Relaxed is enough: the pointer is to code, which needs no synchronisation.
    CLOCK_FN.store(now as usize, Ordering::Relaxed);
}

/// Current tick count.
#[inline(always)]
pub fn now() -> u64 {
    let f = CLOCK_FN.load(Ordering::Relaxed);
    if f != 0 {
        // SAFETY: only `set_clock` stores non-zero values, and it stores a `fn() -> u64`.
        let f: fn() -> u64 = unsafe { core::mem::transmute::<usize, fn() -> u64>(f) };
        return f();
    }
    default_now()
}

/// Tick rate of [`now`].
pub fn hz() -> u32 {
    match CLOCK_HZ.load(Ordering::Relaxed) {
        0 => default_hz(),
        hz => hz,
    }
}

#[cfg(not(feature = "std"))]
#[inline(always)]
fn default_now() -> u64 {
    0
}

#[cfg(not(feature = "std"))]
fn default_hz() -> u32 {
    1
}

#[cfg(feature = "std")]
use hw::{default_hz, default_now};

/// Default `std` clock: the CPU's constant-rate counter where there is one
/// (x86-64 invariant TSC, aarch64 `CNTVCT_EL0`), else `Instant` in nanoseconds.
/// A counter read costs a few nanoseconds; `Instant::now` costs tens in a VM.
#[cfg(feature = "std")]
mod hw {
    use std::sync::OnceLock;
    use std::time::Instant;

    /// Which source the default clock uses; fixed on first use.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Source {
        /// The CPU counter: x86-64 invariant TSC halved, or aarch64 `CNTVCT_EL0`.
        Counter,
        /// `std::time::Instant`, in nanoseconds since the first read.
        Instant,
    }

    use std::sync::atomic::{AtomicU8, Ordering};

    /// 0 = not chosen yet, 1 = counter, 2 = `Instant`. A relaxed byte load on
    /// the log path instead of a `OnceLock` (an acquire load on Arm).
    static SOURCE: AtomicU8 = AtomicU8::new(0);
    static START: OnceLock<Instant> = OnceLock::new();
    static HZ: OnceLock<u32> = OnceLock::new();

    #[cfg(target_arch = "x86_64")]
    #[allow(unused_unsafe)] // `__cpuid` is safe on newer Rust, `unsafe` on older
    fn counter_available() -> bool {
        // CPUID 0x8000_0007 EDX bit 8: invariant TSC (constant rate, all P/C-states).
        // SAFETY: CPUID is always available on x86-64.
        let max_ext = unsafe { core::arch::x86_64::__cpuid(0x8000_0000) }.eax;
        max_ext >= 0x8000_0007
            && unsafe { core::arch::x86_64::__cpuid(0x8000_0007) }.edx & (1 << 8) != 0
    }

    /// TSC ticks halved, so the rate fits the header's `u32` on fast CPUs.
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    fn counter() -> u64 {
        // SAFETY: RDTSC is always available on x86-64.
        unsafe { core::arch::x86_64::_rdtsc() >> 1 }
    }

    #[cfg(target_arch = "x86_64")]
    fn counter_hz() -> u64 {
        // Calibrate once against the monotonic clock (~20 ms, first drain only).
        let (t0, c0) = (Instant::now(), counter());
        while t0.elapsed().as_millis() < 20 {
            std::hint::spin_loop();
        }
        let (dt, dc) = (t0.elapsed().as_nanos() as u64, counter() - c0);
        dc * 1_000_000_000 / dt.max(1)
    }

    #[cfg(target_arch = "aarch64")]
    fn counter_available() -> bool {
        true
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn counter() -> u64 {
        let v: u64;
        // SAFETY: CNTVCT_EL0 is readable from EL0 on Linux, macOS and Windows.
        unsafe {
            core::arch::asm!("mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack, preserves_flags))
        };
        v
    }

    #[cfg(target_arch = "aarch64")]
    fn counter_hz() -> u64 {
        let v: u64;
        // SAFETY: CNTFRQ_EL0 is readable from EL0.
        unsafe {
            core::arch::asm!("mrs {}, cntfrq_el0", out(reg) v, options(nomem, nostack, preserves_flags))
        };
        v
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    fn counter_available() -> bool {
        false
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    fn counter() -> u64 {
        0
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    fn counter_hz() -> u64 {
        0
    }

    /// The source the default clock settled on.
    #[inline(always)]
    pub fn source() -> Source {
        match SOURCE.load(Ordering::Relaxed) {
            1 => Source::Counter,
            2 => Source::Instant,
            _ => choose_source(),
        }
    }

    #[cold]
    #[inline(never)]
    fn choose_source() -> Source {
        // Miri cannot execute counter-reading instructions; it uses `Instant`.
        let s = if cfg!(miri)
            || std::env::var_os("RTLOG_CLOCK").is_some_and(|v| v == "instant")
            || !counter_available()
        {
            Source::Instant
        } else {
            Source::Counter
        };
        // Racing first callers compute the same answer, so a plain store is fine.
        SOURCE.store(if s == Source::Counter { 1 } else { 2 }, Ordering::Relaxed);
        s
    }

    #[inline(always)]
    pub fn default_now() -> u64 {
        match source() {
            Source::Counter => counter(),
            Source::Instant => START.get_or_init(Instant::now).elapsed().as_nanos() as u64,
        }
    }

    pub fn default_hz() -> u32 {
        *HZ.get_or_init(|| match source() {
            Source::Counter => u32::try_from(counter_hz()).unwrap_or(u32::MAX).max(1),
            Source::Instant => 1_000_000_000,
        })
    }
}

#[cfg(feature = "std")]
pub use hw::{source, Source};

#[cfg(all(test, feature = "std"))]
mod tests {
    #[test]
    fn default_clock_is_monotonic_and_calibrated() {
        let hz = u64::from(super::hz());
        assert!(hz >= 1_000_000, "rate {hz} Hz");
        let a = super::now();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let b = super::now();
        assert!(b > a);
        // 20 ms of sleep must read as at least 15 ms and well under a second.
        let ms = (b - a) * 1_000 / hz;
        assert!(
            (15..1_000).contains(&ms),
            "{ms} ms (source {:?})",
            super::source()
        );
    }
}
