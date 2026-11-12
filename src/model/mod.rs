// Everything here is protocol-independent and does no I/O except reading and
// writing the connection file. That is deliberate: it is the part worth having
// tests for, and it lets the UI and the network layer evolve separately.

pub mod bookmarks;
pub mod browser;
pub mod connection;
pub mod entries;
pub mod message;
pub mod order;
pub mod path;
pub mod selection;
pub mod settings;
pub mod store;
pub mod transfer;
