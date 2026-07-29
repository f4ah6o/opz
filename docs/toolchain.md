# Rust toolchain policy

`opz` is developed and verified with the latest stable Rust toolchain. The
repository's `rust-toolchain.toml` selects that channel and installs the
`rustfmt` and `clippy` components needed by `just check`.

The project does not declare or promise a fixed minimum supported Rust version
(MSRV). Contributors may use features available in the current stable release.
CI is the source of truth for the supported compiler.

Cargo commands used by CI and release verification must use `--locked` whenever
Cargo supports that option. This ensures that dependency resolution matches the
committed `Cargo.lock`.
