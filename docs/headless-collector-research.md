# Running the collector without the app, and reading it from Grafana

Research, not a decision. Numbers were measured on this M5 Pro on 2026-10-19 unless a
source is named. The Evidence section at the end gives the command behind each one.

The ask, from 2026-10-19: the collector should "run as just a daemon that exports the
metrics", reached through "a subcommand or something", without opening the UI, and "if
that doesn't introduce any overhead, I would like to keep this". Asked about the local
sysmon stack the same day: "the application we are building here is meant to replace that
stack, but I would love for the possibility of having Grafana connected to our collector
and keep the dashboards there as they are".

## Summary

The daemon is mostly built. The collector runs headless until stopped, the app only
attaches to it or starts it, and any process of the user can read its ring. Missing:
launchd registration (TASK-23), surviving a failed source read (TASK-1), restarting a
persister that dies, which nothing does today, and the waits TASK-5 and TASK-6 describe,
sleep counted as slipped ticks and a stalled SMC call stalling ticks.

Grafana can keep its dashboards in two ways, called paths A and B below:

- **Path A: Flightdeck serves Prometheus text and VictoriaMetrics keeps scraping it.**
  The dashboards stay byte for byte as they are if the scrape renames Flightdeck's
  metrics to sysmon's names, which also keeps their 90 days of history. Flightdeck
  replaces `sysmon-exporter`'s machine and app readings, and VictoriaMetrics stays as
  Grafana's store.
- **Path B: Flightdeck answers Grafana's Prometheus queries itself.** VictoriaMetrics goes
  away for these dashboards. The work is a PromQL evaluator over the recording, a public
  query API, and rewriting the queries that use VictoriaMetrics-only functions. It depends
  on m-6's fast range reads, which do not exist yet, and its query cost is unknown and
  could exceed what VictoriaMetrics costs now.

Recommended: path A once its readings exist, measured before it replaces anything. Keep
path B until the recording's range reads are measured. Building neither keeps today's
stack, which works, and rules out Grafana reading Flightdeck and retiring
`sysmon-exporter`, which costs 0.40% of a core.

An HTTP server inside the collector, or the collector running from the GPUI binary, would
add cost or coupling and is not needed. A separate process reading the ring, as the
persister does, costs nothing when off. Its cost when on is not known: formatting one
scrape takes 0.56 ms, but reading every tick, keeping counters per App and per process
name, and serving HTTP were not measured.

Path A cannot retire the stack today. The machine dashboard reads 65 metrics, and these are
missing from Flightdeck: per-process network, open files and sockets, the GPU's I/O
Registry readings, every temperature and the thermal state, all To Do on m-1, and load
average, boot time, open files against their limit, per-process GPU time and the chip and
model the dashboard filters by, which have no card. The Claude plan limits, which a
dashboard reads from VictoriaMetrics, and the Claude Code telemetry in ClickHouse are
outside what Flightdeck records.

## What exists today

- `collector` reads the Mac once a second and hands each tick to a shared-memory ring that
  only its user can read. With no `--seconds` it runs until stopped, so it already runs
  without a window.
- The collector starts `persister` as its child, and the persister seals each minute to
  Parquet. The collector does not start it again if it exits, so after a persister crash,
  collection continues into a ring that holds two minutes and recording stops.
- The app attaches to an advancing collector or starts its own, and stops only one it
  started. The bundle ships both programs in `Contents/Helpers`.
- The ring was designed for several readers: "the live view, the persister, and any tool
  you point at it" (Moving the data, `docs/recording-research.md`). A reader maps it
  read-only, so it cannot corrupt it, and the writer never waits for a reader.
- TASK-23, on m-5, plans the collector and persister as the user's launch agent registered
  with `SMAppService`. TASK-1, keep recording when one source fails, is in Inbox, deferred
  until TASK-23 starts. Today one failed IOReport or SMC read after a successful open ends
  the run.

An always-on daemon makes its running cost permanent. On a collector the app had started,
over 332 seconds, the collector used 2.38% of a core with 50 MB resident and the persister
0.33% with 64 MB. Since 2026-10-20 the persister also derives each window's App tables as it
seals, and on 2026-10-22 it spent 0.83% of a core with 159 to 242 MB in minutes with no other
load, and about four times as much under outside load, as Cost and budget in
[collector.md](collector.md) explains. The recording research measured the collector at 37.5 and 21.7 mW over
two five-minute runs. m-5 already accepts that cost.

## How other monitors split collection from viewing

| Tool | Collector | Kept readings | Viewing | Export | Entry point |
|---|---|---|---|---|---|
| below (Meta) | `below record`, run as a systemd service | its own store | `below live`, `below replay` | `below dump` as JSON, CSV or OpenMetrics, after the fact | one binary, subcommands |
| atop | `atop -w` to a raw file, with `atopacctd` as a separate accounting daemon | daily raw files | `atop -r` | parseable text output | one binary, flags |
| sysstat | `sadc`, run by cron through `sa1` | daily binary files | `sar` | `sadf` and `sa2` reports | separate binaries |
| Performance Co-Pilot | `pmcd` routing to one agent per domain | `pmlogger` archives | any client, live or from an archive, local or remote | through clients | separate binaries |
| Netdata | one agent, with many collectors as plugin processes | its own tiered database | web server in the agent | export connectors in the agent | one daemon, features turned off by configuration |
| Glances | the same process as the view | none | terminal, `-w` web and REST, `-s`/`-c` client and server | Prometheus and other exports | one program, flags |
| macmon (Apple silicon) | the same process | none | terminal | `macmon serve`: `/metrics` Prometheus text and `/json`, 1000 ms default, `--install` adds a launchd agent | one binary, subcommands |
| Activity Monitor | `sysmond`, a root launch daemon started by a Mach request | none | the app, through `libsysmon` | none | separate |
| sysmon (this Mac) | `sysmon-exporter`, Prometheus text on 127.0.0.1:2113 | VictoriaMetrics, 90 days | Grafana | is the export | `bin/sysmon` subcommands over separate programs under launchd |

Four patterns hold across them:

1. Collection runs in a long-lived process of its own (below, atop, sysstat, PCP,
   Netdata, and `sysmond`, which launchd starts on a Mach request and which here had run
   for 6 days with no Activity Monitor open). Flightdeck already has this.
2. The live view and the history are read the same way. PCP's clients read pmcd or an
   archive alike, and below's live and replay share a viewer. Flightdeck's viewer already
   joins the ring's tail with the sealed minutes.
3. Export reads what the collector collected. Netdata, Glances and macmon run it in the
   collecting process. PCP, below and sysmon keep it in another process or step.
4. One command over separate processes is common (below, macmon, sysmon's `bin/sysmon`),
   though Glances, atop, sysstat and PCP use flags or separate binaries.

Monitors that export to Prometheus keep per-process series out by default. node_exporter
reports none, process-exporter warns that grouping by pid "is likely to result in high
cardinality metrics which Prometheus will have trouble with", and the OpenTelemetry host
metrics receiver turns its per-process scraper off by default. Prometheus' own guidance is
to keep a metric's cardinality below 10 and to look for alternatives past 100. sysmon
already breaks that guidance on purpose, with 512 to 845 series for each main per-process
metric, and VictoriaMetrics holds it at the cost below.

## The stack to replace

On this Mac, `sysmon-exporter` samples the machine and each app and serves Prometheus text,
VictoriaMetrics scrapes it every 10 seconds and keeps 90 days, and Grafana charts it. An
OpenTelemetry collector writes Claude Code's telemetry into ClickHouse beside it. Measured
after 6 days and 39 minutes running:

| Reading | Value |
|---|---|
| `sysmon-exporter` CPU | 0.40% of a core, 34 MB resident |
| VictoriaMetrics CPU | 1.15% of a core, 168 MB resident |
| Series VictoriaMetrics holds | 12,880 live, of which the apps and machine jobs hold 9,578 |
| `/metrics/apps`, one scrape | about 9,100 samples, 810 KB |
| `/metrics/machine`, one scrape | about 520 samples, 40 KB |

The VictoriaMetrics cost includes the scrapes of its own, ClickHouse's, Grafana's and the
collector's metrics, and Grafana's queries.

The Grafana dashboards query VictoriaMetrics through Grafana's Prometheus data source.
sysmon's README says Python scripts write the dashboards' JSON, so a dashboard change
belongs in the script.

- The machine dashboard holds 131 queries over 65 `sysmon_` metrics. They use `rate`,
  `sum by`, `max`, `topk`, `sort_desc`, `round`, `avg`, `time` and `ignoring`, plus two
  functions that only VictoriaMetrics' MetricsQL has, `topk_avg` (6 uses) and
  `sort_by_label_numeric` (2). Its template variables also run `label_values` and
  `query_result` queries.
- The Claude usage dashboard reads ClickHouse, and reads the plan limits,
  `claude_limit_used_percent` and `claude_limit_reset_timestamp_seconds`, from
  VictoriaMetrics, which scrapes them from `sysmon-exporter`'s `claude` collector.
- The services dashboard charts the sysmon stack itself, including
  `sysmon_sample_duration_seconds` and `sysmon_machine_reading_ok`. Flightdeck's
  Self-monitoring page covers only Flightdeck's own pipeline, so while VictoriaMetrics,
  Grafana and ClickHouse run, this dashboard still has work to do.

Flightdeck records most of the machine dashboard's readings. These are missing:

| Dashboard readings | Flightdeck |
|---|---|
| Per-process network bytes | TASK-25, To Do on m-1 |
| Per-process open files and sockets | TASK-24, To Do on m-1 |
| GPU readings from the I/O Registry | TASK-26, To Do on m-1 |
| Every SMC temperature | TASK-28, To Do on m-1 |
| Thermal state | TASK-30, To Do on m-1 |
| Load average, boot time, open files and their limit | not read, no card |
| Per-process GPU time, which `sysmon-exporter` reads from the GPU's clients | not read, no card, and the recording research found only coalition totals |
| Chip and model, which the dashboard's variables filter by | read for the Computer page, not recorded |
| Claude plan limits, Claude Code telemetry | outside the North Star, no card |

Both stacks define an app the same way, as the outermost `.app` bundle a process runs
from. sysmon's `process` label is a process name within an app, summed over its pids, and
its counters carry on after those processes exit. A series drops an hour after the last
process of its name exits. An exporter that keeps the dashboards' meaning has to keep that
ledger per process name as well as per App.

## Overhead

| Cost | Measured | Share of a core |
|---|---|---|
| Exporter off | nothing runs | 0 |
| Formatting 9,100 samples, 860 KB of Prometheus text, Rust 1.98 `-O`, mean of 200 | 0.56 ms | 0.006% at 10 s scrapes, 0.06% at 1 s |
| The persister, the nearest separate reader of the ring | 0.33%, 64 to 97 MB resident, on 2026-10-19; 0.83%, 159 to 242 MB, on 2026-10-22 once it derived App tables | continuous |
| Starting the 57 MB GPUI binary instead of the 0.8 MB collector, both exiting at once on an unknown flag | 10.8 MB against 5.9 MB peak resident, 3.5 MB against 1.9 MB peak footprint, about 170 against 30 million instructions | once, at start |

The formatting benchmark covers only writing the text. It reads no ring, keeps no counters
per App or process name, escapes no labels and writes nothing to a socket. The persister is
a comparison and not a bound: it is a 29.6 MB binary built on Polars, and the exporter has
different work. An exporter's cost needs its own measurement before it is claimed. A query
server for path B has a different cost again: the recording research measured 272 ms of
process CPU for one viewer operation over the recording, and the machine dashboard sends
131 queries every 10 seconds. Even at a tenth of that cost per query, which is arithmetic
and not a measurement, that would be about 35% of a core against VictoriaMetrics' 1.15%.
The recording research measured 53 µs a tick for a ring writer and reader with
610 records, on a prototype built before overflow checks were turned on, so today's
per-tick read cost is unmeasured. An HTTP listener with no scraper waits in `accept` and
should cost no CPU, which is inferred and was not measured.

The cost that grows sits in the store. VictoriaMetrics spends about three times what the
exporter does at 10-second scrapes. Replacing `sysmon-exporter` at the same series count
keeps that cost where it is, and scraping every second would send ten times the samples.
Flightdeck already keeps every second in its own recording, so the export does not need to
carry seconds. Counters such as CPU seconds and bytes read keep their totals exact at any
scrape interval, as Counters make slower reads lossless in total explains in the recording
research.

The 5% budget covers "collection and persistence together". Whether an exporter or a query
server counts against it is for the user to decide.

## Ways to keep the Grafana dashboards

The paths, starting with the one that removes the problem:

- **Keep no Grafana.** Flightdeck's own pages replace the dashboards. This is the
  product's direction, and it rules out the dashboards the user asked to keep.
- **Path A: Flightdeck serves Prometheus text, VictoriaMetrics scrapes it.** Grafana,
  VictoriaMetrics and the dashboards stay as they are. Flightdeck exports under its own
  `flightdeck_` names, and the scrape job in `scrape.yml` renames them to the `sysmon_`
  names with a `metric_relabel_configs` rule, a mechanism the file already uses, so the
  panels keep reading their 90 days of history. That holds as long as the exporter keeps
  sysmon's metric meanings and label names, the per-process-name ledger included. This
  removes `sysmon-exporter`'s reading of the machine and its apps. It can keep running
  with `-collectors claude` for the plan limits. VictoriaMetrics goes on holding a
  10-second copy of what Flightdeck records every second.
- **Path B: Flightdeck answers Grafana's Prometheus queries.** Grafana's Prometheus data
  source works with any project that implements the Prometheus querying API, as Mimir,
  Thanos and VictoriaMetrics do. Flightdeck would serve instant and range queries,
  series, label names and label values from its recording. It needs a PromQL parser, such
  as GreptimeDB's `promql-parser` crate (Apache 2.0, compatible with Prometheus 2.45),
  which would need a recorded case under the dependency rule, and an evaluator written
  here for the functions and template queries the dashboards use. The 8 `topk_avg` and
  `sort_by_label_numeric` queries would be rewritten in PromQL or implemented as well. A
  query server open to any local process needs limits on each query's range, series and
  time, as Tiger Style asks of everything. A panel refreshing every 10 seconds over an
  hour reads 3,600 seconds of every queried series, and a month-wide panel reads a month,
  which is m-6's TASK-32 and is not built. The query cost the Overhead section estimates
  moves from VictoriaMetrics to Flightdeck rather than going away. VictoriaMetrics itself
  goes only if the Claude plan limits and the stack's own metrics go with it.
- **A Grafana data source plugin, or SQL over the Parquet files.** Either one means
  rewriting all 131 queries in another language, so the dashboards do not stay as they
  are.

Path A delivers the ask with the least work. Path B is the one where Grafana connects to
Flightdeck alone. It shares path A's metric names and meanings, so path A's work carries
over.

### Where the exporter or query server runs

- **Inside the collector**, as Netdata, Glances and macmon do. Ruled out: a listening
  socket and request parsing would sit in the process that reads the kernel. A scrape or
  a range query would compete with the tick for that process's time, which breaks the
  rule that nothing a reader does slows collection. Tiger Style's rule that the
  collector allocates its memory at startup would also have to cover request handling.
- **A separate process reading the ring**, as the persister does. Off, it costs nothing.
  A crash or a slow client touches only itself, and its read-only mapping cannot damage
  the ring. Path B would also read the sealed minutes, as the viewer does. It costs
  another executable to build, sign and bundle, a lifecycle of its own, and an HTTP
  server, either written here for a handful of GET routes or taken from a crate with a
  recorded case.
- **Reading only the sealed minutes**, as `below dump` does. Readings arrive up to a
  minute late, which suits a batch dump and not a 10-second scrape.

Both paths fit the separate process.

Pull or push: VictoriaMetrics and Prometheus pull Prometheus text, which is what this
stack reads. Pushing OTLP to the OpenTelemetry collector would add a client, a queue and
retries to the exporter for no consumer that needs them.

### The entry point

The app's executable is already named `flightdeck` and already runs without a window for
`--bundle`, `--check` and `--live-soak`.

- **Subcommands on the app binary that start the helpers**, such as `flightdeck collect`
  and `flightdeck export`, each executing the program in `Contents/Helpers`. The app
  binary's start cost is paid once, and each daemon then runs at its helper's size. The
  launch agents name the helpers directly, so launchd never starts the GUI binary.
  Tailscale ships its macOS command line this way, as the app binary run with arguments.
- **The app binary as the daemon itself.** Each daemon would keep the measured extra 5 MB
  resident and 1.6 MB footprint for its whole life, and would load 20 libraries, 15 of
  them frameworks, against the collector's 4. The cost is small. The coupling decides
  against it: the daemon would run the UI's code and dependencies to do none of the UI's
  work.
- **Separate binaries with no shared command**, as sysstat and PCP ship. The least work,
  and several names to learn.

Subcommands that start the helpers give the one command asked for with no lasting cost.

## Coupling and risks

- **The metric schema becomes a contract.** Dashboards depend on names, labels and
  meanings. The collector's publication and ring code was first committed on 2026-10-15
  and has changed in 60 commits since, so by the one-year stability test in the coupling
  rules it is not stable. The exporter should own its schema and translate the ring into
  it, so that a change inside the collector changes the translation and not the
  dashboards.
- **A third implementation of App totals.** The collector records each process's App
  identity, the viewer crate builds App views, and part of the App attribution lives in
  the desktop workspace. An exporter that sums Apps on its own would answer the same
  question a third way, and the numbers would drift. It should take App and
  process-name totals from the core's code, which may mean moving the desktop's part into
  the viewer crate first.
- **Two readers of the same Mac.** While `sysmon-exporter` and the collector both read the
  machine, the Mac pays for both and their numbers drift apart. Path A removes that once
  the missing readings above exist.
- **Counters that only rise.** An App's CPU seconds summed over its running processes fall
  when one of them exits, which `rate` reads as a counter reset. The exporter has to add
  each exited process's final usage, which the collector already reads, per App and per
  process name. Restarting the exporter resets them, so it should export its start time.
- **Peaks between scrapes.** A 10-second scrape keeps counters exact but misses the peaks
  of gauges such as resident memory and footprint, as the recording research notes for
  any slower read. Flightdeck's own pages keep every second.
- **Who can read it.** The ring is readable only by its user. A TCP listener on 127.0.0.1
  answers any account and any process on the Mac, and a web page can reach it through DNS
  rebinding unless the server checks the Host header. App names reveal what the user is
  doing. In path A the readings then sit in VictoriaMetrics, which here answered a
  request carrying a foreign Host header and sent `Access-Control-Allow-Origin: *`, so
  hardening the exporter alone protects nothing. Either the store gets the same
  hardening, or the exposure is accepted as it is today with `sysmon-exporter`. Path B
  puts the one server to harden in Flightdeck. Either way the export carries no command
  lines, and serving other machines is a separate feature.
- **Always-on duties.** A permanent daemon needs TASK-1, so one failed read does not stop
  collection, a persister that is restarted when it exits, TASK-5, so the Mac's sleep is
  not counted as slipped ticks, and TASK-6, so a stalled SMC call does not stall ticks.
  `SMAppService` registration can leave the agent waiting for the user's approval in
  System Settings, which TASK-23 lists as uncertain.
- **Version skew.** An app update can replace the helpers while an older collector keeps
  writing its ring. The ring's layout has had versions 2, 3 and 4, and readers already
  refuse a version outside that range and a record width that does not match. What is
  left is for an update to restart the agents, so an old daemon does not outlive its
  bundle.
- **Restarts.** Each collector start writes a new ring. The exporter has to follow the lock
  file to the new ring and report the gap.

## Open questions

1. Is keeping VictoriaMetrics acceptable for now, so path A comes first and path B waits
   for m-6's range reads to be measured? Path B is the larger build, and its query cost is
   unknown.
2. Does an exporter or query server count against the 5% budget, which covers collection
   and persistence today?
3. Should the Claude plan limits and telemetry move into Flightdeck, or stay in a smaller
   sysmon? Retiring the stack depends on it.
4. Should the readings the dashboards use and Flightdeck lacks, from load average to
   per-process GPU time, become cards? Path A needs them to keep every panel.

## Evidence

Run on 2026-10-19 on this M5 Pro.

- Process costs: `ps -o pid,etime,time,rss,command -p PID`, CPU time over elapsed time.
  The collector and persister figures come from a pair the app started with
  `--start-collector`, read after 332 seconds.
- Series: `curl -s 'http://127.0.0.1:8428/api/v1/query?query=count by (job)({__name__=~".+"})'`.
- Scrapes: `curl -s http://127.0.0.1:2113/metrics/apps`, counting lines without `#` and bytes.
- Dashboards: the queries under `expr` in sysmon's `grafana/dashboards/*.json`, with the
  function names and `sysmon_` metric names counted.
- Start cost: `/usr/bin/time -l BINARY --no-such-flag`, three runs each, against
  `target/release/collector` and `desktop/target/release/flightdeck`, and `otool -L` on
  both.
- Schema changes: `git log --since='1 year ago' --oneline -- collector/src/publication
  collector/src/publication.rs collector/src/ring`, and the same with `--reverse` for the
  first commit.
- Store exposure: `curl -H 'Host: attacker.example'` and `curl -H 'Origin: ...'` against
  `http://127.0.0.1:8428/api/v1/label/app/values`, which returned 200 and
  `Access-Control-Allow-Origin: *`.
- Missing readings: the `sysmon_` names in the machine dashboard against `grep` over
  `collector/src`, `persistence/src` and `viewer/src`.
- sysmond: `launchctl print system/com.apple.sysmond` shows a running launch daemon whose
  start reason is `ipc (mach)`, `otool -L` on Activity Monitor lists
  `/usr/lib/libsysmon.dylib`, and `top` links no such library.
- Formatting, built with `rustc +1.98.0 -O`:

```rust
use std::fmt::Write;
use std::time::Instant;

fn main() {
    let names: Vec<String> = (0..700).map(|i| format!("Process Name {i}")).collect();
    let metrics = ["cpu_seconds_total", "resident_bytes", "energy_joules_total",
        "disk_read_bytes_total", "disk_written_bytes_total", "idle_wakeups_total",
        "pageins_total", "threads", "footprint_bytes", "open_files", "gpu_seconds_total",
        "count", "open_descriptors"];
    let mut out = String::with_capacity(1 << 20);
    let rounds = 200;
    let mut total = 0u128;
    for r in 0..rounds {
        out.clear();
        let start = Instant::now();
        for m in metrics {
            writeln!(out, "# TYPE flightdeck_process_{m} gauge").unwrap();
            for (i, n) in names.iter().enumerate() {
                writeln!(out, "flightdeck_process_{m}{{app=\"{n}\",process=\"{n}\"}} {}",
                    (i * 31 + r) as f64 * 0.37).unwrap();
            }
        }
        total += start.elapsed().as_nanos();
    }
    println!("bytes {} mean {} us", out.len(), total / rounds as u128 / 1000);
}
```

## Sources

- below: https://github.com/facebookincubator/below
- atop: https://manpath.be/f34/1/atop
- sysstat `sadc`: https://www.mankier.com/8/sadc
- Performance Co-Pilot: https://man.archlinux.org/man/extra/pcp/PCPIntro.1.en
- Netdata: https://learn.netdata.cloud/docs/getting-started/glossary
- Glances: https://pypi.org/project/Glances/
- macmon: https://docs.rs/crate/macmon/0.8.2/source/readme.md
- process-exporter: https://github.com/ncabatoff/process-exporter
- OpenTelemetry host metrics defaults: https://uptrace.dev/opentelemetry/collector/host-metrics
- Prometheus instrumentation practices: https://prometheus.io/docs/practices/instrumentation/
- Prometheus naming and labels: https://prometheus.io/docs/practices/naming/
- Grafana's Prometheus data source: https://grafana.com/docs/grafana/latest/datasources/prometheus/
- `promql-parser`: https://github.com/greptimeteam/promql-parser
- `SMAppService`: https://developer.apple.com/documentation/servicemanagement/smappservice
- Tailscale CLI on macOS: https://tailscale.com/kb/1080/cli?tab=macos
