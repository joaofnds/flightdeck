//! The readings the live window draws, held for the app's life. The store thread writes them
//! into buffers allocated when the store starts, and the main thread reads them through a View
//! that never changes while it holds it.

mod buffers;
mod second;

pub use buffers::{Buffers, Mailbox, Span};
pub use second::{MachineSecond, MemorySecond, Reading, Traffic, readings};
