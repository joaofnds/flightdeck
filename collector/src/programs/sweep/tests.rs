use super::*;
mod fixture;
use fixture::Harness;

#[test]
fn associates_each_unique_pid_with_its_numeric_reading() {
    // Goal: deduplicated sorted metadata and usage remain one-to-one.
    // Method: sweep repeated unordered PIDs through a real identity dictionary.
    let mut harness = Harness::new(4);

    harness.run(&[7, 2, 7, 4], Ok(fixture::process()));

    assert_eq!(harness.listing, [2, 4, 7]);
    assert_eq!(harness.calls, 3);
    assert_eq!(
        harness
            .sweep
            .observations()
            .iter()
            .zip(&harness.numeric.readings)
            .map(|(identity, process)| (
                identity.pid,
                identity.creation().unwrap().unique_id,
                process.usage.user_ns
            ))
            .collect::<Vec<_>>(),
        [(2, 9, 2), (4, 11, 4), (7, 14, 7)]
    );
    assert_eq!(
        harness
            .numeric
            .readings
            .iter()
            .map(|value| value.pid)
            .collect::<Vec<_>>(),
        [2, 4, 7]
    );
    assert_eq!(
        harness
            .sweep
            .observations()
            .iter()
            .map(|value| value.pid)
            .collect::<Vec<_>>(),
        [2, 4, 7]
    );
}

#[test]
fn preserves_identity_when_numeric_usage_is_refused() {
    // Goal: refusal of task usage does not discard readable executable identity.
    // Method: pass refused numeric data beside a stable creation and raw path.
    let mut harness = Harness::new(4);

    harness.run(&[2], Err(Unreadable::Refused));

    assert_eq!(harness.numeric.refused, 1);
    assert!(harness.numeric.readings.is_empty());
    assert_eq!(
        harness.sweep.observations()[0].usage,
        UsageAvailability::Refused
    );
    assert!(harness.sweep.observations()[0].program().is_some());
}

#[test]
fn refuses_attribution_after_the_creation_version_changes() {
    // Goal: an exec during acquisition cannot label numeric usage with the new program.
    // Method: change only the trailing PID version and retain a numeric reading.
    let mut harness = Harness::new(4);

    harness.run_amended(&[2], Ok(fixture::process()), |_, acquired, _| {
        acquired.creation_after = Ok(crate::ring::ProcessCreation {
            unique_id: 9,
            pid_version: 4,
        });
        acquired.candidate = false;
    });

    assert_eq!(harness.numeric.readings, [fixture::process()]);
    assert_eq!(
        harness.sweep.observations()[0].identity,
        ProcessIdentity::Unavailable {
            creation: None,
            reason: IdentityUnavailable::Changed
        }
    );
}

#[test]
fn takes_identity_room_once_for_every_listed_process() {
    // Goal: opt-in identity storage never needs to grow during collection.
    // Method: construct a sweep against a fixed process ceiling.
    let sweep = Sweep::new(23);

    assert!(sweep.observations().is_empty());
    assert!(sweep.capacity() >= 23);
}

macro_rules! unavailable_cases {
    ($($name:ident, $amend:expr, $reason:expr, $creation:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: unavailable identity never destroys or relabels numeric readings.
            // Method: apply one named native failure to a real dictionary-backed sweep.
            let mut harness = Harness::new(4);

            harness.run_amended(&[2], Ok(fixture::process()), $amend);

            assert_eq!(harness.numeric.readings, [fixture::process()]);
            assert_eq!(harness.sweep.observations()[0].identity,
                ProcessIdentity::Unavailable { creation: $creation, reason: $reason });
            assert_eq!(harness.sweep.diagnostics().measured, 0);
            assert!(harness.sweep.diagnostics().last_failure.is_some());
        }
    )*};
}

const STABLE: Option<crate::ring::ProcessCreation> = Some(crate::ring::ProcessCreation {
    unique_id: 9,
    pid_version: 3,
});

unavailable_cases! {
    retains_refused_creation, |_, value, _| {
        value.creation_before = Err(super::super::CreationError::Refused(1));
        value.candidate = false;
    }, IdentityUnavailable::Refused, None;
    retains_gone_creation, |_, value, _| {
        value.creation_after = Err(super::super::CreationError::Gone(3));
        value.candidate = false;
    }, IdentityUnavailable::Gone, None;
    refuses_wrong_width_creation, |_, value, _| {
        value.creation_before = Err(super::super::CreationError::Size(55));
        value.candidate = false;
    }, IdentityUnavailable::Invalid, None;
    preserves_unknown_creation_errno, |_, value, _| {
        value.creation_after = Err(super::super::CreationError::Syscall(14));
        value.candidate = false;
    }, IdentityUnavailable::Unsupported, None;
    retains_unsupported_creation_flavor, |_, value, _| {
        value.creation_before = Err(super::super::CreationError::Unsupported(22));
        value.candidate = false;
    }, IdentityUnavailable::Unsupported, None;
    retains_readiness_failure_without_inventing_permission, |_, value, _| {
        value.readiness = Err(super::super::ReadinessError::Kernel { status: 5, name: 0 });
        value.path = None;
        value.candidate = false;
    }, IdentityUnavailable::Unsupported, STABLE;
    retains_refused_path_with_stable_creation, |_, value, _| {
        value.path = Some(Err(super::super::PathError::Refused(13)));
        value.candidate = false;
    }, IdentityUnavailable::Refused, STABLE;
    retains_gone_path_with_stable_creation, |_, value, _| {
        value.path = Some(Err(super::super::PathError::Gone(2)));
        value.candidate = false;
    }, IdentityUnavailable::Gone, STABLE;
    retains_invalid_path_with_stable_creation, |_, value, _| {
        value.path = Some(Err(super::super::PathError::EmbeddedNul));
        value.candidate = false;
    }, IdentityUnavailable::Invalid, STABLE;
    preserves_unknown_path_errno, |_, value, _| {
        value.path = Some(Err(super::super::PathError::Syscall(14)));
        value.candidate = false;
    }, IdentityUnavailable::Unsupported, STABLE;
    refuses_a_changed_unique_id, |_, value, _| {
        value.creation_after = Ok(crate::ring::ProcessCreation { unique_id: 10, pid_version: 3 });
        value.candidate = false;
    }, IdentityUnavailable::Changed, None;
}

#[test]
fn exposes_raw_readiness_failure_in_fixed_diagnostics() {
    // Goal: a Mach failure remains inspectable without pretending to be EPERM.
    // Method: retain raw task-name status/name with matched creation reads.
    let mut harness = Harness::new(4);

    harness.run_amended(&[2], Ok(fixture::process()), |_, acquired, _| {
        acquired.readiness = Err(super::super::ReadinessError::Kernel {
            status: 5,
            name: 17,
        });
        acquired.path = None;
        acquired.candidate = false;
    });

    let diagnostics = harness.sweep.diagnostics();
    assert_eq!(diagnostics.unsupported, 1);
    assert_eq!(diagnostics.refused, 0);
    assert_eq!(diagnostics.unexpected_syscalls, 0);
    let failure = diagnostics.last_failure.as_ref().unwrap();
    assert_eq!(failure.pid, 2);
    assert_eq!(
        failure.acquired.readiness,
        Err(super::super::ReadinessError::Kernel {
            status: 5,
            name: 17
        })
    );
}

#[test]
fn exposes_unexpected_native_errno_without_allocating_a_message() {
    // Goal: native errno outside supported refusal cases does not disappear in a coarse reason.
    // Method: retain an EFAULT path failure alongside its explicit diagnostic count.
    let mut harness = Harness::new(4);

    harness.run_amended(&[2], Ok(fixture::process()), |_, acquired, _| {
        acquired.path = Some(Err(super::super::PathError::Syscall(14)));
        acquired.candidate = false;
    });

    let diagnostics = harness.sweep.diagnostics();
    assert_eq!(diagnostics.unexpected_syscalls, 1);
    assert_eq!(diagnostics.unsupported, 1);
    assert_eq!(
        diagnostics.last_failure.as_ref().unwrap().acquired.path,
        Some(Err(super::super::PathError::Syscall(14)))
    );
}

#[test]
fn preserves_numeric_data_and_known_references_after_dictionary_exhaustion() {
    // Goal: exhausted new attribution leaves numeric readings and old programs valid.
    // Method: fill one entry, attempt a different executable, then repeat the known key.
    let mut harness = Harness::new(1);
    harness.run(&[2], Ok(fixture::process()));
    let known = harness.sweep.observations()[0].program().copied().unwrap();

    harness.run_amended(&[2], Ok(fixture::process()), |_, _, scratch| {
        scratch[11] = b'y';
    });

    assert_eq!(harness.numeric.readings, [fixture::process()]);
    assert_eq!(
        harness.sweep.observations()[0].identity,
        ProcessIdentity::Unavailable {
            creation: STABLE,
            reason: IdentityUnavailable::Exhausted
        }
    );
    assert_eq!(harness.sweep.diagnostics().exhausted, 1);
    assert_eq!(
        harness
            .sweep
            .diagnostics()
            .last_failure
            .as_ref()
            .unwrap()
            .dictionary,
        Some(IdentityAvailability::Exhausted)
    );
    harness.run(&[2], Ok(fixture::process()));
    assert_eq!(harness.sweep.observations()[0].program(), Some(&known));
    assert_eq!(harness.sweep.diagnostics().measured, 1);
    assert_eq!(harness.sweep.diagnostics().exhausted, 0);
}

#[test]
fn publishes_refused_numeric_usage_with_original_program_bytes() {
    // Goal: the sweep supplies publishable identity even without numeric usage.
    // Method: publish, decode through the exact reader and compare raw non-UTF-8 bytes.
    let mut harness = Harness::new(4);
    harness.run(&[2], Err(Unreadable::Refused));
    let mut reader = crate::ring::RingReader::open_generation(harness.writer.name()).unwrap();
    let mut tick = fixture::tick();
    let mut identities = Vec::with_capacity(4);

    harness.publish();
    let found = reader
        .read_identity_into(&mut tick, &mut identities)
        .unwrap();
    let mut executable = [0; crate::ring::PROGRAM_PATH_BYTES_MAX];
    let mut app = [0; crate::ring::PROGRAM_PATH_BYTES_MAX];
    let lengths = reader
        .copy_program(
            identities[0].program().unwrap(),
            &mut crate::ring::ProgramBuffers {
                executable_name: &mut executable,
                app_path: &mut app,
            },
        )
        .unwrap();

    assert_eq!(
        found,
        crate::ring::Found::Publication {
            publication: 1,
            tick: 1
        }
    );
    assert!(tick.processes.is_empty());
    assert_eq!(tick.processes_refused, 1);
    assert_eq!(identities[0].usage, UsageAvailability::Refused);
    assert_eq!(&executable[..lengths.0], b"x\xff");
    assert_eq!(&app[..lengths.1], b"/A.app");
}

#[test]
fn preserves_known_programs_after_the_dictionary_arena_fills() {
    // Goal: arena exhaustion refuses only new attribution, never existing references.
    // Method: fill one 8-byte arena allocation and request a new same-sized key.
    let mut harness = Harness::with_capacity(crate::ring::DictionaryCapacity {
        entries: 4,
        arena_bytes: 8,
    });
    harness.run(&[2], Ok(fixture::process()));
    let known = harness.sweep.observations()[0].program().copied().unwrap();

    harness.run_amended(&[2], Ok(fixture::process()), |_, _, scratch| {
        scratch[11] = b'y';
    });

    assert_eq!(harness.numeric.readings, [fixture::process()]);
    assert_eq!(
        harness.sweep.observations()[0].identity,
        ProcessIdentity::Unavailable {
            creation: STABLE,
            reason: IdentityUnavailable::Exhausted
        }
    );
    harness.publish();
    harness.run(&[2], Ok(fixture::process()));
    assert_eq!(harness.sweep.observations()[0].program(), Some(&known));
    harness.publish();
}

#[test]
fn retains_gone_numeric_counts_beside_unavailable_identity() {
    // Goal: a vanished process occupies its own aligned identity row and numeric count.
    // Method: return ESRCH for creation and Gone for usage through the same sweep attempt.
    let mut harness = Harness::new(4);

    harness.run_amended(&[2], Err(Unreadable::Gone), |_, acquired, _| {
        acquired.creation_before = Err(super::super::CreationError::Gone(3));
        acquired.creation_after = acquired.creation_before;
        acquired.candidate = false;
    });

    assert_eq!(harness.numeric.gone, 1);
    assert_eq!(harness.numeric.refused, 0);
    assert!(harness.numeric.readings.is_empty());
    assert_eq!(
        harness.sweep.observations()[0],
        IdentityObservation {
            pid: 2,
            usage: UsageAvailability::Gone,
            identity: ProcessIdentity::Unavailable {
                creation: None,
                reason: IdentityUnavailable::Gone
            }
        }
    );
    harness.publish();
}

#[test]
fn propagates_an_unexpected_numeric_error_without_publishing_a_partial_row() {
    // Goal: existing fatal numeric source errors retain their original errno.
    // Method: fail the numeric source seam with EINVAL and inspect the returned error.
    let mut harness = Harness::new(4);
    harness.listing.push(2);

    let result = harness.sweep.run_with(
        &mut harness.numeric,
        &mut harness.listing,
        &mut harness.writer,
        |_, _, _| Err(io::Error::from_raw_os_error(22)),
    );

    assert_eq!(result.unwrap_err().raw_os_error(), Some(22));
    assert!(harness.sweep.observations().is_empty());
    assert!(harness.numeric.readings.is_empty());
}

#[test]
fn counts_each_unexpected_native_call_and_retains_both_creation_errors() {
    // Goal: one coarse unavailability record retains distinct native syscall failures.
    // Method: inject EFAULT before and after acquisition and zero errno from the path call.
    let mut harness = Harness::new(4);

    harness.run_amended(&[2], Ok(fixture::process()), |_, acquired, _| {
        acquired.creation_before = Err(super::super::CreationError::Syscall(14));
        acquired.creation_after = Err(super::super::CreationError::Syscall(14));
        acquired.path = Some(Err(super::super::PathError::Syscall(0)));
        acquired.candidate = false;
    });

    let diagnostics = harness.sweep.diagnostics();
    assert_eq!(diagnostics.unexpected_syscalls, 3);
    let failure = diagnostics.last_failure.as_ref().unwrap();
    assert_eq!(
        failure.acquired.creation_before,
        Err(super::super::CreationError::Syscall(14))
    );
    assert_eq!(
        failure.acquired.creation_after,
        Err(super::super::CreationError::Syscall(14))
    );
    assert_eq!(
        failure.acquired.path,
        Some(Err(super::super::PathError::Syscall(0)))
    );
}

#[test]
fn retains_associations_across_generated_pid_order_and_duplicate_patterns() {
    // Goal: input ordering and duplicates never change aligned creation or numeric readings.
    // Method: generate 128 bounded listings and compare output to an independent ordered set.
    let mut harness = Harness::new(4);
    let mut seed = 0x6df2_3471_u32;

    for _case in 0..128 {
        let mut listed = [0; 4];
        for pid in &mut listed {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *pid = i32::try_from((seed >> 12) % 4 + 1).unwrap();
        }
        let expected: std::collections::BTreeSet<i32> = listed.iter().copied().collect();
        let calls_before = harness.calls;

        harness.run(&listed, Ok(fixture::process()));

        assert_eq!(
            harness.listing,
            expected.iter().copied().collect::<Vec<_>>(),
            "listing={listed:?}"
        );
        assert_eq!(
            harness.calls - calls_before,
            u32::try_from(expected.len()).unwrap(),
            "listing={listed:?}"
        );
        for (identity, reading) in harness
            .sweep
            .observations()
            .iter()
            .zip(&harness.numeric.readings)
        {
            assert_eq!(identity.pid, reading.pid, "listing={listed:?}");
            assert_eq!(
                identity.creation().unwrap().unique_id,
                u64::try_from(reading.pid).unwrap() + 7,
                "listing={listed:?}"
            );
            assert_eq!(
                reading.usage.user_ns,
                u64::try_from(reading.pid).unwrap(),
                "listing={listed:?}"
            );
        }
        assert_eq!(harness.sweep.capacity(), 4);
        assert_eq!(harness.numeric.readings.capacity(), 4);
    }
}

#[test]
fn clears_scheduled_identity_when_collection_returns_to_numeric_only() {
    // Goal: the original numeric path cannot expose identities from a previous opt-in tick.
    // Method: read an actual identity tick, then use the unchanged numeric-only entry point.
    let timebase = Timebase::read().unwrap();
    let mut collector =
        crate::collector::Collector::new(timebase, std::time::Duration::ZERO).unwrap();
    collector.smc = None;
    collector.ioreport = None;
    let mut harness = Harness::new(4);
    let mut meter = crate::cost::Meter::default();

    collector
        .read_with_identity(&mut meter, &mut harness.writer)
        .unwrap();
    let attempted = collector.processes.readings.len()
        + usize::try_from(collector.processes.refused + collector.processes.gone).unwrap();
    assert_eq!(collector.identities.observations().len(), attempted);
    assert!(attempted > 0);
    collector.read(&mut meter).unwrap();

    assert!(collector.identities.observations().is_empty());
    assert_eq!(collector.identities.diagnostics().measured, 0);
    assert!(collector.identities.diagnostics().last_failure.is_none());
    assert!(!collector.processes.readings.is_empty());
}

#[test]
fn counts_a_missing_gate_independently_of_a_gone_creation_reason() {
    // Goal: gate coverage counts every acquisition failure, not only Unsupported outcomes.
    // Method: return Gone creation plus a failed task-name gate in the same attempt.
    let mut harness = Harness::new(4);

    harness.run_amended(&[2], Err(Unreadable::Gone), |_, acquired, _| {
        acquired.creation_before = Err(super::super::CreationError::Gone(3));
        acquired.readiness = Err(super::super::ReadinessError::Kernel { status: 5, name: 0 });
        acquired.path = None;
        acquired.candidate = false;
    });

    assert_eq!(harness.sweep.diagnostics().readiness_unavailable, 1);
    assert_eq!(harness.sweep.diagnostics().gone, 1);
    assert_eq!(harness.sweep.diagnostics().unsupported, 0);
}

#[test]
fn counts_no_missing_gate_when_the_readiness_witness_is_valid() {
    // Goal: measured task-name witnesses never add to unavailable gate coverage.
    // Method: acquire one stable candidate through a valid gate.
    let mut harness = Harness::new(4);

    harness.run(&[2], Ok(fixture::process()));

    assert_eq!(harness.sweep.diagnostics().readiness_unavailable, 0);
    assert_eq!(harness.sweep.diagnostics().measured, 1);
}
