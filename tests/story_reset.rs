//! Native and card reset preserve their distinct contracts under shared ownership.
#[path = "story_reset/card.rs"]
mod card;
#[path = "story_reset/compatibility.rs"]
mod compatibility;
#[path = "story_reset/native.rs"]
mod native;
#[path = "story_reset/orphan.rs"]
mod orphan;

#[path = "story_reset/delivery.rs"]
mod delivery;

#[path = "story_reset/quiescent.rs"]
mod quiescent;
