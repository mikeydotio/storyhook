//! Administrative handoffs fail explicitly until their transactional handler is installed.
use crate::service::Ctx;
use crate::store::{Continuation, Store, StoreError, StoryRow, WriteOps};

pub(super) fn record(
    _tx: &mut impl WriteOps,
    _ctx: &Ctx<'_, impl Store>,
    _record: &Continuation,
    _row: &StoryRow,
) -> Result<(), StoreError> {
    Err(StoreError::Validation(
        "administrative obviation continuation is not supported by this build".into(),
    ))
}
