# Flightdeck

A macOS system monitor with a Rust/GPUI client and a Rust recording core, meant to be
the only monitor the Mac needs. It records process and system readings once a second and
keeps them for later inspection. The aim is every reading of the machine, its processes
and apps, its devices and its events, charts that explore any of them over any range, and
a way to see which processes, apps and events changed with a spike, a lag or a power
draw. Open files, sockets, per-process network, devices, inventory and most events are
not recorded yet, and neither exploration nor that view exists. The design research is in
[docs/recording-research.md](docs/recording-research.md). Running the collector without
the app and reading it from Grafana are weighed in
[docs/headless-collector-research.md](docs/headless-collector-research.md).

## Collector

`collector` reads process and system counters once a second without root and starts a separate
persister. [How the collector works](docs/collector.md) describes its parts, one tick, and why
each part is built the way it is. Each `recording minute` reports `combined_percent` of one
core for collection and persistence together; compare it with the 5% budget. The source-cost
report's budget share and power in mW describe the collector alone. Build both sibling
executables:

```sh
cargo build --release -p collector -p persister --bin collector --bin persister
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

`mise.toml` pins the tools the repository uses, Rust 1.98.0 as CI does and `just`. With
Xcode's command-line tools and [mise](https://mise.jdx.dev) installed, trust and install
them once, then build, package and open the GPUI application:

```sh
mise trust
mise install
just dev
```

`just bundle` builds and packages without opening, and `just --list` shows each recipe.
Mise acts only for a contributor who trusts the file, and without it the recipes in
`justfile` run as they are under Rust 1.98.0.

The movable bundle contains the app, collector, persister and schematic artwork.
Default launch follows the last fifteen minutes of live readings, attaching to an
existing collector or starting its own when collection is absent. Only a collector
started by the app is stopped when it quits. `--recording PATH` opens a recording
read-only; `--reference` opens captured prototype data.

The UI uses GPUI's Metal rendering without a browser runtime. Its separate Cargo
workspace and lockfile keep UI dependencies apart from the recording core.
[Desktop usage and architecture](desktop/README.md) describes the resource pages,
source controls, bounded queries, development checks and performance tools.
[Verification and remaining native checks](desktop/LIVE.md) distinguish automated
checks from interactive macOS observations. Historical framework measurements and
the current toolkit decision remain in [the evaluation](desktop/EVALUATION.md).

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
request, under the toolchain `.github/workflows/check.yml` names. Its check job runs
`just check-root` and its desktop job `just check-desktop`, the two recipes `just check`
runs, so a step added to either runs in CI and on your machine alike. `check-root` formats,
lints and tests the root workspace through `fmt`, `clippy` and `test`. `check-desktop` does
the same for the desktop workspace through the recipes named `desktop-`, then runs
`bundle-check`, which packages the app and compares the bundle's executables with the ones
the build made, and `seal-bench`, which prints what sealing a minute costs.

CI adds only what its runner needs. It skips the tests in modules named `this_mac`, since
its virtual Mac lacks the hardware they read, and runs the ignored tests that need a second
user, which its sudo gives without a password. After each job it refuses a ring lock file
left in Application Support, which on your Mac a running Flightdeck holds. The repository
installs no git hook, so nothing runs on your machine unless you run it. A clone that set
`core.hooksPath` for the hook this repository once carried can drop it with
`git config --unset core.hooksPath`. To get the check's verdict before you push, run it
under that toolchain:

```sh
just check
```

`just coverage` reports the lines the tests run, with the cargo-llvm-cov that `mise.toml`
pins. `just mutants --file PATH` reports the changes to that file that no test of its
package notices, with the cargo-mutants `mise.toml` pins. The collector's process tests sit
in the persister package, beside the persister they start, so for a file of the collector
add `--cargo-test-arg=--package=persister`, which runs both packages' tests on the
unchanged code and on each change and about doubles each change's test time. A change that
keeps the collector running past the tests' time limit shows as TIMEOUT rather than caught.
