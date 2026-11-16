/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Linker script fragment for a hosted ELF program (Linux) whose rtlog
 * descriptors are to be stripped: C or C++ compiled with -DRTLOG_STRIP, Rust
 * with rtlog's `strip` feature. Pass it to GNU ld next to the default script:
 *
 *   cc -DRTLOG_STRIP ... -fuse-ld=bfd -Wl,-T,c/rtlog_strip.ld
 *   RUSTFLAGS="-C link-arg=-fuse-ld=bfd -C link-arg=-Wl,-T,$PWD/c/rtlog_strip.ld"
 *
 * (INFO) sections are written to the file but are not part of the loaded
 * program. `rtlog contract --elf` reads them from the linked binary; then
 *
 *   objcopy -R rtlog_sites -R rtlog_strs -R linkme_SITES -R linkm2_SITES app
 *
 * removes them from the copy that ships. See docs/strip.md.
 *
 * GNU ld only: LLD gives every non-loaded section address 0, which breaks a
 * position-independent executable at start-up. Firmware does not use this
 * file; put the same sections in the firmware's own linker script.
 */
SECTIONS
{
  rtlog_sites (INFO) : { __start_rtlog_sites = .; KEEP(*(rtlog_sites)) __stop_rtlog_sites = .; }
  linkme_SITES (INFO) : { KEEP(*(linkme_SITES)) }
  linkm2_SITES (INFO) : { KEEP(*(linkm2_SITES)) }
  rtlog_strs (INFO) : { KEEP(*(rtlog_strs)) }
}
INSERT AFTER .bss;
