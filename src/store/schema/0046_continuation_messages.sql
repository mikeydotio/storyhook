-- Native Stop feedback can continue several assistant messages in one turn.
-- COALESCE retains uniqueness for historical turn-only receipts (NULLs alone
-- would allow duplicate legacy origins in a SQLite unique index).
DROP INDEX continuation_origin;
CREATE UNIQUE INDEX continuation_origin ON continuations(project_id,story_no,
 json_extract(record,'$.generation.provider'),
 json_extract(record,'$.generation.session_id'),
 json_extract(record,'$.generation.turn_id'),
 json_extract(record,'$.handoff.kind'),
 COALESCE(json_extract(record,'$.generation.message_id'),''));
