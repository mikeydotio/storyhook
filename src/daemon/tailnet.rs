//! This machine's Tailscale identity — the second interface the daemon binds,
//! and the only non-loopback `Host` values it will trust for a mutation.
//!
//! Every rule in [`TailnetBind::trusted_hosts`] is a decision about what an
//! attacker can and cannot forge an origin for, and the reasoning is in the doc
//! comments rather than in a commit message for exactly that reason.
//!
//! # Two types, because a probe is not a bind
//!
//! [`TailnetIdentity`] is what `tailscale status --json` said about this
//! machine. [`TailnetBind`] is what a daemon actually bound, and only a
//! successful `TcpListener::bind` can produce one. Everything with authority —
//! what is trusted, what is advertised — hangs off the second, so neither can
//! be answered from a probe alone.
//!
//! A bind does not have to happen at startup to count: a login-time daemon
//! start can race `tailscaled` coming up and miss the interface entirely, so
//! `crate::daemon::serve` retries in the background until one succeeds
//! (SH-146). What stays true either way is that trust is decided **once**,
//! by whichever `TcpListener::bind` first succeeds — never re-evaluated, and
//! never granted for a bind that has not actually happened yet.

use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// What `tailscale status --json` reported about this machine — a *probe
/// result*, and nothing more.
///
/// Deliberately inert. It carries no authority over what is trusted or
/// advertised, because a probe answers "does this machine have a tailnet?"
/// while every question this daemon actually has is "did *this daemon* bind
/// it?". Those came apart in SH-110. The only thing you can do with an
/// identity is [`Self::into_bound`] it, after a bind has succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailnetIdentity {
    /// The IPv4 address to bind the tailnet listener to.
    pub bind_ip: std::net::Ipv4Addr,
    /// This machine's MagicDNS FQDN, when MagicDNS is enabled.
    pub magic_dns: Option<String>,
}

impl TailnetIdentity {
    /// Promotes a probe result to a [`TailnetBind`], the evidence that the
    /// interface is being served.
    ///
    /// Consuming, and `pub(crate)`, so the probe result is *used up* by the
    /// bind: nothing downstream can reach back past it to the unbound facts.
    /// Call this only where `TcpListener::bind` has just returned `Ok`.
    pub(crate) fn into_bound(self) -> TailnetBind {
        TailnetBind {
            ip: self.bind_ip,
            magic_dns: self.magic_dns,
        }
    }
}

/// A tailnet interface a daemon has bound, and the names that bind earns.
///
/// Only [`TailnetIdentity::into_bound`] constructs one, and only after
/// `TcpListener::bind` returned `Ok`, so a value of this type *is* the
/// evidence that the interface is being served. Both what the daemon trusts
/// ([`Self::trusted_hosts`]) and what it advertises ([`Self::advertise_host`])
/// are projections of this one value, so they cannot disagree — SH-110 was
/// exactly that disagreement, in a daemon that trusted only what it bound and
/// advertised whatever a fresh probe happened to say.
///
/// The fields are private on purpose: with `Deserialize` derived, the only two
/// ways to obtain one are `into_bound` and reading a portfile a daemon wrote
/// after its own bind. A hand-forged portfile could inject one, but that file
/// also carries a full-privilege bearer token at mode 0600, so anyone able to
/// write it already owns the API.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TailnetBind {
    /// The tailnet IPv4 the listener is bound to.
    ip: std::net::Ipv4Addr,
    /// This machine's MagicDNS FQDN, when MagicDNS is enabled.
    magic_dns: Option<String>,
}

impl TailnetBind {
    /// The tailnet IPv4 this listener is bound to.
    pub fn ip(&self) -> std::net::Ipv4Addr {
        self.ip
    }

    /// This machine's MagicDNS FQDN, when MagicDNS is enabled.
    pub fn magic_dns(&self) -> Option<&str> {
        self.magic_dns.as_deref()
    }

    /// `Host` values a browser may legitimately send for *this* machine that
    /// an external attacker can never forge an origin for: the tailnet IPv4
    /// itself, and — when MagicDNS is on — its fully-qualified name. A
    /// `*.ts.net` FQDN only ever resolves (off-tailnet) to NXDOMAIN, so no
    /// attacker can serve a page from that origin — DNS rebinding can't make
    /// a hostile page same-origin with it. The bare single-label short name
    /// (e.g. `psamathe`) is deliberately *not* included: unlike the FQDN, it
    /// can resolve through a DNS search domain that isn't the tailnet's, so
    /// an attacker who influences that search path could rebind exactly that
    /// name to this machine's IP — the very attack class this allowlist
    /// exists to stop.
    pub fn trusted_hosts(&self) -> Vec<String> {
        let mut hosts = vec![self.ip.to_string()];
        hosts.extend(self.magic_dns.clone());
        hosts
    }

    /// The best host to show or copy for reaching this machine: the MagicDNS
    /// name when available — memorable, and, since it's also in
    /// [`Self::trusted_hosts`], guaranteed to work for mutations too — else
    /// the bare tailnet IPv4.
    pub fn advertise_host(&self) -> String {
        self.magic_dns
            .clone()
            .unwrap_or_else(|| self.ip.to_string())
    }
}

/// Mirrors only the fields this module reads from `tailscale status --json`.
#[derive(serde::Deserialize)]
struct TailscaleStatus {
    #[serde(rename = "Self")]
    self_node: Option<TailscaleSelf>,
}

#[derive(serde::Deserialize)]
struct TailscaleSelf {
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
}

/// Parses `tailscale status --json`'s output into this machine's
/// [`TailnetIdentity`]. Pure and dependency-free — exercised by unit tests
/// below against captured fixtures, without a live tailnet. Returns `None`
/// if the JSON has no `Self` entry, isn't valid JSON, or `Self` has no IPv4
/// in `TailscaleIPs` (a v6-only tailnet has no bind target, matching
/// `tailscale ip -4`'s own behavior — no bind, no trust).
fn parse_tailnet_identity(status_json: &str) -> Option<TailnetIdentity> {
    let status: TailscaleStatus = serde_json::from_str(status_json).ok()?;
    let me = status.self_node?;
    let bind_ip = me
        .tailscale_ips
        .iter()
        .find_map(|ip| ip.parse::<std::net::Ipv4Addr>().ok())?;
    // `DNSName` is reported rooted (a trailing `.`) and MagicDNS names are
    // already lowercase, but normalize defensively rather than assume.
    let fqdn = me.dns_name.trim_end_matches('.').to_ascii_lowercase();
    let magic_dns = if fqdn.is_empty() { None } else { Some(fqdn) };
    Some(TailnetIdentity { bind_ip, magic_dns })
}

/// How long `tailscale status --json` gets to answer before a probe attempt
/// gives up and reports no tailnet.
///
/// The CLI talks to `tailscaled`, which does wedge (probes stuck for minutes,
/// orphaned by servers that had already exited, observed on macOS). No
/// tailnet is a degraded dashboard; a dashboard that never answers is a
/// broken one — which is why, since SH-186, this bound no longer sits on the
/// daemon's path to serving its first request at all. `bind_listeners` binds
/// loopback and stops; every tailnet bind, including the first, happens on
/// `serve::tailnet_reprobe`'s background thread, so a probe stuck for the
/// whole of this timeout delays only the tailnet interface's own
/// availability, never the dashboard's.
///
/// `pub` so `tests/tailnet_startup.rs` and
/// `crates/storyhook-test-support`'s deadlines are derived from this value
/// rather than a magic number that could drift out of sync with it.
pub const TAILNET_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Shells out to `tailscale status --json` and parses this machine's
/// [`TailnetIdentity`]. `None` if the CLI is absent, exits non-zero, wedges
/// (see [`TAILNET_PROBE_TIMEOUT`]), or reports nothing usable (see
/// [`parse_tailnet_identity`]).
pub fn tailnet_identity() -> Option<TailnetIdentity> {
    let stdout = tailscale_status_json(TAILNET_PROBE_TIMEOUT)?;
    parse_tailnet_identity(&stdout)
}

/// Runs `tailscale status --json`, returning its stdout, or `None` if it
/// fails or outlives `timeout`. A probe that overruns is killed rather than
/// left behind: an abandoned one holds a pipe this process owns and lingers
/// after it exits.
fn tailscale_status_json(timeout: Duration) -> Option<String> {
    let mut command = Command::new("tailscale");
    command
        .args(["status", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    // Its own process group, so a timeout can kill whatever the probe
    // started as well as the probe itself — killing the leader alone leaves
    // its children orphaned and still running.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let child = command.spawn().ok()?;
    let pid = child.id();

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(_) => None,
        Err(_) => {
            eprintln!(
                "warning: `tailscale status --json` did not answer within {}s; serving \
                 localhost only",
                timeout.as_secs()
            );
            // Negative pid = the whole process group (established above), so
            // nothing the probe spawned survives it. The reaper thread is
            // still in `wait_with_output`, so the killed process is collected
            // rather than left a zombie.
            #[cfg(unix)]
            // SAFETY: libc::kill with the group id of a process this process
            // just spawned and has not yet reaped, so it cannot have been
            // recycled onto an unrelated group.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `tailscale status --json` fixture matching this project's own
    /// machine (captured while writing the #35 fix): dual-stack
    /// `TailscaleIPs`, a rooted `DNSName`, and an uppercase `HostName` that
    /// must NOT leak into the parsed identity (see
    /// `parse_tailnet_identity_ignores_host_name`).
    const STATUS_JSON: &str = r#"{
        "Self": {
            "HostName": "Psamathe",
            "DNSName": "psamathe.tail983f02.ts.net.",
            "TailscaleIPs": ["100.71.206.33", "fd7a:115c:a1e0::6701:ce21"]
        }
    }"#;

    #[test]
    fn parse_tailnet_identity_happy_path() {
        let identity = parse_tailnet_identity(STATUS_JSON).unwrap();
        assert_eq!(identity.bind_ip.to_string(), "100.71.206.33");
        assert_eq!(
            identity.magic_dns.as_deref(),
            Some("psamathe.tail983f02.ts.net")
        );
    }

    #[test]
    fn parse_tailnet_identity_strips_trailing_dot_and_lowercases() {
        let json = r#"{"Self": {"DNSName": "Psamathe.Tail983F02.TS.NET.", "TailscaleIPs": ["100.71.206.33"]}}"#;
        let identity = parse_tailnet_identity(json).unwrap();
        assert_eq!(
            identity.magic_dns.as_deref(),
            Some("psamathe.tail983f02.ts.net")
        );
    }

    #[test]
    fn parse_tailnet_identity_ignores_host_name() {
        // HostName ("Psamathe") must never surface anywhere in the parsed
        // identity — only DNSName is a real, browser-addressable name.
        let identity = parse_tailnet_identity(STATUS_JSON).unwrap();
        assert_ne!(identity.magic_dns.as_deref(), Some("psamathe"));
        assert_ne!(identity.magic_dns.as_deref(), Some("Psamathe"));
    }

    #[test]
    fn parse_tailnet_identity_missing_dns_name_is_ip_only() {
        let json = r#"{"Self": {"DNSName": "", "TailscaleIPs": ["100.71.206.33"]}}"#;
        let identity = parse_tailnet_identity(json).unwrap();
        assert_eq!(identity.bind_ip.to_string(), "100.71.206.33");
        assert_eq!(identity.magic_dns, None);
    }

    #[test]
    fn parse_tailnet_identity_missing_self_is_none() {
        assert_eq!(parse_tailnet_identity(r#"{}"#), None);
    }

    #[test]
    fn parse_tailnet_identity_ipv6_only_is_none() {
        let json = r#"{"Self": {"DNSName": "foo.ts.net.", "TailscaleIPs": ["fd7a:115c:a1e0::6701:ce21"]}}"#;
        assert_eq!(parse_tailnet_identity(json), None);
    }

    #[test]
    fn parse_tailnet_identity_malformed_json_is_none() {
        assert_eq!(parse_tailnet_identity("not json"), None);
        assert_eq!(parse_tailnet_identity(""), None);
    }

    #[test]
    fn parse_tailnet_identity_picks_first_ipv4_among_mixed_order() {
        let json = r#"{"Self": {"DNSName": "", "TailscaleIPs": ["fd7a:115c:a1e0::6701:ce21", "100.71.206.33"]}}"#;
        let identity = parse_tailnet_identity(json).unwrap();
        assert_eq!(identity.bind_ip.to_string(), "100.71.206.33");
    }

    /// A [`TailnetBind`] as a successful bind would produce it, for the tests
    /// below that are about the bind's projections rather than about parsing.
    fn bound(bind_ip: &str, magic_dns: Option<&str>) -> TailnetBind {
        TailnetIdentity {
            bind_ip: bind_ip.parse().expect("a test fixture's IPv4 parses"),
            magic_dns: magic_dns.map(str::to_string),
        }
        .into_bound()
    }

    #[test]
    fn trusted_hosts_includes_ip_and_fqdn_but_not_short_label_or_ipv6() {
        let hosts = parse_tailnet_identity(STATUS_JSON)
            .unwrap()
            .into_bound()
            .trusted_hosts();
        assert_eq!(
            hosts,
            vec![
                "100.71.206.33".to_string(),
                "psamathe.tail983f02.ts.net".to_string(),
            ]
        );
        // The bare single-label short name is deliberately excluded — see
        // TailnetIdentity::trusted_hosts's doc comment for why.
        assert!(!hosts.iter().any(|h| h == "psamathe"));
        // Never bound, so never trusted (see TailnetIdentity's doc comment).
        assert!(
            !hosts
                .iter()
                .any(|h| h.contains(':') && h != "psamathe.tail983f02.ts.net")
        );
    }

    #[test]
    fn trusted_hosts_is_ip_only_without_magic_dns() {
        assert_eq!(
            bound("100.71.206.33", None).trusted_hosts(),
            vec!["100.71.206.33".to_string()]
        );
    }

    #[test]
    fn advertise_host_prefers_magic_dns_over_ip() {
        let bind = parse_tailnet_identity(STATUS_JSON).unwrap().into_bound();
        assert_eq!(bind.advertise_host(), "psamathe.tail983f02.ts.net");
    }

    #[test]
    fn advertise_host_falls_back_to_ip_without_magic_dns() {
        assert_eq!(
            bound("100.71.206.33", None).advertise_host(),
            "100.71.206.33"
        );
    }

    /// The two projections of a bind agree about which names it earned: the
    /// host a user is told to visit is always one the mutation guard will
    /// accept. SH-110 was these two answers coming from different sources.
    #[test]
    fn what_a_bind_advertises_is_always_something_it_trusts() {
        for bind in [
            bound("100.71.206.33", Some("psamathe.tail983f02.ts.net")),
            bound("100.71.206.33", None),
        ] {
            assert!(
                bind.trusted_hosts().contains(&bind.advertise_host()),
                "advertised {} but trusts only {:?}",
                bind.advertise_host(),
                bind.trusted_hosts()
            );
        }
    }
}
