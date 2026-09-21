# Dashboard preferences per named token (SH-747)

The dashboard must restore its filters and display choices when the same named
token opens another browser. Browser storage has no reliable token owner, so
old `localStorage` and `sessionStorage` values are not imported. The server's
`tokens.json` record is the source of truth. Revoking or expiring that record
ends its preferences; a re-minted token starts with defaults.

## Contract

`GET /api/preferences` returns the effective preference object. `PATCH
/api/preferences` accepts an object containing one or more known top-level
fields and returns the effective object after saving. Both require a live
named token through the existing header or cookie rules. PATCH also uses the
dashboard mutation guard. A master daemon token cannot own preferences.
Responses use `Cache-Control: no-store` and contain no credential or hash.

Fields are `filter`, `sort`, `columnSort`, `hiddenColumns`, `view`,
`showArchived`, `hideEmptyColumns`, `keepNotices`, `filtersOpen`,
`drawerSections`, `dispatchDefaults`, and `repoId`. The server validates each
field before saving. A PATCH merges only supplied fields while holding the
token registry lock. Different fields from different tabs therefore do not
overwrite each other; writes to the same field take arrival order. A write
returns success only after the token sidecar is replaced.

The browser loads this object before it chooses a remembered project. It
queues its writes, debounces text search, and discards old replies when a
token exchange changes the credential. A preference PATCH refused with 401
opens the token modal but is never replayed with the replacement token.
A failed write restores the last
confirmed field and shows an error. A failed initial read leaves defaults on
screen but disables preference saves until a later successful load. Existing
cross-project filter pruning and Clear filters behavior remain in effect.
The mobile filter sheet, mobile metadata disclosure, and other temporary
overlays remain in page memory.

SH-746 owns project visibility. Its hidden-project field can coexist on the
same token record; SH-747 does not change that field or the visibility API.
