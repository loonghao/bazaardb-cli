# Static catalog protocol

Protocol versions: `catalogSchemaVersion=1.0.0`, `resolverVersion=1.1.0`.

`bazaardb-cli` is the owner of the normalized static The Bazaar card catalog.
The protocol is designed for replaceable companion adapters and deterministic
agent caches. It contains no action authority and no live match state.

## Identity

Every successful response carries:

```json
{
  "catalogSchemaVersion": "1.0.0",
  "resolverVersion": "1.1.0",
  "databaseSha256": "actual lowercase SHA-256",
  "contentId": "sha256:canonical-catalog-and-contract-hash",
  "cacheKey": "catalog/...",
  "authority": "inspection_only",
  "authorizesAction": false
}
```

`databaseSha256` identifies source bytes. `contentId` also changes when the
canonical normalized payload, schema, or resolver semantics change. Consumers
must fence projections with `contentId`, not database size or mtime.

## Loopback HTTP

`serve` binds only `127.0.0.1`. All catalog responses include
`Cache-Control: no-store, max-age=0` and never expose the local database path.

### `GET /v1/catalog/status`

Returns identity, card count, offline/read-only state, and explicit false action
authority.

### `GET /v1/catalog/search`

Accepts `q` or `query`, `category`, `page`, `limit`, `sortBy`, `order`, and
`showUnobtainable`. Missing query fields use CLI defaults. Results are compact
card projections; raw source templates are not returned.

### `POST /v1/catalog/resolve`

```json
{
  "requests": [
    {
      "templateId": "0022c409-c839-41e8-8022-65a407457dfe",
      "tier": "Silver",
      "enchantmentId": "Fiery"
    }
  ],
  "mode": "strict",
  "includeRawTemplate": false,
  "includeAllEnchantments": false
}
```

Rules:

- Batch size is 1-64; template UUIDs must be unique, canonical lowercase, and
  hyphenated. Input order is output order.
- `strict` is the default. Any missing template/tier/component, tier before the
  card's starting tier, unknown enchantment, or malformed requested data fails
  the whole batch with HTTP 422. `partial` must be explicit.
- SQLite `cards.Id` is authoritative for lookup and stable sorting. Compact
  projections report payload ID consistency. Missing payload IDs are allowed;
  conflicts or malformed payload IDs make strict resolve fail closed.
- For items, template type, starting/requested tier, size, tags, and accumulated
  attributes are required. Version and tooltips are optional, but wrong present
  shapes are malformed. `tooltips` preserves
  `Localization.Tooltips[].Content.Text` order and carries typed shape,
  missing, malformed, and completeness fields.
- Attributes accumulate from the starting tier through the requested tier;
  later layers overwrite the same attribute while sparse earlier values remain.
- Ability and aura IDs retain stable first-seen order. Their definition shape,
  missing IDs, malformed entries, and completeness are typed separately.
- `enchantmentId` is an exact case-sensitive canonical game identifier. Only
  that applied definition is resolved. Without it, status is `not_requested`.
  `includeAllEnchantments=true` is explicit and cannot be combined with a
  per-card enchantment selection.
- Raw source JSON is omitted unless `includeRawTemplate=true`.
- Serialized catalog responses are limited to 8 MiB; oversized responses use
  HTTP 413.

Each resolved result includes a reusable key:

```text
resolve/<contentId>/<templateId>/<tier>/enchantment/<id|not_requested|all>
```

This key intentionally excludes live instance overrides. A companion may key
its own projection with the same tuple.

Every compact card also carries `templateContentId`. It hashes the authoritative
row ID, canonical full static template definition, catalog schema, and resolver
version. Search and resolve therefore expose the same digest for the same
template generation; related static-definition or resolver changes produce a
new digest without requiring `rawTemplate`.

## Errors

Errors use a stable envelope and preserve inspection-only authority:

```json
{
  "catalogSchemaVersion": "1.0.0",
  "resolverVersion": "1.1.0",
  "authority": "inspection_only",
  "authorizesAction": false,
  "error": {
    "code": "unknown_enchantment",
    "message": "strict catalog resolve rejected an unknown enchantment",
    "details": {}
  }
}
```

Malformed JSON/query syntax is HTTP 400, contract or strict-resolution failure
is HTTP 422, oversized output is HTTP 413, and unavailable catalog state is HTTP
500 with a path-free generic message.

## CLI serialization

`resolve` accepts `TEMPLATE_UUID@TIER[#ENCHANTMENT_ID]`. JSON is one batch
response; JSONL emits one record per input in stable order, with catalog identity
on every record. `--include-raw-template` and `--include-all-enchantments` map to
the HTTP request flags.

## Ownership boundary

The catalog may contain static template attributes and definitions. It does not
read `Player.log`, identify the current board/stash/selection, apply permanent
instance damage/ammo/slot overrides, or authorize/emit ActionIntent. Those are
runtime companion responsibilities.
