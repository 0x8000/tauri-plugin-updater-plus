set shell := ["bash", "-c"]

mod bdd "./tests/bdd"

alias test-bdd := bdd::test
alias test-one := bdd::test-one

# Run all unit tests across workspace feature combinations
test:
    cargo test --workspace
    cargo test -p updater-plus --no-default-features --features cli
    cargo test -p updater-plus --no-default-features --features engine
    cargo test -p updater-plus --no-default-features --features http
    cargo test -p updater-plus --no-default-features --features native-installer
    cargo test --workspace --all-features

# Run pre-commit style checks and clippy lints
lint:
    pre-commit run -a
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo clippy -p updater-plus --no-default-features --features engine -- -D warnings
    cargo clippy -p updater-plus --no-default-features --features http -- -D warnings
    cargo clippy -p updater-plus --no-default-features --features native-installer -- -D warnings
    cargo clippy -p updater-plus --no-default-features --features cli --bin blockmap-gen -- -D warnings
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps

# Verify crates.io packaging
check-publish:
    cargo publish --workspace --dry-run
