-- Manual permission is durable; live ownership and cancellation are not.
CREATE TABLE verification_control (
    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
);
