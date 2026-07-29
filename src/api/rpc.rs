//! The daemon's control surface: `/api/v1/*`.
//!
//! # Loopback only, and authenticated
//!
//! This is a full-privilege API — anything the CLI can do, it can do — so it is
//! answered on the loopback listener and nowhere else, and every request must
//! carry the per-daemon token from the mode-0600 portfile.
//!
//! Both halves are necessary. Loopback is not a trust boundary: every process on
//! the machine can reach it, including a browser tab running somebody else's
//! JavaScript, which is exactly the threat the dashboard's CSRF guard exists
//! for. The token is what a page cannot obtain — it is in a file the browser
//! cannot read — and refusing the whole surface on the tailnet listener is what
//! keeps a tailnet peer from reaching it at all.
//!
//! A request that arrives on the wrong interface is answered `404`, not `403`:
//! there is nothing there to be forbidden from.

use tiny_http::{Header, Method};

use crate::api::http::{Reply, header_value, json_reply, text_reply, to_json};
use crate::daemon::lifecycle::{Hello, PROTOCOL};

/// The header carrying the daemon's bearer token.
pub const TOKEN_HEADER: &str = "X-Storyhook-Token";

/// What a routed control request asked for.
pub enum Control {
    /// Answered; send this.
    Reply(Reply),
    /// Answered, and the daemon should now shut down.
    Shutdown(Reply),
}

/// Routes a request under `/api/v1/`, or `None` if it is not one.
///
/// `loopback` says whether the request arrived on the loopback listener. On any
/// other interface this surface does not exist.
pub fn route(
    segments: &[&str],
    method: &Method,
    headers: &[Header],
    loopback: bool,
    token: &str,
    hello: &Hello,
) -> Option<Control> {
    let ["api", "v1", rest @ ..] = segments else {
        return None;
    };
    if !loopback {
        return Some(Control::Reply(text_reply(404, "Not found")));
    }
    if !token_ok(headers, token) {
        return Some(Control::Reply(text_reply(
            401,
            "storyhook daemon: missing or invalid token",
        )));
    }

    Some(match (rest, method) {
        (["hello"], Method::Get) => Control::Reply(match to_json(hello) {
            Ok(body) => json_reply(200, body).no_cache(),
            Err(e) => crate::api::http::error_reply(&e),
        }),
        (["shutdown"], Method::Post) => Control::Shutdown(json_reply(
            200,
            serde_json::json!({"result": "ok", "protocol": PROTOCOL}).to_string(),
        )),
        (["hello"] | ["shutdown"], _) => Control::Reply(text_reply(405, "Method not allowed")),
        _ => Control::Reply(text_reply(404, "Not found")),
    })
}

/// Whether the request carries the daemon's token.
///
/// Compared in constant time. The attack it defends against — measuring how long
/// a mismatch takes, over loopback, to recover 128 bits a byte at a time — is
/// remote, but the defence costs one loop and the alternative is having to
/// argue that it is remote.
fn token_ok(headers: &[Header], expected: &str) -> bool {
    let Some(offered) = header_value(headers, TOKEN_HEADER) else {
        return false;
    };
    constant_time_eq(offered.as_bytes(), expected.as_bytes())
}

/// Byte equality that takes the same time whatever the inputs are.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (x, y) in a.iter().zip(b) {
        difference |= x ^ y;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Hello {
        Hello {
            version: "0.0.0".to_string(),
            protocol: PROTOCOL,
            pid: 42,
            started_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn header(name: &str, value: &str) -> Header {
        Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
    }

    fn status(control: Option<Control>) -> u16 {
        match control.expect("a control route") {
            Control::Reply(reply) | Control::Shutdown(reply) => reply.status,
        }
    }

    #[test]
    fn a_path_outside_the_control_surface_is_not_routed_here() {
        assert!(
            route(
                &["api", "repos"],
                &Method::Get,
                &[header(TOKEN_HEADER, "t")],
                true,
                "t",
                &hello()
            )
            .is_none()
        );
    }

    #[test]
    fn hello_answers_the_daemons_identity() {
        let control = route(
            &["api", "v1", "hello"],
            &Method::Get,
            &[header(TOKEN_HEADER, "t")],
            true,
            "t",
            &hello(),
        );
        assert_eq!(status(control), 200);
    }

    /// The surface does not exist off loopback — not even to be forbidden from,
    /// which is why this is a 404 and not a 403. A tailnet peer must not be able
    /// to learn that there is a control API here at all.
    #[test]
    fn the_control_surface_does_not_exist_off_loopback() {
        for path in [
            ["api", "v1", "hello"].as_slice(),
            ["api", "v1", "shutdown"].as_slice(),
        ] {
            let control = route(
                path,
                &Method::Get,
                &[header(TOKEN_HEADER, "t")],
                false,
                "t",
                &hello(),
            );
            assert_eq!(status(control), 404, "{path:?}");
        }
    }

    #[test]
    fn a_request_without_the_token_is_refused() {
        assert_eq!(
            status(route(
                &["api", "v1", "hello"],
                &Method::Get,
                &[],
                true,
                "t",
                &hello()
            )),
            401
        );
    }

    #[test]
    fn a_request_with_the_wrong_token_is_refused() {
        assert_eq!(
            status(route(
                &["api", "v1", "hello"],
                &Method::Get,
                &[header(TOKEN_HEADER, "not-the-token")],
                true,
                "t",
                &hello()
            )),
            401
        );
    }

    /// Loopback is not a trust boundary: a page in a browser on this machine can
    /// reach it. The token is the thing that page cannot obtain, so the check
    /// must come before anything is served — including the identity endpoint,
    /// which would otherwise confirm to an attacker that storyhook is here.
    #[test]
    fn the_token_is_checked_before_anything_is_served() {
        assert_eq!(
            status(route(
                &["api", "v1", "definitely-not-a-route"],
                &Method::Get,
                &[],
                true,
                "t",
                &hello()
            )),
            401,
            "an unauthenticated request must not be able to tell a real route \
             from a missing one"
        );
    }

    #[test]
    fn shutdown_is_a_post() {
        assert!(matches!(
            route(
                &["api", "v1", "shutdown"],
                &Method::Post,
                &[header(TOKEN_HEADER, "t")],
                true,
                "t",
                &hello()
            ),
            Some(Control::Shutdown(_))
        ));
        assert_eq!(
            status(route(
                &["api", "v1", "shutdown"],
                &Method::Get,
                &[header(TOKEN_HEADER, "t")],
                true,
                "t",
                &hello()
            )),
            405
        );
    }

    #[test]
    fn constant_time_equality_still_answers_correctly() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
