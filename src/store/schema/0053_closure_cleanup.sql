-- Intent and story projection commit together; external effects run later.
CREATE TABLE closure_cleanups (
    project_id INTEGER NOT NULL,
    story_no INTEGER NOT NULL,
    token TEXT NOT NULL UNIQUE,
    record_json TEXT NOT NULL CHECK (json_valid(record_json)),
    PRIMARY KEY (project_id, story_no),
    FOREIGN KEY (project_id, story_no) REFERENCES stories(project_id, story_no) ON DELETE CASCADE
);

WITH closed AS MATERIALIZED (
    SELECT project_id, story_no, head_global_seq, lower(hex(randomblob(16))) AS token
    FROM stories WHERE superstate='CLOSED'
)
INSERT INTO closure_cleanups (project_id, story_no, token, record_json)
SELECT project_id, story_no, token,
       json_object('project',project_id,'story',story_no,'token',token,
                   'generation',head_global_seq,'lease',NULL,'completed',json('false'),
                   'retry_at',NULL,'detail',NULL)
FROM closed;
