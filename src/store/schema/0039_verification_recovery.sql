-- One receipt per project; acknowledgement evidence survives incident deletion.
CREATE TABLE verification_recovery (
    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    receipt TEXT NOT NULL CHECK (json_valid(receipt))
);
