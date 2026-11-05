//! What a recording lists of a run, for test crates that check a real persister's work.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use collector::ring::RunIdentity;
use persistence::Manifest;
use persistence::canonical::ReadWork;
use persistence::canonical::catalog::Catalog;
use persistence::resources::ByteBudget;

/// The windows of `run` the recording lists, newest first, read again while a publication stands
/// in the way, since a reader that meets one learns nothing.
pub fn saved_manifests(root: &Path, run: &RunIdentity) -> Vec<Manifest> {
    const READS_MAX: u32 = 3000;
    for _read in 0..READS_MAX {
        let budget = Arc::new(ByteBudget::new(1 << 28));
        let read = Catalog::open(root, &budget)
            .and_then(|catalog| catalog.windows_of(run, &mut ReadWork::default()));
        match read {
            Ok(windows) => return windows,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("{error}"),
        }
    }
    panic!("the recording stayed busy");
}
