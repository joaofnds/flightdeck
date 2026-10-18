# Verification records

Local checks used Rust 1.95.0 on macOS. Commands ran from the repository root:

```sh
cargo +1.95.0 fmt --manifest-path gpui-prototype/Cargo.toml --check
cargo +1.95.0 clippy --locked --manifest-path gpui-prototype/Cargo.toml --all-targets -- --deny warnings
cargo +1.95.0 test --locked --manifest-path gpui-prototype/Cargo.toml
cargo +1.95.0 build --locked --release --manifest-path gpui-prototype/Cargo.toml --bins
gpui-prototype/target/release/flightdeck-gpui --check
gpui-prototype/target/release/text_backend_check --check
gpui-prototype/target/release/flightdeck-gpui --reference --smoke --seconds 5
```

The test log contains 117 successful tests across four binaries, including shared
helper tests. Format, clippy, optimized build, capture check and font check passed.
The optimized Swift baseline built with Swift 6 and warnings as errors; its final
typecheck-only build also passed. The Swift build/typecheck manifests identify
the helper hash and archived desktop revision.

`text-backend-red-*` and `text-backend-green-*` retain the failing placeholder
metrics and passing real-font metrics. `evaluation-paint-smoke*` records populated
chart canvases and all captured routes, rather than actual screen pixels.

`evaluation-live-ui*` records a 75-second CPU-window run with its own collector and
an external UI-process observer. `evaluation-live-settled-probe*` records backend
selection/identity checks against that recording, including 18 logical CPUs with
an available interval. The probe ran during the UI launch, so its process resource
samples are not isolated live efficiency evidence. The UI accepted 76 updates but
rendered only four times; visible live performance remains unverified.

`evaluation-swift-scroll-probe*` establishes the direct lazy-section scroll target
and shows the shell's original inactive animation state. The later
`evaluation-swift-activity-probe*` shows the actual activation bridge enabling
animation, then suspension when the assembly leaves the viewport. These probes
are neither repeated comparative resource runs nor presentation measurements.

`idle-profile-*` preserves the quiet GPUI observation cited by EVALUATION.md.
Hardware/thermal text records are system queries. Display conditions are described
in EVALUATION.md; hardware identifiers from the display query are not retained.

Native visual/input testing was authorized by the user. The final GPUI access
attempt returned `Computer Use was not approved to use Flightdeck GPUI`; the Swift
attempt returned `Sky Computer Use native pipe closed before response`. No screen
capture or UI interaction through another mechanism bypassed those failures.
