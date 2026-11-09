# When the process crashes

A log call writes into the calling thread's ring and returns; a drain sends the frames out later. So when a process dies, the most recent records may still be in the rings. rtlog's crash handler writes them to a file before the process goes, so you see what happened right before the failure.

Available on Unix (Linux, macOS, the BSDs) with the `std` feature, and in C on POSIX. Windows and bare-metal targets have no crash dump yet ([roadmap](roadmap.md)).

## What to do

1. **Install the handler first thing in `main`**, with a directory on a disk that survives the crash:

   ```rust
   rtlog::crash::install("/var/lib/myapp/crashes")?;
   ```

   In C or C++: `rtlog_crash_install("/var/lib/myapp/crashes");` (the directory must exist).

   Each dump is a new file, `crash-<pid>-<n>.rtlog`, opened with `O_EXCL`: nothing is ever overwritten. A caught panic, a watchdog snapshot, a restart (even in a container where every process is pid 1: the number just counts on) or a forked child each get their own file. `rtlog::crash::dumps()` / `rtlog_crash_dumps()` tells how many this process has written. Panics and snapshots stop after 16 files per process (a service that catches a panic per request cannot fill the disk); fatal signals are always dumped.

2. **Keep the binary and its contract** for every build you ship. A dump decodes only against the build that wrote it: `rtlog contract --elf myapp -o myapp.contract.jsonl` (or `rtlog::contract::write` at startup).

3. **After a crash, decode the dump:**

   ```sh
   rtlog decode --contract myapp.contract.jsonl /var/lib/myapp/crashes/crash-4242-1.rtlog
   ```

   ```text
   0.000793 [main] DEBUG before crash i=998 sensor=2998
   0.000794 [main] WARN  about to fail mode=segv
   0.001983 [main] CRASH SIGSEGV: invalid memory access (signal 11, code 1) addr=0x0 pc=0x562616a7f0d5
   ```

   `--perfetto` shows the crash as a marker across every thread's track. To map `pc` to a source line: `addr2line -e myapp -f -C <pc minus the load address>` (or use a non-PIE build).

4. **Size the rings for your burst.** The dump holds whatever the drain has not sent yet, up to a ring's capacity per thread (64 KiB on `std`, your choice on `no_std`). A full ring drops new records and counts them; the dump reports the count.

5. **Optionally, dump on demand.** A watchdog that detects a deadlock or a missed deadline can call `rtlog::crash::snapshot("deadlock suspected")` (C: `rtlog_crash_snapshot`). That writes the same dump without stopping the program.

6. **Run a drain too.** While none is running, rtlog keeps the rings of the last 16 threads to finish (so a dump still shows the workers that just finished) and releases older ones; with a drain, nothing is held back.

## What is covered

| Failure | Example | Caught as |
| --- | --- | --- |
| Rust panic | integer division by zero, `unwrap()` on `None`, index out of bounds | `CRASH panic: <message and location>` |
| Invalid memory access | null or dangling pointer write | `SIGSEGV` with the faulting address |
| Arithmetic trap | integer division by zero in C, or in machine code (x86) | `SIGFPE` with the faulting instruction's address |
| Abort | `abort()`, `panic = "abort"`, failed `assert` in C | `SIGABRT` |
| Bus error, illegal instruction | misaligned access on some CPUs, corrupted code | `SIGBUS`, `SIGILL` |

Not covered: `SIGKILL` (including the OOM killer), power loss and kernel panics, because no code runs then. Frames the drain had already handed to a buffered transport are in that transport's buffer, not in the rings: flush per poll (the default for `IoTransport`), or log to an unbuffered sink if those last milliseconds matter.

Integer division by zero in Rust is a panic, not a signal, on every architecture. In C it is undefined behaviour: x86 traps with `SIGFPE`, while Arm and RISC-V return a value without trapping, so nothing crashes there.

## How it works

The handler runs while the process is in an unknown state, so it does only what is safe in a signal handler:

- no allocation, no formatting on the signal path, no blocking lock and no sleep: the list of rings has its own lock that is only ever held for a push or a removal, so the dump does not wait for the drain, which may be blocked inside a transport write at that moment;
- it reads each ring's unread frames without consuming them, and writes with `open` / `write` / `close`; the file name is built without `snprintf`;
- before each ring's frames it writes a `ctx_time` frame, so the dump decodes to the right absolute times even though earlier frames went to the main stream;
- if two threads fault at once, the second waits (two seconds at most) for the first dump to finish instead of ending the process under it; the handler restores the default action itself rather than using `SA_RESETHAND`, which would have killed the second thread on the spot;
- it runs on its own 64 KiB alternate signal stack, with a guard page below it (since the release after 0.2.3). `install` sets one up for the calling thread, and every thread gets one when it first logs after that. The 8 KiB stack Rust gives a thread by default on most x86-64 CPUs was too small for the dump in a debug build;
- for fatal signals it first turns logging off, so the dump is a consistent snapshot. If the drain was consuming at that moment, a frame can appear both in the main stream and in the dump, never in neither. Then the default action runs (core dump, exit status naming the signal). After a panic the previous panic hook runs and the program continues if it can.

Tests crash real child processes in each of these ways, including with a drain blocked in a transport write, and check that every record logged before the crash is in the dump: [`crates/rtlog-decode/tests/crash.rs`](../crates/rtlog-decode/tests/crash.rs) for Rust, and [`c/examples/crash.c`](../c/examples/crash.c) through [`scripts/c_e2e.sh`](../scripts/c_e2e.sh) for C.
