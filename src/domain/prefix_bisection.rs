//! Bisection of a red verification batch over prefixes of its merge chain
//! (SH-833; spec position B7 in `docs/spec/verification-batching.md`).
//!
//! A batch of `k` members is a first-parent chain of merge commits
//! `P1..Pk`: `Pj` merges member `j` onto `Pj-1`, and `P0` is the base. The
//! batch gate judged `Pk` red. The search keeps two facts: `green`, the
//! longest prefix known green (the base, 0, is assumed green because it
//! landed through a gate), and `red`, the shortest prefix known red. It gates
//! the prefix halfway between, rounding down, until the two are adjacent:
//! member `red` then turns the green tree `P(red-1)` red, and members
//! `1..=green` are certified together.
//!
//! The invariant `green < red` is the whole proof. Whatever the probes
//! answer, the answer is a real change from green to red, so the search needs
//! no assumption that red stays red as members are added; when culprits are
//! independent it finds the first one. It needs at most `ceil(log2 k)`
//! probes, and a prefix a receipt already certifies needs none.

use std::fmt;

/// Why the search refused an input: a caller defect, never a verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BisectionError(String);

impl fmt::Display for BisectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BisectionError {}

/// What a gate of one prefix found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeVerdict {
    /// The prefix tree passed the gate.
    Green,
    /// The prefix tree failed the gate.
    Red,
}

/// What the search does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Gate the tree of the first `prefix` members.
    Probe(usize),
    /// The search is over.
    Culprit {
        /// The 1-based position of the member whose merge turns a green
        /// prefix red.
        position: usize,
        /// How many leading members are certified together (0: none).
        certified: usize,
    },
}

/// The state of one search over a red batch of two or more members.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefixBisection {
    members: usize,
    green: usize,
    red: usize,
}

impl PrefixBisection {
    /// A search over a batch of `members` whose whole tree is red.
    pub fn new(members: usize) -> Result<Self, BisectionError> {
        if members < 2 {
            return Err(BisectionError(format!(
                "a bisection needs a red batch of two or more members, not {members}"
            )));
        }
        Ok(Self {
            members,
            green: 0,
            red: members,
        })
    }

    /// How many members the red batch has.
    #[must_use]
    pub fn members(&self) -> usize {
        self.members
    }

    /// The longest prefix known green (0 is the base).
    #[must_use]
    pub fn green(&self) -> usize {
        self.green
    }

    /// The shortest prefix known red.
    #[must_use]
    pub fn red(&self) -> usize {
        self.red
    }

    /// Whether the verdict on `prefix` is still unknown and would narrow
    /// the search.
    #[must_use]
    pub fn is_open(&self, prefix: usize) -> bool {
        self.green < prefix && prefix < self.red
    }

    /// Takes a qualifying receipt for `prefix`'s tree as a green verdict
    /// that cost no gate. Answers whether it narrowed the search; a receipt
    /// for a prefix outside the open interval changes nothing.
    pub fn certified(&mut self, prefix: usize) -> bool {
        if self.is_open(prefix) {
            self.green = prefix;
            true
        } else {
            false
        }
    }

    /// The next step: a prefix to gate, or the culprit once green and red
    /// are adjacent.
    #[must_use]
    pub fn next(&self) -> Step {
        if self.red - self.green <= 1 {
            Step::Culprit {
                position: self.red,
                certified: self.green,
            }
        } else {
            Step::Probe(self.green + (self.red - self.green) / 2)
        }
    }

    /// Records the gate's verdict on `prefix`, which must be open.
    pub fn record(&mut self, prefix: usize, verdict: ProbeVerdict) -> Result<(), BisectionError> {
        if !self.is_open(prefix) {
            return Err(BisectionError(format!(
                "prefix {prefix} is not between the green prefix {} and the red prefix {}",
                self.green, self.red
            )));
        }
        match verdict {
            ProbeVerdict::Green => self.green = prefix,
            ProbeVerdict::Red => self.red = prefix,
        }
        Ok(())
    }

    /// The most probes a search over `members` can need: `ceil(log2
    /// members)`, and 0 for fewer than two members.
    #[must_use]
    pub fn max_probes(members: usize) -> u32 {
        if members < 2 {
            0
        } else {
            usize::BITS - (members - 1).leading_zeros()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LARGEST: usize = 8;

    /// Runs a search against `red_at` (which prefixes are red; the whole
    /// batch always is), checking every probe is open and new. Answers the
    /// final step and the probes made.
    fn search(
        mut bisection: PrefixBisection,
        red_at: impl Fn(usize) -> bool,
    ) -> (Step, Vec<usize>) {
        let mut probes = Vec::new();
        loop {
            match bisection.next() {
                Step::Probe(prefix) => {
                    assert!(bisection.is_open(prefix), "{prefix} in {bisection:?}");
                    assert!(!probes.contains(&prefix), "{prefix} probed twice");
                    probes.push(prefix);
                    let verdict = if red_at(prefix) {
                        ProbeVerdict::Red
                    } else {
                        ProbeVerdict::Green
                    };
                    bisection.record(prefix, verdict).unwrap();
                }
                culprit @ Step::Culprit { .. } => return (culprit, probes),
            }
        }
    }

    #[test]
    fn a_single_culprit_is_found_at_every_position_within_the_bound() {
        for members in 2..=LARGEST {
            for culprit in 1..=members {
                let (step, probes) = search(PrefixBisection::new(members).unwrap(), |prefix| {
                    prefix >= culprit
                });
                assert_eq!(
                    step,
                    Step::Culprit {
                        position: culprit,
                        certified: culprit - 1
                    },
                    "k={members} culprit={culprit}"
                );
                assert!(
                    probes.len() as u32 <= PrefixBisection::max_probes(members),
                    "k={members} culprit={culprit}: {probes:?}"
                );
            }
        }
    }

    #[test]
    fn two_independent_culprits_yield_the_first() {
        for members in 3..=LARGEST {
            for first in 1..members {
                for second in first + 1..=members {
                    // Independent culprits: a prefix is red once it holds either.
                    let (step, _) = search(PrefixBisection::new(members).unwrap(), |prefix| {
                        prefix >= first || prefix >= second
                    });
                    assert_eq!(
                        step,
                        Step::Culprit {
                            position: first,
                            certified: first - 1
                        },
                        "k={members} culprits {first} and {second}"
                    );
                }
            }
        }
    }

    /// Every assignment of verdicts to the prefixes below the batch, however
    /// non-monotone: the answer is always a real change from green to red,
    /// within the bound.
    #[test]
    fn any_verdicts_yield_a_real_green_to_red_transition() {
        for members in 2..=LARGEST {
            for mask in 0u32..(1 << (members - 1)) {
                let red_at = |prefix: usize| prefix == members || mask & (1 << (prefix - 1)) != 0;
                let (step, probes) = search(PrefixBisection::new(members).unwrap(), red_at);
                let Step::Culprit {
                    position,
                    certified,
                } = step
                else {
                    unreachable!("the search ends with a culprit");
                };
                assert_eq!(certified + 1, position, "k={members} mask={mask:b}");
                assert!(red_at(position), "k={members} mask={mask:b}");
                assert!(
                    certified == 0 || !red_at(certified),
                    "k={members} mask={mask:b}"
                );
                assert!(probes.len() as u32 <= PrefixBisection::max_probes(members));
            }
        }
    }

    /// Receipts exist only for green trees. Any set of them below the
    /// culprit still finds it, and the probes left fit the narrowed bound.
    #[test]
    fn receipts_raise_the_green_prefix_and_never_cost_a_probe() {
        for members in 2..=LARGEST {
            for culprit in 1..=members {
                for receipts in 0u32..(1 << (culprit - 1)) {
                    let mut bisection = PrefixBisection::new(members).unwrap();
                    // Largest first, as the verifier asks.
                    for prefix in (1..culprit).rev() {
                        if receipts & (1 << (prefix - 1)) != 0 && bisection.certified(prefix) {
                            break;
                        }
                    }
                    let left = bisection.red() - bisection.green();
                    let (step, probes) = search(bisection, |prefix| prefix >= culprit);
                    assert_eq!(
                        step,
                        Step::Culprit {
                            position: culprit,
                            certified: culprit - 1
                        },
                        "k={members} culprit={culprit} receipts={receipts:b}"
                    );
                    assert!(probes.len() as u32 <= PrefixBisection::max_probes(left));
                }
            }
        }
    }

    #[test]
    fn a_receipt_outside_the_open_interval_changes_nothing() {
        let mut bisection = PrefixBisection::new(4).unwrap();
        assert!(!bisection.certified(0));
        assert!(!bisection.certified(4));
        assert!(!bisection.certified(9));
        assert!(bisection.certified(2));
        assert!(!bisection.certified(1), "below the green prefix");
        assert_eq!((bisection.green(), bisection.red()), (2, 4));
    }

    #[test]
    fn the_first_probe_rounds_down_so_the_head_goes_first_in_small_batches() {
        let first = |members| PrefixBisection::new(members).unwrap().next();
        assert_eq!(first(2), Step::Probe(1));
        assert_eq!(first(3), Step::Probe(1));
        assert_eq!(first(4), Step::Probe(2));
        assert_eq!(first(5), Step::Probe(2));
    }

    #[test]
    fn a_verdict_outside_the_open_interval_is_refused() {
        let mut bisection = PrefixBisection::new(4).unwrap();
        for prefix in [0, 4, 5] {
            assert!(bisection.record(prefix, ProbeVerdict::Green).is_err());
        }
        bisection.record(2, ProbeVerdict::Red).unwrap();
        assert!(
            bisection.record(3, ProbeVerdict::Green).is_err(),
            "above red"
        );
        assert!(bisection.record(2, ProbeVerdict::Green).is_err(), "twice");
        assert_eq!((bisection.green(), bisection.red()), (0, 2));
    }

    #[test]
    fn fewer_than_two_members_is_not_a_bisection() {
        assert!(PrefixBisection::new(0).is_err());
        assert!(PrefixBisection::new(1).is_err());
        assert_eq!(PrefixBisection::new(2).unwrap().members(), 2);
    }

    #[test]
    fn the_probe_bound_is_the_ceiling_of_log2() {
        let expected = [0, 0, 1, 2, 2, 3, 3, 3, 3, 4, 4];
        for (members, bound) in expected.iter().enumerate() {
            assert_eq!(PrefixBisection::max_probes(members), *bound, "{members}");
        }
    }
}
