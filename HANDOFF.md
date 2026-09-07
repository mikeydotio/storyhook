# SH-588 — Installed launcher dispatch guard

- Branch: `fix/launcher-dispatch-guard`.
- Failure: the installed-artifact guard explicitly denied `dispatch`, treating
  story/worktree mutations as if they edited the installed launcher.
- Regression reproduced RED before the hook changed; targeted suites passed
  53 tests after the behavior fix. Final gate status belongs on SH-588.
- The exact installer-produced launcher now admits supported dispatch forms.
  Identity, managed-operand and ambiguous-shell restrictions still apply.
- Integration executes the real launcher/helper/CLI/daemon/Git path and checks
  the persisted claim, worktree, prompt delivery and unchanged installed files.
  Provider installation and terminal behavior use the existing test doubles.
- `story doctor install` found plugin 2.4.0 with binary 2.4.2. The fix must ship
  through the release/plugin installer before the live SH-560 retry can work.
- No installed copies, override files or version metadata were changed.
- Adopted baseline gate repair: the mobile-browser coverage assertion omitted
  the existing open-PR-chip exception. Preserve its coverage on both engines
  and update the stale assertion/comments. Core Rust battery passed 3703 tests.
- Adopted baseline isolation repair: pin the original port only on the cookie
  spec's daemon-restart subprocess. The browser runner retains the shared
  ephemeral-port environment; its existing isolation detector stays intact.
- Next: finish review/verification, release and install through the normal
  release workflow, then retry `$story do SH-560`.
