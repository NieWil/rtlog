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

```sh
cargo build --release
rtlog contract --elf target/thumbv7em-none-eabihf/release/firmware -o firmware.contract.jsonl
rtlog decode --contract firmware.contract.jsonl capture.rtlog
```

Size and per-call cost on a Cortex-M4F: see [Performance and size](performance.md). The full firmware used there is [`size/mcu`](../size/mcu).
