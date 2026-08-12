check:
    cargo fmt --all -- --check
    cargo clippy --all-targets --locked --features test-support -- -D warnings
    cargo test --all-targets --locked --features test-support

security-check:
    cargo deny check
    cargo machete --with-metadata
    ./scripts/check-workflow-pins.sh

release-check: check security-check
    ./scripts/check-release-metadata.sh
    ./scripts/check-package-contents.sh
    mkdir -p target
    dist plan --output-format=json > target/dist-plan.json
    ./scripts/check-release-assets.sh target/dist-plan.json
    cargo build --release --locked
    cargo publish --locked --dry-run

release: release-check
    git tag -f latest HEAD
    git push --force origin refs/tags/latest

e2e:
    OPZ_E2E=1 cargo test --locked --test e2e_real_op -- --nocapture
