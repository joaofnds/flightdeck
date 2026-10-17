//! Actual owned-process observations; passing probes do not prove arbitrary kernel race safety.
#[cfg(test)]
#[path = "programs_native/allocation.rs"]
mod allocation;
#[cfg(test)]
#[path = "programs_native/fixture.rs"]
mod fixture;
#[cfg(test)]
#[path = "programs_native/sample.rs"]
mod sample;

use fixture::{Fixture, Mode};
use sample::Sample;

#[test]
fn associates_an_owned_spawned_nested_helper_with_its_acknowledged_image() {
    // Goal: a spawned process's accepted identity matches its controlled image and creation.
    // Method: hold a real nested helper at its startup handshake while acquiring metadata.
    const NAME: &str = "associates_an_owned_spawned_nested_helper_with_its_acknowledged_image";
    if fixture::child_role(NAME) {
        return;
    }
    let mut fixture = None;
    Fixture::start(&mut fixture, NAME, Mode::Spawn);
    let fixture = fixture.as_mut().unwrap();
    let mut sample = Sample::EMPTY;

    sample.capture(fixture.pid());

    sample.assert_image(&fixture.ack, &fixture.image_a, &fixture.app_a);
    sample.report("owned-spawn");
    fixture.finish();
}

macro_rules! exec_cases {
    ($($name:ident, $mode:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: accepted transition samples belong to the acknowledged creation and image.
            // Method: command real exec and compare every accepted version/path to child phases.
            if fixture::child_role(stringify!($name)) { return; }
            let mut fixture = None;
            Fixture::start(&mut fixture, stringify!($name), $mode);

            fixture.as_mut().unwrap().exercise_exec();
        }
    )*};
}

exec_cases! {
    associates_same_path_exec_with_changed_pid_version, Mode::SamePath;
    associates_cross_app_exec_with_the_outermost_bundle, Mode::CrossApp;
    associates_posix_spawn_setexec_with_the_acknowledged_image, Mode::SetExec;
}

#[test]
fn associates_an_owned_fork_with_its_inherited_nested_helper_image() {
    // Goal: a real ordinary fork inherits the controlled parent's executable/App image.
    // Method: hold its child in an async-signal-safe pipe read while the parent acknowledges it.
    const NAME: &str = "associates_an_owned_fork_with_its_inherited_nested_helper_image";
    if fixture::child_role(NAME) {
        return;
    }
    let mut fixture = None;
    Fixture::start(&mut fixture, NAME, Mode::Fork);
    let fixture = fixture.as_mut().unwrap();
    let mut sample = Sample::EMPTY;

    sample.capture(fixture.pid());

    sample.assert_image(&fixture.ack, &fixture.image_a, &fixture.app_a);
    sample.report("ordinary-fork");
    fixture.finish();
}

#[test]
fn retains_numeric_attempts_and_independent_creation_while_an_owned_child_exits() {
    // Goal: exit cannot turn incomplete acquisition into fabricated executable attribution.
    // Method: release the acknowledged child, sample exit, inspect its zombie, then reap it.
    const NAME: &str =
        "retains_numeric_attempts_and_independent_creation_while_an_owned_child_exits";
    if fixture::child_role(NAME) {
        return;
    }
    let mut fixture = None;
    Fixture::start(&mut fixture, NAME, Mode::Spawn);

    fixture.as_mut().unwrap().exercise_exit();
}

#[test]
fn observes_port_entry_occupancy_and_allocations_on_repeated_actual_acquisition() {
    // Goal: repeated actual acquisitions leave observed task port-entry occupancy unchanged.
    // Method: measure 1000 acquisitions in an isolated owned test process with fixed SDK output.
    const NAME: &str =
        "observes_port_entry_occupancy_and_allocations_on_repeated_actual_acquisition";
    if fixture::child_role(NAME) {
        return;
    }
    let mut fixture = None;
    Fixture::start(&mut fixture, NAME, Mode::Ports);

    fixture.as_mut().unwrap().exercise_ports();
}

#[test]
fn publishes_actual_sweep_identity_without_rust_allocation_on_two_ticks() {
    // Goal: actual collection and publication retain owned bytes without hot-loop allocation.
    // Method: hold a nested helper, sweep twice, and resolve references through that exact ring.
    const NAME: &str = "publishes_actual_sweep_identity_without_rust_allocation_on_two_ticks";
    if fixture::child_role(NAME) {
        return;
    }
    let mut fixture = None;
    Fixture::start(&mut fixture, NAME, Mode::Spawn);

    fixture.as_mut().unwrap().exercise_sweep();
    fixture.as_mut().unwrap().finish();
}
