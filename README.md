# Flightdeck

A macOS system monitor with a SwiftUI/AppKit client and a Rust recording core. It records
the whole machine once a second, keeps the recording, and lets you go back to any second and see what every process,
file and socket was doing. The design research is in
[docs/recording-research.md](docs/recording-research.md).

## Collector

`collector` reads the whole machine once a second without root and starts a separate
persister. Each `recording minute` reports `combined_percent` of one core for collection
and persistence together; compare it with the 5% budget. The source-cost report's
budget share and power in mW describe the collector alone. Build both sibling executables:

```sh
cargo build --release -p collector -p persistence --bin collector --bin persister
./target/release/collector --seconds 300
```

It reports every 10 seconds and every minute, and with `--seconds` over the whole run,
ending with the processes it missed, counted against the pid sequence. It gives no count
when the pids may have wrapped, when it found more processes than the pids say started,
or when a marker child it starts for the count fails. By default it rescans the pid
list after every wake that carries a fork. `--rescan-ms N` rescans at most once every N ms
instead, which costs less in bursts of forks and misses more of the processes that start
and exit between two rescans.

It hands each tick to a shared-memory ring that only its user can read. `--ring NAME`
(default `flightdeck`) names the lock file that holds the ring's name,
`~/Library/Application Support/Flightdeck/NAME.lock`, and the collector exits with an error
while another collector holds that lock. The home directory is resolved from the user's
uid. The default private recording is beside that lock at `NAME.recording`;
`--recording PATH` selects an isolated recording destination independently of `--ring`.
One collector owns that recording at a time. A second collector using another ring and
the same recording continues collecting but cannot persist its readings there.
Choose a UTF-8 recording path containing none of `*`, `?` or `[`. Creation accepts other
paths, but the query API and checker refuse them because the engine interprets them as
file patterns. The persister keeps immutable Parquet windows of 60 elapsed seconds,
with shorter final windows and
explicit availability and gaps. Stopping and restarting continues the recording.

The ring stays after the collector exits, about 800 MB of address space, and the recording
registry retains at most the active and one earlier ring. Disk recordings survive the
rings' removal or a reboot; unsealed data may be lost. A missing, refused or failed
persister reports failure while collection continues into the bounded ring. The collector
starts it once at startup and does not respawn it during collection. Finite runs, Ctrl-C
and SIGTERM drain and reap their child, and report the drain's CPU separately. If an
earlier persister still owns the recording, its replacement reports that ownership and
unavailable CPU while collection continues. Recording directories are created with
owner-only access; files use mode 0600 and inherited access-control entries are removed.

The diagnostic query API is `persistence::query::processes_at_second`. The native viewer
uses `viewer::Session` for complete saved-tick inspection. `recording_check` compares a live
source with the saved recording across all ten tables. Build it with
`cargo build --release -p persistence --bin recording_check`.
Start `./target/release/recording_check capture --lock-path PATH --reference FILE --seconds N`
before the collector, using a fresh reference file and
a duration longer than the collection run. Once both finish, run
`./target/release/recording_check verify --recording PATH --reference FILE`.
The reference contains private readings too.

## Native desktop app

With Xcode's command-line tools and Rust installed, build the SwiftUI/AppKit application:

```sh
cargo run --locked -p viewer --bin flightdeck -- --build
open target/Flightdeck.app
```

Opening the app without arguments shows the captured prototype reference: eleven native
views, shared time inspection, app/process drilldowns and the animated MacBook schematic.
This mode uses bundled historical samples, clearly labelled as captured reference data.
It does not describe the current Mac.

Choose **Flightdeck recording** in the data-source picker to inspect an actual recording.
`--run --recording PATH` opens that mode directly. Its default directory is the current
user's `Library/Application Support/Flightdeck/flightdeck.recording`. The viewer opens
existing data read-only and never starts a collector or repairs history. Open a recording
from the File menu. Refresh discovers newly sealed windows; **Live** follows the newest
shared-memory ticks registered for that recording, including its unsealed tail. Pausing
live following retains the tail for inspection.

Recording mode currently provides Overview, CPU/process inspection, Memory and all ten recorded
tables, with 512-row pages and an exact-field row inspector. Memory charts show independent
recorded VM categories and swap, preserving missing observations and boundaries between collector runs.
Its charts cover the 15 minutes
ending at the selected second. Enter a Unix second, step one second or inspect a chart.
Repeated seconds retain separate run/publication alternatives. A missing second clears
readings. Names and app identities are not yet in the recording schema, so this mode shows
recorded PIDs and creation identities. The other reference views still need recording-backed
sources; the captured reference is a presentation comparison, not full functional parity.

The app uses native controls and drawing with no browser runtime. Theme tokens are shared
across the dark UI and charts. The build uses optimized Swift and Rust, is
signed locally for development, and is not a notarized distribution. No build step installs
a toolchain or changes local hooks.

The builder’s `--check` also runs native model, decoder and table checks; CI runs that same
command. Optional measurement tools report query wall/CPU time and process physical
footprint separately from resident memory:

```sh
cargo run --locked --release -p viewer --bin query_bench -- \
  --recording PATH --iterations 100
cargo run --locked --release -p viewer --bin process_watch -- --pid PID --seconds 60
```

Record the app binary, recording/window/row counts, selected seconds, visibility and
interaction state with each measurement. `query_bench` reads the 900-second context and
first page of each table at up to 30 recent seconds; it labels missing-second iterations
separately. `--second N` selects the ending second. These are opt-in observations, not a
whole-process memory limit or an automatic application-performance budget.

## Recording inspection tools

`ring_reader` prints each tick it reads, the events, and what it missed, until it has read
the publications asked for. It finds the ring through `--ring NAME`, or through
`--lock PATH` for a lock file elsewhere:

```sh
cargo build --release -p collector
./target/release/ring_reader --ring flightdeck --publications 60
```

To measure process tracking under a load you choose, `forkrate` forks children at a
fixed rate, `followprobe` counts the children the tracker finds and misses at a given
rescan interval, and `burst_minute` runs `followprobe` 15 seconds into one minute of the
collector, with a 30-second burst of forks from two generators whose children live 6 ms.
Its `--rescan-ms` sets only followprobe's interval, so give the collector its own after
`--`. It prints the date, the load average and the power source before both reports.

```sh
cargo build --release
./target/release/followprobe 5 1000 30 6 1   # RESCAN_MS PER_SECOND SECONDS CHILD_MS GENERATORS
./target/release/burst_minute --rescan-ms 5 --per-second 1000 -- \
  ./target/release/collector --rescan-ms 5
```

## Check

New and changed code here follows Tiger Style, as [docs/tiger-style.md](docs/tiger-style.md)
reads it into Rust. The collector's older code does not follow it yet.

GitHub Actions runs the check that page describes on every push to `main` and every pull
request, with the steps and the toolchain `.github/workflows/check.yml` names. The
repository installs no git hook, so nothing runs on your machine unless you run it. A clone
that set `core.hooksPath` for the hook this repository once carried can drop it with
`git config --unset core.hooksPath`. To get the check's verdict before you push, run its
steps under that toolchain:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- --deny warnings
cargo test --locked --workspace
cargo run --locked -p viewer --bin flightdeck -- --check
```
