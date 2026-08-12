# Gameplay profiles

`bazaardb-cli profile` produces a season-specific pre-game handbook without a
network request. The JSON contract has `schemaVersion: 1` and contains:

- a profile identity fenced by the database SHA-256, all observed content
  versions, and the explicit season label;
- exact hero-pool partitions for `Always`, `GuidOnly`, `Never`, and unknown
  spawning eligibility;
- rules from `game_modes`, choices from `level_ups`, and exact season evidence
  from `seasons`;
- Piggles core/support cards, tier attributes, tooltips, and adjacency notes;
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

## dcc-cua knowledge directory

Pass `--dcc-knowledge-dir PATH` to also emit a directly consumable JSON
playbook under `playbooks/<season>/<hero>.json` and merge its entry into
`index.json`. Existing entries for other seasons and heroes are preserved. The
playbook uses `profileId: "the-bazaar"`, fences the exact database SHA-256, and
marks absent run evidence as `tenWinEvidence.status: "unavailable"`.
