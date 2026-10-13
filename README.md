# Flightdeck

A macOS system monitor in Rust and GPUI that records the whole machine once a second,
keeps the recording, and lets you go back to any second and see what every process,
file and socket was doing. The design research is in
[docs/recording-research.md](docs/recording-research.md).

## Collector

`collector` reads the whole machine once a second without root and reports what the
reading costs: its CPU as a share of one core against the 5% budget, its power, and
the CPU of each source.

```sh
cargo run --release -p collector -- --seconds 300
```

It reports every 10 seconds and every minute, and with `--seconds` over the whole run,
ending with the processes it missed, counted against the pid sequence. It gives no count
when the pids may have wrapped, when it found more processes than the pids say started,
or when a marker child it starts for the count fails. By default it rescans the pid
list after every wake that carries a fork. `--rescan-ms N` rescans at most once every N ms
instead, which costs less in bursts of forks and misses more of the processes that start
and exit between two rescans.

To measure process tracking under a load you choose, `forkrate` forks children at a
fixed rate, `followprobe` counts the children the tracker finds and misses at a given
rescan interval, and `collector/scripts/burst-minute.sh` runs both inside one collector
minute.

```sh
cargo build --release
./target/release/followprobe 5 1000 30 6 1   # RESCAN_MS PER_SECOND SECONDS CHILD_MS GENERATORS
```

## Check

New and changed code here follows Tiger Style, as [docs/tiger-style.md](docs/tiger-style.md)
reads it into Rust. The collector's older code does not follow it yet.

GitHub Actions holds every push to `main` and every pull request to the check that page
describes, with the steps and the toolchain `.github/workflows/check.yml` names. The
repository installs no git hook, so nothing runs on your machine unless you run it. To run
its steps before you push:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- --deny warnings
cargo test --locked --workspace
```
