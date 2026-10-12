# Glossary

**App**: the outermost `.app` bundle a process runs from, such as Brave Browser with all
its helper processes. Processes outside any bundle group by executable name, so eleven
`claude` processes are one app.

**Process**: one running program, identified by its pid. macOS reuses pids, so a pid can
name a new process once the old one exits.

**Super cores and performance cores**: the two CPU core clusters of the M5 Pro. Super
cores are the faster cluster. macOS names them in `sysctl hw.perflevel0.name` and
`hw.perflevel1.name`. Older Apple chips call the pair performance and efficiency cores.

**Resident memory**: the memory a process has in RAM right now. Pages shared between
processes count once for each of them, so the resident memory of all processes adds up
to more than the machine uses.

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
