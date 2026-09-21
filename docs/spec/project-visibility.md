# Per-token project visibility (SH-746)

## Intent

A person with many registered projects can choose which ones appear on the dashboard for each named browser token. The preference organizes the view; it does not restrict access. Settings always lists every registered project, and a direct project URL remains usable.

## Behavior

- Existing and newly minted tokens show every project until a checkbox is cleared. A newly registered project is shown by default.
- Settings has one labeled Show checkbox per project, including projects without a checkout. Saving fails visibly and restores the previous choice when the sidecar cannot be written.
- Home cards and totals, the header project menu, global Drafts, and the new-story project choice use the visible subset. An already selected project remains available as the new-story target even if it was hidden while its board was open.
- When the visible subset is empty, Home links to Settings. An explicit deep link can still open a hidden project; a remembered hidden project does not reopen automatically.
- A successful change emits a catalog event, so other tabs refresh. The existing catalog safety poll also refreshes preferences after a lost event.

## Storage and API

`tokens.json` stores a set of hidden project UUIDs on each named token record. UUIDs keep a deleted project's preference from affecting a later project with the same slug. Missing fields in older sidecars deserialize as an empty set. Revocation deletes the record and its preferences; a re-minted token starts with an empty set.

`GET /api/repos` still returns the full catalog and adds `visible: boolean` to each entry. It uses the named header or same-origin cookie accepted by the normal admission gate; a master-token request sees every entry as visible. The response is not cached.

`PATCH /api/repos/{slug}/visibility` accepts a JSON object with a required boolean `visible`. It requires a live named token and the normal mutation guard, resolves the slug to the current project UUID, and saves that token's preference before returning `{"project":"slug","visible":true|false}`. Unknown projects return 404; a master token cannot write a per-token preference. Successful writes publish a catalog change. A storage error returns 500 and leaves the in-memory preference unchanged.

## Verification

Registry tests cover default visibility, token isolation, reload, revocation and re-mint, and write rollback. Routed API tests cover full-catalog visibility, input and authorization failures, and change-feed publication. Browser tests cover checkbox persistence, filtered navigation, an empty visible set, direct links, and a second token.
