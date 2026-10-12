# Recording a Mac every second

Research for a Rust and GPUI app that records the whole machine once a second, keeps
the recording, and lets you go back to any second and join what every process, file
and socket was doing. Every number below was measured on this M5 Pro on 2026-10-12
unless it cites a source. The evidence programs are listed at the end.

## Decisions

- The whole machine is read at most once a second. The measurements go down to
  100 ms to show the headroom, and a later mode may read one process tree at 100 ms.
- The collector gets 3% of one core.
- The collector runs without root. Root may come later as an option the user turns on
  in the app.
- A kernel panic or a power loss may lose the minute not yet sealed.

Without root and once a second, the collector in Rust measured 2.3% of a core over five
minutes of this Mac's use, inside the budget. It stays at 1.0 to 2.6% while processes
fork under 200 times a second and goes over the budget, up to 8.6%, in bursts of 300 to
1,600 forks a second, because it rescans the pid list on every fork. A kqueue watch on
every process sees nearly every process start and exit without root, which polling
cannot. Root would add the usage and open files of the third of processes owned by
other users, and file and socket events as they happen.

## Reading the kernel is what costs CPU, not moving the data

| Step | CPU |
|---|---|
| `proc_pid_rusage` and task info for the 665 to 710 processes readable without root, paced every 100 ms or once a second at the default quality of service | 6.0 to 8.3 ms and 2.3 mJ a pass, 9 to 12 µs a process. At either pace, 4.8 to 5.2 ms and 4.3 mJ with half its instructions on Super cores, and 2.8 to 3.2 ms and 8.1 to 9.3 mJ with 78 to 95% on them |
| The same calls in a tight loop, for comparison | 1.7 µs a process, which a loop that sleeps between passes does not reach |
| CPU ticks of every core, memory, network interfaces, the pid list, in C, tight loop | 3.2 µs, 0.4 µs, 25 µs, 8.8 µs |
| Usage of all 679 coalitions, in C, tight loop | 1.0 ms |
| Listing each process's open files | 1.4 ms a pass |
| Resolving the 7,462 open files to a path or socket | 8 to 20 ms a pass |
| The SMC: 268 temperature keys and 2 fans, in the Go collector | 6.0 ms CPU, 52 ms wall |
| One SMC key, paced every 100 ms | 33 µs CPU, 272 µs wall |
| IOReport: CPU and GPU frequency states and GPU energy | 5.9 ms in Go. In Rust, paced, 4.5 ms for the 18 cores' states, 0.5 ms for the GPU's, 0.65 ms for its energy |
| GPU from the I/O Registry, in Go | 2.2 ms |
| Watching every process's fork, exec and exit with kqueue, rescanning the pid list on each fork | 0.13% of a core at 7 process starts a second, 0.32% at 34. In the collector, 44 µs a fork at 1,600 forks a second, 60 to 64 µs over a whole run |
| Running `nettop` once for per-process network bytes | about 30 ms CPU, 5 s wall |
| Handing a tick of 610 records (9.8 KB) to another process through a shared-memory ring, paced at 10 ticks a second, the reader woken once a tick | 33 µs writer, 20 µs reader, 10 µJ together, per tick |
| The same through a Unix socket | 50 µs writer, 30 µs reader, 12 µJ together |
| The ring with a reader that looks at each 60 Hz screen frame instead of being woken | 14 µs writer, 52 µs reader, 13 µJ together, 7.3 reader wakeups a tick |

Reading every process once a second costs 0.3 to 0.8% of one core and 2.3 to 9.3 mW.
The more of a pass runs on Super cores, the less CPU time and the more energy it takes.
The same at 100 ms costs 3 to 8%, and reading every source above at 100 ms, open files
and the SMC included, adds up to 31 to 45% of a core from parts measured apart, so a
100 ms mode has to narrow to a process tree. At 9 to 12 µs a process, a tree of 50
processes read every 100 ms costs 4.5 to 6 ms a second, 0.5% of a core. The Go rows
include Go's runtime: the CPU ticks cost 186 µs a read through the Go collector and
3.2 µs in C, so those rows are upper bounds. IOReport is the exception, at about the
same cost in Rust.

Handing a tick over costs 53 to 80 µs of CPU for writer and reader together, so at
one tick a second the transport is under 0.01% of a core. The ring with a reader woken
once a tick is the cheapest. A reader paced to the screen costs more, because it wakes
seven times for each tick it finds. NATS, ZeroMQ, Kafka and Iggy are built for millions
of messages a second between machines. This workload is one tick a second on one
machine.

## Collecting once a second

### Counters make slower reads lossless in total

CPU time, energy, instructions, disk bytes, wakeups and page-ins are counters since
the process started. Reading them once a second keeps every total and loses only the
shape inside the second. Reading a slow source every 10 or 60 seconds loses nothing
more.

Gauges do not have that property. Resident memory and footprint are the value at the
moment of the read, so a peak between two reads is lost. The rusage record also keeps
each process's peak footprint since a reset, `ri_interval_max_phys_footprint`. The
call that resets it, `proc_reset_footprint_interval`, is exported by the system
library but has no header in the SDK, so a peak per second rests on a private call.

### What is read, and how often

| Every | What | Cost per read, measured apart | In the collector, ms of CPU a second |
|---|---|---|---|
| 1 s | CPU ticks of every core, memory and swap, network interfaces, the pid list | under 0.1 ms in a tight loop | 0.03 to 0.07 for the CPUs and memory, 0.17 to 0.33 for the interfaces, the pid list counted with the processes |
| 1 s | Every process readable without root | 2.8 to 8.3 ms CPU and 2.3 to 9.3 mJ, paced, by the cores it runs on | 2.5 to 4.1 |
| 1 s | Every coalition | 1.0 ms in a tight loop | 1.7 to 3.2 |
| 1 s | IOReport, GPU | 5.9 ms and 2.2 ms in Go | 4.8 to 6.1 for IOReport, the I/O Registry's GPU not read yet |
| 1 s | The open-file list of every process readable without root, resolving only descriptors that are new | 1.4 ms to list, the few changes to resolve | not read yet |
| 1 s | A few SMC keys: hottest SoC sensor, fans, system power | 33 µs CPU a key, paced | 0.09 to 0.15 for 4 keys |
| 1 s | Bytes and operations per disk | not measured | not read yet |
| 1 s | Network bytes per process, through the framework under `nettop` | not measured | not read yet |
| 10 s | The rest of the 268 SMC temperatures, on their own thread | 6 ms CPU, 52 ms wall | not read yet |
| 60 s | Battery charge and health | not measured | not read yet |
| 60 s | Every open file and socket, resolved in full, as a keyframe | 10 to 20 ms | not read yet |
| As they happen | Process fork, exec and exit, from kqueue | 0.13 to 0.32% of a core | 0.1 to 15 at 1 to 170 forks a second, 22 to 74 at 300 to 1,700 |

Network interfaces have to be read with `sysctl` and `NET_RT_IFLIST2`, whose `if_data64`
counts bytes in 64 bits. The `if_data` that `getifaddrs` hands back counts them in 32
bits, which wrap every 4 GB. Per-process network bytes come from the private
NetworkStatistics framework that `nettop` uses, and without root `nettop` reported
processes of root and four system users beside the user's own. Running `nettop` itself
costs about 30 ms of CPU and 5 seconds a sample, so the collector has to call the
framework directly. Per-process GPU time has not been located beyond the coalition
totals.

Summed from parts measured apart, the collector was estimated at 20 to 22 ms a second,
2.0 to 2.2% of one core. The collector in Rust reads every row above with a cost in its
last column, and over five minutes it spent 22.9 ms a second, 2.29% of a core, and
37.5 mW. `time` agreed at 2.33%. The rows it reads were estimated at 16 to 18 ms, so it
came in 5 to 7 ms over, and process tracking accounts for more than that. Tracking cost
11.7 ms a second against 1.3 to 3.2 ms estimated, because processes forked 182 times a
second on average during the run, against 7 to 34 process starts a second when
tracking was measured apart. The other rows together came in under their estimate,
11.1 ms against 14.4. The sweep of every process cost 3.2 ms against 7.3, mostly on Super
cores, and IOReport 5.1 against 5.9. Coalitions cost 2.3 ms against 1.0 in a tight
loop.

Over 10-second windows, the 25 of 30 windows under 200 forks a second cost 1.0 to 2.6%
of a core, 1.57% and 28.6 mW on average, which leaves 0.4 to 2.0% of a core for the
rows not read yet and for the persister. The five windows at 316 to 1,585 forks a
second cost 3.3 to 8.6% and up to 114 mW, over the budget, and tracking took 22 to 73 ms
of each of their seconds. A run without IOReport and the SMC split the same way, 0.95%
on average under 200 forks a second and 6.5 to 8.0% in bursts of 870 to 1,650. Ticks
started 2.9 ms late on average and 8 ms at most, with no slips. The budget holds for
the reads, and the rescan on every fork breaks it during bursts.

Ticks have to be scheduled on absolute deadlines from the monotonic clock. The
sampler used here slept one second after each pass and drifted 9 seconds over 300,
so 10 of its intervals were two seconds long. A shared tick number is what lets every
reading of the same second join on equality. At the default quality of service, paced
ticks started 3.9 ms late on average and 5.1 ms at most. At utility and background they
started 45 to 61 ms late on average across runs, with the same CPU per tick, so the
lower classes save nothing measured here.

### Coalitions keep what polling misses

macOS puts every process in a resource coalition, roughly one per app or launchd job:
998 processes in 679 coalitions here. `coalition_info_resource_usage` reads a
coalition's CPU time, wakeups, disk bytes, GPU time and energy, and every one of the
679 answered without root, including the coalitions of root's processes. Its counters
include tasks that have exited: this session's own coalition reports 259,887 tasks
started and 259,860 exited. A child that burned 2 seconds of CPU and was reaped still
counted in its coalition's CPU time afterwards. So the cost of short-lived processes
lands in their coalition's totals, and so does the usage of processes the collector
cannot read without root.

A coalition counts tasks, which is not the same as processes: in one minute the
coalitions counted 2,670 tasks started while 2,025 pids were handed out. The CPU time
counts mach ticks, 24 million a second here, not nanoseconds: two seconds of CPU in
one process moved its coalition's count by 59 million.

The function is exported by the kernel library but has no header in the SDK. The
struct layout comes from XNU's open source and can change with any macOS update. How
closely coalitions match the outermost `.app` bundle is not checked, so their totals
are not yet known to be per-app totals.

### Process starts and exits without root

Polling once a second misses nearly every process. In a minute with 2,016 processes
that started and exited, 2,007 lived under a second, with a median of 6 ms and 90% under
53 ms.

A kqueue watch (`EVFILT_PROC` with `NOTE_FORK`, `NOTE_EXEC` and `NOTE_EXIT`) can be
placed on every process without root, root's included: 3,037 watches in one minute and
none refused. The kernel no longer names the child on a fork (`sys/event.h` says
`NOTE_TRACK` has been unsupported since 10.5), so on each fork the collector rescans
the pid list and watches whatever is new. In two runs that found 835 of 845 and 2,024
of 2,025 new pids, each in the rescan right after its parent's fork, and a poll once a
second found none the rescans had missed. `posix_spawn` reports a fork to the watch on
its parent the same as `fork` does, so the rescan catches processes started either way.

An exited process's final usage stays readable until its parent reaps it. Reading it
on the exit event succeeded for 87% and 65% of exits in the two runs (730 of 835, 1,316
of 2,017). In the collector's two five-minute runs, at 166 and 182 forks a second, it
succeeded for 83% and 82% (41,920 of 50,346, 44,926 of 54,990). The rescan found 61%
and 55% of new processes already exited, too late to watch, and the collector read
their usage when it found them instead of on an exit event. The rest still count in
their coalition's totals.

kqueue does not report file opens, file closes or socket connections. Those come from
Endpoint Security, which needs root and an entitlement Apple grants, or from
`/usr/bin/eslogger`, which needs root and Full Disk Access and whose man page says its
output is not an API.

### Open files without root

The open-file list of 325 of 1,015 processes, all of them other users', is refused
without root. For the rest, listing every process's descriptors each second costs
1.4 ms, and only the descriptors whose number or type is new need resolving. That
misses a descriptor reused for a different file: in the sample, 51 of about 950
changes reused the same number and type, and 10.7% of descriptors (pipes, kqueues and
others) carry no identity to compare. The full keyframe every 60 seconds corrects those.

### What root would add

Without root, 321 of 990 processes refuse `proc_pid_rusage`, `launchd` and
`WindowServer` among them, and the same third refuse their open-file list. Their
starts and exits still come through kqueue, their network bytes through
NetworkStatistics, and their usage as coalition totals. Root would read them one by
one and add Endpoint Security's file and socket events. An app installs a root launch
daemon with `SMAppService` (macOS 13). The SDK header says an app that contains a
launch daemon must be notarized, and a registered daemon can wait on the user's
approval in System Settings.

Other sources were ruled out:

- The audit trail behind `/dev/auditpipe` has been deprecated since macOS 11 and
  disabled since macOS 14, with Endpoint Security named as its replacement.
- The kernel's trace buffer, which `fs_usage` reads through the private ktrace
  framework, sees system calls and file activity as they happen but needs root and a
  private interface.
- `sysmond` runs as root and Activity Monitor reads it through `libsysmon`, which the
  SDK ships as a library with no header and no documentation.
- BSD process accounting records carry no pid and a command name cut to 10 characters
  (`sys/acct.h`), so they cannot be joined to the process table.

## Moving the data

One writer, the collector, and several readers: the live view, the persister, and any
tool you point at it.

The writer never waits for a reader. It overwrites the oldest tick, and each slot
carries its tick number. A reader copies a slot and then checks the tick number again,
as a single-writer seqlock does, so a slot overwritten during the copy is detected and
dropped instead of read torn, and a reader that falls behind knows which ticks it
missed. A slow or stopped reader, the viewer under a debugger or a persister waiting
on disk, cannot slow collection. This is the sampling port of the aircraft systems
below.

| Option | What it costs here |
|---|---|
| Shared-memory ring with a single writer, as in the LMAX Disruptor, Aeron and Chronicle Queue | 53 µs a tick for writer and reader together with one wake a tick, measured above. Readers map it read-only and read in place, so the viewer draws live data with no request. |
| Unix socket, or ZeroMQ's `ipc` transport, which is a socket | 80 µs a tick, and each reader receives its own copy. ZeroMQ's published numbers put `ipc` far below its in-process transport. |
| NATS, JetStream, Kafka, Redpanda, Iggy | A broker process between writer and readers, two socket hops and a protocol to parse per message. They earn that cost across machines. The durable replay that JetStream and Kafka offer is what the sealed files below provide. |

Recommendation: the shared-memory ring, made with `shm_open`. The object outlives the
process that wrote it: a second process read what the first wrote after the first had
exited. A reader that opens it read-only cannot make its mapping writable, since the
kernel refused the attempt. The writer wakes readers once a tick with
`os_sync_wake_by_address_any` and the `SHARED` flag, which macOS has had since 14.4.
[iceoryx2](https://github.com/eclipse-iceoryx/iceoryx2) offers the same in Rust,
publish-subscribe with events over shared memory, and lists macOS at its tier 2, "a
restricted security and safety feature set". It is the library to try before writing
the ring by hand, and it is not measured here.

Because the ring outlives the collector, a crash of the collector loses nothing the
ring holds. On restart it seals whatever the ring holds past the last sealed tick
before writing new ticks, so the ring should hold two sealing windows, 1.2 MB for the
changes measured here. A kernel panic or a power loss loses the open window, as
decided.

## What aerospace, automotive and robotics systems do

These systems split their data the way this recorder has to, and give each kind its
own delivery rule.

State, the latest value of something, goes through a port that keeps only the latest
value. An ARINC 653 sampling port, the channel between partitions of an avionics
computer, holds one message, each write replaces it, reads do not consume it, and the
port is configured with a refresh period. A read returns the message with a validity
flag. The exact rule that sets the flag is in ARINC 653 Part 1, which is paywalled and
was not read. DDS gets the same with a history of depth one (`KEEP_LAST`) and a
`DEADLINE`, a period by which the writer promises new data, with a missed deadline
reported to readers. Here that is the ring: overwritten, every slot stamped, so a
reader knows how old its data is and what it missed, and a gap is recorded as a gap.

Events, where every occurrence counts, go through a queue. An ARINC 653 queuing port
is first in, first out, and discards a message that arrives when it is full. DDS uses
`KEEP_ALL` history with reliable delivery, and `TRANSIENT_LOCAL` durability to fill a
reader that joins late with past samples. Here process starts and exits cannot share
the ring's overwrite rule. They go in a queue of their own with a count of what it
dropped.

Scheduling follows one clock. NASA JPL's F´ flight software divides one clock into
rate groups, 1,000 Hz into 100, 10 and 1 Hz in its own example, and detects a slip
when a group's work overruns its period. That is the table above: one tick a second,
groups at 10 and 60 seconds, and each slip stored as data so a missing reading carries
its reason.

| System | What it is | Fit here |
|---|---|---|
| DDS: Fast DDS, Cyclone DDS, RTI Connext | The OMG publish-subscribe standard that ROS 2 robots run on: discovery, typed topics and the QoS above, across a network of nodes. Cyclone DDS reaches shared memory through iceoryx. eProsima, which sells Fast DDS, reports severe problems with Cyclone and iceoryx above 1,000 messages a second, a competitor's claim. | Its QoS vocabulary fits. Discovery, a network protocol and C or C++ libraries buy nothing for one writer on one Mac. |
| iceoryx2 | Eclipse's Rust rewrite of iceoryx: zero-copy shared memory, publish-subscribe, events, request-response and a blackboard. | The library candidate for the ring. |
| Zenoh | A Rust publish-subscribe and query protocol that ROS 2 can also run on. Its 1.5 release reports over 10 million 8-byte messages a second on MacBooks, and 3 million a second through shared memory. Its storages, with filesystem, RocksDB, S3 and InfluxDB backends, answer queries for stored data under the same keys as live data. | Only if the recording should also stream to another machine. Its storages are not a columnar engine, so DuckDB would still be needed. |
| Aeron, Chronicle Queue, LMAX Disruptor | Single-writer rings from trading. Chronicle Queue keeps the ring in a memory-mapped file. | The pattern measured above. |

All of them, on one machine, come down to a single-writer ring in shared memory, which
the measurement above already prices. What they add is discovery, schemas, networks and
many writers, which one collector and a few readers on one Mac do not need. iceoryx2
and Zenoh are not measured on this Mac.

Recording is where the same split shows again.

- [Yamcs](https://docs.yamcs.org/yamcs-server-manual/data-management/parameter-archive/),
  an open-source mission control system, keeps its parameter archive in segments of
  about 70 minutes. Inside a segment the values of one parameter are stored together,
  integers with delta-of-delta and floats with a published float compression, and
  each segment is a value in RocksDB. A segment can be queried only once it has been
  written. Until then the realtime filler, which holds the segment being built, and a
  cache of the latest values answer for it. That is the design recommended here: the
  ring is the cache and the open window, and the sealed windows are the segments.
- [MCAP](https://mcap.dev/spec), the robotics log format from Foxglove, is append-only,
  so a file cut off by a crash or a power loss can be recovered. Messages sit in chunks
  compressed with zstd or lz4, and an optional index at the end gives each chunk's time
  range and offset. It has a Rust crate. It would fit a raw log of ticks kept to survive
  a panic, which this design does not keep, and not analytic queries, which need
  columns.
- ASAM MDF4, the automotive measurement format, stores raw bus and sensor values with
  the rules to convert them, uses 64-bit links that lift MDF3's 4 GB limit, and has
  compression since 4.1. It is tied to automotive tools, and nothing here needs it.
- LTTng's snapshot mode, from Linux tracing, keeps per-CPU ring buffers in memory,
  overwriting the oldest data with no disk I/O, and writes them out only when a
  snapshot is asked for. A later 100 ms mode for one process tree could work the same
  way, keeping its detail in memory and writing it out only on request.

## Storing and querying

### Three shapes of data

- Samples: numbers per process or per part of the machine, each tick.
- State that changes rarely: the process table (pid, parent, start time, path,
  arguments, app), open files, sockets and listening ports. Stored as intervals, one
  row per thing with when it appeared and when it went.
- Events: process start, exec and exit, memory pressure and thermal state changes, and
  each slip of the collector itself.

Open files changed by 1,943 rows in 299 seconds, about 560,000 a day. A full snapshot
of state, the keyframe that bounds how far back a reader looks to rebuild a moment, is
7,445 rows. One a minute would be 10.7 million rows a day, nineteen times the changes,
so the keyframe interval sets the size of the state tables, not the churn.

### Compression

The 300-second sample of 201,300 rows, 18 integer columns each, in `clickhouse-local`
26.8, in bytes per stored value:

| Encoding | Sorted by process, then time | Sorted by time, then process |
|---|---|---|
| ZSTD(3) | **0.174** | 0.394 |
| Delta, ZSTD(3) | 0.181 | 0.531 |
| DoubleDelta, ZSTD(3) | 0.244 | 1.900 |
| T64, ZSTD(3) | 0.301 | 0.653 |
| Delta, LZ4 | 0.305 | 0.917 |
| LZ4 | 0.380 | 0.706 |
| Parquet from ClickHouse, zstd, default settings | 0.427 | |
| The same without the bloom filter ClickHouse writes by default | 0.316 | |
| The same with counters stored as deltas | 0.218 | |

Sorting each process's values together makes the data 1.9 to 7.8 times smaller
depending on the codec. Gorilla does not apply to integers, and ClickHouse refuses it
on them. It, or DuckDB's ALP, matters only for the machine's float readings such as
temperatures and watts. Four of the 18 columns (second, pid, parent, start time) are
nearly free, so over the 14 metric columns the best figure is 0.217 bytes. ClickHouse's
own format and VictoriaMetrics, at 0.164 bytes a sample at 10 seconds today, 0.175
counting its index, compress better than the best Parquet figure.

The window the persister seals at sets the size too, since a file sorted by process
inside a short window holds short runs:

| Sealing window | Bytes per value, ZSTD(3), all 18 columns |
|---|---|
| 10 s | 0.49 |
| 60 s | 0.24 |
| 300 s | 0.19 |

A 60-second window is half the size of a 10-second one and within a third of a
five-minute one, and it keeps the open window, the part only the ring can answer for,
to one minute.

At the best figure, per-process numbers once a second come to about 175 MB a day for
669 processes. That window had 994 processes, near the 7-day median of 953. The 7-day
maximum is 2,163 processes, and busier seconds change more counters, so the cost at
peak is not measured. Per-process GPU and network are missing from the 14 metric values
sampled, so the figure is low. Netdata's answer to the same arithmetic is tiers: native
resolution for 14 days, per-minute for 3 months, per-hour for 2 years, each rollup
keeping minimum, maximum, average and last so a spike survives.

### Where it is kept

| Option | For | Against |
|---|---|---|
| Sealed Parquet files, one per table per time window, sorted by series inside, queried with DuckDB or DataFusion | Readers never block the writer, any tool opens them, delta encoding and no bloom filter reach 0.218 above | Sealing, compaction, tiers and retention are ours to write |
| [DuckLake](https://ducklake.select/), DuckDB's catalog over Parquet files | Keeps small inserts in the catalog until flushed, merges adjacent files and expires old snapshots, which is most of the code the row above leaves to us | One more moving part, the catalog database, and young |
| Sealed DuckDB files per window, opened read-only | DuckDB's own compression, including ALP for floats | Not measured. Only one process may have a DuckDB file open for writing, so the open window lives in the ring |
| tsink | Gorilla, WAL, retention and PromQL in one Rust crate | Young, changed its disk format between 0.8 and 0.9, numbers only |
| ClickHouse, or chDB embedded | Best compression measured, SQL, `ASOF JOIN` | A 360 MB server here, or experimental Rust bindings to a large library |
| VictoriaMetrics, GreptimeDB, InfluxDB 3 | Proven or built on Arrow, DataFusion and Parquet | Servers, and numbers only in VictoriaMetrics' case |
| SQLite | Many readers while one writes | Row storage, wrong for samples. Fits state and events |
| Compressed snapshots, as Meta's `below` stores them | Simplest | No query engine |
| TigerBeetle | | A financial transactions database of accounts and transfers, with no notion of a reading over time |

A file covering a time window, sorted by series inside, serves both ways of asking.
"Everything at second T" opens one window. "One process over a month" reads one run
per file across a month of windows, or the per-minute tier. How fast either query is
has not been measured on any option, so the choice among the first rows waits on that
benchmark.

The viewer joins history with the live tail in one query. DuckDB's Rust crate scans
Arrow record batches held in the same process (`ArrowVTab`, behind the `vtab-arrow`
feature), so the viewer copies the unsealed ticks out of the ring into Arrow batches
and queries them together with the sealed files. The ring could hold Arrow IPC batches
instead of fixed records, which saves that copy and costs the writer the Arrow
encoding every tick. Neither is measured. The DuckDB command line opens the sealed
files but sees only what is sealed.

## Security

Without root the recording holds only what the user can already read, plus the totals
of root's coalitions. Command lines still often carry tokens, and the recording keeps
them for as long as its retention, so the ring and the sealed files need modes that let
only the user read them, and arguments may need redacting before they are kept.

Readers map the ring read-only, so no reader can corrupt it. The writer takes no lock,
waits for no acknowledgment, and wakes readers with a broadcast, so no reader can stall
it. The event queue is the one place that does not overwrite, and it drops and counts
rather than block when full.

## Recommended pipeline

1. A Rust collector runs as the user's launch agent, registered with `SMAppService`, so
   recording goes on while the app is closed. It runs one clock that ticks each second,
   with groups at 10 and 60 seconds, on absolute deadlines at the default quality of
   service, and records its own slips. It reads coalitions for per-app totals and
   watches every process with kqueue, rescanning on each fork and reading each exiting
   process's final usage, into an event queue that counts what it drops. Everything
   carries one tick number.
2. It writes each tick into a shared-memory ring that only the user can read, holding
   two sealing windows. The writer never waits for a reader and wakes readers once a
   tick. iceoryx2 is the library to try for this before writing it by hand.
3. A persister in the same agent seals each 60-second window into Parquet or DuckLake,
   sorted by series, delta encoded, zstd, no bloom filter, and compacts sealed windows
   into the per-minute and per-hour tiers. On start it first seals what the ring holds
   past the last sealed tick.
4. The GPUI viewer draws live data from the ring, asks DuckDB for history over the
   sealed files, and joins the two through the ring's unsealed tail as Arrow batches.

The storage choice in step 3 waits on the query benchmark below.

## Next measurements

1. Rescanning the pid list at most once per interval while processes fork hundreds of
   times a second: the CPU it saves, and the processes it misses that a rescan on every
   fork finds.
2. The rows the collector does not read yet, against the room left in the budget: GPU
   from the I/O Registry, open files, disk, battery, and the 10 s and 60 s rows.
3. Query speed on a week of synthetic data in each storage option: one process over
   the week at screen resolution, top apps over a day, everything at one second.
4. Bytes a day at peak load.
5. Per-process network bytes through NetworkStatistics directly, and what it costs.
6. Whether coalitions group processes the way the app's notion of an app does.
7. iceoryx2 on this Mac against the ring measured here.
8. Parquet against Vortex, which claims much faster random access and cannot yet read
   files written by a newer version of itself.
9. For the root option: `eslogger`'s events a second and its CPU, file opens most of
   all.

## Evidence

The programs are in `/tmp/sysmon-xray` on this Mac and will not survive a reboot:
`xray.c` (one full pass), `sample.c` (300 seconds of processes and open files),
`lab/codecs.sh` (encodings), `lab/churn.py` (what changes per tick),
`lab/cheap.c` and `lab/tick.c` (the paced loop, its CPU, lateness and energy),
`lab/paced.c` (handing a tick over, ring against socket),
`lab/coal.c`, `lab/units.c` and `lab/exited.c` (coalitions, the unit of their CPU
time, and an exited child's CPU),
`lab/shmlife.c` (shared memory outliving its writer), `lab/track.c` and
`lab/track2.c` (kqueue process tracking against the coalitions' count). The sealing
windows were compared with `clickhouse-local` over `procs.csv`. The machine readings
were timed with a test added to a copy of the Go collector in `/tmp/sysmon-bench`.

The collector is in `collector/`, and `cargo run --release -- --seconds 300` prints its
cost for each 10-second window and for the run. Its two five-minute runs were release
builds of d8788c8, before IOReport and the SMC, and 737719d, on AC power at load
averages of 7 to 14. `/tmp/fd-probe` holds the probes for IOReport's units and groups
(`unit/unit.c`, `unit/groups.c`), an SMC key's cost and the SoC sensors
(`smc/smccost.c`, `smc/keys.c`), `posix_spawn` against the fork watch (`spawnfork.c`),
and `tick1s.c`, which is `lab/tick.c` paced once a second.

## Sources

- [Tmog](https://tmog.org/), [TigerBeetle](https://docs.tigerbeetle.com/)
- [DuckDB concurrency](https://duckdb.org/docs/current/connect/concurrency.html),
  [AsOf join](https://duckdb.org/docs/current/guides/sql_features/asof_join),
  [ALP](https://duckdb.org/library/alp/),
  [DuckDB Rust client](https://duckdb.org/docs/current/clients/rust/overview)
- [DuckLake data inlining](https://www.ducklake.select/2026/04/02/data-inlining-in-ducklake/),
  [merging files](https://ducklake.select/docs/stable/duckdb/maintenance/merge_adjacent_files)
- [Netdata disk requirements and retention](https://learn.netdata.cloud/docs/netdata-agent/resource-utilization/disk-%26-retention)
- [below, time-travelling resource monitor](https://developers.facebook.com/blog/post/2021/09/21/below-time-travelling-resource-monitoring-tool/)
- [tsink](https://docs.rs/tsink)
- [Vortex](https://vortex.dev), [Vortex vs Parquet benchmarks](https://dataengineeringcentral.substack.com/p/benchmarking-vortex-file-format-vs)
- [chDB Rust](https://clickhouse.com/docs/chdb/install/rust), [GreptimeDB](https://github.com/GreptimeTeam/greptimedb),
  [InfluxDB 3 FDAP architecture](https://influxdata.com/blog/flight-datafusion-arrow-parquet-fdap-architecture-influxdb)
- [Mechanical sympathy and the single-writer principle](https://martinfowler.com/articles/mechanical-sympathy-principles.html),
  [Chronicle Queue](https://rustdoc.chronicle.software/chronicle/queue/index.html),
  [iceoryx2](https://github.com/eclipse-iceoryx/iceoryx2)
- [ZeroMQ transport numbers from CZTop](https://www.rubydoc.info/gems/cztop),
  [ipc-bench](https://github.com/goldsborough/ipc-bench),
  [NATS bench](https://www.synadia.com/glossary/nats-bench),
  [Iggy benchmarking](https://iggy.apache.org/docs/server/benchmarking/)
- [os_sync_wait_on_address](https://developer.apple.com/documentation/os/os_sync_wait_on_address),
  [Seqlock](https://en.wikipedia.org/wiki/Seqlock),
  [Chromium's one-writer seqlock](https://chromium.googlesource.com/chromium/src/+/098ef3d2d/content/common/one_writer_seqlock.h)
- [Endpoint Security](https://developer.apple.com/documentation/endpointsecurity),
  [Apple forums on nettop alternatives](https://developer.apple.com/forums/thread/757294),
  [WWDC22, what's new in Endpoint Security, on the audit trail's deprecation](https://developer.apple.com/videos/play/wwdc2022/110345)
- [ARINC 653 sampling port, PTC Integrity Modeler](https://support.ptc.com/help/modeler/r9.0/en/Integrity_Modeler/arinc/ARINC_Sampling_port.html),
  [Reasoning about separation kernels with channel-based communication](https://arxiv.org/pdf/1510.05091)
- [Fast DDS standard QoS policies](https://eprosima-Fast-RTPS.readthedocs.io/en/latest/fastdds/dds_layer/core/policy/standardQosPolicies.html),
  [eProsima's zero-copy DDS comparison for ROS 2](https://discourse.ros.org/t/zero-copy-dds-performance-comparison-for-rmw-providers-in-ros-2/22696)
- [Zenoh 1.5 release](https://zenoh.io/blog/2025-07-28-zenoh-hong/),
  [ROS 2 over Zenoh with shared memory](https://learn.arm.com/learning-paths/cross-platform/ros2-zenoh-arm/enable-shared-memory/)
- [F´ cheatsheet](https://fprime.jpl.nasa.gov/cheatsheet.pdf)
- [Yamcs parameter archive](https://docs.yamcs.org/yamcs-server-manual/data-management/parameter-archive/),
  [Yamcs parameter retrieval](https://docs.yamcs.org/yamcs-server-manual/services/instance/parameter-retrieval-service/)
- [MCAP specification](https://mcap.dev/spec),
  [Introducing MCAP](https://foxglove.dev/blog/introducing-the-mcap-file-format),
  [MCAP Rust crate](https://mcap.dev/docs/rust/mcap/read/index.html)
- [ASAM MDF](https://www.asam.net/standards/detail/mdf/wiki/),
  [MF4 explained, CSS Electronics](https://www.csselectronics.com/pages/mf4-mdf4-measurement-data-format)
- [LTTng snapshot howto](https://lists.lttng.org/pipermail/lttng-dev/2013-July/020920.html),
  [LTTng 2.14 and per-CPU buffer memory](https://lttng.org/blog/2025/05/12/lttng-2.14-memory/)
