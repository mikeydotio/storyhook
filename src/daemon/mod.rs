//! The storyhook daemon.
//!
//! One process per machine, owning the store and serving everything that talks
//! to it. It grew out of the dashboard's web server, which was already a
//! multi-project, tailnet-native daemon with a pidfile and a lifecycle — the
//! rearchitecture promoted it rather than writing a second one.
//!
//! * [`bus`] — the change feed every `/api/events` client subscribes to, fed by
//!   the request boundary and by a `PRAGMA data_version` poller.
//! * [`serve`] — the listeners and the accept loop.
//! * [`tailnet`] — this machine's Tailscale identity: the interface the
//!   dashboard is served on, and the only non-loopback `Host` the mutation guard
//!   will trust.

pub mod bus;
pub mod serve;
pub mod tailnet;
