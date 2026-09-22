-- Complexity is metadata, not a queue sort key. The snapshot is the read
-- model for it, as it is for priority_assessed. Do not invent choice events.
UPDATE stories SET snapshot = json_insert(snapshot,
    '$.complexity', 'medium', '$.complexity_assessed', json('false'));
