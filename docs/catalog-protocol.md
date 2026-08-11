# Static catalog protocol

Protocol versions: `catalogSchemaVersion=1.1.0`, `resolverVersion=1.2.0`.

`bazaardb-cli` is the owner of the normalized static The Bazaar card catalog.
The protocol is designed for replaceable local clients and deterministic
caches. It is read-only and contains no live match state.

## Identity

Every successful response carries:

```json
{
  "catalogSchemaVersion": "1.1.0",
  "resolverVersion": "1.2.0",
  "externalIdentitySchemaVersion": "1.0.0",
  "externalIdentityContentId": "sha256:external-reference-catalog-hash",
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
`externalIdentityContentId` independently fences the bundled, reviewed
BazaarDB alias map. Updating an external alias does not change GameData
`contentId`; response cache keys include both identities.

## Loopback HTTP

`serve` binds only `127.0.0.1`. All catalog responses include
`Cache-Control: no-store, max-age=0` and never expose the local database path.

### `GET /v1/catalog/status`

Returns identity, card count, offline/read-only state, and explicit false action
authority.

### `GET /v1/catalog/search`

Accepts `q` or `query`, `category`, `page`, `limit`, `sortBy`, `order`, and
`showUnobtainable`. Missing query fields use CLI defaults. Results are compact
card projections; raw source templates are not returned. Unknown fields and
unsupported `sortBy` values are structured HTTP 400 errors.

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

- Batch size is 1-64; template UUIDs must be canonical lowercase and
  hyphenated. Duplicate full resolution tuples `(templateId,tier,selector)` are
  rejected, while the same template at different tiers or enchantment selectors
  is valid. Input order is output order.
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
  Selected ability, aura, and enchantment definitions must be JSON objects;
  null, scalar, and array definitions are malformed and fail strict resolve.
- `enchantmentId` is an exact case-sensitive canonical game identifier. Only
  that applied definition is resolved. Without it, status is `not_requested`.
  `includeAllEnchantments=true` is explicit and cannot be combined with a
  per-card enchantment selection.
- Raw source JSON is omitted unless `includeRawTemplate=true`.
- Serialized catalog responses are limited to 8 MiB; oversized responses use
  HTTP 413.

Each resolved result includes a reusable key:

```text
resolve/<contentId>/<templateId>/<tier>/selector/not_requested
resolve/<contentId>/<templateId>/<tier>/selector/all
resolve/<contentId>/<templateId>/<tier>/selector/exact/<enchantmentId>
```

This key intentionally excludes live instance overrides. A client may key its
own projection with the same tuple.

Every compact card also carries `templateContentId`. It hashes the authoritative
row ID, canonical full static template definition, catalog schema, and resolver
version, plus the catalog content fence. Search and resolve therefore expose
the same digest for the same template generation; referenced cross-template
static-definition or resolver changes produce a new digest without requiring
`rawTemplate`.

Compact projections also carry `externalReferences`. These are reviewed,
provenance-bearing BazaarDB aliases joined by authoritative local template UUID,
canonical name, and card type. They are optional inspection metadata and never
override local attributes.

## Errors

Errors use a stable envelope and preserve inspection-only authority:

```json
{
  "catalogSchemaVersion": "1.1.0",
  "resolverVersion": "1.2.0",
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
and `authority=inspection_only`, `authorizesAction=false` on every record.
`--include-raw-template` and `--include-all-enchantments` map to the HTTP request
flags.

## Ownership boundary

The catalog contains static template attributes and definitions. It does not
read player logs, inspect the current board or stash, or apply live instance
overrides.
