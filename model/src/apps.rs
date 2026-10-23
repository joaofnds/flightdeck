//! The App rule: what each App's processes spent between two ticks of one collector run.
use crate::programs::{App, Programs};

/// The processes of one tick as columns of one length, sorted by pid and then creation.
pub struct Processes<'columns> {
    pub pid: &'columns [i32],
    /// The process's creation, its `unique_id`, which tells two processes of one pid apart.
    pub creation: &'columns [u64],
    /// The process's program, as its entry in the run's dictionary.
    pub program: &'columns [u32],
    pub user_ns: &'columns [u64],
    pub system_ns: &'columns [u64],
}

impl Processes<'_> {
    fn count(&self) -> usize {
        let count = self.pid.len();
        assert_eq!(self.creation.len(), count);
        assert_eq!(self.program.len(), count);
        assert_eq!(self.user_ns.len(), count);
        assert_eq!(self.system_ns.len(), count);
        count
    }

    /// The row of the process `pid` of `creation`, if the tick holds it.
    fn find(&self, pid: i32, creation: u64) -> Option<usize> {
        let count = self.count();
        let mut candidate = self.pid.partition_point(|candidate| *candidate < pid);
        while candidate < count {
            if self.pid[candidate] != pid {
                break;
            }
            if self.creation[candidate] == creation {
                return Some(candidate);
            }
            candidate += 1;
        }
        None
    }
}

/// What one App's processes spent between the two ticks.
#[derive(Debug, PartialEq, Eq)]
pub struct Totals {
    pub app: App,
    pub members: u32,
    pub cpu_ns: Subtotal,
}

/// The sum of what the App's members spent, over the members it could be measured for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Subtotal {
    /// Exact sum of measured members. Zero with no measured members is not measured zero.
    pub known: u64,
    pub measured_members: u32,
    pub unavailable_members: u32,
    pub overflowed: bool,
}

/// The Apps of one tick, in buffers taken at startup.
pub struct Apps {
    /// The totals of the Apps the tick's processes belong to.
    totals: Vec<Totals>,
    /// Each App's row in `totals`, or `SLOT_EMPTY` for an App no process of the tick runs.
    slots: Vec<u32>,
}

const SLOT_EMPTY: u32 = u32::MAX;

impl Apps {
    #[must_use]
    pub fn new(processes_max: u32, programs: &Programs) -> Self {
        Self {
            totals: Vec::with_capacity(crate::usize_from_u32(processes_max)),
            slots: vec![SLOT_EMPTY; programs.apps_max()],
        }
    }

    /// Derives the Apps of `selected`, the tick after `previous`.
    pub fn derive(&mut self, programs: &Programs, previous: &Processes, selected: &Processes) {
        self.clear();

        for row in 0..selected.count() {
            let app = programs.app(selected.program[row]);
            let cpu_ns = derive_cpu_ns(previous, selected, row);
            let totals = self.totals_of(app);
            totals.members += 1;
            totals.cpu_ns.add(cpu_ns);
        }
    }

    /// The Apps of the tick last derived.
    #[must_use]
    pub fn totals(&self) -> &[Totals] {
        &self.totals
    }

    fn clear(&mut self) {
        for totals in &self.totals {
            self.slots[totals.app.index()] = SLOT_EMPTY;
        }
        self.totals.clear();
    }

    fn totals_of(&mut self, app: App) -> &mut Totals {
        let slot = self.slots[app.index()];
        if slot == SLOT_EMPTY {
            assert!(self.totals.len() < self.totals.capacity());

            self.slots[app.index()] = crate::u32_from_length(self.totals.len());
            self.totals.push(Totals {
                app,
                members: 0,
                cpu_ns: Subtotal::default(),
            });
        }
        let slot = crate::usize_from_u32(self.slots[app.index()]);
        &mut self.totals[slot]
    }
}

/// The CPU time the process of `selected`'s `row` spent since `previous`, if it can be known.
fn derive_cpu_ns(previous: &Processes, selected: &Processes, row: usize) -> Option<u64> {
    let before = previous.find(selected.pid[row], selected.creation[row])?;
    let user_ns = selected.user_ns[row].checked_sub(previous.user_ns[before])?;
    let system_ns = selected.system_ns[row].checked_sub(previous.system_ns[before])?;
    user_ns.checked_add(system_ns)
}

impl Subtotal {
    fn add(&mut self, value: Option<u64>) {
        match value {
            Some(value) => {
                self.known += value;
                self.measured_members += 1;
            }
            None => self.unavailable_members += 1,
        }
    }
}

#[cfg(test)]
mod tests;
