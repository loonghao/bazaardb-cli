# Provider reference

Last verified: 2026-08

## Selection

`--provider auto` is the default.

1. Use `--game-data PATH` when supplied.
2. Otherwise detect a valid local `GameData.db`.
3. Fail with a local setup error if no database exists.

`auto` is offline-only and never falls back to a network provider.

Force a provider when deterministic source selection matters:

```powershell
bazaardb-cli --provider game-data --game-data C:\path\to\GameData.db search poison
```

Environment equivalents are `BAZAARDB_PROVIDER` and `BAZAARDB_GAME_DATA`.

## Local game-data provider

The provider opens SQLite with read-only and query-only flags. It reads only
`SELECT Data FROM cards ORDER BY Id`. It does not modify the game database,
launch the game, or call a network endpoint.

Auto-detection checks these narrow roots and their immediate environment child
directories:

- Windows: `%USERPROFILE%\AppData\LocalLow\Tempo Storm\The Bazaar`
- macOS: `~/Library/Application Support/Tempo Storm/The Bazaar`
- Linux/Proton: Steam compatibility prefix for app `1617400`

The common Windows file is:

```text
%USERPROFILE%\AppData\LocalLow\Tempo Storm\The Bazaar\prod\cache\GameData.db
```

The newest valid candidate wins. An explicit path always wins over detection.

### Category mapping

| CLI category | Local game field |
| --- | --- |
| `items` | `Type = Item` |
| `skills` | `Type = Skill` |
| `merchants` | `Tags` contains `Merchant` |
| `trainers` | Trainer `EventEncounter`, normally identified by `Level Up` |
| `monsters` | `Type = CombatEncounter` |
| `events` | Other event, step, and pedestal encounters |
| `all` | Union of the categories above |

By default, templates marked `SpawningEligibility = Never`, debug templates,
and template placeholders are excluded. Pass `--show-unobtainable` to include
them.

Search is case-insensitive across the complete serialized card object. `get`
matches a card ID, localized title, or internal name exactly and preserves the
complete JSON object.

### Cache behavior

The gateway publishes two separate content identities:

- `databaseSha256` is the actual SHA-256 of `GameData.db`.
- `contentId` hashes the canonical normalized catalog together with
  `catalogSchemaVersion` and `resolverVersion`.

A canonical-path plus main-DB/`-wal` length/high-resolution-mtime memo decides
whether the database must be rehashed and the catalog re-read. This catches
committed WAL-only updates. Volatile `-shm` reader/lock churn is deliberately
excluded so readers cannot invalidate an unchanged catalog. The public identity
never substitutes these file stamps for an actual hash.

On a cold miss, the gateway reads cards on a blocking worker and writes a
content-addressed normalized snapshot. The snapshot header contains its format,
schema, resolver, database SHA, payload SHA, content ID, and row count. Writes
use temp-file flush, file sync, atomic rename, and directory sync. A warm process
validates the snapshot and skips both database hashing and SQLite/JSON parsing.
Corrupt payloads and format/schema/resolver mismatches rebuild automatically.

Set `RUST_LOG=bazaardb_cli::catalog_cache=info` to record `hit` or
`miss_rebuilt`, reason, duration, database bytes, SQLite rows, snapshot I/O,
generation count/bytes, and prune activity.
The response-cache key includes the content-addressed catalog cache key. Run
`bazaardb-cli cache prune` to remove expired response entries and enforce the
catalog policy of three generations, 30 days, and 1 GiB. `cache status` reports
both stores; `cache clear --yes` clears both through narrowly matched files.

Concurrent `--all` pages share the in-memory generation. `--cache-mode offline`
may use a local snapshot or read the local database because neither performs a
network request.

### Static catalog protocol

`resolve` is strict by default and accepts 1-64 canonical template requests.
Duplicate full resolution tuples are rejected; distinct tiers/selectors for the
same template remain valid.
It accumulates attributes from the starting tier through the requested tier,
with later layers overriding earlier values. Ability and aura IDs remain in
stable first-seen order. Compact results contain the source SQLite row
ID, payload-ID consistency, type, tier, size, tags, resolved attributes, and
typed component completeness. For item resolution, type, tier, size, tags, and
tier attributes are required. Version and tooltips are optional, but a present
field with the wrong shape is malformed. A payload `Id` conflict never changes
lookup identity and makes strict resolve fail closed.

Compact search and resolve projections include ordered normalized tooltip text
with typed shape/missing/malformed status. They also include a per-template
`templateContentId` over the SQLite row ID and canonical full static
definition plus the catalog content fence, allowing client caches to verify
template and cross-template dependency integrity without raw templates or live
instance overrides.

Per-card `enchantmentId` uses exact, case-sensitive game identifiers. Without
one, enchantments report `not_requested`. Only the selected definition is
returned unless `includeAllEnchantments=true`; raw templates similarly require
`includeRawTemplate=true`.

See [Catalog protocol](catalog-protocol.md) for loopback endpoints, typed errors,
authority metadata, and the static/runtime ownership boundary.

## Troubleshooting

### `GameData.db was not found; launch The Bazaar once or pass --game-data PATH`

Launch The Bazaar and wait for its data update to finish. Then retry. If the
game uses a nonstandard cache root, pass the exact file with `--game-data` or
set `BAZAARDB_GAME_DATA`.

### `GameData.db is not a SQLite 3 database`

The selected file is not the game database or is incomplete. Point the CLI at
the `prod/cache/GameData.db` file and let the game finish updating before retrying.

## Data boundary

`GameData.db` is an installed local snapshot. It can differ by version,
platform, region, or update state. The provider does not request BazaarDB
website content or claim parity with it.

`ten-wins` is separate from the card provider. It analyzes user-supplied run
exports locally and never treats run outcomes as static catalog fields. See
[Ten-win combinations](ten-win-combinations.md).
