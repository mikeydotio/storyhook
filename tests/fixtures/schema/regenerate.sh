#!/usr/bin/env bash
#
# Regenerates tests/fixtures/schema/v1.db.
#
# The fixture is a storyhook store at schema version 1, carrying one small
# project with the shapes a future migration is most likely to have to touch:
# both directions of a relation, labels, custom states and types, a member,
# fully-populated settings, an archived story, and a github base.
#
# It exists because a migration needs an *old* database to migrate, and once
# this version's writing code has moved on there is no way to produce one. It
# is built through the store's public API rather than by hand-written SQL, so
# what it contains is what storyhook actually writes.
#
# `tests/store_schema_fixture.rs` compares the committed file against a
# database freshly built from the migration list on every `make test` run, so a
# migration edited in place instead of appended fails the gate rather than
# silently invalidating the fixture. Run this only when the schema legitimately
# changes, and review the resulting binary diff deliberately.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../.."

STORYHOOK_REGENERATE_SCHEMA_FIXTURE=1 \
  cargo test --workspace --test store_schema_fixture -- \
  the_committed_fixture_is_a_schema_v1_database_that_still_matches_the_migrations \
  --exact --nocapture

echo "Regenerated tests/fixtures/schema/v1.db — review the diff before committing."
