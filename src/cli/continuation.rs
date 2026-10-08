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
    let usage = || AppError::Usage(crate::cli::model::usage::CONTINUATION_1.into());
    let action = args
        .get(1)
        .and_then(|word| super::model::ContinuationVerb::find(word))
        .ok_or_else(usage)?;
    use super::model::ContinuationVerb;
    if action == ContinuationVerb::Capabilities && args.len() == 2 {
        return Ok(Invocation::Continuation {
            id: String::new(),
            action: ContinuationAction::Capabilities,
        });
    }
    let id = args.get(2).ok_or_else(usage)?.clone();
    let action = match action {
        ContinuationVerb::Request if args.len() == 4 && args[3] == "--stdin" => {
            ContinuationAction::Request
        }
        ContinuationVerb::Status if args.len() == 3 => ContinuationAction::Status,
        ContinuationVerb::Receipt if args.len() == 5 && args[4] == "--stdin" => {
            ContinuationAction::Receipt {
                request: args[3].clone(),
            }
        }
        ContinuationVerb::Retry if args.len() == 4 => ContinuationAction::Retry {
            request: args[3].clone(),
        },
        ContinuationVerb::Ack if args.len() == 12 => {
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
        ContinuationVerb::Capabilities
        | ContinuationVerb::Request
        | ContinuationVerb::Status
        | ContinuationVerb::Receipt
        | ContinuationVerb::Retry
        | ContinuationVerb::Ack => return Err(usage()),
    };
    Ok(Invocation::Continuation { id, action })
}
