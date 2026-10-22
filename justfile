# Recipes for working on Flightdeck, run from anywhere in the checkout. mise.toml pins the
# tools they run. The CI check runs these same recipes, so a step added here runs there too.

# List the recipes.
default:
    @just --list

# Opening brings a running Flightdeck forward instead of the new build, so quit it first.

# Build, package and open the desktop app.
dev: bundle
    open desktop/target/Flightdeck.app

# The bundle must carry the very executables the build made, so this compares their digests.
# Build the app with its collector and persister helpers, and package them as one bundle.
bundle:
    cargo build --locked --release -p collector -p persister --bin collector --bin persister
    cargo build --locked --release --manifest-path desktop/Cargo.toml --bin flightdeck
    shasum -a 256 desktop/target/release/flightdeck target/release/collector \
        target/release/persister > desktop/target/bundle-built.sha256
    desktop/target/release/flightdeck --bundle
    desktop/target/Flightdeck.app/Contents/MacOS/flightdeck --bundle --check
    shasum -a 256 desktop/target/Flightdeck.app/Contents/MacOS/flightdeck \
        desktop/target/Flightdeck.app/Contents/Helpers/collector \
        desktop/target/Flightdeck.app/Contents/Helpers/persister \
        > desktop/target/bundle-packaged.sha256
    cat desktop/target/bundle-built.sha256 desktop/target/bundle-packaged.sha256
    cut -c 1-64 desktop/target/bundle-built.sha256 > desktop/target/bundle-built-digests.sha256
    cut -c 1-64 desktop/target/bundle-packaged.sha256 \
        > desktop/target/bundle-packaged-digests.sha256
    diff -u desktop/target/bundle-built-digests.sha256 \
        desktop/target/bundle-packaged-digests.sha256
    # The comparison must refuse a bundle whose helper bytes changed, so change a copy's.
    rm -rf "desktop/target/Flightdeck tampered.app"
    cp -R desktop/target/Flightdeck.app "desktop/target/Flightdeck tampered.app"
    printf changed >> "desktop/target/Flightdeck tampered.app/Contents/Helpers/collector"
    shasum -a 256 "desktop/target/Flightdeck tampered.app/Contents/MacOS/flightdeck" \
        "desktop/target/Flightdeck tampered.app/Contents/Helpers/collector" \
        "desktop/target/Flightdeck tampered.app/Contents/Helpers/persister" \
        > desktop/target/bundle-tampered.sha256
    cut -c 1-64 desktop/target/bundle-tampered.sha256 \
        > desktop/target/bundle-tampered-digests.sha256
    ! diff -u desktop/target/bundle-built-digests.sha256 \
        desktop/target/bundle-tampered-digests.sha256

# Run the steps of the CI check on this Mac, with no test skipped for lack of hardware.
check: fmt clippy test desktop-fmt desktop-clippy desktop-test bundle seal-bench

# Check the formatting of the root workspace.
fmt:
    cargo fmt --all --check

# Lint the root workspace.
clippy:
    cargo clippy --locked --workspace --all-targets -- --deny warnings

# Test the root workspace, passing any arguments on to the test binaries.
[positional-arguments]
test *arguments:
    cargo test --locked --workspace --no-fail-fast -- "$@"

# Check the formatting of the desktop workspace.
desktop-fmt:
    cargo fmt --manifest-path desktop/Cargo.toml --check

# The control socket builds only with the automation feature, so lint with it as well.
# Lint the desktop workspace.
desktop-clippy:
    cargo clippy --locked --manifest-path desktop/Cargo.toml --all-targets -- --deny warnings
    cargo clippy --locked --manifest-path desktop/Cargo.toml --all-targets --features automation \
        -- --deny warnings

# Test the desktop workspace, then run the desktop app's own checks.
desktop-test:
    cargo test --locked --manifest-path desktop/Cargo.toml
    cargo run --locked --manifest-path desktop/Cargo.toml --bin flightdeck -- --check
    cargo run --locked --manifest-path desktop/Cargo.toml --bin text_backend_check -- --check

# It shows what a change to sealing costs, and fails only when the run does, never on a
# number, since a shared machine's numbers vary.
# Seal six generated minutes through the persister's worker and print each window's CPU time.
seal-bench:
    cargo run --locked --release -p persister --bin seal_bench -- \
        --windows 6 --processes 1050 --derive apps

# Report the lines the tests of each workspace run, as HTML under its target/llvm-cov/html.
coverage:
    cargo llvm-cov --locked --workspace --html
    cargo llvm-cov --locked --manifest-path desktop/Cargo.toml --html

# A whole workspace takes days, so name a file: just mutants --file viewer/src/byte_display.rs
# Report the changes to the code that no test of its package notices, into target/mutants.out.
[positional-arguments]
mutants *arguments:
    cargo mutants --output target "$@"
