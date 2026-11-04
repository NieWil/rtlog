# Releasing

1. Set the version in the workspace `Cargo.toml` (`[workspace.package] version`) and add a `## vX.Y.Z` section at the top of [`CHANGELOG.md`](../CHANGELOG.md). Merge that to `main`.
2. On GitHub, open **Actions → release → Run workflow**. Pick `main`, type the version (for example `v0.1.0`) and run it.

The workflow checks that the version is well formed, matches `Cargo.toml` and is not already tagged. It then builds the `rtlog` CLI for every host and packages the C/C++ header with the Zephyr module. Finally it creates the tag on the commit it built and publishes the release, with that version's changelog section as the notes and a `SHA256SUMS` file.

Pushing a `vX.Y.Z` tag yourself does the same. Running the workflow with an empty version is a dry run: it builds and packages, and publishes nothing.
