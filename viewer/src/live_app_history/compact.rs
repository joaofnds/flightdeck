use std::cmp::Ordering;
use std::sync::Arc;

use crate::apps::AppKey;
use crate::live_app_response::{Key, KeyKind, Measure};

use super::App;

pub(super) struct HistoryKey {
    kind: KeyKind,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy)]
pub(super) struct CompactMeasure {
    known: u64,
    measured_members: u16,
    unavailable_members: u16,
    overflowed: bool,
}

const _: () = {
    assert!(crate::apps::PROCESS_INTERVALS_MAX < 65_536);
    assert!(align_of::<HistoryKey>() <= align_of::<usize>());
    assert!(size_of::<CompactMeasure>() <= 16);
    assert!(size_of::<App>() <= 80);
};

impl HistoryKey {
    pub(super) fn view(&self) -> AppKey<'_> {
        super::key(self.kind, &self.bytes)
    }

    pub(super) fn bytes(&self) -> usize {
        // Arc retains two reference counts beside the owned key and its byte allocation.
        2 * size_of::<usize>() + size_of::<Self>() + self.bytes.capacity()
    }
}

impl CompactMeasure {
    pub(super) fn capture(source: &Measure) -> Self {
        Self {
            known: source.known,
            measured_members: count(source.measured_members),
            unavailable_members: count(source.unavailable_members),
            overflowed: source.overflowed,
        }
    }

    fn measure(self) -> Measure {
        Measure {
            known: self.known,
            measured_members: u32::from(self.measured_members),
            unavailable_members: u32::from(self.unavailable_members),
            overflowed: self.overflowed,
        }
    }

    pub(super) fn json(self) -> serde_json::Value {
        self.measure().json()
    }

    pub(super) fn measured(self) -> Option<u64> {
        self.measure().measured()
    }
}

pub(super) fn count(value: u32) -> u16 {
    let maximum = u32::try_from(crate::apps::PROCESS_INTERVALS_MAX)
        .unwrap_or_else(|_| unreachable!("App member limit fits u32"));
    assert!(value <= maximum);

    u16::try_from(value).unwrap_or_else(|_| unreachable!("App member limit fits u16"))
}

pub(super) fn capture_key(
    source: &Key,
    previous: &[App],
    previous_index: &mut usize,
) -> Arc<HistoryKey> {
    let target = super::key(source.kind, &source.bytes);

    for _attempt in 0..=previous.len() {
        let Some(app) = previous.get(*previous_index) else {
            break;
        };

        match app.key.view().cmp(&target) {
            Ordering::Less => *previous_index += 1,
            Ordering::Equal => return Arc::clone(&app.key),
            Ordering::Greater => break,
        }
    }

    Arc::new(HistoryKey {
        kind: source.kind,
        bytes: source.bytes.clone(),
    })
}
