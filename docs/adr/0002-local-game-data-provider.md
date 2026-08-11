# ADR-0002: Prefer the local game-data cache for no-key queries

## Status

Accepted

## Context

The installed game maintains a local `GameData.db` cache. Its SQLite `cards`
table contains the static JSON objects required by this CLI. Reading that file
locally avoids credentials and does not require a BazaarDB website request.

## Decision

- Add `game-data` as a read-only adapter and make `auto` require it when a valid
  local database can be detected.
- Do not ship a BazaarDB website or independent scraping-wrapper provider.
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
- Normalize each row using SQLite `cards.Id` as the lookup key plus JSON payload. An
  absent payload `Id` is recorded; a conflicting or malformed payload `Id`
  fails strict resolution without changing lookup/sort identity.
- Preserve raw card JSON for explicit `get` and `includeRawTemplate` operations.
  Use compact projections for default search and batch resolve responses.
- Include ordered normalized tooltip text and a per-template content digest in
  compact projections so consumers do not need raw templates for effect
  classification or cache integrity. Fence that digest with catalog content so
  cross-template static dependencies invalidate safely.
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
- Process static catalog data only. Player logs, live board/stash/selection, and
  per-instance overrides remain outside this process.
- Do not claim that local data matches BazaarDB or includes website-derived
  history, builds, statistics, or inferred relationships.

## Consequences

### Positive

- Users with The Bazaar installed can query their local card snapshot without
  an API key or network request.
- Public artifacts do not redistribute a game database or website-derived
  identifier map.
- Cold searches normalize the database once; later processes load an
  integrity-checked snapshot without rehashing or parsing the database.

### Negative

- Users must launch The Bazaar once or pass a valid database path.
- The local catalog follows the installed game-data generation and can differ
  from other data sources.
- Merchant and trainer categorization depends on game conventions and requires
  regression tests when those conventions change.

### Neutral

- `--cache-mode offline` can read local game data because that operation
  performs no network I/O.

## Alternatives Considered

**Request, scrape, or replay BazaarDB website routes**

Rejected because no public developer API or permission basis was established.

**Bundle or redistribute `GameData.db`**

Rejected because the installed game already maintains the data and the CLI does
not need to take ownership of third-party content distribution.

**Use an independent scraping wrapper**

Rejected because a wrapper cannot grant rights to source-site content or change
the source site's terms.

## References

- https://bazaardb.gg/terms
- https://www.playthebazaar.com/mod-policy
