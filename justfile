# Recipes for working on Flightdeck, run from anywhere in the checkout. mise.toml pins the
# tools they run. Each CI job runs one recipe here, check-root or check-desktop, so a step
# added to either runs in CI and in just check alike.

# Arguments for the root workspace's test binaries, which CI sets on the command line to skip
# the tests its runner lacks the hardware for: just test_arguments='--skip ::this_mac::' test
test_arguments := ""

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

# The comparison must also refuse a helper whose bytes changed, so it runs once more with a
# copy of the collector that has bytes added, in a directory of its own that it removes.
# Package the app and check that the bundle carries the very executables the build made.
bundle-check: bundle
    directory="$(mktemp -d)" && trap 'rm -rf "$directory"' EXIT && \
    contents=desktop/target/Flightdeck.app/Contents && \
    shasum -a 256 desktop/target/release/flightdeck target/release/collector \
        target/release/persister | cut -c 1-64 > "$directory/built" && \
    shasum -a 256 "$contents/MacOS/flightdeck" "$contents/Helpers/collector" \
        "$contents/Helpers/persister" | cut -c 1-64 > "$directory/packaged" && \
    diff -u "$directory/built" "$directory/packaged" && \
    cp "$contents/Helpers/collector" "$directory/collector" && \
    printf changed >> "$directory/collector" && \
    shasum -a 256 "$contents/MacOS/flightdeck" "$directory/collector" \
        "$contents/Helpers/persister" | cut -c 1-64 > "$directory/tampered" && \
    ! diff -u "$directory/built" "$directory/tampered"

# The tag must name what it ships, so the tree must be clean. The next version counts from
# Info.plist's, which must be the newest tag: a run that commits and stops before the tag
# leaves Info.plist ahead of every tag, so tag that commit by hand rather than bump again.
# sed keeps the file's layout, which plutil would rewrite. The level is patch by default.
# Raise the app's version a level, commit it and tag it, for a push to release: just release minor
[positional-arguments]
release level="patch":
    test -z "$(git status --porcelain)" || { echo "Commit or stash first." >&2; exit 1; }
    case "$1" in major | minor | patch) ;; \
        *) echo "$1 is not major, minor or patch." >&2; exit 1 ;; esac
    current="$(plutil -extract CFBundleShortVersionString raw desktop/Info.plist)" && \
    newest="$(git tag --list | grep -E '^[0-9]+\.[0-9]+\.[0-9]+$' | sort -V | tail -n 1)" && \
    { [ "$newest" = "$current" ] || \
        { echo "Info.plist gives $current but the newest tag is $newest." >&2; exit 1; }; } && \
    major="${current%%.*}" && rest="${current#*.}" && minor="${rest%%.*}" && \
    patch="${rest#*.}" && \
    case "$1" in \
        major) next="$((major + 1)).0.0" ;; \
        minor) next="$major.$((minor + 1)).0" ;; \
        patch) next="$major.$minor.$((patch + 1))" ;; \
    esac && \
    key='<key>CFBundleShortVersionString</key><string>' && \
    sed -i '' "s|$key$current<|$key$next<|" desktop/Info.plist && \
    [ "$(plutil -extract CFBundleShortVersionString raw desktop/Info.plist)" = "$next" ] && \
    git add desktop/Info.plist && \
    git commit --quiet --message "set the app's version to $next" && \
    git tag "$next" && \
    echo "Tagged $next."

# Run the CI check on this Mac, with no test skipped for lack of hardware.
check: check-root check-desktop

# Check the root workspace as CI's check job does.
check-root: fmt clippy test

# The bundle-check comes last, after every other cargo run, so the bundle CI hands to a
# release is the one whose executables it compared with the build's.
# Check the desktop workspace and the bundle as CI's desktop job does.
check-desktop: desktop-fmt desktop-clippy desktop-test seal-bench bundle-check

# Check the formatting of the root workspace.
fmt:
    cargo fmt --all --check

# Lint the root workspace.
clippy:
    cargo clippy --locked --workspace --all-targets -- --deny warnings

# Test the root workspace.
test:
    cargo test --locked --workspace --no-fail-fast -- {{test_arguments}}

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
    cargo test --locked --manifest-path desktop/Cargo.toml --features automation --bin flightdeck
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

# Report the changes to the code that no test of its package notices, into target/mutants.out.
# A whole workspace takes days, so name a file: just mutants --file viewer/src/byte_display.rs
[positional-arguments]
mutants *arguments:
    cargo mutants --output target "$@"
