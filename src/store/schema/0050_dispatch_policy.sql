-- Installation and project policy have independent lifetimes. Project policy
-- follows the project identity across its checkouts and is deleted with it.
CREATE TABLE dispatch_policy_global (
    agent TEXT NOT NULL CHECK (agent IN ('codex', 'claude')),
    complexity TEXT NOT NULL CHECK (complexity IN ('low', 'medium', 'high')),
    model TEXT,
    effort TEXT,
    PRIMARY KEY (agent, complexity),
    CHECK (model IS NULL OR (agent = 'codex' AND model IN ('gpt-6-astra', 'gpt-5.6-sol'))
        OR (agent = 'claude' AND model IN ('opus', 'fable'))),
    CHECK (effort IS NULL OR effort IN ('low', 'medium', 'high', 'xhigh', 'max')
        OR (agent = 'codex' AND effort IN ('none', 'ultra')))
);
CREATE TABLE dispatch_policy_project (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    agent TEXT NOT NULL CHECK (agent IN ('codex', 'claude')),
    complexity TEXT NOT NULL CHECK (complexity IN ('low', 'medium', 'high')),
    model TEXT,
    effort TEXT,
    PRIMARY KEY (project_id, agent, complexity),
    CHECK (model IS NULL OR (agent = 'codex' AND model IN ('gpt-6-astra', 'gpt-5.6-sol'))
        OR (agent = 'claude' AND model IN ('opus', 'fable'))),
    CHECK (effort IS NULL OR effort IN ('low', 'medium', 'high', 'xhigh', 'max')
        OR (agent = 'codex' AND effort IN ('none', 'ultra')))
);
