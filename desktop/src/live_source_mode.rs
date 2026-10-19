#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SourceMode {
    #[default]
    Live,
    Recorded,
}

impl SourceMode {
    pub fn title(self) -> &'static str {
        match self {
            Self::Live => "Flightdeck · Live",
            Self::Recorded => "Flightdeck · Recorded",
        }
    }

    pub fn sidebar(self) -> &'static str {
        match self {
            Self::Live => "Live system readings",
            Self::Recorded => "Recorded system readings",
        }
    }

    pub fn footer(self) -> &'static str {
        match self {
            Self::Live => "Readings from this Mac's collector · charts show the last 15 minutes",
            Self::Recorded => "Readings from an opened recording · charts show 15 minutes",
        }
    }

    pub fn following_label(self, paused: bool) -> &'static str {
        match (self, paused) {
            (Self::Live, true) => "Live",
            (Self::Live, false) => "Pause live",
            (Self::Recorded, true) => "Latest",
            (Self::Recorded, false) => "Pause latest",
        }
    }
}
