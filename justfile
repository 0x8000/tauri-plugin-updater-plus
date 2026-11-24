set shell := ["bash", "-c"]

# Run all unit tests across feature combinations
test:
    cargo test
    cargo test --no-default-features --features cli
    cargo test --all-features

# Run formatting and clippy lints
lint:
    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo clippy --no-default-features --features cli --bin blockmap-gen -- -D warnings

# Verify crates.io packaging
check-publish:
    cargo publish --workspace --dry-run
