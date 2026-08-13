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
//! * [`routes`] — every route named once, and what authority each requires.
//!   Imports nothing but a `Method`, so the gate that classifies a request
//!   before reading its body structurally cannot reach a store (SH-254).
//! * [`rpc`] — the daemon's control surface: loopback only, token-authenticated.
//! * [`session`] — the dashboard's scoped, revocable capability (SH-254): what
//!   a redeemed coupon buys, instead of the daemon's master token.
//! * [`tokens`] — named, persistent, revocable bearer tokens (SH-255): the
//!   one credential concept that replaces the loopback read exemption,
//!   `Authority::Public`/`Session`, and [`session`] itself.
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
pub mod session;
pub mod tokens;
pub mod wire;
