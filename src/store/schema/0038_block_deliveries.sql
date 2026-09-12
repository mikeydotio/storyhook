-- Ordered effects belong to the mutation transaction; terminal delivery is external.
CREATE TABLE block_deliveries (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    story_no INTEGER NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('interrupt', 'resume')),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN
        ('pending', 'attempting', 'delivered', 'unreached', 'uncertain', 'superseded')),
    target TEXT,
    detail TEXT NOT NULL DEFAULT '',
    FOREIGN KEY (project_id, story_no) REFERENCES stories(project_id, story_no) ON DELETE CASCADE
);
CREATE INDEX block_delivery_pending ON block_deliveries(status, id);
