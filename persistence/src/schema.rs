/// All columns retain their source units. Availability: measured=0, refused=1,
/// gone=2, not applicable=3. Missing source values are SQL NULL, never invented zero.
pub(crate) const TABLES: [(&str, &str, &str); 10] = [
    (
        "ticks",
        "publication UBIGINT, run_tick UBIGINT, wall_ns UBIGINT, \
        continuous_ns UBIGINT, awake_ns UBIGINT, ioreport_read BOOLEAN, smc_read BOOLEAN, \
        gpu_energy_nj UBIGINT, power_watts FLOAT, soc_celsius FLOAT, \
        processes_refused UINTEGER, processes_gone UINTEGER, events_first UBIGINT, \
        events_end UBIGINT, dropped_starts UBIGINT, dropped_execs UBIGINT, dropped_exits UBIGINT, \
        gpu_state_count UINTEGER, core_count UINTEGER",
        "publication",
    ),
    (
        "memory",
        "publication UBIGINT, total_bytes UBIGINT, free_bytes UBIGINT, \
        active_bytes UBIGINT, inactive_bytes UBIGINT, speculative_bytes UBIGINT, \
        wired_bytes UBIGINT, compressed_bytes UBIGINT, anonymous_bytes UBIGINT, \
        file_backed_bytes UBIGINT, purgeable_bytes UBIGINT, swap_total_bytes UBIGINT, \
        swap_used_bytes UBIGINT, swapped_in_bytes UBIGINT, swapped_out_bytes UBIGINT, \
        pageins UBIGINT, pageouts UBIGINT, compressions UBIGINT, decompressions UBIGINT, \
        pressure UTINYINT",
        "publication",
    ),
    (
        "cpus",
        "publication UBIGINT, cpu UINTEGER, user_ticks UINTEGER, system_ticks UINTEGER, \
        nice_ticks UINTEGER, idle_ticks UINTEGER",
        "cpu, publication",
    ),
    (
        "interfaces",
        "publication UBIGINT, interface USMALLINT, name VARCHAR, \
        received_bytes UBIGINT, sent_bytes UBIGINT, received_packets UBIGINT, \
        sent_packets UBIGINT, receive_errors UBIGINT, send_errors UBIGINT, \
        receive_drops UBIGINT",
        "interface, publication",
    ),
    (
        "gpu_states",
        "publication UBIGINT, state UINTEGER, active_ns UBIGINT",
        "state, publication",
    ),
    (
        "core_states",
        "publication UBIGINT, core UINTEGER, state UINTEGER, active_ns UBIGINT",
        "core, state, publication",
    ),
    (
        "fans",
        "publication UBIGINT, fan UINTEGER, rpm FLOAT",
        "fan, publication",
    ),
    (
        "processes",
        "publication UBIGINT, pid INTEGER, awake_ns_at_start UBIGINT, \
        user_ns UBIGINT, system_ns UBIGINT, energy_nj UBIGINT, instructions UBIGINT, \
        super_instructions UBIGINT, cycles UBIGINT, idle_wakeups UBIGINT, \
        interrupt_wakeups UBIGINT, pageins UBIGINT, resident_bytes UBIGINT, \
        footprint_bytes UBIGINT, disk_read_bytes UBIGINT, disk_written_bytes UBIGINT, \
        threads INTEGER, faults INTEGER, context_switches INTEGER, mach_syscalls INTEGER, \
        unix_syscalls INTEGER",
        "pid, awake_ns_at_start, publication",
    ),
    (
        "coalitions",
        "publication UBIGINT, coalition UBIGINT, tasks_started UBIGINT, \
        tasks_exited UBIGINT, cpu_ns UBIGINT, interrupt_wakeups UBIGINT, idle_wakeups UBIGINT, \
        disk_read_bytes UBIGINT, disk_written_bytes UBIGINT, gpu_ns UBIGINT, energy_nj UBIGINT",
        "coalition, publication",
    ),
    (
        "events",
        "publication UBIGINT, event UBIGINT, run_tick UBIGINT, pid INTEGER, \
        kind UTINYINT, coalition UBIGINT, availability UTINYINT, noticed UTINYINT, \
        awake_ns_at_start UBIGINT, user_ns UBIGINT, system_ns UBIGINT, energy_nj UBIGINT, \
        instructions UBIGINT, super_instructions UBIGINT, cycles UBIGINT, idle_wakeups UBIGINT, \
        interrupt_wakeups UBIGINT, pageins UBIGINT, resident_bytes UBIGINT, \
        footprint_bytes UBIGINT, disk_read_bytes UBIGINT, disk_written_bytes UBIGINT",
        "pid, awake_ns_at_start, event",
    ),
];
