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
