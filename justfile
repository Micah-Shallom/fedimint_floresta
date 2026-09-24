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
    cargo clippy --locked --all-targets --workspace -- -D warnings

# Build and run all tests
test:
    cargo build --locked --workspace
    cargo test --locked --workspace

# Fail on declared-but-unused dependencies (one-time: cargo install cargo-machete)
machete:
    cargo machete

# Everything CI checks, in CI order
pre-push: check test machete
