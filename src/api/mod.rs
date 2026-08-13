//! Storyhook's HTTP surface.
//!
//! One daemon serves two listeners over these modules:
//!
//! * [`admission`] — the token gate every other `/api/**` route answers
//!   behind (SH-187), reads and writes alike, on both listeners.
//! * [`dispatch`] — the dashboard's dispatch endpoint (SH-50): token-gated,
//!   answered off the store-owning thread, on both listeners.
//! * [`handoff`] — the one-shot coupon `story web open` arms and the dashboard
//!   redeems for the token (SH-251), so a one-click dashboard never prompts
//!   and nothing anywhere is relaxed to achieve it.
//! * [`http`] — the plumbing both share: response shaping, the security
//!   headers, the CSRF and DNS-rebinding guard, body reading, SSE framing.
//! * [`rest`] — the dashboard's resource API, over the service layer.
//! * [`routes`] — every route this daemon serves, named once, for
//!   [`rest`] to answer. Imports nothing but a `Method`, so a request can be
//!   classified before its body is read.
//! * [`rpc`] — the daemon's control surface: loopback only, token-authenticated.
//! * [`tokens`] — named, persistent, revocable bearer tokens (SH-255): the
//!   one credential concept that replaced the loopback read exemption, the
//!   `Authority::Public`/`Session` split, and the scoped session capability
//!   (SH-254) that used to live in this module as `session`.
//! * [`wire`] — the `/api/v1/invoke` envelope, shared by the daemon and its
//!   client.
//!
//! Keeping the plumbing apart from the routes is not tidiness. The guard code is
//! the difference between "a page on the internet cannot write to your tracker"
//! and "it can", and two listeners that each grew their own copy would
//! eventually disagree about it.

pub mod admission;
pub mod dispatch;
pub mod handoff;
pub mod http;
pub mod rest;
pub mod routes;
pub mod rpc;
pub mod tokens;
pub mod wire;
