# bazaardb-cli

Fast, no-key The Bazaar card queries from the game's read-only `GameData.db`,
with reviewed BazaarDB reference aliases, durable bounded caching, a DCC CUA
profile bridge, a content-addressed static catalog, and verified self-update.

`bazaardb-cli` follows the same data path as
[BazaarPlusPlus](https://github.com/BazaarPlusPlus/BazaarPlusPlus): it reads the
SQLite game-data cache created by The Bazaar instead of calling BazaarDB's
private website endpoints. `get` preserves complete JSON card objects, while
agent-facing catalog search and resolve use compact projections by default.

> [!IMPORTANT]
> Local game data contains the canonical current card definitions. BazaarDB
> adds its own derived content, including history, builds, run statistics, and
> inferred relationships. Those website-only enrichments are not present in
> the `game-data` provider.

## Install

Download the archive for your platform from
[GitHub Releases](https://github.com/loonghao/bazaardb-cli/releases), extract
`bazaardb-cli` (or `bazaardb-cli.exe`), and put it on `PATH`.

Launch The Bazaar once so it creates its local data cache. No API key is needed.

## Quick start

```powershell
# Auto-detect GameData.db and search the current catalog.
bazaardb-cli search poison --category items --limit 25

# Print compact, human-readable results.
bazaardb-cli --output table search "Eagle Talisman" --category items

# Return the complete card object.
bazaardb-cli get "Eagle Talisman"

# Resolve through Silver and apply one exact enchantment definition.
bazaardb-cli resolve `
  "0022c409-c839-41e8-8022-65a407457dfe@Silver#Fiery"
```

Expected table columns:

```text
NAME            TYPE    SIZE    TIER
Eagle Talisman  Item    Small   Silver
```

If auto-detection cannot find the database, pass it explicitly:

```powershell
bazaardb-cli --provider game-data `
  --game-data "$env:USERPROFILE\AppData\LocalLow\Tempo Storm\The Bazaar\prod\cache\GameData.db" `
  search shield
```

## Providers

| Provider | Authentication | Use case |
| --- | --- | --- |
| `auto` | None | Default. Require local `GameData.db`; never introduce a hidden network dependency. |
| `game-data` | None | Read the installed game's SQLite database in query-only mode. |
| `parse` | `BAZAARDB_API_KEY` | Use the documented third-party BazaarDB Parse API. |

Select the remote provider explicitly when you need it. It is never an
automatic fallback:

```powershell
$env:BAZAARDB_API_KEY = "..."
bazaardb-cli --provider parse search poison --category items
```

`PARSE_API_KEY` remains a compatibility fallback. Credentials never enter the
response cache, profile, logs, or JSON output.

Both providers expose `search_cards` and `get_card` through the same CLI
contract. Categories are `all`, `items`, `skills`, `merchants`, `trainers`,
`monsters`, and `events`. Search supports zero-based pages, limits, sorting,
ordering, unobtainable-card inclusion, and bounded concurrent `--all` queries.

Run `bazaardb-cli endpoints` for the machine-readable provider surface. See
[Provider reference](docs/providers.md) for source selection, platform paths,
category mapping, cache behavior, and troubleshooting.

## Output for agents and scripts

JSON is the default. Every JSON command uses schema version `1.0.0` and reports
its selected source and cache disposition.

```powershell
bazaardb-cli search shield --category all --all --concurrency 8 --max-pages 20
bazaardb-cli --output jsonl search sword
bazaardb-cli --output table search sword
```

The CLI loads local SQLite data on a blocking worker, shares the in-memory
snapshot across concurrent page requests, and applies bounded concurrency.

`resolve` accepts 1-64 canonical template requests and preserves input order.
Only duplicate `(templateId,tier,enchantment selector)` tuples are rejected;
the same template at distinct tiers or selectors is valid. Strict whole-batch
mode is the default: missing templates, tiers before
the starting tier, unknown enchantments, and malformed definitions fail closed.
Use `--mode partial` only when explicit partial results are acceptable.

```powershell
# Compact stable JSONL; no raw template or unrequested enchantments.
bazaardb-cli --output jsonl resolve `
  "0022c409-c839-41e8-8022-65a407457dfe@Silver"

# Expensive diagnostic forms must be explicit.
bazaardb-cli resolve --include-raw-template --include-all-enchantments `
  "0022c409-c839-41e8-8022-65a407457dfe@Silver"
```

Each result includes a `resolutionKey` over `(contentId, templateId, tier,
enchantment selector)`. No enchantment request is reported as `not_requested`
and never serializes every enchantment definition.

Compact search and resolve cards include ordered normalized tooltip text and a
`templateContentId` digest of the canonical static template definition. Agents
can classify effects and fence per-template caches without requesting raw JSON.
The digest includes the catalog content fence so referenced cross-template
static definitions cannot change unnoticed.

The standalone catalog also owns reviewed `externalReferences` from
`data/card-identities.json`. `externalIdentityContentId` versions this map
independently from local GameData `contentId`; references remain optional
inspection metadata and never override the installed game's definitions.

## Cache

The cache directory has two stores:

- Successful query responses use a cross-process transactional redb database.
  Search responses expire after 15 minutes; complete cards expire after 6 hours.
- The normalized static catalog uses an atomic, content-addressed snapshot with
  schema, resolver, database, payload, and content hashes. Warm CLI processes
  load it without rehashing or reparsing `GameData.db`. It retains at most three
  generations for 30 days and 1 GiB total; rebuilds prune automatically.

```powershell
bazaardb-cli cache status
bazaardb-cli --cache-mode refresh search poison
bazaardb-cli --cache-mode offline get "Eagle Talisman"
bazaardb-cli cache prune
bazaardb-cli cache clear --yes
```

The public identity exposes the actual `databaseSha256`, plus a `contentId`
derived from the canonical normalized catalog, `catalogSchemaVersion`, and
`resolverVersion`. A canonical-path plus main-DB/`-wal` length/mtime memo avoids
redundant database hashing but never replaces the published hashes. Volatile
`-shm` reader state is excluded. Snapshot writes use a flushed and synced temp
file, atomic rename, payload verification, and automatic rebuild after
corruption or contract mismatch.
`cache status` reports catalog generation count and bytes; `cache prune` and
`cache clear --yes` maintain both response and catalog stores.

Response keys include the catalog cache key, endpoint, and sorted query
parameters. Parse keys include the API base instead. Neither key includes a
credential. `offline` can query a local catalog snapshot on a response-cache
miss; Parse requires an existing cached response.

## DCC CUA profile

[`profiles/bazaardb-cua.json`](profiles/bazaardb-cua.json) follows the dcc-cua
semantic profile schema v3. Browser surfaces stay declarative while the CLI
exposes fast card state through a bounded loopback source.

```powershell
bazaardb-cli serve poison --category items --port 7878

# In another terminal with dcc-cua installed:
dcc-cua profile --profile-file .\profiles\bazaardb-cua.json
dcc-cua profile-state --profile-file .\profiles\bazaardb-cua.json --watch
```

The server binds only `127.0.0.1`. Its canonical static catalog API is:

- `GET /v1/catalog/status`
- `GET /v1/catalog/search`
- `POST /v1/catalog/resolve`

Every catalog response is `Cache-Control: no-store`,
`authority=inspection_only`, and `authorizesAction=false`; no local path is
exposed. `/v1/state` and `/healthz` remain compatibility surfaces. This
standalone CLI owns only the static card catalog: it does not read `Player.log`,
current board/stash/selection, instance overrides, or issue ActionIntent. See
the [catalog protocol](docs/catalog-protocol.md).

## Update

```powershell
bazaardb-cli update --check
bazaardb-cli update
bazaardb-cli update --yes
```

The updater selects the archive matching the running Rust target, downloads
`SHA256SUMS`, verifies the archive digest, and replaces only the current
executable. `GITHUB_TOKEN` or `GH_TOKEN` can raise GitHub's API rate limit; the
token is not logged or cached.

Versions `0.1.0` and `0.1.1` require one manual upgrade because their download
request returned GitHub asset metadata instead of binary bytes. Install `0.1.2`
or newer once, then use the normal update command.

## Develop with vx + just

```powershell
vx sync
vx just check
vx just build-release
```

`vx.toml` pins Rust and just. The `justfile` is the shared local and CI command
surface. Conventional commits drive release-please; merging a release PR builds
Windows, Linux, Intel macOS, and Apple Silicon archives plus `SHA256SUMS`.

Architecture decisions are recorded in
[ADR-0001](docs/adr/0001-provider-cache-and-cua-boundary.md) and
[ADR-0002](docs/adr/0002-local-game-data-provider.md).

## License and data

The CLI source is MIT licensed. BazaarDB and The Bazaar data, names, art, and
other content remain subject to their respective owners' terms and rights. The
CLI opens the user's local game database read-only and does not redistribute it.
