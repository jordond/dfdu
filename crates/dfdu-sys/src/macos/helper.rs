//! The privileged helper behind the GUI's scans as administrator: a launchd daemon in
//! `dfdu.app`, registered with [`service::register`], that runs an agent as root for the user of
//! each GUI that asks.
//!
//! The GUI makes a socket pair and sends one end with [`connect`]. The daemon, in [`serve`],
//! checks the client's code signature, takes the socket and the client's euid ([`Peer`]), and
//! starts the agent with the socket as its standard input and output and `DFDU_ACTING_UID` set
//! to that euid ([`crate::acting`]). From then on dfdu's own protocol runs over the socket, as
//! it does over ssh or pkexec.

pub mod service;
mod xpc;

pub use xpc::{Peer, Started, connect, serve};

#[cfg(test)]
mod tests;
