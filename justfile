# Recipes for working on Flightdeck, run from anywhere in the checkout. mise.toml pins the
# tools they run.

# List the recipes.
default:
    @just --list

# Opening brings a running Flightdeck forward instead of the new build, so quit it first.

# Build, package and open the desktop app.
dev: bundle
    open desktop/target/Flightdeck.app

# Build the app with its collector and persister helpers, and package them as one bundle.
bundle:
    cargo build --locked --release -p collector -p persister --bin collector --bin persister
    cargo build --locked --release --manifest-path desktop/Cargo.toml --bin flightdeck
    desktop/target/release/flightdeck --bundle
    desktop/target/Flightdeck.app/Contents/MacOS/flightdeck --bundle --check

# Run the steps of the CI check on this Mac, with no test skipped for lack of hardware.
check: fmt clippy test bundle

# Check the formatting of both workspaces.
fmt:
    cargo fmt --all --check
    cargo fmt --manifest-path desktop/Cargo.toml --check

# Lint both workspaces, and the desktop app again with the control socket built in.
clippy:
    cargo clippy --locked --workspace --all-targets -- --deny warnings
    cargo clippy --locked --manifest-path desktop/Cargo.toml --all-targets -- --deny warnings
    cargo clippy --locked --manifest-path desktop/Cargo.toml --all-targets --features automation \
        -- --deny warnings

# Test both workspaces, then run the desktop app's own checks.
test:
    cargo test --locked --workspace
    cargo test --locked --manifest-path desktop/Cargo.toml
    cargo run --locked --manifest-path desktop/Cargo.toml --bin flightdeck -- --check
    cargo run --locked --manifest-path desktop/Cargo.toml --bin text_backend_check -- --check

# Report the lines the tests of each workspace run, as HTML under its target/llvm-cov/html.
coverage:
    cargo llvm-cov --locked --workspace --html
    cargo llvm-cov --locked --manifest-path desktop/Cargo.toml --html

# Report the changes to the code that no test of its package notices, into target/mutants.out.
# A whole workspace takes days, so name a file: just mutants --file viewer/src/byte_display.rs
[positional-arguments]
mutants *arguments:
    cargo mutants --output target "$@"
