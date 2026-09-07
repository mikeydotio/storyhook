# SH-588 — Installed launcher dispatch guard

- Hook fix and dialog readiness merged in PRs #683/#684.
- Current follow-up: `fix/release-pointer-fixtures`.
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
- Release gate found three Chromium failures (414 passed): focus measurement
  still assumed a toolbar stepper; two Enter tests outran the deletion plan.
- Follow-up opens the real Full Auto dialog before measuring focus and makes
  shared deletion helpers await the completed server plan. A delayed-response
  regression checks both filled and empty confirmation fields.
- A resumed browser gate exposed stale raw-pointer coordinates in the blocked
  reference race tests. Reuse settledBoundingBox immediately before each press;
  apply the same repair to the drawer-open race sibling. Keep split gestures.
- WebKit 2336 also hangs before issuing navigation requests after roughly 65
  fresh contexts. A 128-context dashboard probe reproduced it on 1.62.1 and
  passed on 1.63.0. The final probe uses a fixture document to avoid SSE buildup.
  Upgrade pinned Playwright to 1.63.0, carrying the upstream fix for
  microsoft/playwright#42385; existing retry and timeout policies remain.
- Chromium and upgraded WebKit each passed all 15 affected cases, including
  the final navigation probe. Desktop sign-in resolved the separate native
  startup failure. With the desktop active, the old browser also passes the
  fixture-document probe; that comparison is not new RED evidence.
- PR #685 carries the follow-up. Both behavior commits independently pass
  `make test`; full release validation belongs on SH-588. `npm ci` restored
  the committed Playwright 1.63.0 dependency after the old-browser control.
- Adopted release-order repair: gate the versioned tree before push/install.
  Five real-script regressions reproduced certification of the old tree and
  an unchecked push/install after the bump. Cover public and local paths,
  successful and failed gates, and failed bumps against a disposable Git remote.
- Next: complete `make test-full`, submit PR #685 for centralized verification,
  then run the normal patch release and publication flow. Install the packaged
  Codex plugin and retry `$story do SH-560` through its installed launcher.
