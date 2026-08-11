# ADR-0001: Keep provider, cache, and HTTP boundaries explicit

## Status

Accepted. The local provider decision is refined by
[ADR-0002](0002-local-game-data-provider.md).

## Context

The CLI must query an installed local card snapshot quickly, avoid repeating
identical work, preserve unknown card fields, and ship as a standalone binary.
BazaarDB does not publish a public developer API for third-party use. Its terms
limit copying, distribution, reverse engineering, and competitive use of the
site. The project cannot establish redistribution rights for data returned by
independent scraping wrappers.

Some local tools also need the normalized catalog over HTTP. That surface must
remain loopback-only, read-only, bounded, and independent from the command-line
presentation layer.

## Decision

- Keep data access behind an application port and use only the read-only local
  game-data adapter in public builds.
- Preserve unknown card JSON at the provider boundary.
- Cache successful local responses in transactional redb storage. Derive keys
  from the local catalog identity, endpoint, and canonically sorted query pairs.
- Keep `use`, `refresh`, and `offline` cache modes deterministic and local.
- Bind the optional HTTP service to `127.0.0.1`. Expose no mutation routes or
  local filesystem paths, and cap serialized responses.
- Do not ship BazaarDB website adapters, independent scraping wrappers, copied
  website identifiers, or arbitrary-host proxy behavior.
- Keep ten-win analysis local and deterministic over user-supplied JSON/JSONL
  files.

## Consequences

### Positive

- Card queries remain local and straightforward to test.
- Repeated queries avoid database rehashing and reparsing.
- Unknown card fields survive round trips.
- Release artifacts contain code and documentation, not third-party datasets.

### Negative

- The installed game cache is required for card queries.
- Website-only history, builds, and statistics are outside project scope.

## Alternatives considered

**Call BazaarDB website requests directly**

Rejected because no public developer API or permission basis was established.

**Use an independent scraping wrapper**

Rejected because a wrapper cannot grant rights to source-site content and does
not change the source site's terms.

**Store one JSON file per cached response**

Rejected because concurrent CLI processes need transactional replacement and
bounded maintenance without partial files.

## References

- https://bazaardb.gg/terms
- https://www.playthebazaar.com/mod-policy
