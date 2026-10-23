//! The App index: the App of each entry of a run's program dictionary, which the ring adapter
//! hands out numbered from 0 in the order the collector added them.

/// A program as the run's dictionary holds it: the executable's name, and the path of the
/// outermost `.app` bundle it runs from, if it runs from one.
#[derive(Clone, Copy)]
pub struct Program<'bytes> {
    pub executable_name: &'bytes [u8],
    pub app_path: Option<&'bytes [u8]>,
}

/// What the index holds at most, taken at startup.
#[derive(Clone, Copy)]
pub struct Capacity {
    /// Entries of a run's dictionary, at most 65,536 (collector/src/ring/identity.rs).
    pub entries: u32,
}

/// An App the index holds, numbered from 0 in the order the index first met it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct App(u32);

impl App {
    #[must_use]
    pub fn index(self) -> usize {
        crate::usize_from_u32(self.0)
    }
}

/// The App of each dictionary entry of one run.
pub struct Programs {
    apps: Vec<App>,
}

impl Programs {
    #[must_use]
    pub fn new(capacity: Capacity) -> Self {
        Self {
            apps: Vec::with_capacity(crate::usize_from_u32(capacity.entries)),
        }
    }

    /// Indexes the dictionary's next entry and returns its number.
    pub fn add(&mut self, _program: Program<'_>) -> u32 {
        assert!(self.apps.len() < self.apps.capacity());

        let entry = self.count();
        self.apps.push(App(entry));
        entry
    }

    /// The App of `entry`.
    #[must_use]
    pub fn app(&self, entry: u32) -> App {
        self.apps[crate::usize_from_u32(entry)]
    }

    /// The Apps the index can hold at most, which bounds an App's number.
    #[must_use]
    pub fn apps_max(&self) -> usize {
        self.apps.capacity()
    }

    fn count(&self) -> u32 {
        crate::u32_from_length(self.apps.len())
    }
}
