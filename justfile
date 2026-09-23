# Development recipes. `just pre-push` mirrors the CI gates.

# List available recipes
default:
    @just --list

# Format the workspace in place
fmt:
    cargo fmt --all

# Fail on formatting or clippy warnings, as CI does
check:
    cargo fmt --all --check
    cargo clippy --all-targets --workspace -- -D warnings

# Build and run all tests
test:
    cargo build --workspace
    cargo test --workspace

# Everything CI checks, in CI order
pre-push: check test
