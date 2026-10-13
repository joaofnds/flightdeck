# Flightdeck

A macOS system monitor in Rust and GPUI that records the whole machine once a second,
keeps the recording, and lets you go back to any second and see what every process,
file and socket was doing. The design research is in
[docs/recording-research.md](docs/recording-research.md).

## Collector

`collector` reads the whole machine once a second without root and reports what the
reading costs: its CPU as a share of one core against the 3% budget, its power, and
the CPU of each source.

```sh
cargo run --release -p collector -- --seconds 300
```

It reports every 10 seconds, every minute and over the run. With `--seconds` it ends with
the processes it missed, counted against the pid sequence, unless the pids may have
wrapped over the run. `--rescan-ms N` sets how often a fork may make it rescan the pid list, 5 ms
by default.

To measure process tracking under a load you choose, `forkrate` forks children at a
fixed rate, `followprobe` counts the children the tracker finds and misses at a given
rescan interval, and `collector/scripts/burst-minute.sh` runs both inside one collector
minute.

```sh
cargo build --release
./target/release/followprobe 5 1000 30 6 1   # RESCAN_MS PER_SECOND SECONDS CHILD_MS GENERATORS
```

The check is `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.
