//! The bisection of a red verification batch, as its record keeps it
//! (SH-833; spec B7). A red batch ends `released` and carries its bisection:
//! every probe the search made and how the search ended. A probe of two or
//! more members is a batch record of its own, which names the bisection it
//! serves with [`BisectionOf`].

use super::BatchId;
use crate::domain::gate_verdict::GateVerdict;
use serde::{Deserialize, Serialize};

/// The bisection a probe batch serves: its parent red batch and how many of
/// the parent's leading members it merges.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BisectionOf {
    /// The red batch being bisected.
    pub parent: BatchId,
    /// How many leading members of the parent the probe merges; the probe
    /// has exactly these members.
    pub prefix: u32,
}

/// Why a prefix tree was judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeKind {
    /// A gate the search asked for.
    Search,
    /// A qualifying receipt for the tree, found before the search: green
    /// without a gate.
    Receipt,
    /// A gate of the certified prefix so that it can land, after the search
    /// found the culprit.
    Landing,
}

/// One prefix tree the bisection judged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BisectionProbe {
    /// How many leading members of the red batch the tree merges.
    pub prefix: u32,
    /// Why it was judged.
    pub kind: ProbeKind,
    /// The probe's own batch record, for a prefix of two or more members.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchId>,
    /// The prefix's merge tree: the tree its verdict counts for.
    pub tree: String,
    /// What the gate (or the receipt) said.
    pub verdict: GateVerdict,
    /// The gate's full log, when it failed its tests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// The gate's own words about the result, or why it did not count.
    pub detail: String,
    /// How long the gate ran; 0 for a receipt.
    pub seconds: u64,
}

/// How a bisection ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum BisectionOutcome {
    /// The member at `position` turns the green tree of the members before
    /// it red.
    Culprit {
        /// The culprit's public id.
        story_id: String,
        /// Its 1-based position in the red batch.
        position: u32,
        /// The red tree: the culprit merged onto the members before it.
        tree: String,
        /// The red gate's full log.
        log: String,
        /// How many leading members are certified together (0: none).
        certified: u32,
        /// What the verifier did with the finding, as an operator reads it.
        #[serde(default)]
        detail: String,
    },
    /// The search ended without a verdict it could attribute; no story was
    /// blamed.
    Inconclusive {
        /// Why.
        detail: String,
    },
    /// The verifier stopped (an operator stop or a restart) before the
    /// search ended; no story was blamed.
    Interrupted {
        /// Why.
        detail: String,
    },
}

/// The bisection of one red batch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchBisection {
    /// Every prefix tree judged, in order.
    #[serde(default)]
    pub probes: Vec<BisectionProbe>,
    /// How the search ended; absent while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<BisectionOutcome>,
}

impl BatchBisection {
    /// Whether the search has not recorded how it ended: it is running, or
    /// its verifier stopped before it could say.
    #[must_use]
    pub fn is_unfinished(&self) -> bool {
        self.outcome.is_none()
    }
}
