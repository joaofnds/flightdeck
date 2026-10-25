# Recording a Mac every second

Research for a Rust/GPUI app with a Rust recording core that records the whole machine once a second, keeps
the recording, and lets you go back to any second and join what every process, file
and socket was doing. Unless dated otherwise or attributed to a source, numbers below
were measured on this M5 Pro on 2026-10-12 or 2026-10-13. The evidence programs are
described at the end.

## Decisions

- The whole machine is read at most once a second. The measurements go down to
  100 ms to show the headroom, and a later mode may read one process tree at 100 ms.
- Collection and persistence together get 5% of one core, averaged over each minute,
  for now (confirmed 2026-10-15). Completeness
  comes first, then benchmarks that run on CI, then optimizing against them ("let's try
  to keep it under 5% for now and aim for completeness", 2026-10-13).
- The collector runs without root. Root is planned as an option the user turns on in
  the app, after the readings that need no root ("every single possible visibility point
  that we can make on the computer", 2026-10-19).
- The recording keeps its readings for a retention period the user sets in minutes,
  hours or days, one day by default ("we should definitely have a configurable limit in
  time duration (minutes/hours/days) and default to maybe 1 day", 2026-10-19). At the
  1.10 GB a day the recording reached on 2026-10-18, the default keeps about 1.1 GB.
- A kernel panic or a power loss may lose the minute not yet sealed.
- The collector stays in Rust ("agree, stay in rust", 2026-10-13). Measured against
  Zig, performance did not decide between the two, as Rust against Zig below shows.
- The core is everything a UI client uses, shared by the macOS client and any Windows
  or Linux client that follows. Flightdeck's own work is collecting, storing and
  retrieving metrics so a rich dashboard can be built on them. A UI client may use a UI
  toolkit such as GPUI or Skia. The core takes a dependency only for infrastructure
  that would be a large build of its own, such as persistence or communication between
  processes, and only a battle-tested, fast one. Each crate outside the workspace needs
  a recorded case that weighs how much of it Flightdeck would use, whether code written
  here matches its performance, and what that code would cost to maintain, so iceoryx2
  is weighed against a ring written here ("not just persistence, but like inter process
  communication ... we can just use a package for that is battle tested and really,
  really, really performant", "I would totally allow iceoryx2", "we need to see how much
  of it we would actually use, and if just writing a ring buffer ourselves ... we can
  achieve the same performance, and if it wouldn't be too much of a burden to
  maintain", 2026-10-13).

Every cost here comes from release builds made before 1a4b016, which turned on overflow
checks and abort on a panic. Measure that change on its own before comparing a new run
with these rows.

Without root and once a second, the collector in Rust measured 2.29% and 1.75% of a core
and 37.5 and 21.7 mW over two five-minute runs of this Mac's use. Seven of their ten
minutes cost 1.2 to 2.2%, and the three that held a burst of forks cost 3.1 to 3.2%,
all inside the budget. It does not yet read the GPU, open files, disk, per-process
network, battery or most of the SMC, and adding the ones measured apart puts it at about
2.7% and 2.2%, against the 2.0 to 2.2% estimated. IOReport costs the most of the rows
read every second, then the sweep of every process and the coalitions, and process
tracking cost more than those three together in the busier run. Over 10-second windows
the collector stayed at 1.0 to 2.6% while processes forked up to 161 times a second, as
they did in 53 of 60 windows here, and rose to 3.3 to 8.7% in bursts of 316 to 1,745.
Under one process forking at a steady rate it cost 3.0 to 4.7% a minute from 100 to
1,000 forks a second, and at 1,600 it cost 9.9% over a minute, over the budget, and
19.8% in the 10 seconds the Mac forked 2,932 times a second, mostly from rescanning the
pid list after forks. The collector rescans after every wake that carries a fork,
because that misses the fewest processes. A minute holding a generated 30-second burst
of 1,744 forks a second cost 4.87 and 5.04% of a core, the second just over the budget,
and 8.1% in a third run where a burst of this Mac's own fell in the same
minute. Those runs used the build before the interval existed, which rescans after every
wake as an interval of zero does. Its option to
rescan at most once every 5 ms brings that minute to 2.7 to 2.8%, and a probe tracking
at the same interval missed none of 52,320 children that lived 6 ms and were reaped within
10 ms of exiting. In five minutes of this Mac's use while a test suite ran, at up to 76
forks a second over 10 seconds, the collector cost the same 2% a minute at 5 ms as when
it rescanned after every wake, and missed 1,910 of 3,077 processes against 36. A cap
that rescans after every fork until rescans pass a rate might keep both, which is
unmeasured and waits for benchmarks that run on CI. A kqueue
watch on every process sees nearly every process start and exit without root, which
polling cannot. Root would add the usage and open files of the third of processes owned
by other users, and file and socket events as they happen.

## Reading the kernel is what costs CPU, not moving the data

| Step | CPU |
|---|---|
| `proc_pid_rusage` and task info for the 665 to 699 processes readable without root, paced every 100 ms or once a second at the default quality of service | 2.8 to 8.3 ms and 2.3 to 9.3 mJ a pass, 4 to 12 µs a process, varying from run to run at either pace |
| The same calls in a tight loop, for comparison | 1.7 µs a process, which a loop that sleeps between passes does not reach |
| CPU ticks of every core, memory, network interfaces, the pid list, in C, tight loop | 3.2 µs, 0.4 µs, 25 µs, 8.8 µs |
| Usage of all 679 coalitions, in C, tight loop | 1.0 ms |
| Listing each process's open files | 1.4 ms a pass |
| Resolving the 7,462 open files to a path or socket | 8 to 20 ms a pass |
| The SMC: 268 temperature keys and 2 fans, in the Go collector | 6.0 ms CPU, 52 ms wall |
| One SMC key, paced every 100 ms | 33 µs CPU, 272 µs wall |
| IOReport: CPU and GPU frequency states and GPU energy | 5.9 ms in Go. In C, paced, 4.5 ms for the 18 cores' states, 0.5 ms for the GPU's, 0.65 ms for its energy |
| GPU from the I/O Registry, in Go | 2.2 ms |
| Watching every process's fork, exec and exit with kqueue, rescanning the pid list after each wake that carries a fork, as the collector does by default | 0.13% of a core at 7 process starts a second, 0.32% at 34. In the collector, 53 to 65 µs a fork over a whole run of this Mac's use, 43 µs in a burst of 1,745 forks a second, and 139 µs and 34 µs a fork under one process forking at a steady 100 and 1,000 a second, 31 to 63 µs in the windows of a steady 1,600 |
| Running `nettop` once for per-process network bytes | about 30 ms CPU, 5 s wall |
| Handing a tick of 610 records (9.8 KB) to another process through a shared-memory ring, paced at 10 ticks a second, the reader woken once a tick | 33 µs writer, 20 µs reader, 10 µJ together, per tick |
| The same through a Unix socket | 50 µs writer, 30 µs reader, 12 µJ together |
| The ring with a reader that looks at each 60 Hz screen frame instead of being woken | 14 µs writer, 52 µs reader, 13 µJ together, 7.3 reader wakeups a tick |

Reading every process once a second costs 0.3 to 0.8% of one core and 2.3 to 9.3 mW.
Where the sweep runs moves its CPU time: over the 10-second windows of the collector's
three runs, the more of a window's instructions ran on Super cores, the less CPU time
the sweep took, with correlations of −0.47 to −0.94, while its energy followed no
consistent direction. The same sweep at 100 ms costs 3 to 8%, and reading every source
above at 100 ms, open files and the SMC included, adds up to 28 to 48% of a core from
parts measured apart, so a 100 ms mode has to narrow to a process tree. At 4 to 12 µs a
process, a tree of 50 processes read every 100 ms costs 2 to 6 ms a second, 0.2 to 0.6%
of a core. The Go rows include Go's runtime: the CPU ticks cost 186 µs a read through
the Go collector and 3.2 µs in C, so those rows are upper bounds. IOReport and the SMC
are the exceptions. IOReport cost 4.8 to 9.0 ms a second in the collector in Rust,
against 5.9 ms in Go. An SMC key costs 27 to 33 µs in the collector in Rust, against
about 22 µs a key in the Go collector, so the rest of the SMC may cost 7 to 9 ms a read
in Rust rather than 6, which the 100 ms sum above uses.

A count of the malloc family's calls after startup finds 143 a tick, all inside
`IOReportCreateSamples` and the release of the samples it returns, as Evidence
describes. The count does not see memory the kernel allocates, such as the array
`host_processor_info` returns on every read of the cores' CPU ticks, which the collector
frees with `vm_deallocate`. docs/tiger-style.md names both as a departure, since no call found
gives these readings without root in memory the collector owns. Without root,
`host_processors`, which would read each processor into the caller's buffer, and
`host_get_host_priv_port` both return KERN_INVALID_ARGUMENT, and `kern.cp_times` does not
exist (probed 2026-10-14 on macOS 27.0.1). No IOReport call the collector knows takes a
sample into memory the caller passes.

The GPU's memory and each process's GPU time come from the GPU accelerator's
`PerformanceStatistics` dictionary and the `AppUsage` array of each of its user clients.
`IORegistryEntryGetProperty`, which copies a property into the caller's buffer, returns
kIOReturnBadArgument (0xe00002c2) for both, while it copies the 22 bytes of a client's
`IOUserClientCreator` and the GPU's `gpu-core-count` (probed 2026-10-22 on macOS 27.0.1).
So the collector copies the creator into its own buffer and reads the dictionary and the
arrays with `IORegistryEntryCreateCFProperty`, which allocates on every read and falls
under the same departure. The malloc-family calls these reads add to a tick are not
counted yet.

Handing a tick over costs 53 to 80 µs of CPU for writer and reader together, so at
one tick a second the transport is under 0.01% of a core. The ring with a reader woken
once a tick is the cheapest. A reader paced to the screen costs more, because it wakes
seven times for each tick it finds. NATS, ZeroMQ, Kafka and Iggy are built for millions
of messages a second between machines. This workload is one tick a second on one
machine.

### Rust against Zig

A rewrite keeps the same system calls, so what a language can change is mostly user
time, the time spent in the collector's own code and the libraries it calls. Over a
minute on AC power that held 14 forks, the collector in Rust spent 0.08 s in user time
and 0.99 s in system time, 124 ms of it starting up, and 1.58% of a core after that.
User time also counts the code of IOReport and the system library, so rewriting the
collector as it reads today could save at most 1.3 ms a second, 0.13% of a core, and
only by taking its own code to zero. That historical bound left out the
open-file list, the GPU, and the persister, which sorts, encodes and compresses each
window in user space, where a language and its compiler matter most. The persister's
2026-10-15 implementation measurements appear under Persistence measurements. It leaves out
tracking in a burst of forks too, which was not measured this way.

The sweep of every process and every coalition was also written in Zig and run against
the same sweep through the collector's own Rust code, alternating, three runs of each
back to back and seven once a second. Both made the same calls, reading 469 processes
and 572 coalitions a round back to back, and 464 to 481 processes and 572 to 579
coalitions once a second. Zig's user time was lower on average at both paces, 139
against 144 µs a round back to back and 357 against 375 µs once a second, which once a
second is 18 µs, 0.002% of a core. That difference belongs to the two builds as much as
to the two languages. Zig was built as one unit with `-OReleaseFast`, which turns its
assertions off, while the Rust program called the collector crate under the default
release profile and keeps the coalitions in a `BTreeSet` where the Zig code scans an
array. System time was 91% of a round back to back and 96% once a second, and it varied
more between runs of one language than between the two. Once a second, single runs took
7,700 to 9,405 µs of CPU a round in Rust and 7,809 to 9,236 µs in Zig, and on average
Zig took 211 µs more. Those rounds averaged 8.6 to 8.8 ms, against 6.2 ms a second for
the same reads inside the collector over the minute above. The comparison sweeps
straight after a one-second sleep, where the collector sweeps after its other reads in
the same wake, which may explain the gap but was not tested. These figures compare the
two languages and do not give the sweep's cost. Only CPU time was compared, not energy,
memory or startup.

On this evidence performance does not decide between the two languages.

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
| 1 s | CPU ticks of every core, memory and swap, network interfaces, the pid list | under 0.1 ms in a tight loop | 0.03 to 0.11 for the CPUs and memory, 0.17 to 0.62 for the interfaces, the pid list counted with the processes |
| 1 s | Every process readable without root | 2.8 to 8.3 ms CPU and 2.3 to 9.3 mJ, paced, varying from run to run | 2.5 to 5.3 |
| 1 s | Every coalition | 1.0 ms in a tight loop | 1.7 to 3.5 |
| 1 s | IOReport, GPU | 5.9 ms and 2.2 ms in Go | 4.8 to 9.0 for IOReport, the I/O Registry's GPU not read yet |
| 1 s | The open-file list of every process readable without root, resolving only descriptors that are new | 1.4 ms to list, the few changes to resolve | not read yet |
| 1 s | A few SMC keys: the SoC sensor that was hottest at startup, fans, system power | 33 µs CPU a key, paced, in C | 0.09 to 0.19 for 4 keys |
| 1 s | Bytes and operations per disk | not measured | not read yet |
| 1 s | Network bytes per process, through the framework under `nettop` | not measured | not read yet |
| 10 s | The rest of the 268 SMC temperatures, on their own thread | 6 ms CPU and 52 ms wall in Go | not read yet |
| 60 s | Battery charge and health | not measured | not read yet |
| 60 s | Every open file and socket, resolved in full, as a keyframe | 10 to 20 ms | not read yet |
| As they happen | Process fork, exec and exit, from kqueue, rescanning the pid list after every wake that carries a fork, or at most once every N ms with `--rescan-ms` | 0.13 to 0.32% of a core, rescanning after every fork | 0.1 to 15 at 1 to 161 forks a second as this Mac forked, 22 to 74 in its bursts of 316 to 1,745 and 14 to 184 under one process forking at a steady 100 to 1,600 when it rescanned after every wake that carried a fork; with `--rescan-ms 5`, 13.6 to 14.7 over a minute holding a 30-second burst of 1,744. A probe's tracker alone, under a steady 100 to 1,600, took 13.1 to 44.1 rescanning after every wake and 12.9 to 24.8 at 5 ms |

Network interfaces have to be read with `sysctl` and `NET_RT_IFLIST2`, whose `if_data64`
counts bytes in 64 bits. The `if_data` that `getifaddrs` hands back counts them in 32
bits, which wrap every 4 GB. Per-process network bytes come from the private
NetworkStatistics framework that `nettop` uses, and without root `nettop` reported
processes of root and four system users beside the user's own. Running `nettop` itself
costs about 30 ms of CPU and 5 seconds a sample, so the collector has to call the
framework directly. Per-process GPU time is readable without root: each client of the
GPU's `AGXAccelerator` entry in the I/O Registry names its process in
`IOUserClientCreator` and carries an `accumulatedGPUTime` in nanoseconds. Summing a pid's
clients would give that process's GPU time, which no run here has compared with another
source. On 2026-10-19 the user's account read 95
such clients with `ioreg -l -w0 -r -c AGXAccelerator`. The collector does not read them
yet, and their cost is not measured.

Summed from parts measured apart, the collector was estimated at 20 to 22 ms a second,
2.0 to 2.2% of one core. The collector in Rust reads every row above with a cost in its
last column, rows estimated at 16 to 18 ms. In two five-minute runs it spent 22.9 and
17.5 ms a second, 2.29% and 1.75% of a core, at 37.5 and 21.7 mW, and `time` agreed
with the first at 2.33%. Processes forked 182 and 85 times a second on average, and
process tracking accounts for the difference. It cost 11.7 and 4.6 ms a second, against
1.3 to 3.2 ms estimated from 7 to 34 process starts a second. The other rows came in
under their estimate, 11.1 and 12.8 ms against 14.4. IOReport cost the most, 5.1 and
6.2 ms against 5.9, then the sweep of every process at 3.2 and 3.6 ms against 7.3, a
figure from an earlier paced probe near the top of the 2.8 to 8.3 ms measured since,
and the coalitions at about 2.4 ms in both against 1.0 in a tight loop.

At their costs measured apart, the rows not read yet add 4.4 to 4.8 ms a second for the
GPU, the open-file list, the rest of the SMC every 10 seconds and the keyframe every 60.
That puts the busier run at about 2.7% of a core over its five minutes, half a point
over the estimate, and the quieter at about 2.2%, the top of the estimate. Minute by
minute, the three minutes that held a burst would come to about 3.5 to 3.7%, inside the
budget, before disk, per-process network, battery and the persister, which that
observation did not measure. Persistence measurements below cover the current encoder.

Over 10-second windows, the 53 of 60 windows at 3 to 161 forks a second cost 1.0 to 2.6%
of a core, 1.39% and 16.5 mW on average in the quieter run and 1.57% and 28.6 mW in the
busier. The seven windows at 316 to 1,745 forks a second cost 3.3 to 8.7% and up to 117
mW, and tracking took 22 to 74 ms of each of their seconds. In the two bursts of the
quieter run, rescanning the pid list took 78% of tracking, the kevent calls and their
events 11 to 13%, and reading and watching the new and exited processes 9 to 11%. A run
without IOReport and the SMC split the same way, 0.95% on average at up to 147 forks a
second and 6.5 to 8.0% in bursts of 870 to 1,650. Ticks started 2.9 and 3.0 ms late on
average and 8 ms at most, with no slips. Every one of the seven burst windows fell in
one of the three minutes that cost 3.1 to 3.2%.

By default the collector rescans once per wake that carries a fork, and a wake returns
up to 64 events, so forks that arrive together share a rescan and forks spread evenly each pay
for one. Under one process forking at an even rate for a minute, each child living
6 ms, the collector counted 103 to 146 forks a second at a rate of 100, since the rest
of the Mac forks too, and cost 3.0% of a core, 2.6 to 3.2% in its windows, with
tracking at 14 to 20 ms. At 300 it cost 4.7% and at 1,000 it cost 4.4%. Between those
two, tracking rose only from 28 to 32 ms a second to 33 to 35 ms, because more forks
shared each wake, and the totals differ by where the collector ran: 65% and 100% of its
instructions on Super cores, and 16.5 and 9.7 ms for the rows read every second. At
1,600 the generator missed 96,470 of its 105,600 deadlines and caught up in bursts, so
the collector counted 1,505 to 2,932 forks a second in its windows. It cost 9.9% over
the minute, 5.7 to 7.9% in the four windows at 1,505 to 1,634 forks a second, and 13.3%
and 19.8% in the windows at 2,297 and 2,932, where tracking took 121 and 184 ms and 55%
and 26% of instructions ran on Super cores. Rescanning took 62 to 84% of tracking in
every window, 114 µs a fork at 100 a second and 21 µs at 1,000. Rescanning after every wake
keeps a minute of steady forking inside the budget up to 1,000 forks a second and puts
it over at 1,600. With the 4.4 to 4.8 ms a second of the rows not read yet, the minute
at 300 would come to about 5.2%, over the budget, and the one at 1,000 to about 4.9%.

With `--rescan-ms`, the collector rescans at most once per interval instead. A fork
rescans at once when the last rescan's listing ended at least an interval ago, and
otherwise arms a timer for an interval after it, which rescans if a fork is still
pending. The once-a-second listing counts as a rescan, though it is not itself held back,
so it can land within an interval of a rescan once a second. In a
minute of the collector holding a 30-second burst of 1,744 forks a second from two
generators, each child living 6 ms and reaped by its generator within 10 ms of exiting,
the collector cost:

| Rescan | Collector over the minute, three runs | Tracking, ms a second | Children the probe missed at the same interval, of 52,320 | Children whose final usage the probe read |
|---|---|---|---|---|
| After every wake carrying a fork | 4.87 to 5.04%, and 8.08% in a run that also held a burst of this Mac's own, 73,302 forks in the minute against 51,896 to 53,290 | 35.3 to 35.6, and 68.9 | 0 | 99.5 to 99.8% |
| At most every 5 ms | 2.74 to 2.78% | 13.6 to 14.7 | 0 | 99.7 to 99.8% |
| At most every 10 ms | 2.47 to 2.63% | 10.3 to 11.5 | 2,205 to 2,473 | 95.2 to 95.7% |
| At most every 20 ms | 2.16 to 2.31% | 7.8 to 8.8 | 20,485 to 21,118 | 59.5 to 60.7% |
| At most every 50 ms | 1.69 to 2.06% | 5.4 to 5.9 | 39,180 to 39,554 | 24.2 to 25.0% |

The probe ran at the interval of its row, 0 for the first, which rescans whenever a fork
is pending, and it ran beside the collector, costing 5.2 to 9.2% of a core in the first
row and 2.7 to 2.9% at 5 ms, so contention may have raised the first row's cost. A child stays listed from its fork until its generator reaps it, 6 to 16 ms,
so the misses rise sharply once the interval passes that span, since a child that exits
and is reaped between two rescans is never listed. A process that lives under 5 ms and
is reaped at once, as many of this Mac's own are, can still be missed at 5 ms, and these
runs cannot show how often. All four intervals kept the minute at 2.8% or less,
and no interval between 0 and 5 ms was tried. With the 4.4 to 4.8 ms a second of the
rows not read yet, such a minute would come to about 3.3% at 5 ms, inside the budget at
every interval tried, and to about 5.3 to 5.5% rescanning after every wake, over it.

Under one process forking steadily for 30 seconds, each child living 6 ms, the probe's
tracker cost, rescanning after every wake against at most every 5 ms, with the final
usage read for 99.5 to 99.9% of children in every row:

| Forks a second | Tracking, ms a second | Rescanning, ms a second | Children missed |
|---|---|---|---|
| 100 | 13.1 against 12.9 | 10.1 against 9.8 | none of 3,000 either way |
| 300 | 27.9 against 24.8 | 20.0 against 16.3 | none of 9,000 |
| 1,000 | 30.2 against 15.9 | 19.3 against 4.5 | none of 30,000 |
| 1,600, from two generators | 44.1 against 23.3 | 27.2 against 6.1 | none of 48,000 |

At 100 a second forks come 10 ms apart, so nearly every one still rescans at once, and
the interval saves little until forks come closer together than 5 ms. The rest of
tracking, the kevent calls, the reads and the watches, grows with the forks and is not
throttled: at 1,600 it came to 17.2 ms a second. No collector ran at a steady rate at 5 ms.
With the other rows at the 13.1 to 13.8 ms a second they took in the 5 ms burst minutes,
the probe's tracking would put the collector at about 3.8% at a steady 300 and 3.7% at
1,600, against about 4.1 to 4.2% and 5.7 to 5.8% rescanning after every wake, all of
it inference. The collector itself cost 9.9% at 1,600 rescanning after every wake, but in
that run the generator caught up in bursts of up to 2,932 forks a second.

This Mac's own processes can be missed far more often than the generator's children.
Two collectors ran side by side for five minutes, one rescanning after every wake that
carried a fork and one at most every 5 ms, while a test suite ran and the Mac forked at
up to 76 a second over 10 seconds. Both cost 1.95 to 2.05% a minute, 2.00 and 2.01% over
the run, so the interval saved no CPU there. Against the pid sequence the first missed
36 of 3,078 processes started and the one at 5 ms missed 1,910 of 3,077. In five minutes of
quieter use at 5 ms, at up to 7 forks a second over 10 seconds, the collector cost 1.81
to 1.91% a minute and missed 144 of 708. The likely reason, not yet measured, is
that many of these processes exit and are reaped within a few milliseconds, so they are
gone before a rescan held back for 5 ms lists them, while the generator's stayed listed
for 6 to 16 ms. At 5 ms, with two of the 30-second bursts of 1,744 forks a second added to five
minutes of use, the collector cost 1.92 to 2.82% a minute. Rescanning after every wake,
in five minutes of quiet use at up to 13 forks a second over 10 seconds, the collector
cost 1.31 to 1.72% a minute and missed none of 888 processes, and in the next five
minutes, while a test suite built and ran, at up to 54 forks a second, 1.60 to 1.77% a
minute and 46 of 2,616, or 1.8%. In the five minutes after that, 10 seconds of this
Mac's own use held 4,904 forks, 490 a second with 190 execs, from no generator and from a
source not identified. That window cost 3.68% of a core and tracking 20.2 ms a second,
its minute 1.98%, and the run's minutes 1.77 to 1.98%. It missed 175 of 5,887
processes, or 3.0%.

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
one process moved its coalition's count by 59 million. Energy counts nanojoules, since a
process's coalition gained 1.01 to 1.15 times the energy the process itself reported
over two runs. GPU time counts nanoseconds. Summed over every coalition it came to 0.26
to 0.92 of the GPU's active time in IOReport, and read as mach ticks the same sum would
be 11 to 38 times that active time. The rest of the GPU's active time, 8 to 74%, was
charged to no coalition the probe read.

The collector learns a coalition only from a process the rescan finds, so a coalition
whose every process is gone before the rescan is never read. In two minutes in which
launchd forked 20 times, the lookup succeeded for all 47 new processes, and each of the
3 new coalitions among them stayed readable for at least a second after its process
exited. How often the collector misses a coalition over a longer run is not measured.

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
none refused, and the collector placed about 8,700 in five minutes with none refused,
finding the other 16,947 of its 25,647 new processes already exited. The kernel no
longer names the child on a fork (`sys/event.h` says `NOTE_TRACK` has been unsupported
since 10.5), so after a fork the collector rescans the pid list, once per wake that
carries a fork, and watches whatever is new. Two runs of a lab probe that rescanned after every
fork found 835 of 845 and 2,024 of 2,025 new pids, each in the rescan right after its
parent's fork, and a poll once a second found none the rescans had missed. `posix_spawn` reports a fork to the watch on its parent the
same as `fork` does, so the rescan catches processes started either way.

An exited process's final usage stays readable until its parent reaps it. Reading it
on the exit event succeeded for 87% and 65% of exits in those two runs (730 of 835,
1,316 of 2,017), and for 60% in a five-minute run of the collector (5,238 of 8,688),
where the parent had reaped 38% before the read. That run's rescans found 66% of new
processes already exited, too late to watch, and the collector read 96% of those when
it found them (16,283 of 16,947). Over every exit, seen on its event or found already
exited, the collector read the final usage of 82 to 84% in three five-minute runs. The
rest still count in their coalition's totals, provided the collector learned that
coalition.

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
carries a publication number, which rises by one a slot and goes on across a restart
that finds the previous ring, beside the tick number of the collector's run. A reader copies a slot and then checks
its sequence word again, as a single-writer seqlock does, so a slot overwritten during
the copy is detected and dropped instead of read torn. A reader that falls behind knows
which publications it missed from a gap in their numbers, and tells that from a tick the
collector slipped, which is a gap in tick numbers alone. A slow or stopped reader, the viewer under a debugger or a persister waiting
on disk, cannot slow collection. This is the sampling port of the aircraft systems
below.

| Option | What it costs here |
|---|---|
| Shared-memory ring with a single writer, as in the LMAX Disruptor, Aeron and Chronicle Queue | 53 µs a tick for writer and reader together with one wake a tick, measured above. Readers map it read-only and copy each slot out, so the viewer draws live data with no request. |
| Unix socket, or ZeroMQ's `ipc` transport, which is a socket | 80 µs a tick, and each reader receives its own copy. ZeroMQ's published numbers put `ipc` far below its in-process transport. |
| NATS, JetStream, Kafka, Redpanda, Iggy | A broker process between writer and readers, two socket hops and a protocol to parse per message. They earn that cost across machines. The durable replay that JetStream and Kafka offer is what the sealed files below provide. |

Recommendation: the shared-memory ring, made with `shm_open`. The object outlives the
process that wrote it: a second process read what the first wrote after the first had
exited. A reader that opens it read-only cannot make its mapping writable, since the
kernel refused the attempt. The writer wakes readers once a tick with
`os_sync_wake_by_address_all` and the `SHARED` flag, available from macOS 14.4, because
several readers may wait.
[iceoryx2](https://github.com/eclipse-iceoryx/iceoryx2) offers the same in Rust,
publish-subscribe with events over shared memory, and lists macOS at its tier 2, "a
restricted security and safety feature set". The ring is written here instead, in
`collector/src/ring`. iceoryx2 keeps a publisher's history in the publisher's own process
(`iceoryx2/src/port/publisher.rs`), and its cleanup of stale resources removes a dead
publisher's data segment (`iceoryx2/src/service/stale_resource_cleanup.rs`), both read in
its source at commit 0763bbc and not run. Its ticks would die with the collector, which
the restart below rules out. Its cleanup of dead nodes is configuration that defaults to
on (`iceoryx2/src/config.rs`), and its blackboard pattern was not surveyed. Its latency
and CPU were not compared. TASK-18 took this choice unattended, and it waits on the user's
ruling, since the Decisions above ask for that weighing. The ring here is about 3,550
lines in `collector/src/ring`, its unit tests included, and about 1,750 lines of tests in
`collector/tests/ring.rs`.

Because the ring outlives the collector, a crash leaves its retained publications
recoverable. Each start writes a new ring and registers it with its private recording.
The registry holds at most two sources, and the persister seals recoverable older
publications before newer ones. Fully drained sources retire without an eviction;
exceeding the source bound reports that the lost extent is unknown. An unreadable
source also leaves an explicit unknown extent while healthy collection continues.
If registration fails, bounded anonymous-ring cleanup retains the current ring and its
actual predecessor; those sources have no recording identity and recovery is unavailable.
Each active ring holds two sealing windows, 120 ticks. Each slot
holds full records with room for every process and coalition the collector's limits
allow, about 500 MB of address space for the slots together, of which only the pages
written take memory. The
event log beside the slots holds 120 ticks of events at the tracker's bound, about
300 MB, and the writer fills it in a circle, so once it wraps all of it is in memory: at
the 43 to 418 events a second that runs on 2026-10-15 measured, that takes from about
12.7 hours down to about 1.3. The object is
796 MB, and after a restart two of them exist, both staying in memory or swap after the
collector exits. A kernel panic or a power loss loses the open window, as decided.

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
dropped. The ring as built does otherwise, as the end of this section says: its event
log overwrites its oldest records, and a reader counts what it missed.

Scheduling follows one clock. NASA JPL's F´ flight software divides one clock into
rate groups, 1,000 Hz into 100, 10 and 1 Hz in its own example, and detects a slip
when a group's work overruns its period. That is the table above: one tick a second,
groups at 10 and 60 seconds, and each slip stored as data so a missing reading carries
its reason.

| System | What it is | Fit here |
|---|---|---|
| DDS: Fast DDS, Cyclone DDS, RTI Connext | The OMG publish-subscribe standard that ROS 2 robots run on: discovery, typed topics and the QoS above, across a network of nodes. Cyclone DDS reaches shared memory through iceoryx. eProsima, which sells Fast DDS, reports severe problems with Cyclone and iceoryx above 1,000 messages a second, a competitor's claim. | Its QoS vocabulary fits. Discovery, a network protocol and C or C++ libraries buy nothing for one writer on one Mac. |
| iceoryx2 | Eclipse's Rust rewrite of iceoryx: zero-copy shared memory, publish-subscribe, events, request-response and a blackboard. | The library candidate for the ring, set aside under Moving the data. |
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
| Sealed DuckDB files per window, opened read-only | DuckDB's own compression, including ALP for floats | The week-query experiment below used 4.8 times Parquet's allocated space and slower week scans. The open window lives in the ring |
| tsink | Gorilla, WAL, retention and PromQL in one Rust crate | Young, changed its disk format between 0.8 and 0.9, numbers only |
| ClickHouse, or chDB embedded | Best compression measured, SQL, `ASOF JOIN` | A 360 MB server here, or experimental Rust bindings to a large library |
| VictoriaMetrics, GreptimeDB, InfluxDB 3 | Proven or built on Arrow, DataFusion and Parquet | Servers, and numbers only in VictoriaMetrics' case |
| SQLite | Many readers while one writes | Row storage, wrong for samples. Fits state and events |
| Compressed snapshots, as Meta's `below` stores them | Simplest | No query engine |
| TigerBeetle | | A financial transactions database of accounts and transfers, with no notion of a reading over time |

A file covering a time window, sorted by series inside, serves both ways of asking.
"Everything at second T" opens one window. "One process over a month" reads one run
per file across a month of windows, or the per-minute tier. Query speed for the first
three options was measured on 2026-10-15. Immutable Parquet is selected
for the recording: it used the least disk space and native write CPU in this experiment,
and needs no mutable catalog. DuckLake's slightly faster week scans do not establish a
reliable speed ranking or a new chart latency requirement.

The M5 Pro, arm64, macOS 27.0.1 build 26A434 experiment used DuckDB 1.5.6
(069cc9f9b5), DuckLake ac7595b0 and SQLite extension 5274128. Its synthetic week has
10,080 windows, 953 sampled processes a second with specified missing ticks, and 64
events a second, including event-only exiting processes. Each format held 576,317,220
samples and 38,707,200 events. Three rotated passes ran each query twice in one engine
process. These are median seconds, first query / repeat:

| Format | Everything at one second | One process, week into 1,280 buckets | Synthetic app CPU totals, day |
|---|---:|---:|---:|
| Parquet | 0.006459 / 0.003927 | 1.611054 / 0.763221 | 0.599288 / 0.300055 |
| Sealed DuckDB | 0.005047 / 0.003391 | 13.953710 / 12.456842 | 1.745486 / 1.408108 |
| DuckLake | 0.158556 / 0.085003 | 1.558944 / 0.706168 | 0.682634 / 0.362450 |

Parquet allocated 7,232,847,872 bytes, sealed DuckDB 34,470,232,064 and DuckLake
7,924,731,904 including its catalog. Their generation and encoding/writing CPU totals
were 566.30, 687.43 and 655.41 seconds respectively. These exclude the production
full-sync publication protocol. The point timings begin with selected files, so they
exclude application discovery. The production layout partitions immutable directories
by run and elapsed day, with wall-time bounds selecting candidate windows; readers
validate canonical commit markers and filter actual tick timestamps.

Peak query-process RSS was 713,785,344 bytes for Parquet, 68,108,288 for sealed DuckDB
and 315,457,536 for DuckLake. Parquet's memory cost is a tradeoff of the selection.
These query processes are separate from the production recording worker's memory
budget; the future viewer's query-process boundary and memory allowance remain unsettled.

The preserved driver commands are `benchmark --mode build --fixture baseline
--minutes 10080 --processes 953`, `benchmark_checked --mode build-lake --fixture baseline
--minutes 10080 --processes 953`, and `benchmark_queries --mode queries --fixture baseline
--minutes 10080 --processes 953`. Every
timed engine used `-no-init -batch -bail`, four threads, a 2 GB buffer limit and a 4 GB
spill limit. Parquet uses explicit selected-file lists, series order, Zstandard level 3,
16,384-row groups and no Bloom filters. Installed DuckLake writes Bloom filters and
rejects the disabling option, so the configurations differ there. All 54 outputs
matched a separate Rust oracle. A later explicit app-ranking supplement also matched
all 54 outputs; its day medians were Parquet 0.598066/0.300822, DuckDB
2.770925/1.402990 and DuckLake 0.675318/0.355611 seconds. Metadata scans interfered
with the early ranked passes. Neither family establishes cold-cache performance.

Exact source/binary families, SQL, settings, individual timings and incident records
remain in `backlog/evidence/TASK-19/storage/report.txt` and
`source_binary_sha256.txt` on this Mac. The private board is git-ignored and absent
from a clone. The optional peak-load week was omitted. The original
monitor failed during a WAL removal race while its bounded engine child continued;
sealed outputs and subsequent queries completed. A separate post-run version command
read a user init file and failed, outside the timed engines. Those limitations do not
change the returned answers, and the complete production resource checks below remain
separate from the format experiment.

The viewer copies bounded unsealed observations out of the ring into typed Polars
DataFrames and queries each frame on its own, apart from the immutable Parquet sources. The collector keeps
its fixed-record ring format and does not run the query engine. The experiment above
compared storage formats, not this viewer.

## Security

Without root the recording holds only what the user can already read, plus the totals
of root's coalitions. Command lines still often carry tokens, and the recording keeps
them for as long as its retention, so the ring and the sealed files need modes that let
only the user read them, and arguments may need redacting before they are kept.

Readers map the ring read-only, so no reader can corrupt it. While publishing, the
writer waits for no acknowledgment and wakes readers with a broadcast, so no reader
can stall it. The ring's event log overwrites its oldest records, and a reader learns
what it missed from the gap. The writer holds a startup exclusion lock file, and a
process of the same user that takes that lock keeps a new collector from starting.
The recording also has a lifetime `collector.lock`; another process of the same user
holding it prevents persistence for a new collector, which continues collection. The
collector that takes it removes the last holder's `collector.announcement`, and once its
run is registered writes a new one naming that run and a fingerprint of the collector's
source, so a reader asks the kernel whether the lock is held, without taking it, and
reads which run and which build hold it.

Every user shares the names of shared-memory objects, so each ring's name carries 60
random bits that the collector writes nowhere but its lock file. The lock file is
`NAME.lock`, where NAME is the ring name `--ring` gives, in
`Library/Application Support/Flightdeck` under the home the user database gives for the
user's uid, whatever HOME says. The collector refuses that directory unless its mode lets
only the user enter it, and it reads the mode alone, not an access control list. Another
user therefore cannot know the names a writer will make, and cannot take them first to keep
the collector from starting. That follows from the code, and no run has tried it as
another user yet, since the CI test that does has not run. The writer draws the bits once
and keeps them for as long as the lock file names a ring, so the collector prints no ring
name to its log, and a process that once read the lock file knows the names of every later
ring. A lock file lost or rewritten by another program, or a deleted directory, makes the
next writer draw new bits, and the rings named under the old ones stay until the Mac
restarts, about 800 MB of address space each at the default shape, since nothing knows
their names any more. The directory is outside both paths dirhelper's binary names. Its
launch daemon runs it daily at 03:35 with `CLEAN_FILES_OLDER_THAN_DAYS` set to 3, as
`/System/Library/LaunchDaemons/com.apple.bsd.dirhelper.plist` shows, and the only absolute
paths `strings /usr/libexec/dirhelper` prints are `/tmp/` and `/var/folders/`. That it
removes files older than three days under those two, where a collector running longer
would lose the lock file its single writer and its readers rely on, is inferred from those
two readings, and no run has watched it.

## Recommended pipeline

1. A Rust collector runs as the user's launch agent, registered with `SMAppService`, so
   recording goes on while the app is closed. It runs one clock that ticks each second,
   with groups at 10 and 60 seconds, on absolute deadlines at the default quality of
   service, and records its own slips. It reads coalitions for per-app totals and
   watches every process with kqueue, rescanning the pid list after every wake that
   carries a fork, or at most once per interval as an option, and reading each exiting process's final usage, into an event queue that counts what it drops. Everything carries one tick number.
2. It writes each tick into a shared-memory ring that only the user can read, holding
   two sealing windows. The writer never waits for a reader and wakes readers once a
   tick. The ring is written here, as Moving the data says, with process events in a
   log of their own beside the ticks.
3. A separate Rust persister seals each 60-elapsed-second window into immutable Parquet,
   sorted by series with Zstandard and no Bloom filters. It seals recoverable older
   publications before newer disk windows while collection continues. Recording, run,
   boot and publication identify readings across restarts; actual observation clocks
   preserve gaps and backward wall time. Complete table sets become visible through
   full-synced canonical no-overwrite commit markers. Empty windows produce no files,
   and stopping may seal a partial window. The retention period under Decisions is not
   built yet, and per-minute and per-hour compaction remain future work.
4. The GPUI desktop client reads live and saved observations through the Rust viewer
   adapter, combining sealed history with the ring's unsealed tail under bounded
   operation, result and cache limits.

The dated week-query experiment under Where it is kept selects step 3's storage.
The collector and separate persister implement step 3. Launch-agent registration remains a future caller. Legacy layout-2 rings retain their
readings, with unknown observation timestamps and an all-zero unknown boot identity.
Choose UTF-8 recording roots without `*`, `?` or `[`. Creation accepts other paths, but
the query API and checker refuse them so an engine file pattern cannot expand a private
path into another recording.

## Crates from outside the workspace

Each crate from outside the workspace that a workspace member depends on directly has its
case here, as the dependency decision above asks. A crate is from outside unless it is one
of the members the workspace manifest lists, so a crate read from a path, which Cargo.lock
gives no `source`, needs a case as a crate from a registry does. A case weighs what its
crate pulls in with it, so a crate reached only through another has no case of its own.

Each case sits under a `###` heading whose text is the crate's name as Cargo.lock writes
it, in backticks or not. It holds three entries, each a line that starts with its label and
says its part on that line: `- Used:` how much of the crate Flightdeck uses,
`- Performance:` whether code written here would match its performance, and
`- Maintenance:` what that code would cost to maintain. The tidy test finds this section by
its heading, and fails when the heading is gone, on a dependency with no case, on a case
missing an entry, and on a case whose crate no member depends on.


### serde_json

- Used: The viewer-only Rust adapter projects recording data into bounded JSON values for the Rust desktop client and diagnostic tools. Raw query cells remain decimal strings or NULL, preserving stored integers without floating-point conversion. The crate is pinned to =1.0.154, the version polars-core, polars-io, polars-utils and Polars' object-store dependency also use.
- Performance: Requests use 512-row pages and at most 4096 contextual observations. Rust callers consume values directly. Whole-client allocation, query latency and footprint still require measurement. Another JSON encoder would duplicate escaping and integer correctness work.
- Maintenance: Keep Cargo.lock and check wide integers, NULL and refused reads through the Rust projection. This reuses serde, serde_core, itoa, memchr and zmij already present in the engine graph. Recheck bounds and the serde_json changelog on upgrades.


### polars

- Used: Persistence constructs typed DataFrames and writes sorted, immutable Parquet tables of schema version 1; the viewer runs bounded native SQL plans over saved files and staged frames. The source is pinned to the Python Polars 2.0.0 release commit `22a147de3d2bb2e44b97338a2510816c7105c9f2`, whose Rust workspace version is 0.55.1. The published Rust crate 0.55.2 is a different release. Default features are disabled; Flightdeck selects lazy execution, SQL, Parquet and narrow unsigned integers. The collector does not depend on Polars.
- Performance: A prototype on Rust Polars 0.55.2 returned the same typed answers as DuckDB on representative snapshot, context and process-history scans, including wide counters and binary paths. Implementing its columnar optimizer, SIMD kernels, Parquet decoding and Zstandard encoding here would add substantial maintenance work. The Viewer query measurements section compares the 2.0 release with DuckDB through the viewer's public calls.
- Maintenance: Keep the exact release commit, `Cargo.lock` and `desktop/Cargo.lock`. The release also requires its upstream `hashbrown` patch at `d76a6f81abd8f388676aff15068236a8d21c7e1a`; downstream workspace roots must repeat that Cargo patch. Its SQL crate enables a broad set of expression features, and the Parquet graph includes asynchronous I/O, HTTP/TLS and object-store support even though Flightdeck uses local files and synchronous application code. Default lazy collection uses Streaming at this pin; explicitly selected engines remain separate paths. Query cancellation is cooperative. Each viewer request records the cancellation epoch it started under and refuses its result once a newer request has advanced the epoch, even if a kernel finishes after cancellation. Row limits on the frames copied from the ring, and result and cache limits, bound logical work, but Polars supplies no working-memory quota of its own. The persister retains its separate process and physical-footprint watchdog. Startup defaults both native worker pools to one thread through a same-PID re-exec before engine or UI initialization, except the desktop app's compute pool, which starts with six, since the app reads a chart context and its older tail on six threads at once and each waits on that pool for its queries, and the app sets its collector's settings to the one-thread defaults whatever settings the app runs with, explicit ones included, so the persister that collector starts keeps one; a `sample` profile of the viewer benchmark worker still showed four `tokio-rt-worker` threads besides those pools, and each viewer request runs a short-lived cancellation watcher thread. Explicit positive settings from 1 through 64 are preserved; other values return an error. Library callers choose their own startup policy. Resource measurements must record effective settings and the observed native pool size. Recheck types, ordering, query cancellation, memory and compiler support on upgrades. Sources: [2.0 release](https://github.com/pola-rs/polars/releases/tag/py-2.0.0), [release dependency pins](https://github.com/pola-rs/polars/blob/22a147de3d2bb2e44b97338a2510816c7105c9f2/Cargo.toml).


### base64

- Used: Before the native reader opens a saved file, Flightdeck checks its metadata against fixed bounds. This decodes the embedded Arrow schema so Flightdeck can check its IPC envelope before the pinned native reader converts it. Version =0.22.1 already belongs to the Polars Parquet graph; default features are disabled and only allocation support is requested.
- Performance: The existing 4 MiB footer bound limits this metadata decode. Admission checks the two native envelope forms before schema inference, avoiding a second handwritten Base64 implementation.
- Maintenance: Retain malformed short-envelope regressions alongside valid native and legacy metadata cases. Recheck the guard against the pinned Parquet reader on upgrades.

### polars-buffer

- Used: The opt-in sealed-table adapter moves admitted immutable bytes and their compressed-byte reservation into `Buffer::from_owner`, then supplies `ScanSources::Buffers` to the existing lazy Parquet scanner. This crate already belongs to the Polars graph; its direct pin uses the same `22a147de3d2bb2e44b97338a2510816c7105c9f2` revision with defaults disabled.
- Performance: The bridge moves the original byte allocation; Buffer clones and slices share its owner. Implementing compatible reference counting and native scan sources here would duplicate the dependency's ownership machinery. Native schema inspection still copies footer bytes, and decoding/results require separate accounting.
- Maintenance: Keep both lockfiles and this revision aligned with polars. Check backing-pointer preservation, clone/slice lifetime and destruction before reservation release. The foreign-owner box, shared-storage box and source Arc remain unresolved native wrapper allocations; their sizes cannot be added to the reservation inside the foreign owner because it releases before those wrappers are freed. Default lazy collection at this pin uses Streaming; explicitly chosen engines are separate paths. Preserve scan options and query ordering when upgrading.

### polars-arrow-format

- Used: The saved-file checks examine embedded Arrow IPC schemas through native borrowed generated accessors before Polars converts their types. Version =0.2.1 already belongs to the selected native graph; default features are disabled and only IPC support is requested.
- Performance: Admission limits fields, metadata entries and the aggregate bytes a native conversion may copy, including repeated references to a shared FlatBuffer string. It rejects unsupported nested, dictionary and extension types without recursively unpacking a schema. A separate IPC parser would duplicate the format's offset and tag validation.
- Maintenance: Keep this version aligned with polars-arrow. Retain abort-child regressions for unsupported native type tags, exact scalar-width compatibility, and shared-string amplification when upgrading. These metadata bounds do not establish an allocator quota for native value decoding.

### polars-parquet

- Used: The saved-file checks read native Parquet footer and page metadata before the native reader opens the file. This crate uses the same exact Polars release commit as the encoder and viewer, with default features disabled. It is already part of Polars' Parquet dependency graph.
- Performance: Admission checks page headers and seeks past compressed bodies before value decoding. It limits declared file, footer, page, row and expanded sizes; these checks are admission estimates, not an allocator quota. Reimplementing Parquet metadata conversion would duplicate the decoder's format handling.
- Maintenance: Keep its revision aligned with polars. Exercise malformed lengths, offsets and allocation declarations through the bounded Thrift wrapper before upgrading. Runtime value decoding remains the native engine's responsibility.

### polars-parquet-format

- Used: The saved-file checks wrap this generated Thrift protocol and metadata reader with container, allocation and nesting limits. Version =0.1.0 is already a transitive dependency of the pinned Polars release; default features are disabled.
- Performance: The wrapper rejects oversized container declarations before the generated reader reserves their elements, without decoding compressed values. A separate parser would duplicate both the Parquet schema and compact Thrift encoding.
- Maintenance: Keep the direct version aligned with polars-parquet. Recheck generated collection allocation and field-identifier handling on upgrades, retaining regressions for oversized metadata and malformed field deltas.

## Desktop animation dependencies

The separate desktop workspace uses these direct dependencies for the optional fan
surface. They were already present transitively through GPUI; none runs in the collector.

On 2026-10-24, experiment pfan09 compared a 30 Hz independent Metal surface with the
dashboard-driven animation from f4501b4, both on GPUI 8766c4ad and common sources from
9df54ba. On a MacBook Pro Mac17,8 (M5 Pro, 48 GB, macOS 27.0.1), two counterbalanced
observations per condition averaged 25.94 versus 113.17 ms of application CPU per
second. Each CPU interval lasted about 35 seconds. This supports the independent
surface choice; it does not measure this production integration, displayed frame
pacing or whole-machine energy. Raw results and probe sources are kept in the external
board at `backlog/evidence/TASK-90/current-pin` and doc-68, which are not included in a
repository clone. Animation remains an opt-in resource cost.

### `metal` and `foreign-types`

- Used: Metal device, texture, pipeline, command buffer and drawable ownership, plus the
  pointer bridge to the typed QuartzCore layer. Two instanced draws share one render pass.
- Performance: the independent-surface experiment above reduced application CPU against
  dashboard-driven animation. That measures the rendering design, not a binding overhead
  comparison; hand-written Objective-C calls would submit the same GPU work.
- Maintenance: these bindings already serve GPUI's renderer. Replacing their ownership and
  API declarations locally would duplicate platform bindings without an established gain.

### `raw-window-handle`

- Used: GPUI's trait implementation supplies the AppKit view that hosts the fan layer.
- Performance: the handle is checked on ordinary paints, never obtained for each fan frame.
  A local replacement would still need GPUI's published handle contract.
- Maintenance: the crate provides that shared typed contract, avoiding guessed native pointers.

### `objc2-quartz-core` and `objc2-core-foundation`

- Used: typed layer attachment, clipping, hidden state and transactions, with CGRect/CGSize
  types for coordinates. AppKit's existing dependency supplies the Reduce Motion preference.
- Performance: layer geometry changes only with ordinary layout; the worker handles animation.
  Hand-written declarations would invoke the same native methods.
- Maintenance: generated Apple bindings keep signatures and object ownership explicit instead
  of adding local Objective-C declarations for these APIs.

## Persistence measurements

The implementation observations in this section were measured on 2026-10-15, against the
DuckDB encoder. The figures have not been recorded again for the Polars encoder, whose
capacity profile, growth bound and deliberate native leak remain encoder test gates.

The complete-schema encoder uses ten typed Parquet tables: ticks, memory, CPUs,
interfaces, GPU states, core states, fans, processes, coalitions and raw numbered events.
Child rows key back to their publication; the immutable window metadata identifies the
recording, run and boot. The files use series order, Zstandard level 3, 16,384-row groups
and disabled Bloom filters. Unread sources and unavailable exit usage produce NULL values
with explicit source/availability state. Unknown legacy observation times remain NULL.
No mutable catalog or runtime extension installation is needed.

On the same Mac, a fresh-target `cargo +1.95.0 build --locked -p persistence --lib
--target-dir /tmp/task19-cold-bundled-20261015` completed in 49.36 seconds wall,
517.01 user and 52.71 system, with 3,581,493,248-byte maximum compiler RSS. The registry
was already fetched; no target artifact existed. This includes Arrow and bundled C++
compilation. A fresh macos-26 CI run of 6e450cc took 6m 51s for the workspace
dependency build and clippy, then 7m 39s to compile the test executables. These timings
include Rust and native work, not an isolated C++ measurement. The 15-minute job limit
canceled the run after tests began, so the job allows 30 minutes for cold builds and
the complete suite. The completed tests reported no assertion failure before that
cutoff; recording-file privacy had not run yet. Source:
[CI run 38787804116](https://github.com/joaofnds/flightdeck/actions/runs/38787804116).

The debug encoder's full injected capacity profile has 61 populated ticks with 12,000
processes, 28,384 coalitions, 1,638 interfaces, 18 CPUs/cores, 64 states, 10 fans and
16,384 events per tick. A carried late tick and sixty scheduled observations can share
one elapsed minute. It staged 407,435,592 payload bytes in 78,092,042 ns and completed
Rust feed plus encoding in 18,248,752,167 ns, with native physical-footprint lifetime peak
761,382,136 bytes, below the 2 GiB working allowance and without a watchdog trip.
The generated source has no ring mapping; the worker's mapping closure and durable
publication are separate checks. These debug timings exclude full-sync publication and
are not the combined recording CPU acceptance observation.

An isolated process encoded 8 warmups and 64 repeated populated small windows. Completed
window physical footprint rose from 18,743,896 to 22,102,640 bytes over the observed 64;
the 8 MiB growth bound passed. A native `malloc` mutant retaining and touching 1 MiB per
window failed that same check (31,081,048 to 99,369,608 bytes). A separate child touched a
16 MiB native allocation against a limit 4 MiB above its initial lifetime peak. The
watchdog observed 21,971,520 current/peak bytes against a 9,355,840-byte limit and
terminated the child with status 74 before a successful encode. The observer polls
`proc_pid_rusage` V6 every 100 ms, checks current physical footprint and kernel lifetime
peak, and reports failure before process exit. This is an observed termination threshold
with possible overshoot, not an OS allocation quota. The parent integrates refusal,
reaping and continued collection separately. Another guard test reached real Parquet
writes in a pending directory, exited with status 74, and left no canonical commit.
A guarded blocked operation exits with status 75 after its deadline. Immediate native
exit avoids running C/C++ cleanup concurrently with the engine.

The DuckDB encoder configured one thread before opening each engine connection. Opening
with defaults started seventeen extra workers on this Mac before a later SQL setting
removed them. The default configuration also exceeded the growth bound once in three runs
(13,975,552 bytes); the growth checks passed in all three preconfigured comparisons.
One comparison suite also hit a temporary-directory name collision, since fixed with
an atomic ordinal. The original spike's
cause remains unsettled. A malloc cache-release probe released no bytes and was not
added. The final uninstrumented nine-test encoder suite passed with the figures above;
the growth assertion and deliberate native leak remain regression gates.

Those runs used `cargo +1.95.0 test --locked -p persistence --test encoder -- --nocapture`,
which now runs the Polars encoder's tests and so cannot reproduce these figures. The full
test used explicit capacities rather than the host's smaller live shape. Source-to-staging
byte and row bounds refused excess input before copying, and the DuckDB encoder closed its
connection after each window. Raw build and resource output is retained
in `backlog/evidence/TASK-19/build/review-fixes/encoder-verified.log` on this Mac,
alongside the failed and comparison runs. This private board evidence is git-ignored
and absent from a clone. Earlier encoder observations remain under `build/encoder`.

## Viewer query measurements

The viewer's query path was measured on the M5 Pro (48 GiB, macOS 27.0.1) on 2026-10-19,
comparing the DuckDB viewer at commit 68416989 with the Polars 2.0 viewer. Both workers
compile the same benchmark source with Rust 1.98.0, release optimization, overflow checks
and `panic = "abort"`. DuckDB runs with one thread, as the application configured it.
Polars runs under the application's startup policy, which sets its compute and async pools
to one thread each, and the Polars worker reports the compute pool size it observed.

Each operation selects one second of a frozen recording with its 15-minute context, then
reads the first page of all ten tables, through the viewer's public `Session` calls. The
two engines return identical answers, compared field by field with no normalization.
Seven independent fresh processes run per engine, alternating which engine goes first,
and each runs two warmups and five measured operations over a warm file cache. Results
are medians of the seven process medians. Peak footprint is the median of each
process's lifetime maximum. The host's one-minute load average read 4.2, 3.7 and 3.1 at
the start, middle and end of the runs, with a browser and an unrelated test run active.

| | DuckDB | Polars 2.0 |
|---|---|---|
| Wall time per operation | 460 ms | 268 ms |
| Process CPU per operation | 459 ms | 272 ms |
| Peak physical footprint | 51.4 MB | 50.6 MB |
| Footprint after seven operations | 38 to 60 MB | 28 or 51 MB |
| Opening the recording | 405 ms | 397 ms |

Polars took less time and less CPU in every one of the seven pairs, by 38% to 43%. Every
process reached about 51 MB of peak footprint by the end of its first operation, which
includes opening the recording, and the records do not separate the two. Afterwards no
Polars peak rose by more than 0.5 MB, and two DuckDB peaks rose to 60 MB. DuckDB's current
footprint grew on every operation in all seven processes, by 1.1 to 6.2 MB a step, for
example from 29 to 43 MB, while Polars grew by at most 0.3 MB a step after its first
operation. In three Polars processes the footprint fell by about 22 MB after the first or
second operation and stayed near 28 MB, while the other four stayed near 51 MB. These
records do not show what memory was released or when, and seven operations do not show
where DuckDB's growth stops.

The Polars viewer builds, once per saved file, the validated SQL that maps the file's
columns onto its table, and reuses it while the engine keeps the file registered. Against a
Polars build without that reuse, the final build took 15% less time (267 against 314 ms)
and 15% less CPU (270 against 320 ms), with per-pair reductions of 6% to 17% and peak
footprints within 3% of each other. The two builds also differ outside that reuse, so this
neither isolates nor bounds its effect. Profiling the selection attributed about a third of
the main thread's samples to re-validating each window's saved commitment once per table
read, which the DuckDB viewer also did.

These are repeated warm-cache viewer operations. They do not measure cold start from
disk, encoding, collector throughput or desktop presentation. The trials and raw
per-process records are kept under `target/polars_migration/` on this Mac
(`trials_final_duckdb`, `trials_final_v4polars`, and the Final section of
`reproduce.txt`); that directory is git-ignored and absent from a clone.

## Next measurements

1. Answered under "What is read, and how often": the collector rescans after every wake
   that carries a fork, and `--rescan-ms` trades missed processes for CPU in bursts.
   Still open is which pace keeps both, rescanning after every fork wake, a cap that
   does so until rescans pass a rate, or Tiger Style's own pace of at most once an
   interval. A benchmark that runs on CI decides it by weighing the CPU each costs
   against the processes each misses ("I think we should benchmark this and weigh the
   pros and cons and see what the tradeoffs are", 2026-10-13).
2. The rows the collector does not read yet, against the room left in the budget: GPU
   from the I/O Registry, open files, disk, battery, and the 10 s and 60 s rows.
3. Answered for Parquet, sealed DuckDB and DuckLake under Where it is kept: one process
   over a synthetic week at screen resolution, app CPU totals/ranking over a day and
   everything at one second. Peak-load storage remains measurement 4.
4. Bytes a day at peak load.
5. Per-process network bytes through NetworkStatistics directly, and what it costs.
6. Whether coalitions group processes the way the app's notion of an app does, and how
   many coalitions the collector never learns because their processes are gone before a
   rescan.
7. Taken without measuring and open for the user's ruling, under Moving the data: the
   ring is written here, since iceoryx2 loses a dead publisher's history. The ring's 53 µs
   a tick for writer and reader together, at one tick a second, is about 0.005% of a core,
   so iceoryx2 could save at most that much CPU. Not done: the latency and CPU of a tick
   through iceoryx2, how much of it Flightdeck would use, and a survey of its blackboard.
8. Parquet against Vortex, which claims much faster random access and cannot yet read
   files written by a newer version of itself.
9. For the root option: `eslogger`'s events a second and its CPU, file opens most of
   all.

## Evidence

The collector declares the kernel calls it makes instead of taking them from libc,
which added 631 lines to `collector/src` and removed 172 (72a35a7..de6cc13). Each struct
and constant covered by the retained C probes matches the macOS 27.0 SDK values those
probes checked. Compile-time assertions pin Rust sizes and field offsets.
`git log --grep 'xcrun clang'` finds the commit messages containing the probes,
including the ring's `struct passwd` and shared-memory constants. Commit 7536d79 retains
the re-runnable `sigaction` and `proc_bsdshortinfo` static assertions. The documentation
commit adding Persistence measurements also retains the probe for the recording
child's `timeval`/`rusage`, both resource observers' `rusage_info_v6`, and both
attribute-list layouts. Those static assertions compiled on 2026-10-15. These pins
alone do not compare Rust declarations with a new header or check function argument types.
`coalitions.rs` and `smc.rs` declare structures with no SDK header to check against.

A reboot on 2026-10-13 cleared `/tmp`, so the programs and output kept there are gone
and only their descriptions remain. The programs were in `/tmp/sysmon-xray`:
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

Build the sibling binaries with `cargo build --release -p collector -p persistence
--bin collector --bin persister`, then run `./target/release/collector --seconds 300`.
It prints each source's cost, the combined recording CPU, and separate drain cost.
The historical collector's three
five-minute runs were release builds of d8788c8, before IOReport and the SMC, 737719d
and c7bb9ba, at load averages of 5 to 14. The third ran on AC power, and the first two
did not record the power source. Their output was in `/tmp/fd-probe` as `run1.txt`,
`run2.txt` and `run3.txt`, split into windows by `windows.py` and, for the third,
`windows2.py`. The steady fork rates came from `forkrate/forkrate.c`, run by
`forkrate/rates.sh` against the same c7bb9ba build, on AC power at load averages of 2.7
to 6.3, into `forkrate/rate-*.txt`, which `windows2.py` also splits. `supercorr.py`
correlates each window's Super-core share with the sweep's CPU and energy. `minutes.py`
averages each run over every minute.

The rescan interval was measured with programs in the repository. `forkrate` forks at a
fixed rate and writes its children's pids, `followprobe` runs the tracker at a given
interval and counts those children found and missed, and `collector/scripts/burst-minute.sh`
ran both inside one minute of the collector. The `burst_minute` binary replaced that script
in b7acfb0, and `git show b7acfb0^:collector/scripts/burst-minute.sh` prints the script,
which `sweep.sh` below still calls.
The runs used release builds of 3c61963 to ee7fd68, whose tracking is the same, at intervals
of 5 to 50 ms, and a build of 52e24f0, whose collector is c7bb9ba's, before the
throttle. The probe in the rows before the throttle ran at an interval of 0. The runs
were on AC power, which the steady runs did not record, at one-minute load averages of
0.58 to 12.40. Their output is in `~/.cache/flightdeck-task10/runs` on this Mac, outside
the repository, with `sweep.sh` and `steady.sh` beside it, which made the burst and the
steady runs, and `summarize.py`, which reads them. The side-by-side and quieter
five-minute runs (`paired-0.txt`, `paired-5.txt`, `natural-2.txt`) used a release build
of 657fe1d, the first with the miss count fixed. The runs at the default interval of zero
(`default-<n>.txt`) used a release build of 336d278, run back to back for five minutes
each by `default-runs.sh` in `~/.cache/flightdeck-task10/scripts`.
`/tmp/fd-probe` also held the probes for IOReport's units and groups (`unit/unit.c`,
`unit/groups.c`), an SMC key's cost and the SoC sensors (`smc/smccost.c`, `smc/keys.c`),
`posix_spawn` against the fork watch (`spawnfork.c`), a coalition outliving its process
(`coallife/coallife.c`, output in `coallife-120s.txt`), the units of a coalition's
energy and GPU time (`idleprobe/src/bin/units.rs`, output of its later run in
`units.txt`), and `tick1s.c`, which is `lab/tick.c` paced once a second, with output of
its later runs in `tick1s-default.txt`.

The language comparison is in `~/.cache/flightdeck-lang` on this Mac, outside the
repository. `ab.zig` is the sweep in Zig with static buffers sized at compile time, and
`abrust-main.rs`, built with `abrust-Cargo.toml`, runs the same rounds through the
collector's own `list_pids`, `Sweep` and `Coalitions` at 3e39e0e. Both read `ROUNDS`
and `PACE_MS` from the environment and print the median and percentiles of each
round's CPU time with the process's user and system time a round. `results.txt` holds
every run: three of 300 rounds back to back and seven of 15 rounds at once a second for
each language, taking turns, built with `zig build-exe -OReleaseFast` on Zig 0.17.0 and
`cargo build --release` with the default release profile on Rust 1.95.0, while the Mac
listed 757 to 778 pids and refused 293 to 297 of them. `whole/run-60s.txt` is the
collector at 209b539 run for a minute under `/usr/bin/time -p`, at one-minute load
averages of 1.64 to 1.91.

The collector's malloc-family calls after startup were counted with `malloc-count.c`, a
library loaded through `DYLD_INSERT_LIBRARIES` that counts every call into the 30
malloc-family functions it interposes, frees included, and groups the calls by the
collector's frames they came through. A typed entry point such as `malloc_type_calloc`
calls an untyped one such as `calloc`, so the library counts only the outermost call on
each thread. It does not interpose `vfree`, and a call made straight through a zone's
function pointers skips the functions it interposes, so neither is counted.
`git log -1 --format=%B --grep='^BEGIN malloc-count\.c$'` prints its source between the
lines `BEGIN malloc-count.c` and `END malloc-count.c`, with how to build and run it. A
release build of 155ddc6 ran for 65 seconds twice, once on a quiet Mac and once beside
`forkrate 400 64 6`, which forks 400 children a second for 64 seconds, each sleeping
6 ms before it exits, and the collector saw 385 to 462 forks a second in its 10-second
windows. The count covered the 63 ticks from 1.5 to 64.5 seconds of each run. Both runs
made 9,009 calls, 143 a tick, 79 inside `IOReportCreateSamples` and 64 releasing the
samples it returns, all under the collector's IOReport read. docs/tiger-style.md names
these calls as a departure. The same count
of a release build of 719b976, which read IOReport's state names and energy unit on
every read, found 432 calls a tick, 251 of them under the names and the unit. The output
is in `~/code/backlog/boards/flightdeck/evidence/TASK-13.4/final-guarded` on this Mac,
outside the repository, with the counter's source and a probe that calls each of the 30
functions 100 times, for which the counter prints 100 calls each.

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
