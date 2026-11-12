// What the background engine sends back to the UI thread.
//
// The matching command type lives in the app rather than here, because it has
// to carry resolved credentials and `core` must not depend on `creds`.
//
// Everything crosses this boundary by value over a channel. The UI never holds
// a lock the engine wants and never awaits, so no amount of network latency can
// stall a frame.
//
// Two ideas do the heavy lifting for high-latency links:
//
// * `Generation` — every request carries one, every reply echoes it. Navigating
//   away bumps the generation, so replies for the directory you already left
//   are recognised as stale and dropped instead of flashing on screen.
// * `Listing { last }` — results arrive in chunks and are drawn as they land,
//   so a slow directory fills in progressively rather than appearing all at
//   once after a long blank.

use crate::model::connection::ConnectionId;
use crate::model::entries::Entries;
use crate::model::transfer::{Conflict, Direction, TransferId};

/// A single file discovered inside a folder, with where it should end up.
#[derive(Debug, Clone)]
pub struct ExpandedItem {
    pub direction: Direction,
    pub remote: String,
    pub local: std::path::PathBuf,
    pub size: u64,
}

/// One live connection to a server. Distinct from `ConnectionId`: the same
/// saved connection can be opened more than once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u32);

/// Bumped whenever the UI asks for something new. Replies carrying an older
/// generation are discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Generation(pub u32);

impl Generation {
    pub fn bump(&mut self) -> Self {
        self.0 = self.0.wrapping_add(1);
        *self
    }
}

/// Engine → UI thread.
#[derive(Debug)]
pub enum Event {
    Opened {
        session: SessionId,
        connection: ConnectionId,
        /// Where the server says we start, so the UI does not have to guess.
        home: String,
    },
    /// The session could not be established at all. Distinct from `Failed`,
    /// which belongs to a particular request and is matched by generation.
    OpenFailed {
        session: SessionId,
        message: String,
    },
    /// A chunk of a directory listing. `last` marks the final chunk.
    Listing {
        session: SessionId,
        generation: Generation,
        entries: Entries,
        last: bool,
    },
    Cancelled {
        session: SessionId,
        generation: Generation,
    },
    /// Something went wrong. The message is written to be shown to a person, so
    /// it must say what failed and what to do about it.
    Failed {
        session: SessionId,
        generation: Generation,
        message: String,
    },
    Closed {
        session: SessionId,
    },

    // ---- folder expansion ----
    /// One batch of files found inside a folder that was asked for. Sent in
    /// batches so a large tree fills the queue progressively rather than after
    /// the whole walk.
    Expanded {
        session: SessionId,
        items: Vec<ExpandedItem>,
        last: bool,
    },
    /// The walk itself failed — permissions, or the folder vanished.
    ExpandFailed {
        session: SessionId,
        message: String,
    },

    // ---- transfers ----
    /// What is already at the destination, before anything is moved.
    TransferProbed {
        transfer: TransferId,
        conflict: Conflict,
    },
    /// The transfer has begun, and the size is now known.
    TransferStarted {
        transfer: TransferId,
        total: u64,
    },
    /// Bytes moved so far. Absolute, not a delta, and throttled by the engine
    /// so a fast transfer cannot flood the UI thread.
    TransferProgress {
        transfer: TransferId,
        done: u64,
    },
    TransferDone {
        transfer: TransferId,
    },
    TransferCancelled {
        transfer: TransferId,
    },
    TransferFailed {
        transfer: TransferId,
        message: String,
    },

    // ---- file operations ----
    /// A create, rename, or delete finished. The path is the directory that
    /// needs re-listing.
    OperationDone {
        session: SessionId,
        message: String,
    },
    OperationFailed {
        session: SessionId,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generations_increase_and_compare() {
        let mut generation = Generation::default();
        let first = generation.bump();
        let second = generation.bump();
        assert!(second > first);
    }
}
