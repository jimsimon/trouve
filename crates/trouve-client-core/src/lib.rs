//! Shared client logic for trouve UIs (invariant 1: clients speak the
//! protocol, nothing else). The Slint app, and later mobile/web clients,
//! compose [`client::ProtocolClient`] for commands and
//! [`viewmodel::ThreadViewModel`] to fold the event stream into renderable
//! chat items.

pub mod client;
pub mod viewmodel;
