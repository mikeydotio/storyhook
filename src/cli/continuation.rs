//! Strict grammar for durable session handoffs.
use super::Invocation;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
/// Context handoff lifecycle operations; approval is deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContinuationAction {
    /// Read this daemon's supported protocol version.
    Capabilities,
    /// Capture and durably record a handoff document from standard input.
    Request,
    /// Read all requests and current story sequence.
    Status,
    /// Record the native provider's compaction event.
    Receipt {
        /// Request UUID.
        request: String,
    },
    /// Explicitly reconsider a needs-attention request.
    Retry {
        /// Request UUID.
        request: String,
    },
    /// Acknowledge a receiving root's current story and Git review.
    Ack {
        /// Request UUID.
        request: String,
        /// Exact current story global sequence.
        reviewed_seq: i64,
        /// Exact reviewed Git commit.
        head: String,
        /// Provider spelling, codex or claude.
        provider: String,
        /// Receiving root session identity.
        session_id: String,
    },
}
pub(super) fn parse(args: &[String]) -> Result<Invocation, AppError> {
    let usage = || {
        AppError::Usage("usage: story continuation request <id> --stdin | status <id> | receipt <id> <request> --stdin | retry <id> <request> | ack <id> <request> --reviewed-seq <n> --head <sha> --provider <codex|claude> --session-id <id> | capabilities".into())
    };
    let action = args.get(1).map(String::as_str).ok_or_else(usage)?;
    if action == "capabilities" && args.len() == 2 {
        return Ok(Invocation::Continuation {
            id: String::new(),
            action: ContinuationAction::Capabilities,
        });
    }
    let id = args.get(2).ok_or_else(usage)?.clone();
    let action = match action {
        "request" if args.len() == 4 && args[3] == "--stdin" => ContinuationAction::Request,
        "status" if args.len() == 3 => ContinuationAction::Status,
        "receipt" if args.len() == 5 && args[4] == "--stdin" => ContinuationAction::Receipt {
            request: args[3].clone(),
        },
        "retry" if args.len() == 4 => ContinuationAction::Retry {
            request: args[3].clone(),
        },
        "ack" if args.len() == 12 => {
            let mut values = std::collections::BTreeMap::new();
            for pair in args[4..].as_chunks::<2>().0 {
                if values.insert(pair[0].as_str(), pair[1].as_str()).is_some() {
                    return Err(usage());
                }
            }
            let reviewed_seq = values
                .remove("--reviewed-seq")
                .ok_or_else(usage)?
                .parse::<i64>()
                .map_err(|_| usage())?;
            let head = values.remove("--head").ok_or_else(usage)?.to_string();
            let provider = values.remove("--provider").ok_or_else(usage)?.to_string();
            let session_id = values.remove("--session-id").ok_or_else(usage)?.to_string();
            if !values.is_empty()
                || reviewed_seq < 1
                || !matches!(head.len(), 40 | 64)
                || !head.bytes().all(|c| c.is_ascii_hexdigit())
                || !matches!(provider.as_str(), "codex" | "claude")
                || session_id.is_empty()
            {
                return Err(usage());
            }
            ContinuationAction::Ack {
                request: args[3].clone(),
                reviewed_seq,
                head,
                provider,
                session_id,
            }
        }
        _ => return Err(usage()),
    };
    Ok(Invocation::Continuation { id, action })
}
