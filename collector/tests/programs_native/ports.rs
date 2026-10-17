use std::mem::{align_of, offset_of, size_of};
use std::path::Path;
use std::time::Instant;

use crate::fixture::{Ack, HELPER};
use crate::sample::Sample;

pub fn observe(ack: &Ack, root: &Path) {
    let app = root.join("A.app");
    let image = app.join(HELPER);
    let mut sample = Sample::EMPTY;
    sample.capture(ack.pid);
    sample.assert_image(ack, &image, &app);
    let warm = entries().unwrap_or_else(|status| {
        panic!("warm port-entry measurement failed: raw Mach status={status}")
    });
    let before = entries().unwrap_or_else(|status| {
        panic!("before port-entry measurement failed: raw Mach status={status}")
    });
    let started = Instant::now();
    let mut allocations = 0_u64;
    for _attempt in 0..1000 {
        sample.capture(ack.pid);
        sample.assert_image(ack, &image, &app);
        allocations += sample.allocations;
    }
    let elapsed_ns = started.elapsed().as_nanos();
    let after = entries().unwrap_or_else(|status| {
        panic!("after port-entry measurement failed: raw Mach status={status}")
    });
    eprintln!(
        "port_entry_observation warm={warm:?} before={before:?} after={after:?} \
        actual_self_acquisitions=1000 elapsed_ns={elapsed_ns} rust_allocator_calls={allocations}"
    );
    assert_eq!(before, after);
    assert_eq!(0, allocations);
    // Entry occupancy is not per-name user-reference accounting or universal leak proof.
}

fn entries() -> Result<u32, i32> {
    let mut info = IpcInfoSpaceBasic {
        iisb_genno_mask: 0,
        iisb_table_size: 0,
        iisb_table_next: 0,
        iisb_table_inuse: 0,
        iisb_reserved: [0; 2],
    };
    let status = unsafe { mach_port_space_basic_info(mach_task_self_, &raw mut info) };
    if status != 0 {
        return Err(status);
    }
    Ok(info.iisb_table_inuse)
}

#[repr(C)]
#[allow(clippy::struct_field_names)] // Mirrors SDK mach_debug/ipc_info.h.
struct IpcInfoSpaceBasic {
    iisb_genno_mask: u32,
    iisb_table_size: u32,
    iisb_table_next: u32,
    iisb_table_inuse: u32,
    iisb_reserved: [u32; 2],
}

const _: () = {
    assert!(size_of::<IpcInfoSpaceBasic>() == 24);
    assert!(align_of::<IpcInfoSpaceBasic>() == 4);
    assert!(offset_of!(IpcInfoSpaceBasic, iisb_table_inuse) == 12);
};

unsafe extern "C" {
    static mach_task_self_: u32;
    fn mach_port_space_basic_info(task: u32, information: *mut IpcInfoSpaceBasic) -> i32;
}
