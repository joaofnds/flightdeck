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
pub(crate) struct CompactMeasure {
    // Bytes keep the exact amount without imposing eight-byte alignment on every measure.
    known: [u8; 8],
    coverage: u32,
}

const MEMBERS_BITS: u32 = 15;
const MEMBERS_MASK: u32 = (1 << MEMBERS_BITS) - 1;
const OVERFLOWED: u32 = 1 << (2 * MEMBERS_BITS);

const _: () = {
    assert!(crate::apps::PROCESS_INTERVALS_MAX < 1 << MEMBERS_BITS);
    assert!(align_of::<HistoryKey>() <= align_of::<usize>());
    assert!(align_of::<CompactMeasure>() <= align_of::<u32>());
    assert!(size_of::<CompactMeasure>() <= 12);
    assert!(size_of::<App>() <= 96);
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
    pub(crate) fn capture(source: &Measure) -> Self {
        // Each count fits fifteen bits under the validated process population bound.
        let measured_members = u32::from(count(source.measured_members));
        let unavailable_members = u32::from(count(source.unavailable_members));

        Self {
            known: source.known.to_le_bytes(),
            coverage: measured_members
                | (unavailable_members << MEMBERS_BITS)
                | (u32::from(source.overflowed) * OVERFLOWED),
        }
    }

    pub(crate) fn measure(self) -> Measure {
        Measure {
            known: u64::from_le_bytes(self.known),
            measured_members: self.coverage & MEMBERS_MASK,
            unavailable_members: (self.coverage >> MEMBERS_BITS) & MEMBERS_MASK,
            overflowed: self.coverage & OVERFLOWED != 0,
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
