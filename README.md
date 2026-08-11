# bazaardb-cli

Unofficial, no-key CLI queries for The Bazaar cards from the game's read-only
`GameData.db`, with durable bounded caching, ten-win combination analysis, a
read-only local HTTP API, and verified self-update.

`bazaardb-cli` reads the SQLite cache created by the installed game. It does not
connect to `bazaardb.gg`. `get` preserves complete local JSON card objects,
while catalog search and resolve use compact machine-readable projections by
default.

> [!IMPORTANT]
> Local game data is an installed snapshot and may differ by game version,
> platform, region, or update state. The CLI does not claim parity with
> BazaarDB website content.

## Project status and data-use notice

This is an unofficial, independent open-source project. It is not affiliated
with, sponsored by, approved by, or endorsed by BazaarDB, Azaro Labs LLC,
Tempo Games, Tempo Storm, or the developers or publishers of The Bazaar.

`BazaarDB`, `The Bazaar`, related names, logos, artwork, and game data are the
property of their respective owners. The project does not bundle
`GameData.db`, card artwork, BazaarDB pages, BazaarDB identifiers, or player-run
data. The MIT license applies only to this project's source code and does not
grant rights to third-party content.

Card and run-data commands read only files already present on your device or
files you explicitly provide. They do not call, scrape, mirror, reverse
engineer, or replay requests to BazaarDB. The optional `update` command contacts
GitHub Releases for this repository; `serve` binds only to local loopback. You
are responsible for ensuring that you have the right to access and process each
input file and for complying with applicable game policies, website terms,
licenses, privacy obligations, and law.

Review the current [BazaarDB Terms of Use](https://bazaardb.gg/terms) and
[The Bazaar Mod Policy](https://www.playthebazaar.com/mod-policy) before use.
This notice is informational and is not legal advice. See [NOTICE](NOTICE.md).

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

# Find frequent two-card combinations in an exported set of ten-win runs.
bazaardb-cli ten-wins --input .\runs.json --hero Dooley --min-runs 2
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

The CLI has no BazaarDB website provider and does not accept a BazaarDB API key.

The local provider exposes `search_cards` and `get_card` through the CLI
contract. Categories are `all`, `items`, `skills`, `merchants`, `trainers`,
`monsters`, and `events`. Search supports zero-based pages, limits, sorting,
ordering, unobtainable-card inclusion, and bounded concurrent `--all` queries.

Run `bazaardb-cli endpoints` for the machine-readable provider surface. See
[Provider reference](docs/providers.md) for source selection, platform paths,
category mapping, cache behavior, and troubleshooting.

## Machine-readable output

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
`templateContentId` digest of the canonical static template definition. Callers
can classify effects and fence per-template caches without requesting raw JSON.
The digest includes the catalog content fence so referenced cross-template
static definitions cannot change unnoticed.

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
parameters. `offline` can query a local catalog snapshot on a response-cache
miss.

## Ten-win combinations

`ten-wins` reads a local JSON or JSONL run export, keeps records with exactly ten
wins, and ranks card combinations by run count and support. It does not require
`GameData.db`, a network request, or an API key.

```powershell
bazaardb-cli ten-wins `
  --input .\runs.json `
  --hero Dooley `
  --card "Monitor Lizard" `
  --combination-size 2 `
  --min-runs 3 `
  --limit 20
```

The import accepts `{"runs": [...]}`, a JSON array, or one run per JSONL line.
Each record contains `wins`, `hero`, and `cards`. Duplicate card names inside a
run count once. Results are sorted by run count, then card name, so repeated
queries are deterministic. See [Ten-win combinations](docs/ten-win-combinations.md).

The CLI does not obtain run data from BazaarDB. Import only data that you have
the right to process, then query it locally.

## Local HTTP API

Run the optional loopback server when another local tool needs card queries:

```powershell
bazaardb-cli serve poison --category items --port 7878
```

The server binds only `127.0.0.1`. Its static catalog API is:

- `GET /v1/catalog/status`
- `GET /v1/catalog/search`
- `POST /v1/catalog/resolve`

Every catalog response is `Cache-Control: no-store`,
`authority=inspection_only`, and `authorizesAction=false`; no local path is
exposed. `/v1/state` and `/healthz` remain compatibility surfaces. The HTTP
service is read-only and contains no live match state. See the
[catalog protocol](docs/catalog-protocol.md).

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
[ADR-0001](docs/adr/0001-provider-cache-boundary.md) and
[ADR-0002](docs/adr/0002-local-game-data-provider.md).

## License and data

The CLI source is MIT licensed. Third-party names and data are not licensed by
this repository. See [Project status and data-use notice](#project-status-and-data-use-notice)
and [NOTICE](NOTICE.md).
