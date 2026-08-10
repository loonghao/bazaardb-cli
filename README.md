# bazaardb-cli

Fast, asynchronous, agent-friendly card queries for [BazaarDB](https://bazaardb.gg/),
with a transactional local cache and a read-only DCC CUA profile bridge.

The CLI uses the documented BazaarDB API provider on Parse. It covers both
published endpoints (`search_cards` and `get_card`) and every documented card
category: items, skills, merchants, trainers, monsters, and events. Card objects
are kept schema-flexible so newly added fields remain available immediately.

> [!IMPORTANT]
> BazaarDB does not publish an official developer API. This project does not
> bypass the website's private endpoints or anti-abuse controls. Use the tool
> for personal, noncommercial workflows and review BazaarDB's current terms.

## Install

Download the archive for your target from
[GitHub Releases](https://github.com/loonghao/bazaardb-cli/releases), extract
`bazaardb-cli` (or `bazaardb-cli.exe`), and put it on `PATH`.

Set one of the supported API-key variables. The value is never stored in the
cache, profile, logs, or output.

```powershell
$env:BAZAARDB_API_KEY = "..."
# PARSE_API_KEY is accepted as a compatibility fallback.
```

## Query

```powershell
# Discover the complete supported API surface.
bazaardb-cli endpoints

# Search one page; JSON is the default agent-safe output.
bazaardb-cli search poison --category items --limit 25

# Fetch pages concurrently, bounded by --max-pages.
bazaardb-cli search "shield" --category all --all --concurrency 8

# Preserve the provider's complete card object.
bazaardb-cli get "Bar of Soap"

# JSON Lines and a compact table are also available.
bazaardb-cli search sword --output jsonl
bazaardb-cli search sword --output table
```

Categories are `all`, `items`, `skills`, `merchants`, `trainers`, `monsters`,
and `events`. Search supports the provider's page, limit, sort, order, and
unobtainable-card controls.

## Cache

Successful GET responses are stored in a cross-process transactional redb
database under the platform cache directory. Search entries expire after 15
minutes, complete cards after 6 hours, and a stale response may be used for up
to 7 days when the provider is temporarily unavailable.

```powershell
bazaardb-cli cache status
bazaardb-cli --cache-mode refresh search poison
bazaardb-cli --cache-mode offline get "Bar of Soap"
bazaardb-cli cache prune
bazaardb-cli cache clear --yes
```

Cache keys contain only the provider base URL, endpoint, and sorted query
parameters. They never contain an API key.

## DCC CUA profile

[`profiles/bazaardb-cua.json`](profiles/bazaardb-cua.json) follows the current
dcc-cua semantic profile schema v3. Browser surfaces remain declarative; fast
card data is exposed through a bounded loopback state source.

```powershell
bazaardb-cli serve "poison" --category items --port 7878

# In another terminal with dcc-cua installed:
dcc-cua profile --profile-file .\profiles\bazaardb-cua.json
dcc-cua profile-state --profile-file .\profiles\bazaardb-cua.json --watch
```

The server binds only `127.0.0.1`, exposes `GET /v1/state` and `GET /healthz`,
supports ETags, and has no mutation endpoint. The profile is optional/fail-soft,
so visual CUA remains available when the companion is not running.

## Update

```powershell
bazaardb-cli update --check
bazaardb-cli update
bazaardb-cli update --yes
```

The updater selects the GitHub Release archive matching the running Rust target,
downloads `SHA256SUMS`, verifies the archive digest, and replaces only the
current executable. Release checks use semantic version ordering. Public
repositories work anonymously; high-frequency or CI environments can set
`GITHUB_TOKEN` (preferred) or `GH_TOKEN` to avoid GitHub's anonymous API rate
limit. The token is used only for release metadata requests and is never logged
or cached.

## Develop with vx + just

```powershell
vx sync
vx just check
vx just build-release
```

`vx.toml` pins Rust and just. The `justfile` is the single local/CI command
surface. Conventional commits drive release-please; a release PR updates the
Rust version and changelog, and merging it builds multi-platform archives plus
`SHA256SUMS`. GitHub protects the release workflow and artifacts; the project
does not yet claim independent code signing.

See [ADR-0001](docs/adr/0001-provider-cache-and-cua-boundary.md) for provider,
cache, failure, security, and CUA ownership decisions.

## License and data

The CLI source is MIT licensed. BazaarDB and The Bazaar data, names, art, and
other content remain subject to their respective owners' terms and rights.
