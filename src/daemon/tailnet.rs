//! This machine's Tailscale identity — the second interface the daemon binds,
//! and the only non-loopback `Host` values it will trust for a mutation.
//!
//! **Moved here verbatim.** Every rule in [`TailnetIdentity::trusted_hosts`] is
//! a decision about what an attacker can and cannot forge an origin for, and the
//! reasoning is in the doc comments rather than in a commit message for exactly
//! that reason.

use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// This machine's Tailscale identity, derived from a single `tailscale
/// status --json` invocation — the IPv4 to bind the tailnet listener to and,
/// when MagicDNS is enabled, the fully-qualified MagicDNS name a tailnet
/// peer's browser sends as `Host` when it reaches this machine by name
/// rather than by raw IP (see [`parse_tailnet_identity`], which is what
/// actually derives this).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailnetIdentity {
    /// The IPv4 address to bind the tailnet listener to.
    pub bind_ip: String,
    /// This machine's MagicDNS FQDN, when MagicDNS is enabled.
    pub magic_dns: Option<String>,
}

impl TailnetIdentity {
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
        let mut hosts = vec![self.bind_ip.clone()];
        hosts.extend(self.magic_dns.clone());
        hosts
    }

    /// The best host to show or copy for reaching this machine: the MagicDNS
    /// name when available — memorable, and, since it's also in
    /// [`Self::trusted_hosts`], guaranteed to work for mutations too — else
    /// the bare tailnet IPv4.
    pub fn advertise_host(&self) -> &str {
        self.magic_dns.as_deref().unwrap_or(&self.bind_ip)
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
        .find(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok())?
        .clone();
    // `DNSName` is reported rooted (a trailing `.`) and MagicDNS names are
    // already lowercase, but normalize defensively rather than assume.
    let fqdn = me.dns_name.trim_end_matches('.').to_ascii_lowercase();
    let magic_dns = if fqdn.is_empty() { None } else { Some(fqdn) };
    Some(TailnetIdentity { bind_ip, magic_dns })
}

/// How long `tailscale status --json` gets to answer before the dashboard
/// gives up on the tailnet and serves loopback only.
///
/// The probe is not optional-in-timing the way it is optional-in-outcome:
/// it runs *after* the loopback listener is bound, so for as long as it
/// blocks, the dashboard accepts connections and answers nothing — a state a
/// client cannot tell from a healthy server. The CLI talks to `tailscaled`,
/// which does wedge (probes stuck for minutes, orphaned by servers that had
/// already exited, observed on macOS). No tailnet is a degraded dashboard; a
/// dashboard that never answers is a broken one.
const TAILNET_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

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

/// The best host to show or copy for reaching this machine, used by `story
/// daemon status`/`story web address` output: this machine's MagicDNS FQDN when
/// [`tailnet_identity`] reports one (memorable, and guaranteed to work for
/// mutations too — see [`TailnetIdentity::trusted_hosts`]), else its bare
/// tailnet IPv4, else loopback if no tailnet identity is available at all.
pub fn reachable_host() -> String {
    tailnet_identity()
        .map(|identity| identity.advertise_host().to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string())
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
        assert_eq!(identity.bind_ip, "100.71.206.33");
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
        assert_eq!(identity.bind_ip, "100.71.206.33");
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
        assert_eq!(identity.bind_ip, "100.71.206.33");
    }

    #[test]
    fn trusted_hosts_includes_ip_and_fqdn_but_not_short_label_or_ipv6() {
        let identity = parse_tailnet_identity(STATUS_JSON).unwrap();
        let hosts = identity.trusted_hosts();
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
        let identity = TailnetIdentity {
            bind_ip: "100.71.206.33".to_string(),
            magic_dns: None,
        };
        assert_eq!(identity.trusted_hosts(), vec!["100.71.206.33".to_string()]);
    }

    #[test]
    fn advertise_host_prefers_magic_dns_over_ip() {
        let identity = parse_tailnet_identity(STATUS_JSON).unwrap();
        assert_eq!(identity.advertise_host(), "psamathe.tail983f02.ts.net");
    }

    #[test]
    fn advertise_host_falls_back_to_ip_without_magic_dns() {
        let identity = TailnetIdentity {
            bind_ip: "100.71.206.33".to_string(),
            magic_dns: None,
        };
        assert_eq!(identity.advertise_host(), "100.71.206.33");
    }
}
