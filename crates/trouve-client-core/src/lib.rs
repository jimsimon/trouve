//! Shared client logic for trouve UIs (invariant 1: clients speak the
//! protocol, nothing else). Native hosts and web/mobile clients
//! compose [`client::ProtocolClient`] for commands and
//! [`viewmodel::ThreadViewModel`] to fold the event stream into renderable
//! chat items.

pub mod client;
pub mod protocol_compatibility;
pub mod team_viewmodel;
pub mod viewmodel;
