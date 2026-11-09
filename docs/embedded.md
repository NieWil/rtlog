# Embedded Rust (`no_std`)

`rtlog` builds without `std` for Cortex-M3/M4/M7/M33 and RV32IMAC; CI checks all four. On bare metal, each task owns a static ring and logs through its producer. The drain runs from the idle loop or a DMA callback.

```rust
static MOTOR: rtlog::Ring<1024> = rtlog::Ring::new();

fn cycles() -> u64 { /* DWT_CYCCNT, extended to 64 bits */ }

#[entry]
fn main() -> ! {
    rtlog::set_clock(cycles, 168_000_000);
    let (mut p, mut c) = MOTOR.split(1).unwrap();
    let mut scratch = [0u8; rtlog::FRAME_MAX];
    let _ = rtlog::drain::write_header(&mut uart);
    loop {
        rtlog::info!(to: &mut p, "adc ch={} raw={}", 0u8, read_adc());
        let s = rtlog::span!(to: &mut p, "pid.update");
        /* ... */
        p.span_exit(s);
        rtlog::drain::drain_ring(&mut c, &mut uart, &mut scratch); // idle work
    }
}
```

`uart` is any `rtlog::Transport`: a few lines around your HAL's write, RTT, or a DMA buffer.

## Add the crate

Turn off the default `std` feature:

```toml
[dependencies]
rtlog = { version = "0.2", default-features = false }
```

or run `cargo add rtlog --no-default-features`.

## Write a transport

A transport takes one whole frame per call. This one writes to a UART data register and waits for each byte to leave:

```rust
use core::ptr::{read_volatile, write_volatile};

const USART_ISR: *const u32 = 0x4001_381C as *const u32; // your UART's status register
const USART_TDR: *mut u32 = 0x4001_3828 as *mut u32;     // and its transmit data register
const TXE: u32 = 1 << 7;

struct Uart;

impl rtlog::Transport for Uart {
    fn write(&mut self, frame: &[u8]) -> Result<(), rtlog::Full> {
        for &b in frame {
            while unsafe { read_volatile(USART_ISR) } & TXE == 0 {}
            unsafe { write_volatile(USART_TDR, u32::from(b)) };
        }
        Ok(())
    }
}
```

The write is all or nothing. A transport that cannot wait, such as a DMA buffer, checks for room first and returns `Err(rtlog::Full)` without writing anything; the frame stays in the ring for the next pass.

## Contexts

`Ring::split(ctx)` returns the ring's producer and consumer, and tags every frame from that ring with the context ID `ctx`. The decoder shows each context as its own track. Context 0 is the ISR by convention, so number tasks from 1. Each ring is split once; a second call returns `None`.

Name the contexts after the stream header with `drain::write_ctx_register`, so the decoder prints names instead of numbers:

```rust
fn name_contexts(t: &mut impl rtlog::Transport) {
    let _ = rtlog::drain::write_ctx_register(t, 0, "isr");
    let _ = rtlog::drain::write_ctx_register(t, 1, "motor");
}
```

## Log from an interrupt

Give the ISR its own ring. Split it in `main` before you enable the interrupt, and move the producer into a static that the handler can reach. The consumer stays with the drain. This example uses the `critical-section` crate:

```rust
use core::cell::RefCell;
use critical_section::Mutex;

static ISR_RING: rtlog::Ring<256> = rtlog::Ring::new();
static ISR_LOG: Mutex<RefCell<Option<rtlog::Producer<'static, 256>>>> =
    Mutex::new(RefCell::new(None));

// in main, before enabling the interrupt:
let (isr_p, mut isr_c) = ISR_RING.split(0).unwrap();
critical_section::with(|cs| ISR_LOG.borrow_ref_mut(cs).replace(isr_p));

#[interrupt]
fn TIM2() {
    critical_section::with(|cs| {
        if let Some(p) = ISR_LOG.borrow_ref_mut(cs).as_mut() {
            rtlog::trace!(to: p, "tim2 cnt={}", read_cnt());
        }
    });
}
```

With RTIC, put the producer in the task's `#[local]` resources instead; then no critical section is needed. A ring has one producer, so two handlers that can preempt each other need a ring each.

## Drain and resync

Drain every ring from the idle loop. About every 64 KiB of output (`rtlog::wire::RESYNC_EVERY`), write a resync frame with `drain::write_resync` and name the contexts again. If a byte is corrupted on the link, the decoder then loses only the frames up to the next resync, not the rest of the capture:

```rust
let mut uart = Uart;
let mut scratch = [0u8; rtlog::FRAME_MAX];
let _ = rtlog::drain::write_header(&mut uart);
name_contexts(&mut uart);
let mut since_resync = 0;
loop {
    /* ... task work and logging ... */
    since_resync += rtlog::drain::drain_ring(&mut motor_c, &mut uart, &mut scratch).bytes;
    since_resync += rtlog::drain::drain_ring(&mut isr_c, &mut uart, &mut scratch).bytes;
    if since_resync >= rtlog::wire::RESYNC_EVERY && rtlog::drain::write_resync(&mut uart).is_ok() {
        name_contexts(&mut uart);
        since_resync = 0;
    }
}
```

Times right after a gap are exact only if you also write `drain::write_ctx_time` for each context; see section 5 of the [specification](spec.md).

[`examples/static_rings.rs`](../examples/static_rings.rs) runs the same flow on the host.

## Keep the descriptors in flash

Call-site descriptors (the contract) go into the `linkme_SITES` section. With `cortex-m-rt`, add this to `memory.x` so the linker keeps them in flash with the other read-only data:

```text
SECTIONS
{
  linkme_SITES : { KEEP(*(linkme_SITES)) } > FLASH
  linkm2_SITES : { KEEP(*(linkm2_SITES)) } > FLASH
} INSERT AFTER .rodata;
```

The log path never reads a descriptor: a site's ID is a compile-time constant. The stream header's contract hash does read them once, so they must be loaded.

## Get the contract and decode

Save the raw bytes from the board to `capture.rtlog`. Start the capture before you reset the board, so the file begins with the stream header.

- **UART** (a USB-serial adapter or the probe's virtual COM port): put the port in raw mode first. Otherwise the tty driver translates CR/LF bytes and corrupts frames.

  ```sh
  stty -F /dev/ttyACM0 115200 raw -echo -ixon -ixoff -crtscts
  cat /dev/ttyACM0 > capture.rtlog     # Ctrl-C to stop
  ```

- **RTT**: write the frames to an RTT up channel instead of a UART, and save that channel's bytes with your probe tool. For example, OpenOCD's `rtt server start 9090 0` serves channel 0 on a TCP port, and `nc localhost 9090 > capture.rtlog` saves it.

Then extract the contract from the same firmware build and decode:

```sh
cargo build --release
rtlog contract --elf target/thumbv7em-none-eabihf/release/firmware -o firmware.contract.jsonl
rtlog decode --contract firmware.contract.jsonl capture.rtlog
```

Size and per-call cost on a Cortex-M4F: see [Performance and size](performance.md). The full firmware used there is [`size/mcu`](../size/mcu).
