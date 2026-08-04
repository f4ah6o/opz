# Vendored opz-plugin registry snapshot

Source: `https://github.com/opz-rs/opz-plugin`

Snapshot commit: `643de2a` (`feat: establish declarative plugin registry`)

Only the registry index and immutable plugin manifests required by the `opz` runtime are vendored. The upstream project is MIT licensed; see `LICENSE`. Runtime use always verifies each manifest against the SHA-256 recorded in `registry.toml`.
