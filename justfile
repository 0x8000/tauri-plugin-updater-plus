set shell := ["bash", "-c"]

mod bdd "./tests/bdd"

alias test-bdd := bdd::test
alias test-one := bdd::test-one

# Run all unit tests across feature combinations
test:
    cargo test
    cargo test --no-default-features --features cli
    cargo test --all-features

# Run pre-commit style checks and clippy lints
lint:
    pre-commit run -a
    cargo clippy --all-targets --all-features -- -D warnings
    cargo clippy --no-default-features --features cli --bin blockmap-gen -- -D warnings

# Verify crates.io packaging
check-publish:
    cargo publish --workspace --dry-run
