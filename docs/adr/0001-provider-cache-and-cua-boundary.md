# ADR-0001: Keep providers, cache, and CUA state boundaries explicit

## Status

Superseded in part by [ADR-0002](0002-local-game-data-provider.md). The cache,
CUA, schema-flexibility, and private-endpoint boundaries remain accepted.

## Context

The CLI must query all documented BazaarDB card categories quickly, avoid paying
for or repeating identical requests, remain usable by agents, and ship as a
standalone binary. BazaarDB itself does not publish a developer API. Its website
terms restrict reverse engineering and its private endpoints use anti-abuse
tokens. A separate documented API currently exposes `search_cards` and
`get_card` for the full public catalog.

The CUA semantic-profile contract also forbids credentials, arbitrary headers,
process launch commands, and application API code in profile JSON. It permits a
bounded, read-only loopback HTTP JSON state source.

## Decision

- Depend on an `ApiGateway` port in the application layer. Implement the first
  adapter against the documented Parse BazaarDB API and allow an alternate base
  URL for contract tests or a future official provider.
- Preserve card payloads as JSON values at the provider boundary so new card
  fields do not require a binary release.
- Cache successful GET responses in a transactional, pure-Rust redb database.
  Keys are SHA-256 hashes of provider base, endpoint, and sorted query pairs;
  credentials never enter keys or values.
- Use endpoint-specific TTLs, explicit `use`, `refresh`, and `offline` modes,
  and a seven-day stale-if-error window.
- Expose CUA compatibility state through `127.0.0.1:7878/v1/state`, with schema
  version, monotonic tick, ETag support, response-size bounds in the profile,
  and no mutation routes. ADR-0002 adds the canonical static catalog routes.
- Limit the typed provider surface to the two documented endpoints. Do not
  bypass private website tokens or expose an arbitrary-host HTTP proxy.

## Consequences

### Positive

- The core use cases are independent from one vendor and are straightforward to
  mock.
- Repeated queries are local, secrets stay out of cache/profile/output, and
  offline agent workflows are deterministic.
- The CUA profile can observe card state without weakening exact-window or
  action-fencing ownership.
- Unknown card fields survive round trips.

### Negative

- A valid provider API key is required only when the explicit Parse provider is
  selected. The default local provider needs no credential.
- The third-party provider can be unavailable even while BazaarDB's website is
  healthy.
- A future official API requires another adapter and contract tests.

### Neutral

- The public release includes a profile file, but users start the loopback
  companion explicitly; the profile never launches it.

## Alternatives Considered

**Call BazaarDB's private website endpoints**

Rejected because they are undocumented, token-protected, brittle, and conflict
with the published website contract.

**Store one JSON file per response**

Rejected because concurrent CLI processes need transactional replacement and
bounded maintenance without partial files.

**Put CLI commands or API credentials in the CUA profile**

Rejected because profile JSON is declarative routing vocabulary, not an
execution or secret-distribution mechanism.

## References

- https://global.bazaardb.gg/terms
- https://global.bazaardb.gg/docs
- https://parse.bot/marketplace/155ad353-a423-43ac-9825-c1e430c5cb06/bazaardb-gg-api
