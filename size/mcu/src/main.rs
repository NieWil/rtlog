//! Size probe firmware. Each variant runs the same loop: read sensor-like
//! values from volatile registers and report them over a fake UART data
//! register. Only the reporting differs:
//!
//! - `baseline`: raw bytes, no logging at all (the reference);
//! - `rtlog1`, `rtlog10`: rtlog with 1 or 10 call sites, one static ring and
//!   an idle-loop drain to the UART;
//! - `fmt10`: the same 10 messages formatted on the device with `core::fmt`.

#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};
use cortex_m_rt::entry;
use panic_halt as _;

const UART_DR: *mut u32 = 0x4001_1004 as *mut u32;
const SENSOR: *const u32 = 0x4001_2000 as *const u32;
#[allow(dead_code)]
const DWT_CYCCNT: *const u32 = 0xE000_1004 as *const u32;

fn sensor(i: usize) -> u32 {
    unsafe { read_volatile(SENSOR.add(i)) }
}

fn uart(b: u8) {
    unsafe { write_volatile(UART_DR, u32::from(b)) }
}

#[cfg(feature = "baseline")]
fn report() {
    for i in 0..10 {
        for b in sensor(i).to_le_bytes() {
            uart(b);
        }
    }
}

#[cfg(any(feature = "rtlog1", feature = "rtlog10"))]
mod logging {
    use super::*;
    use rtlog::{Ring, Transport, FRAME_MAX};

    pub static RING: Ring<1024> = Ring::new();

    struct Uart;
    impl Transport for Uart {
        fn write(&mut self, frame: &[u8]) -> Result<(), rtlog::Full> {
            frame.iter().for_each(|&b| uart(b));
            Ok(())
        }
    }

    fn cycles() -> u64 {
        u64::from(unsafe { read_volatile(DWT_CYCCNT) })
    }

    pub fn run() -> ! {
        rtlog::set_clock(cycles, 168_000_000);
        let (mut p, mut c) = RING.split(1).unwrap();
        let mut scratch = [0u8; FRAME_MAX];
        let _ = rtlog::drain::write_header(&mut Uart);
        loop {
            #[cfg(feature = "rtlog1")]
            rtlog::info!(to: &mut p, "adc ch={} raw={}", 0u8, sensor(0));
            #[cfg(feature = "rtlog10")]
            {
                rtlog::info!(to: &mut p, "adc ch={} raw={}", 0u8, sensor(0));
                rtlog::debug!(to: &mut p, "temp={} limit={}", sensor(1) as i16, 85u8);
                rtlog::info!(to: &mut p, "pid out={} err={}", sensor(2) as f32, sensor(3) as i32);
                rtlog::warn!(to: &mut p, "vbat low mv={}", sensor(4) as u16);
                rtlog::trace!(to: &mut p, "tick={}", sensor(5));
                rtlog::info!(to: &mut p, "motor rpm={} dir={}", sensor(6), sensor(7) & 1 == 1);
                rtlog::error!(to: &mut p, "fault code={} addr={}", sensor(8) as u8, sensor(9));
                rtlog::debug!(to: &mut p, "queue depth={}", sensor(0) as u16);
                rtlog::info!(to: &mut p, "can rx id={} len={}", sensor(1), sensor(2) as u8);
                rtlog::warn!(to: &mut p, "overrun count={}", sensor(3));
            }
            rtlog::drain::drain_ring(&mut c, &mut Uart, &mut scratch);
        }
    }
}

#[cfg(feature = "fmt10")]
fn report() {
    use core::fmt::Write;
    struct Uart;
    impl Write for Uart {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            s.bytes().for_each(uart);
            Ok(())
        }
    }
    let t = unsafe { read_volatile(DWT_CYCCNT) };
    let mut u = Uart;
    let _ = writeln!(u, "{t} INFO adc ch={} raw={}", 0u8, sensor(0));
    let _ = writeln!(u, "{t} DEBUG temp={} limit={}", sensor(1) as i16, 85u8);
    let _ = writeln!(u, "{t} INFO pid out={} err={}", sensor(2) as f32, sensor(3) as i32);
    let _ = writeln!(u, "{t} WARN vbat low mv={}", sensor(4) as u16);
    let _ = writeln!(u, "{t} TRACE tick={}", sensor(5));
    let _ = writeln!(u, "{t} INFO motor rpm={} dir={}", sensor(6), sensor(7) & 1 == 1);
    let _ = writeln!(u, "{t} ERROR fault code={} addr={}", sensor(8) as u8, sensor(9));
    let _ = writeln!(u, "{t} DEBUG queue depth={}", sensor(0) as u16);
    let _ = writeln!(u, "{t} INFO can rx id={} len={}", sensor(1), sensor(2) as u8);
    let _ = writeln!(u, "{t} WARN overrun count={}", sensor(3));
}

#[entry]
fn main() -> ! {
    #[cfg(any(feature = "rtlog1", feature = "rtlog10"))]
    logging::run();
    #[cfg(any(feature = "baseline", feature = "fmt10"))]
    loop {
        report();
    }
}
