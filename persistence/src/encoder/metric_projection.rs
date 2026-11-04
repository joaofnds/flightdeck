//! One source publication projected through the same raw table encoders as an ordinary seal.
use crate::resources::{ByteBudget, ByteReservation};
use crate::{IdentitySource, Limits, Observation, Result, Window, WindowIdentity};
use collector::ring::{Found, RingReader};
use polars::prelude::*;
use std::sync::Arc;

#[derive(Clone, Copy, Default)]
pub(crate) struct ColumnGroups(u8);
impl ColumnGroups {
    pub(crate) fn battery(self) -> bool {
        self.0 & 1 != 0
    }
    pub(crate) fn disks(self) -> bool {
        self.0 & 2 != 0
    }
    pub(crate) fn system(self) -> bool {
        self.0 & 4 != 0
    }
    pub(crate) fn temperatures(self) -> bool {
        self.0 & 8 != 0
    }
    pub(crate) fn include(&mut self, window: &Window) {
        self.0 |= u8::from(window.battery_captured())
            | (u8::from(window.disks_captured()) << 1)
            | (u8::from(window.system_captured()) << 2)
            | (u8::from(window.temperatures_captured()) << 3);
    }
}

pub(crate) struct MetricTick {
    pub window: Window,
    _reservation: ByteReservation,
}
pub(crate) struct MetricFrame {
    pub frame: DataFrame,
    _reservation: ByteReservation,
}
impl MetricTick {
    pub(crate) fn read(
        reader: &mut RingReader,
        identity: &WindowIdentity,
        publication: u64,
        budget: &Arc<ByteBudget>,
        work: Option<&dyn crate::owned_identity::ProgramReadWork>,
    ) -> Result<Self> {
        let shape = reader.shape();
        let limits = Limits::from_shape(&shape)?;
        let tick_bytes = crate::window::tick_workspace_bytes(&limits)
            + limits.processes
                * (size_of::<collector::ring::IdentityObservation>() + size_of::<Option<u64>>())
            + limits.cores * limits.states * size_of::<u32>();
        let _source = budget
            .reserve(tick_bytes)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
        let mut tick = crate::tick_buffer(&shape)?;
        let mut identities = Vec::with_capacity(limits.processes);
        reader.seek_publication(publication)?;
        let captured = reader.dictionary_capacity().is_some();
        let found = if captured {
            reader.read_identity_into(&mut tick, &mut identities)?
        } else {
            reader.read_into(&mut tick)?
        };
        if !matches!(found, Found::Publication { publication: actual, .. } if actual == publication)
        {
            return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock).into());
        }

        let source = IdentitySource {
            reader,
            processes: &identities,
            events: &[],
        };
        Self::read_stage(&tick, &source, identity, publication, budget, work, &limits)
    }
    fn read_stage(
        tick: &collector::ring::Tick,
        source: &IdentitySource<'_>,
        identity: &WindowIdentity,
        publication: u64,
        budget: &Arc<ByteBudget>,
        work: Option<&dyn crate::owned_identity::ProgramReadWork>,
        limits: &Limits,
    ) -> Result<Self> {
        let captured = source.reader.dictionary_capacity().is_some();
        let tick_bytes = crate::window::tick_workspace_bytes(limits)
            + limits.processes
                * (size_of::<collector::ring::IdentityObservation>() + size_of::<Option<u64>>())
            + limits.cores * limits.states * size_of::<u32>();
        let bytes = if captured {
            crate::owned_identity::Programs::allocation_bytes()
                + crate::owned_identity::Prepared::peak_bytes(source)
        } else {
            0
        };
        let mut reservation = budget
            .reserve(
                bytes
                    + tick_bytes
                    + size_of::<Window>()
                    + crate::window::WINDOW_TICKS_MAX * size_of::<crate::window::StagedTick>()
                    + 64 * 1024,
            )
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
        let observation = Observation {
            publication,
            wall_ns: tick.times.map(|times| times.wall_ns),
            continuous_ns: tick.times.map(|times| times.continuous_ns),
            awake_ns: tick.times.map(|times| times.awake_ns),
        };
        let mut window = Window::new(*identity, *limits);
        if captured {
            window.push_tick_with_identity_observed(&observation, tick, &[], source, work)?;
        } else {
            window.push_tick(&observation, tick, &[])?;
        }
        let retained = usize::try_from(window.staged_bytes())
            .map_err(|_| crate::Error::Bound("metric staging"))?;
        if retained > reservation.bytes() {
            return Err(crate::Error::Bound("metric staging reservation"));
        }

        reservation.shrink_to(retained);
        Ok(Self {
            window,
            _reservation: reservation,
        })
    }
    pub(crate) fn frame(
        &self,
        table: crate::schema::Table,
        offset: usize,
        count: usize,
        budget: &Arc<ByteBudget>,
    ) -> Result<MetricFrame> {
        let bytes = if matches!(table, crate::schema::Table::Processes) {
            // Each native path is at most4095 bytes; both paths plus scalar/row/sort work.
            count
                .checked_mul(2 * collector::ring::PROGRAM_PATH_BYTES_MAX + 1024)
                .and_then(|bytes| bytes.checked_add(8 * 1024 * 1024))
                .ok_or(crate::Error::Bound("metric projection"))?
        } else {
            usize::try_from(
                super::frame_bytes(table, &self.window)?
                    .checked_mul(2)
                    .ok_or(crate::Error::Bound("metric sort workspace"))?,
            )
            .map_err(|_| crate::Error::Bound("metric projection"))?
        };
        let reservation = budget
            .reserve(bytes)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
        let frame = if matches!(table, crate::schema::Table::Processes) {
            crate::identity_encoder::processes_projection(&self.window, offset, count)?
        } else {
            super::frame(table, &self.window)?
                .sort(
                    table.catalog().key.to_vec(),
                    SortMultipleOptions {
                        multithreaded: false,
                        maintain_order: true,
                        nulls_last: vec![true],
                        ..Default::default()
                    },
                )?
                .slice(
                    i64::try_from(offset).map_err(|_| crate::Error::Bound("metric offset"))?,
                    count,
                )
        };
        Ok(MetricFrame {
            frame,
            _reservation: reservation,
        })
    }
    pub(crate) fn rows(&self, table: crate::schema::Table) -> usize {
        super::bounds::rows(table, &self.window)
    }
}
