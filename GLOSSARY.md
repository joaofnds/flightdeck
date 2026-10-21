# Glossary

**Recording**: the readings kept so the user can return to an earlier second. A recording
continues across collector restarts.

**Retention period**: how long the recording keeps each reading, which the user sets in
minutes, hours or days. A reading older than the retention period is deleted.

**Tick**: one scheduled collection of the Mac's readings. A missed tick has no readings.

**Sealed minute**: the saved, immutable readings of a window of 60 elapsed seconds of a
collector run. The window may contain gaps. An empty window is not saved, and stopping
or recovery may seal a shorter final window.

**App**: the outermost `.app` bundle a process runs from, such as Brave Browser with all
its helper processes. Processes outside any bundle group by executable name, so eleven
`claude` processes are one app.

**Process**: one running program, identified by its pid. macOS reuses pids, so a pid can
name a new process once the old one exits. A process that replaces its own program
(`exec`) keeps its pid and stays the same process.

**Fork**: one process starting another, whether by copying itself (`fork`) or by starting
another program in one step (`posix_spawn`). An `exec` starts no new process, so it is not
a fork.

**Super cores and performance cores**: the two CPU core clusters of the M5 Pro. Super
cores are the faster cluster. macOS names them in `sysctl hw.perflevel0.name` and
`hw.perflevel1.name`. Older Apple chips call the pair performance and efficiency cores.

**Resident memory**: the memory a process has in RAM right now. Pages shared between
processes count once for each of them, so the resident memory of all processes adds up
to more than the machine uses.

**App memory**: the memory apps asked for themselves, not counting what macOS may discard
when memory runs short. Activity Monitor shows the same quantity under that name.

**Compressed memory**: memory macOS has compressed in RAM so it takes less room, which
puts off moving memory to swap.

**Swap**: memory macOS has moved from RAM to disk because RAM ran short. Swap that keeps
growing means the machine needs more memory than it has.

**Memory pressure**: macOS's own verdict on memory, from normal through warning to
critical. At warning and at critical it tells apps to use less memory. Compressed memory
and swap stay in use at normal too, so neither alone means the pressure is up.

**Page-in and page-out**: memory read in from disk because it was not in RAM, such as an
app's code, and memory written back out. macOS also counts the reads and writes of swap
on their own, as swap-ins and swap-outs.

**Thermal state**: macOS's own verdict on heat, from nominal through fair and serious to
critical. Only critical means heat is cutting the system's performance. Fair and serious
ask apps to cut back their work before that happens.

**Thermal pressure**: the level macOS publishes for heat, from nominal through moderate,
heavy and trapping to sleeping. Flightdeck shows it as the thermal state, since it is the
verdict macOS gives apps.

**SMC**: the System Management Controller, the part of the Mac that measures its
temperatures, fan speeds and power draw. It names each reading with a four-character key.

**Load average**: the work running or waiting for a CPU, averaged over 1, 5 and 15
minutes. Above the core count, work waits for a core.

**Frequency**: the clock a CPU core or the GPU runs at. macOS moves each between a fixed
set of frequencies as the work changes.

**Battery health**: what the battery holds when full, as a share of what it was designed
to hold, and the charge cycles it has been through. A new battery can hold more than its
design, so the share can pass 100%.

**External traffic**: network bytes on any interface other than loopback, which includes
VPN tunnels and virtual machine bridges. Traffic between processes on this Mac is not
external.

**Kernel time**: CPU time macOS spends on its own work, which belongs to no app.

**Energy**: what the chip spent running a process, in joules. Energy per second is power,
in watts.

**Resource coalition**: the group macOS puts a process in together with related
processes. macOS keeps CPU time, energy, disk bytes and GPU time per coalition, and a
process's CPU time stays in those totals after it exits. A coalition and an App are
separate groupings, so a coalition's totals are not an App's totals.

**Bucket**: a stretch of the clock, such as three seconds, at whose end a chart draws one point,
the mean of the readings of that bucket and of the buckets just before it. A chart of readings a
second apart takes the shortest length, of at least one second, that leaves each bucket a pixel
of the chart, so a wider chart or a shorter span draws shorter buckets, and each point covers at
least four seconds and a second more than its bucket. Buckets start at multiples of their length,
so a point keeps its place as the chart scrolls. Readings further apart, such as the collector's
ten-second reports or the older readings of a chart that reaches back more than sixteen minutes,
which stand ten seconds apart, make buckets at least that long and points that cover at least
four times that, so forty seconds for readings ten seconds apart.
