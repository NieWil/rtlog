# Releasing

1. Set the version in the workspace `Cargo.toml` (`[workspace.package] version`) and add a `## vX.Y.Z` section at the top of [`CHANGELOG.md`](../CHANGELOG.md). Also update the exact `rtlog-core = { version = "=X.Y.Z", … }` requirement in `crates/rtlog-decode/Cargo.toml` (the build fails until it matches). Merge that to `main`.
2. On GitHub, open **Actions → release → Run workflow**. Pick `main`, type the version (for example `v0.1.0`) and run it.

The workflow checks that the version is well formed, matches `Cargo.toml` and is not already tagged. It then builds the `rtlog` CLI and the [demo package](demo.md) for every host, smoke-tests them (the whole demo natively on x86-64 Linux and Apple silicon, a trace under QEMU elsewhere), and packages the C/C++ header with the Zephyr module. Finally it creates the tag on the commit it built and publishes the release, with that version's changelog section as the notes and a `SHA256SUMS` file. After the release it publishes `rtlog-core` and then `rtlog-decode` to crates.io. A crate version that is already there is skipped, so a failed job can be re-run.

Pushing a `vX.Y.Z` tag yourself does the same. Running the workflow with an empty version is a dry run: it builds and packages, and publishes nothing.

## crates.io token (once)

Create a token at <https://crates.io/settings/tokens> with the `publish-new` and `publish-update` scopes, limited to the `rtlog-core` and `rtlog-decode` crates once they exist. Add it in **Settings → Secrets and variables → Actions** as `CARGO_REGISTRY_TOKEN`. Every run, dry runs included, packages and verifies both crates with `cargo publish --workspace --dry-run`, so manifest errors are caught on the pull request.
