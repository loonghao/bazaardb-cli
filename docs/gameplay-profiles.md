# Gameplay profiles

`bazaardb-cli profile` produces an installed-snapshot pre-game handbook without
a network request. The JSON handbook contract has `schemaVersion: 1` and
contains:

- a profile identity fenced by the database SHA-256, all observed content
  versions, and the explicit season label;
- exact hero-pool partitions for `Always`, `GuidOnly`, `Never`, and unknown
  spawning eligibility;
- rules from `game_modes`, choices from `level_ups`, and exact season evidence
  from `seasons`;
- Piggles core/support cards, public and hidden archetype tags, tier
  attributes, tooltips, and adjacency notes;
- explicit local ten-win evidence, or an unavailable marker when no matching
  run export was supplied;
- source boundaries and warnings.

## Season mapping

Pass the installed snapshot's canonical label, for example
`--season-label "Season 1"`. Matching is exact. An omitted or unmatched label
remains unverified; ordering and maximum IDs are not treated as evidence that a
season is current.

## Local supplements

A supplement adds human-maintained UI layout or strategy notes while preserving
their provenance. The CLI reads at most 2 MiB, accepts at most 32 sources,
rejects unknown fields, and never fetches a URL.

```json
{
  "schemaVersion": 1,
  "seasonLabel": "Season 1",
  "sources": [
    {
      "url": "https://example.com/the-bazaar-season-1-notes",
      "title": "Season 1 notes",
      "publishedAt": "2026-08-01T00:00:00Z",
      "retrievedAt": "2026-08-13T00:00:00Z",
      "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "scope": "rules and UI layout",
      "appliesTo": "Season 1",
      "confidence": "primary"
    }
  ],
  "uiLayout": {
    "board": "center",
    "stash": "lower-left chest"
  },
  "strategy": {
    "skillChoices": "Take a usable skill rather than wasting the choice."
  }
}
```

The SHA-256 records the source material captured by the supplement author. The
CLI validates its shape but cannot independently verify the referenced content
because profile generation is intentionally offline.

## Generic context documents

Pass `--knowledge-root PATH` to also emit the schema 2 context contract used by
profile consumers:

```text
<knowledge-root>/the-bazaar/index.json
<knowledge-root>/the-bazaar/documents/gameplay-<hero>-piggles.json
```

The index contains only the generic `schemaVersion`, `profileId`, and
`documents[]` fields. Each document entry has `id`, `path`, `identities`, and
`selectors`. The document repeats the same identity map exactly as `fences`.
No season-specific branch, hero-specific index field, catalog alias, or
playbook directory is part of this contract.

Generated identities are exact and case-sensitive:

- `database-sha256=sha256:<digest>` identifies the complete local SQLite file;
- `content-version=<versions>` identifies the sorted observed version set. A
  single version is written directly; multiple versions are joined by commas.

The generic selectors are `hero=<canonical hero>` and `archetype=piggles`.
For example, a consumer can request this document with:

```powershell
dcc-cua profile context --id the-bazaar `
  --identity database-sha256=sha256:<digest> `
  --identity content-version=5.0.0 `
  --selector hero=Pygmalien `
  --selector archetype=piggles
```

Existing schema 2 documents with other IDs are preserved. A schema 1 index is
rejected instead of being guessed or silently migrated. The document keeps
explicit season evidence inside its evidence payload and marks missing local
run data as `evidence.tenWin.status: "unavailable"`; it never claims a ten-win
strategy without matching supplied runs.
