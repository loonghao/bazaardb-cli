# ADR-0002: Prefer the local game-data cache for no-key queries

## Status

Accepted

## Context

The initial CLI adapter used a documented third-party Parse API. A cold query
therefore required an API key, and a configured key returned HTTP 401 during
live validation.

BazaarPlusPlus demonstrates a different boundary. Its collection feature asks
The Bazaar's `JsonGameDataManager` for the full card map. That manager reads the
game's cached `GameData.db`, a SQLite database whose `cards.Data` column contains
the complete JSON card definitions. BazaarPlusPlus uses separate HTTP routes
only for BazaarDB snapshot upload and account linking; it does not query a
hidden BazaarDB card API.

The installed game already owns download and update of this database. Reading
it locally avoids credentials and avoids bypassing BazaarDB's private website
endpoints.

## Decision

- Add `game-data` as a read-only adapter and make `auto` require it when a valid
  local database can be detected.
- Keep `parse` as an explicit optional provider only. Default CLI queries must
  not acquire a hidden API-key or network dependency.
- Accept an explicit `--game-data` path and auto-detect only narrow,
  platform-specific The Bazaar cache roots. Do not recursively scan unrelated
  user directories.
- Validate the SQLite header, enforce database and JSON size limits, open the
  connection with SQLite read-only and query-only flags, and require a `cards`
  table.
- Run SQLite loading on a blocking worker. Persist a cross-process normalized
  catalog snapshot and share its generation in memory.
- Publish the actual database SHA separately from a content ID over canonical
  normalized catalog bytes plus catalog-schema and resolver versions.
- Write snapshots with temp-file flush, sync, atomic rename, payload SHA, and
  automatic corruption/contract-mismatch rebuild. Use canonical path plus main
  DB and WAL length/mtime only as a local memo for avoiding redundant database
  hashes. Ignore volatile SHM reader state.
- Normalize each row as authoritative SQLite `cards.Id` plus JSON payload. An
  absent payload `Id` is recorded; a conflicting or malformed payload `Id`
  fails strict resolution without changing lookup/sort identity.
- Preserve raw card JSON for explicit `get` and `includeRawTemplate` operations.
  Use compact projections for default search and batch resolve responses.
- Include ordered normalized tooltip text and a per-template content digest in
  compact projections so consumers do not need raw templates for effect
  classification or cache integrity. Fence that digest with catalog content so
  cross-template static dependencies invalidate safely.
- Own a small reviewed BazaarDB identity map in the standalone CLI. Publish its
  content ID separately from GameData identity and expose matches only as
  provenance-bearing, non-authoritative `externalReferences`.
- Derive categories from canonical game fields: card `Type`, the `Merchant`
  tag, and trainer encounter identity. Treat this mapping as adapter policy,
  not a new domain schema.
- Include the content-addressed catalog identity in response-cache keys. A game
  update or resolver/schema change creates a new cache generation automatically.
- Expose a loopback-only read API at `/v1/catalog/status`, `/search`, and
  `/resolve`. Every response is inspection-only, non-authoritative for actions,
  no-store, and path-free.
- Resolve 1-64 canonical template tuples with strict whole-batch failure,
  cumulative tier attributes, object-only selected definitions, typed component
  completeness, collision-free structured selectors, and bounded output.
  Resolve only the explicitly applied enchantment by default.
- Bound persistent snapshots to three generations, 30 days, and 1 GiB; expose
  count/bytes and lifecycle through status, automatic/manual prune, and clear.
- Own static catalog data only. Player logs, live board/stash/selection, and
  per-instance overrides remain outside this process.
- Do not claim that local data includes BazaarDB's derived history, builds,
  stats, or inferred relationships.

## Consequences

### Positive

- Users with The Bazaar installed can query current cards without an API key or
  network request.
- The default path follows an existing open-source integration and respects the
  ownership boundary between game data and BazaarDB website enrichment.
- Cold searches normalize the database once; later processes load an
  integrity-checked snapshot without rehashing or parsing the database.
- Parse remains available without leaking its authentication concerns into the
  local adapter.

### Negative

- Users must launch The Bazaar once or pass a valid database path.
- The local catalog follows the installed game-data generation and can differ
  from BazaarDB's currently indexed patch.
- Merchant and trainer categorization depends on game conventions and requires
  regression tests when those conventions change.

### Neutral

- `--cache-mode offline` can still read local game data because that operation
  performs no network I/O. Parse retains strict offline-cache semantics.

## Alternatives Considered

**Scrape or replay BazaarDB website requests**

Rejected because those endpoints are private, token-protected, and not the
mechanism BazaarPlusPlus uses for its card collection.

**Bundle or redistribute `GameData.db`**

Rejected because the installed game already maintains the data and the CLI does
not need to take ownership of third-party content distribution.

**Remove the Parse provider**

Rejected because it remains useful on systems without the game and preserves a
tested remote-provider seam.

## References

- https://github.com/BazaarPlusPlus/BazaarPlusPlus/blob/master/bazaarplusplus-mod/src/BazaarPlusPlus/GameInterop/StaticCards/BppStaticDataAccess.cs
- https://github.com/BazaarPlusPlus/BazaarPlusPlus/blob/master/bazaarplusplus-mod/src/BazaarPlusPlus.ModApi/ModApiRoutes.cs
- https://www.playthebazaar.com/mod-policy
