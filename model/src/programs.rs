//! The App index: the App of each entry of a run's program dictionary, which the ring adapter
//! hands out numbered from 0, in the order a tick first names each.

/// A program as the run's dictionary holds it: the executable's name, and the path of the
/// outermost `.app` bundle it runs from, if it runs from one.
#[derive(Clone, Copy)]
pub struct Program<'bytes> {
    pub executable_name: &'bytes [u8],
    pub app_path: Option<&'bytes [u8]>,
}

impl<'bytes> Program<'bytes> {
    /// The App the program belongs to: its bundle, or its executable's name outside one.
    fn key(self) -> AppKey<'bytes> {
        match self.app_path {
            Some(path) => AppKey::Bundle(path),
            None => AppKey::Executable(self.executable_name),
        }
    }
}

/// What tells one App from another, in the order the Apps of a tick come out in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppKey<'bytes> {
    Bundle(&'bytes [u8]),
    Executable(&'bytes [u8]),
}

/// What the index holds at most, taken at startup.
#[derive(Clone, Copy)]
pub struct Capacity {
    /// Entries of a run's dictionary, at most 65,536 (collector/src/ring/identity.rs).
    pub entries: u32,
    /// The bytes of every App's key together.
    pub key_bytes: usize,
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

/// The App of each dictionary entry of one run, and each App's key.
pub struct Programs {
    /// The App of each entry, none until the index meets the entry.
    entries: Vec<Option<App>>,
    /// Each App's key, as a range of `arena`.
    keys: Vec<KeyRange>,
    arena: Vec<u8>,
    /// The Apps in key order.
    ordered: Vec<App>,
    /// Each App's place in `ordered`.
    ranks: Vec<u32>,
}

/// Where an App's key lies in the index's arena.
#[derive(Clone, Copy)]
struct KeyRange {
    bundle: bool,
    start: usize,
    end: usize,
}

impl Programs {
    #[must_use]
    pub fn new(capacity: Capacity) -> Self {
        let entries_max = crate::usize_from_u32(capacity.entries);
        Self {
            entries: vec![None; entries_max],
            keys: Vec::with_capacity(entries_max),
            arena: Vec::with_capacity(capacity.key_bytes),
            ordered: Vec::with_capacity(entries_max),
            ranks: Vec::with_capacity(entries_max),
        }
    }

    /// Indexes `entry`, which the index has not met, as the entry of `program`.
    pub fn add(&mut self, entry: u32, program: Program<'_>) {
        assert!(!self.knows(entry));

        let key = program.key();
        let app = match self.search(key) {
            Ok(position) => self.ordered[position],
            Err(position) => self.add_app(key, position),
        };
        self.entries[crate::usize_from_u32(entry)] = Some(app);
    }

    /// Whether the index has met `entry`.
    #[must_use]
    pub fn knows(&self, entry: u32) -> bool {
        self.entries[crate::usize_from_u32(entry)].is_some()
    }

    /// The App of `entry`, which the index has met.
    #[must_use]
    pub fn app(&self, entry: u32) -> App {
        let Some(app) = self.entries[crate::usize_from_u32(entry)] else {
            unreachable!("the App rule reads only the entries the index has met");
        };
        app
    }

    /// The key of `app`.
    #[must_use]
    pub fn key(&self, app: App) -> AppKey<'_> {
        let range = self.keys[app.index()];
        let bytes = &self.arena[range.start..range.end];
        if range.bundle {
            AppKey::Bundle(bytes)
        } else {
            AppKey::Executable(bytes)
        }
    }

    /// The place of `app` among the index's Apps in key order.
    #[must_use]
    pub fn rank(&self, app: App) -> u32 {
        self.ranks[app.index()]
    }

    /// The Apps the index can hold at most, which bounds an App's number.
    #[must_use]
    pub fn apps_max(&self) -> usize {
        self.keys.capacity()
    }

    /// The place of `key`'s App in key order, or where it would be.
    fn search(&self, key: AppKey<'_>) -> Result<usize, usize> {
        self.ordered
            .binary_search_by(|app| self.key(*app).cmp(&key))
    }

    /// Adds the App of `key`, whose place in key order is `position`.
    fn add_app(&mut self, key: AppKey<'_>, position: usize) -> App {
        assert!(self.keys.len() < self.keys.capacity());
        assert!(self.ordered.len() < self.ordered.capacity());

        let (bundle, bytes) = match key {
            AppKey::Bundle(bytes) => (true, bytes),
            AppKey::Executable(bytes) => (false, bytes),
        };
        assert!(bytes.len() <= self.arena.capacity() - self.arena.len());
        let start = self.arena.len();
        self.arena.extend_from_slice(bytes);

        let app = App(crate::u32_from_length(self.keys.len()));
        self.keys.push(KeyRange {
            bundle,
            start,
            end: self.arena.len(),
        });
        self.ordered.insert(position, app);
        self.ranks.push(0);
        for (rank, later) in self.ordered.iter().enumerate().skip(position) {
            self.ranks[later.index()] = crate::u32_from_length(rank);
        }
        app
    }
}

#[cfg(test)]
mod tests;
