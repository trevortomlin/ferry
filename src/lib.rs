//! Ferry's engine room: everything that is not drawing.
//!
//! - [`model`]: plain data: bookmarks, listings, sorting, selection, the
//!   transfer queue. No I/O beyond reading and writing config,
//!   so it is all directly testable.
//! - [`remote`]: the SFTP and S3 providers.
//! - [`secrets`]: the system keychain and the SSH agent.
//! - [`engine`]: one background thread that owns every socket; the UI posts
//!   commands to it and drains events from it.
//!
//! The interface (`src/ui`, in the binary) sits on top and never awaits the
//! network.

pub mod engine;
pub mod model;
pub mod remote;
pub mod secrets;
