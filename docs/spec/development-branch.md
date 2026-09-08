# Development and stable branch policy — SH-595

StoryHook uses two protected long-lived branches. `dev` is GitHub's default
branch and the integration line for ongoing work. `main` contains only stable
public releases.

| Branch | Purpose | Receives |
| --- | --- | --- |
| `dev` | Default integration branch | Ordinary feature and repair PRs |
| `main` | Stable public-release branch | `release/*` PRs created by `scripts/release.sh` |

Feature branches start from current `dev`. Direct pushes to either long-lived
branch are forbidden by GitHub rulesets and refused locally when they lack a
test receipt. Merge commits are the only accepted merge method. Generic
StoryHook scaffolding names the repository's default branch rather than
assuming these repository-specific names.

## Stable release flow

Public releases start only from a clean local `dev` equal to `origin/dev`.
`scripts/release.sh` creates `release/<version>` from that commit, applies the
version bump, and runs the full release gate. It then:

1. Opens the stable-release PR from `release/<version>` to `main` and lands it
   through `scripts/land-pr.sh`.
2. Re-pushes the same local release branch, opens a synchronization PR to
   `dev`, and lands that PR through the same guarded path.
3. Deletes the local release branch only after both merges succeed.
4. Returns to stable `main` to build artifacts, create the tag, and create the
   draft GitHub release.

The second merge is intentionally before tagging. If `dev` moved and the
resulting merge tree has no qualifying receipt, the guarded merge refuses and
release creation stops before a tag or draft exists.

## Observers

Browser and coverage observers track `origin/dev`, because they measure the
health and selective-test baseline of ongoing development. The release
observer stays on `origin/main`, because it independently validates the stable
source used for tags and published artifacts. `scripts/branch-policy.sh` is
the repository-specific shell source of truth for these roles.

## Out of scope

This policy does not create a beta release command, prerelease channel, or
alternate installer feed. Those can build on `dev` separately without
weakening the stable meaning of `main`.
