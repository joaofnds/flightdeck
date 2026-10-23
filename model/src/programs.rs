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
    /// The bytes of every entry's executable name and bundle path together, which the run's
    /// dictionary bounds with its arena.
    pub program_bytes: u32,
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

/// The program and the App of each dictionary entry of one run, and each App's key.
pub struct Programs {
    /// Each entry's program and App, none until the index meets the entry.
    entries: Vec<Option<Entry>>,
    /// The bytes of every entry's program, end to end.
    arena: Vec<u8>,
    /// Each App's key, as a range of `arena`.
    keys: Vec<KeyRange>,
    /// The Apps in key order.
    ordered: Vec<App>,
    /// Each App's place in `ordered`.
    ranks: Vec<u32>,
    /// The entries `intern` numbered, in program order.
    interned: Vec<u32>,
}

/// What the index holds of one entry.
#[derive(Clone, Copy)]
struct Entry {
    app: App,
    executable_name: Bytes,
    app_path: Option<Bytes>,
}

/// Where some bytes lie in the index's arena.
#[derive(Clone, Copy)]
struct Bytes {
    start: u32,
    end: u32,
}

/// Where an App's key lies in the index's arena.
#[derive(Clone, Copy)]
struct KeyRange {
    bundle: bool,
    bytes: Bytes,
}

impl Programs {
    #[must_use]
    pub fn new(capacity: Capacity) -> Self {
        let entries_max = crate::usize_from_u32(capacity.entries);
        Self {
            entries: vec![None; entries_max],
            arena: Vec::with_capacity(crate::usize_from_u32(capacity.program_bytes)),
            keys: Vec::with_capacity(entries_max),
            ordered: Vec::with_capacity(entries_max),
            ranks: Vec::with_capacity(entries_max),
            interned: Vec::with_capacity(entries_max),
        }
    }

    /// Indexes `entry`, which the index has not met, as the entry of `program`, which fits the
    /// bytes the index has left, since the run's dictionary holds no more than the capacity it
    /// was sized to.
    pub fn add(&mut self, entry: u32, program: Program<'_>) {
        assert!(!self.knows(entry));
        assert!(self.interned.is_empty());

        self.entries[crate::usize_from_u32(entry)] = Some(self.add_entry(program));
    }

    /// The entry of `program`, for a source that names each program by its bytes rather than by
    /// its entry in the run's dictionary, which gets one entry for each program, numbered from 0
    /// in the order the index first meets each. Gives `None` where the index has no room for a
    /// program it has not met. An index takes its entries from one kind of source.
    pub fn intern(&mut self, program: Program<'_>) -> Option<u32> {
        let position = match self.intern_search(program) {
            Ok(position) => return Some(self.interned[position]),
            Err(position) => position,
        };
        let entry = crate::u32_from_length(self.interned.len());
        if self.interned.len() == self.entries.len() {
            return None;
        }
        if self.knows(entry) {
            unreachable!("an index takes its entries from one kind of source");
        }
        if program_size(program) > self.arena.capacity() - self.arena.len() {
            return None;
        }

        self.entries[crate::usize_from_u32(entry)] = Some(self.add_entry(program));
        self.interned.insert(position, entry);
        Some(entry)
    }

    /// Whether the index has met `entry`.
    #[must_use]
    pub fn knows(&self, entry: u32) -> bool {
        self.entries[crate::usize_from_u32(entry)].is_some()
    }

    /// The App of `entry`, which the index has met.
    #[must_use]
    pub fn app(&self, entry: u32) -> App {
        self.entry(entry).app
    }

    /// The program of `entry`, which the index has met.
    #[must_use]
    pub fn program(&self, entry: u32) -> Program<'_> {
        let entry = self.entry(entry);
        Program {
            executable_name: self.bytes(entry.executable_name),
            app_path: entry.app_path.map(|path| self.bytes(path)),
        }
    }

    /// The key of `app`.
    #[must_use]
    pub fn key(&self, app: App) -> AppKey<'_> {
        let range = self.keys[app.index()];
        let bytes = self.bytes(range.bytes);
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

    /// The entries the index can hold at most, which bounds an entry's number.
    #[must_use]
    pub fn entries_max(&self) -> usize {
        self.entries.len()
    }

    /// The Apps the index can hold at most, which bounds an App's number.
    #[must_use]
    pub fn apps_max(&self) -> usize {
        self.keys.capacity()
    }

    fn entry(&self, entry: u32) -> Entry {
        let Some(entry) = self.entries[crate::usize_from_u32(entry)] else {
            unreachable!("the App rule reads only the entries the index has met");
        };
        entry
    }

    fn bytes(&self, bytes: Bytes) -> &[u8] {
        &self.arena[crate::usize_from_u32(bytes.start)..crate::usize_from_u32(bytes.end)]
    }

    /// The place of `program` among the interned entries in program order, or where it would
    /// be.
    fn intern_search(&self, program: Program<'_>) -> Result<usize, usize> {
        self.interned.binary_search_by(|entry| {
            let held = self.program(*entry);
            (held.executable_name, held.app_path).cmp(&(program.executable_name, program.app_path))
        })
    }

    /// Copies `program` into the arena and finds its App, which it adds where the index has
    /// not met the App's key.
    fn add_entry(&mut self, program: Program<'_>) -> Entry {
        assert!(program_size(program) <= self.arena.capacity() - self.arena.len());

        let executable_name = self.add_bytes(program.executable_name);
        let app_path = program.app_path.map(|path| self.add_bytes(path));
        let key = match app_path {
            Some(path) => KeyRange {
                bundle: true,
                bytes: path,
            },
            None => KeyRange {
                bundle: false,
                bytes: executable_name,
            },
        };
        let app = match self.search(program.key()) {
            Ok(position) => self.ordered[position],
            Err(position) => self.add_app(key, position),
        };
        Entry {
            app,
            executable_name,
            app_path,
        }
    }

    fn add_bytes(&mut self, bytes: &[u8]) -> Bytes {
        let start = crate::u32_from_length(self.arena.len());
        self.arena.extend_from_slice(bytes);
        Bytes {
            start,
            end: crate::u32_from_length(self.arena.len()),
        }
    }

    /// The place of `key`'s App in key order, or where it would be.
    fn search(&self, key: AppKey<'_>) -> Result<usize, usize> {
        self.ordered
            .binary_search_by(|app| self.key(*app).cmp(&key))
    }

    /// Adds the App of `key`, whose place in key order is `position`.
    fn add_app(&mut self, key: KeyRange, position: usize) -> App {
        assert!(self.keys.len() < self.keys.capacity());
        assert!(self.ordered.len() < self.ordered.capacity());

        let app = App(crate::u32_from_length(self.keys.len()));
        self.keys.push(key);
        self.ordered.insert(position, app);
        self.ranks.push(0);
        for (rank, later) in self.ordered.iter().enumerate().skip(position) {
            self.ranks[later.index()] = crate::u32_from_length(rank);
        }
        app
    }
}

/// The bytes the index's arena takes of `program`.
fn program_size(program: Program<'_>) -> usize {
    program.executable_name.len() + program.app_path.map_or(0, <[u8]>::len)
}

#[cfg(test)]
mod tests;
