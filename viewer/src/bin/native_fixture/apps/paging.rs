use super::{
    Fixture, IdentityObservation, Process, ProcessCreation, ProcessIdentity, ProgramBytes, Task,
    Usage, UsageAvailability, io,
};

pub(super) fn initialize_members(fixture: &mut Fixture) -> io::Result<()> {
    let writer = fixture.writer.as_mut().unwrap_or_else(|| unreachable!());
    let mut main = None;
    writer
        .intern_program(
            &ProgramBytes {
                executable_name: b"main\xff",
                app_path: Some(b"/Applications/Example\xff.app"),
            },
            &mut main,
        )
        .map_err(|reason| io::Error::other(format!("paging main program: {reason:?}")))?;
    let main = main.ok_or_else(|| io::Error::other("paging main dictionary exhausted"))?;
    for pid in 1..=513 {
        push_identity(&mut fixture.identities, pid, main);
    }
    for index in 0..512 {
        let path = format!("/Applications/Other-{index:04}.app");
        let mut program = None;
        writer
            .intern_program(
                &ProgramBytes {
                    executable_name: b"helper",
                    app_path: Some(path.as_bytes()),
                },
                &mut program,
            )
            .map_err(|reason| io::Error::other(format!("paging program: {reason:?}")))?;
        let program = program.ok_or_else(|| io::Error::other("paging dictionary exhausted"))?;
        push_identity(&mut fixture.identities, 1001 + index, program);
    }

    assert_eq!(fixture.identities.len(), 1025);
    Ok(())
}

fn push_identity(
    identities: &mut Vec<IdentityObservation>,
    pid: i32,
    program: collector::ring::ProgramReference,
) {
    assert!(identities.len() < identities.capacity());
    identities.push(IdentityObservation {
        pid,
        usage: UsageAvailability::Measured,
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: 10_000 + u64::from(pid.cast_unsigned()),
                pid_version: 1,
            },
            program,
        },
    });
}

struct Population {
    members: i32,
    others: i32,
    second: u64,
}

pub(super) fn publish(fixture: &mut Fixture) -> io::Result<()> {
    for population in [
        Population {
            members: 513,
            others: 512,
            second: 1,
        },
        Population {
            members: 511,
            others: 510,
            second: 2,
        },
    ] {
        fixture
            .identities
            .retain(|identity| survives(identity.pid, &population));
        fixture.processes.clear();
        for identity in &fixture.identities {
            let pid = identity.pid;
            assert!(fixture.processes.len() < fixture.processes.capacity());
            fixture.processes.push(Process {
                pid,
                usage: Usage {
                    awake_ns_at_start: 17,
                    user_ns: population.second * 1000,
                    system_ns: 0,
                    energy_nj: population.second * 100,
                    instructions: 0,
                    super_instructions: 0,
                    cycles: 0,
                    idle_wakeups: 0,
                    interrupt_wakeups: 0,
                    pageins: 0,
                    resident_bytes: 1024,
                    footprint_bytes: 2048,
                    disk_read_bytes: 0,
                    disk_written_bytes: 0,
                },
                task: Task {
                    threads: 1,
                    faults: 0,
                    context_switches: 0,
                    mach_syscalls: 0,
                    unix_syscalls: 0,
                },
            });
        }
        fixture.publish_observation(population.second)?;
    }

    Ok(())
}

fn survives(pid: i32, population: &Population) -> bool {
    if pid <= 513 {
        pid <= population.members
    } else {
        assert!(pid >= 1001);

        pid <= 1000 + population.others
    }
}
