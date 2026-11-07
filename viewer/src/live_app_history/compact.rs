use crate::live_app_response::Measure;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    assert!(align_of::<CompactMeasure>() <= align_of::<u32>());
    assert!(size_of::<CompactMeasure>() <= 12);
};

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
