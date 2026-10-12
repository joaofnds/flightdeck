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

The check is `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.
