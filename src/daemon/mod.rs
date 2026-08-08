//! The storyhook daemon.
//!
//! One process per machine, owning the store and serving everything that talks
//! to it. It grew out of the dashboard's web server, which was already a
//! multi-project, tailnet-native daemon with a pidfile and a lifecycle — the
//! rearchitecture promoted it rather than writing a second one.
//!
//! * [`backup`] — the daily verified snapshot of the store.
//! * [`bus`] — the change feed every `/api/events` client subscribes to, fed by
//!   the request boundary and by a `PRAGMA data_version` poller.
//! * [`commands`] — `story daemon start|stop|status|install|uninstall|token`.
//! * [`http1`] — the HTTP/1.1 connection layer: parsing, framing, and every
//!   deadline and cap a peer socket is held to (SH-177).
//! * [`lifecycle`] — the portfile, the pidfile lock, and auto-spawn.
//! * [`serve`] — the listeners and the accept loop.
//! * [`tailnet`] — this machine's Tailscale identity: the interface the
//!   dashboard is served on, and the only non-loopback `Host` the mutation guard
//!   will trust.

pub mod backup;
pub mod bus;
pub mod commands;
pub mod http1;
pub mod lifecycle;
pub mod serve;
pub mod tailnet;
