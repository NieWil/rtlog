# Ship without the format strings

By default a binary that logs with rtlog is self-describing: every call site's descriptor (level, file, line) and its format string are in the image, where `rtlog contract --elf` and `rtlog::contract::write` read them. The log path never touches them. A stripped build keeps them in the ELF file the linker writes, for the host tools, and out of what is flashed or shipped:

- the image is smaller: per call site, the descriptor (40 B on a 32-bit target in Rust, 28 B in C) plus its strings;
- the log texts, file names and module paths are not in the product (threat V2 of the [specification](spec.md)).

Measured:

| Build | Unstripped | Stripped | Saved |
| --- | ---: | ---: | ---: |
| Rust, Cortex-M4F, 10 call sites ([`size/mcu`](../size/mcu), `opt-level = "s"`, LTO), flash | 4536 B | 3796 B | 740 B (16 %) |
| Zephyr `hello` on `qemu_cortex_m3`, 4 call sites, `zephyr.bin` | 10108 B | 9896 B | 212 B |
| C demo on x86-64 Linux, 8 call sites, file size | 29152 B | 27312 B | 1840 B |

ELF targets only: firmware and Linux. Mach-O and PE binaries are not stripped.

## How it works

Three things change in a stripped build:

1. **The strings get a section of their own.** Normally a format string is a literal in `.rodata`, mixed with the program's other constants. With the `strip` feature (Rust) or `RTLOG_STRIP` (C, C++), each site's strings are objects in an `rtlog_strs` section, next to the descriptors (`linkme_SITES` in Rust, `rtlog_sites` in C).
2. **The linker script marks both sections as not loaded** (`(INFO)`, or `(COPY)` in Zephyr). They are written to the ELF file, with their contents, but are in no loadable segment: `objcopy -O binary`, a flash tool or the program loader leave them out.
3. **rtlog stops reading descriptors at run time.** In Rust, `site::sites()` is empty, `contract::write` returns an error, and the stream header carries contract hash 0, which the decoder reads as "not stated". In C nothing changes: the header hash only needs the number of sites, which is a difference of two link-time addresses.

The contract always comes from the ELF file of the same build:

```sh
rtlog contract --elf firmware.elf -o firmware.contract.jsonl
```

Archive it with the release; without it a capture cannot be decoded.

## Rust firmware

```toml
[dependencies]
rtlog = { version = "0.2", default-features = false, features = ["strip"] }
```

In the linker script (`memory.x` with `cortex-m-rt`), replace the [section that keeps the descriptors in flash](embedded.md#keep-the-descriptors-in-flash) with:

```text
SECTIONS
{
  linkme_SITES (INFO) : { KEEP(*(linkme_SITES)) }
  linkm2_SITES (INFO) : { KEEP(*(linkm2_SITES)) }
  rtlog_strs (INFO) : { KEEP(*(rtlog_strs)) }
}
```

Works with GNU ld and with LLD (`rust-lld`, the default on `thumbv*` targets). [`size/mcu/layout/sites-strip.x`](../size/mcu/layout/sites-strip.x) is the file CI builds.

Enable `strip` in the final binary crate only. A library that enables it changes `contract::write` and `site::sites()` for every crate of the program.

## C and C++ on bare metal

Compile every file with `-DRTLOG_STRIP` and place the two sections in your linker script:

```text
rtlog_sites (INFO) : { __start_rtlog_sites = .; KEEP(*(rtlog_sites)) __stop_rtlog_sites = .; }
rtlog_strs (INFO) : { KEEP(*(rtlog_strs)) }
```

With `RTLOG_STRIP` the contract has no function names (`module` is empty): `__func__` is a variable of the enclosing function and cannot be put in a section.

## Zephyr

```ini
CONFIG_RTLOG=y
CONFIG_RTLOG_STRIP=y
```

The module then puts both sections in Zephyr's `DEVNULL_REGION` (it selects `CONFIG_LINKER_DEVNULL_MEMORY`; available on Cortex-M and 32-bit RISC-V). `zephyr.bin` and `zephyr.hex` have no descriptors and no format strings; `zephyr.elf` has them.

## Linux programs

A hosted program has no linker script of its own, so rtlog ships a fragment, [`c/rtlog_strip.ld`](../c/rtlog_strip.ld), that GNU ld adds to its default script. After linking, read the contract, then remove the sections from the copy that ships:

```sh
# C
cc -O2 -DRTLOG_STRIP -pthread -I c/include app.c -fuse-ld=bfd -Wl,-T,c/rtlog_strip.ld -o app
# Rust (rtlog with features = ["strip"])
RUSTFLAGS="-C link-arg=-fuse-ld=bfd -C link-arg=-Wl,-T,$PWD/c/rtlog_strip.ld" cargo build --release

rtlog contract --elf app -o app.contract.jsonl
objcopy -R rtlog_sites -R rtlog_strs -R linkme_SITES -R linkm2_SITES app app.shipped
```

GNU ld is required here (`-fuse-ld=bfd`). LLD gives every non-loaded section address 0, and a position-independent executable linked that way crashes at start-up. Firmware, which is not position-independent, links with either.

A Rust program built with `strip` cannot use `rtlog_decode::stdout::init()` (it would know no site) or `rtlog::contract::write` (it returns an error): both need the descriptors in the running program. Keep an unstripped build for development.

[`examples/stripped.rs`](../examples/stripped.rs) and [`scripts/strip_e2e.sh`](../scripts/strip_e2e.sh) run this flow for Rust and C; CI runs the script.

## Check what ships

```sh
rtlog verify --contract firmware.contract.jsonl firmware.bin
```

`rtlog verify` searches each image for every format string and span name of the contract and fails if it finds one:

```text
firmware.bin: 0 of 10 format strings found in the image (0 shorter than 6 bytes not searched for)
```

```text
firmware.bin: contains the format string of site 4139228395 (src/main.rs:69): "temp={} limit={}"
10 format string(s) ship in the image; it is not stripped
```

Run it in the release pipeline on the file that leaves the building: the `.bin` or `.hex`-converted image, or the executable after `objcopy`. Run on the linked ELF file it fails, as it should: that file still holds the strings. Strings shorter than 6 bytes are not searched for, because a few bytes such as `x={}` occur in unrelated data by chance.

## What you give up

| | Unstripped | Stripped |
| --- | --- | --- |
| Contract | from the binary, or written by the program (`contract::write`) | from the linked ELF file only; archive it |
| Header contract hash (Rust) | checked by the decoder | 0, not checked. A wrong contract still fails at the first frame whose site ID it does not have, because Rust IDs are content hashes |
| Header contract hash (C) | checked | checked (it depends on the number of sites only) |
| `rtlog_decode::stdout::init()` | works | not available |
| Function names in C contracts | yes | no |
| Crash dumps | decode against the contract | the same |
