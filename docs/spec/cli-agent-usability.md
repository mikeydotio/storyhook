# CLI agent usability (SH-755)

Baseline: v3.0.3. The CLI is the supported agent interface. Remove the MCP
server and plugin registration. Keep a usage error and help topic that explain
migration. The retired command must not read protocol input or start a daemon.

Preserve CLI syntax, output fields, exit codes, and existing mutation rules.
Correct help against the parser and renderer. Explain output exceptions,
project selection, literal text, guarded writes, and uncertain write outcomes.
Unknown-flag diagnostics must name a command, not a title or story ID, and
must point to a help topic that exists.

Keep command redesign separate. Record the full command inventory, evidence,
recommendations, compatibility costs, and acceptance criteria in
[the HTML audit](../reports/SH-755-cli-audit.html). SH-757 is the related, low-priority,
no-auto follow-up story. Approval of this story does not approve that redesign.

Regression tests exercise CLI retirement, plugin configuration, parser errors,
and renderer contracts. Run the changed-tree selector and only new and directly
impacted tests. The central verifier owns the full suite and submission.

The MCP module is removed from the Rust library API. No store schema changes
are required. Current CLI JSON consumers retain their existing contracts.
