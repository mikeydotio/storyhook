//! Storyhook's HTTP surface.
//!
//! One daemon serves two listeners over these modules:
//!
//! * [`http`] — the plumbing both share: response shaping, the security
//!   headers, the CSRF and DNS-rebinding guard, body reading, SSE framing.
//! * [`rest`] — the dashboard's resource API, over the service layer.
//! * [`rpc`] — the daemon's control surface: loopback only, token-authenticated.
//! * [`wire`] — the `/api/v1/invoke` envelope, shared by the daemon and its
//!   client.
//!
//! Keeping the plumbing apart from the routes is not tidiness. The guard code is
//! the difference between "a page on the internet cannot write to your tracker"
//! and "it can", and two listeners that each grew their own copy would
//! eventually disagree about it.

pub mod http;
pub mod rest;
pub mod rpc;
pub mod wire;
