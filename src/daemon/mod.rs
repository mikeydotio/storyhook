//! The storyhook daemon.
//!
//! One process per machine, owning the store and serving everything that talks
//! to it. It grew out of the dashboard's web server, which was already a
//! multi-project, tailnet-native daemon with a pidfile and a lifecycle — the
//! rearchitecture promoted it rather than writing a second one.
//!
//! * [`agent`] — the launchd login agent's plist, written and read (SH-411),
//!   one per store rather than one machine-wide (SH-414).
//! * [`backup`] — the daily verified snapshot of the store.
//! * [`bus`] — the change feed every `/api/events` client subscribes to, fed by
//!   the request boundary and by a `PRAGMA data_version` poller.
//! * [`commands`] — `story daemon start|stop|status|install|uninstall|token`.
//! * [`crash`] — the panic hook and the crash ledger: what the daemon leaves
//!   behind when it does not exit cleanly, and what the next one does about
//!   it (SH-287).
//! * [`github_poll`] — the unattended background poll for merged pull
//!   requests (SH-212), spending the credential `story github-auth login`
//!   stored in the OS keychain. `github-pr`-gated: absent entirely from a
//!   build without it.
//! * [`install_guard`] — what `story daemon install` refuses to give a
//!   permanent seat on this machine (SH-411).
//! * [`http1`] — the HTTP/1.1 connection layer: parsing, framing, and every
//!   deadline and cap a peer socket is held to (SH-177).
//! * [`lifecycle`] — the portfile, the pidfile lock, and auto-spawn.
//! * [`serve`] — the listeners and the accept loop.
//! * [`subscribe`] — a client for [`bus`]'s change feed: what
//!   [`crate::tui::event::EventSource`] uses instead of a store handle of its
//!   own (SH-150).
//! * [`tailnet`] — this machine's Tailscale identity: the interface the
//!   dashboard is served on, and the only non-loopback `Host` the mutation guard
//!   will trust.
//! * [`watch`] — attributes a store-wide change to specific projects, shared by
//!   the request boundary and the `data_version` poller (SH-202).

pub mod agent;
pub mod backup;
pub mod bus;
pub mod commands;
pub mod crash;
#[cfg(feature = "github-pr")]
pub mod github_poll;
pub mod http1;
pub mod install_guard;
pub mod lifecycle;
pub mod serve;
pub mod subscribe;
pub mod tailnet;
pub mod verification;
pub mod watch;
