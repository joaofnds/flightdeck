# Flightdeck

A macOS system monitor in Rust and GPUI that records the whole machine once a second,
keeps the recording, and lets you go back to any second and see what every process,
file and socket was doing. The design research is in
[docs/recording-research.md](docs/recording-research.md).

## Collector

`collector` reads the whole machine once a second without root and starts a separate
persister. It reports each source's CPU and the combined collector and persister CPU
against the 5% budget for one core. Build both sibling executables before running it:

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
`--recording PATH` selects an isolated recording destination independently of `--ring`. The persister
keeps immutable Parquet windows of 60 elapsed seconds, with shorter final windows and
explicit availability and gaps. Stopping and restarting continues the recording.

The ring stays after the collector exits, about 800 MB of address space, and the recording
registry retains at most the active and one earlier ring. Disk recordings survive the
rings' removal or a reboot; unsealed data may be lost. A missing, refused or failed
persister reports failure while collection continues into the bounded ring. The collector
starts it once at startup and does not respawn it during collection. Finite runs drain
and reap their child, and report the drain's CPU separately. Recording files and pending
artifacts are private to the user.

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
```
