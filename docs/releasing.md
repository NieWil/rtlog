# Releasing

1. Set the version in the workspace `Cargo.toml` (`[workspace.package] version`) and add a `## vX.Y.Z` section at the top of [`CHANGELOG.md`](../CHANGELOG.md). Also update the exact `rtlog = { version = "=X.Y.Z", … }` requirement in `crates/rtlog-decode/Cargo.toml` (the build fails until it matches). Merge that to `main`.
2. On GitHub, open **Actions → release → Run workflow**. Pick `main`, type the version (for example `v0.2.4`) and run it.

The workflow checks that the version is well formed, matches `Cargo.toml` and is not already tagged. It then builds the `rtlog` CLI and the [demo package](demo.md) for every host, smoke-tests them (the whole demo natively on x86-64 Linux and Apple silicon, a trace under QEMU elsewhere), and packages the C/C++ header with the Zephyr module. It also builds Debian/Ubuntu packages with `scripts/build_deb.sh`: `rtlog_<version>-1_<arch>.deb` for amd64, i386, arm64 and armhf from the static musl binaries, and `rtlog-dev_<version>-1_all.deb` with the header and a pkg-config file. The amd64 package is installed, run and checked with lintian, and the dev package is installed and a C program built against it through pkg-config. Finally it creates the tag on the commit it built and publishes the release, with that version's changelog section as the notes and a `SHA256SUMS` file. After the release it publishes `rtlog` and then `rtlog-decode` to crates.io. A crate version that is already there is skipped, so a failed job can be re-run.

Pushing a `vX.Y.Z` tag yourself does the same. Running the workflow with an empty version is a dry run: it builds and packages, and publishes nothing.

## crates.io access (once per crate)

The release publishes with [Trusted Publishing](https://crates.io/docs/trusted-publishing): no token is stored. The `crates-io` job gets an identity token from GitHub, and `rust-lang/crates-io-auth-action` exchanges it for a crates.io token that expires after about 30 minutes. crates.io only hands one out if the run comes from the repository and workflow named in the crate's settings.

For each crate (`rtlog`, `rtlog-decode`), open its page on crates.io, then **Settings → Trusted Publishing → Add**, and enter owner `NieWil`, repository `rtlog`, workflow `release.yml`. Leave the environment empty.

A new crate added to the workspace must be published once by hand with an API token (`cargo login`, then `cargo publish -p <crate>`), because Trusted Publishing can only be set up for a crate that already exists.

Every run, dry runs included, packages and verifies both crates with `cargo publish --workspace --dry-run`, so manifest errors are caught on the pull request.
