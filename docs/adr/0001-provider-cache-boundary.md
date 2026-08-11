# ADR-0001: Keep provider, cache, and HTTP boundaries explicit

## Status

Accepted. The local provider decision is refined by
[ADR-0002](0002-local-game-data-provider.md).

## Context

The CLI must query The Bazaar card categories quickly, avoid repeating
identical work, preserve unknown card fields, and ship as a standalone binary.
BazaarDB does not publish a general developer API for its website. Its terms
restrict reverse engineering, and private website requests use anti-abuse
controls. A separate documented Parse integration exposes `search_cards` and
`get_card`.

Some local tools also need the normalized catalog over HTTP. That surface must
remain loopback-only, read-only, bounded, and independent from the command-line
presentation layer.

## Decision

- Depend on provider ports in the application layer. Keep provider URLs,
  authentication, retries, and response limits in infrastructure adapters.
- Preserve unknown card JSON at the provider boundary.
- Cache successful remote responses in transactional redb storage. Derive keys
  from provider identity, endpoint, and canonically sorted query pairs; never
  include credentials.
- Use endpoint-specific TTLs plus explicit `use`, `refresh`, and `offline`
  modes. Permit stale fallback only within a bounded documented window.
- Bind the optional HTTP service to `127.0.0.1`. Expose no mutation routes or
  local filesystem paths, and cap serialized responses.
- Limit remote adapters to documented endpoints. Do not replay private website
  requests or expose an arbitrary-host proxy.
- Keep ten-win analysis local and deterministic over user-supplied JSON/JSONL
  exports until a documented run-data API is available.

## Consequences

### Positive

- Core use cases are independent from one provider and straightforward to test.
- Repeated card queries are local, and credentials stay out of cache and output.
- Unknown card fields survive round trips.
- Ten-win analysis works without taking a dependency on protected web pages.

### Negative

- The explicit Parse provider still needs a valid key.
- Website-only history, builds, and statistics require an authorized export or
  a future documented API.

## Alternatives considered

**Call BazaarDB private website requests**

Rejected because they are undocumented, protected, brittle, and unsuitable as
a public CLI contract.

**Store one JSON file per remote response**

Rejected because concurrent CLI processes need transactional replacement and
bounded maintenance without partial files.

## References

- https://bazaardb.gg/terms
- https://bazaardb.gg/docs
- https://parse.bot/marketplace/155ad353-a423-43ac-9825-c1e430c5cb06/bazaardb-gg-api
