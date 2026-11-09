# Contributing

Thanks for helping. Two kinds of contribution are especially welcome: **timing results from your board** (no code needed, see below) and fixes. Coding agents: read [AGENTS.md](AGENTS.md) too.

## Share your board's results

The demo measures what a log call costs on your machine. To add your board to [results/](results/README.md):

1. Download `rtlog-demo-<version>-<target>` for your board from the [releases page](https://github.com/NieWil/rtlog/releases) and run it ([demo guide](docs/demo.md)), or from a clone run:

   ```sh
   cargo run --release -p rtlog-examples --example timing -- --board "Raspberry Pi 5" --out timing.md
   ```

2. Either **open an issue** with the [board results form](https://github.com/NieWil/rtlog/issues/new?template=board-results.yml) and paste `timing.md`, or **open a pull request** that adds it as `results/<version>/<board>.md` (for example `results/0.2.4/raspberry-pi-5.md`) and runs `python3 scripts/results.py` to refresh the index.

**On a microcontroller** with Zephyr, flash the benchmark firmware ([`c/zephyr/samples/benchmark`](c/zephyr/samples/benchmark), see [Zephyr](docs/zephyr.md)), capture its serial output to a file, and turn it into a report:

```sh
python3 scripts/results.py mcu capture.txt --board "ST NUCLEO-L432KC" --cpu "Arm Cortex-M4F (STM32L432KC)" \
  --os "Zephyr 4.2.0" --rtlog 0.2.4 --toolchain "Zephyr SDK 0.17.2, GCC 12.2.0, -O2" \
  --target nucleo_l432kc/stm32l432xx --out results/0.2.4/st-nucleo-l432kc.md
```

Pass several captures (one per reset) to combine runs. [`results/0.2.2/st-nucleo-l432kc.md`](results/0.2.2/st-nucleo-l432kc.md) is an example.

CI checks that each results file names its version, board, CPU, OS and Rust version (toolchain and API for microcontrollers), and that it sits in the folder of its version. Results are never edited after they are merged; a new version gets a new file.

## How quality is enforced

Every pull request runs these checks in CI, and all of them must pass before merging:

| Check | Tool | Config |
| --- | --- | --- |
| Formatting | `cargo fmt --check` | defaults |
| Lints, warnings as errors | `cargo clippy --workspace --all-targets -D warnings` | defaults |
| Every public item documented, no broken doc links | `cargo doc` with `RUSTDOCFLAGS=-D warnings`, `#![warn(missing_docs)]` | crate roots |
| Tests: Linux, macOS, Windows, QEMU (arm, aarch64, riscv64, i686) | `cargo test` | `.github/workflows/ci.yml` |
| `no_std` builds for Cortex-M3, M4F/M7, M33 and RV32IMAC | `cargo build --no-default-features` | |
| Line coverage at least 85 % | `cargo llvm-cov`, reported to [Codecov](https://codecov.io/gh/NieWil/rtlog) | `codecov.yml`, ci.yml |
| Memory safety of the unsafe ring code | Miri, AddressSanitizer, ThreadSanitizer | ci.yml |
| Spec rules: real-time hot path, wire format, usability, size budgets | `scripts/spec_check.py` | `docs/spec.md` |
| C/C++: build with `-Wall -Wextra -Werror`, end-to-end and crash tests | `scripts/c_e2e.sh` | |
| C/C++ static analysis | `clang-tidy` | `.clang-tidy` |
| Dependencies: advisories, licences, sources | `cargo deny check` | `deny.toml` |
| Spelling | `typos` | `_typos.toml` |
| Workflows and shell scripts | `actionlint`, `shellcheck` | |
| Markdown | `markdownlint` | `.markdownlint-cli2.yaml` |

A failing check is fixed in the code, never by loosening the tool's configuration. If a rule must change (a budget, a coverage minimum, a disabled lint), change it in its config file with the reason, in the same pull request.

**Maintainers:** to make these checks binding, turn on branch protection for `main` (Settings → Branches → Add rule): require a pull request, require the checks of the `ci` and `docs` workflows to pass (pick them under "Require status checks"), and require branches to be up to date.

## Run the checks locally

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo test --workspace
python3 scripts/spec_check.py
scripts/c_e2e.sh gcc g++
```

The extra tools, once:

```sh
cargo install --locked cargo-deny typos-cli cargo-llvm-cov
pip install shellcheck-py actionlint-py      # or your package manager
cargo deny check && typos && actionlint && shellcheck scripts/*.sh scripts/demo/*.sh
cargo llvm-cov --workspace --summary-only
```

## Pull requests

- One topic per pull request. Say why, what changed and which checks you ran.
- Add a line to `CHANGELOG.md` under `## Unreleased`, and update the guide in `docs/` that covers the change.
- Changes to the hot path, the wire format or the crash handler follow the rules in [AGENTS.md](AGENTS.md#rules-that-are-easy-to-break) and [docs/spec.md](docs/spec.md).
