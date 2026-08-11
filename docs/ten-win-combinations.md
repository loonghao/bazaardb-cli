# Ten-win card combinations

Last verified: 2026-08

Use `ten-wins` to find card combinations that recur in successful run exports.
The command is local-only: it does not require The Bazaar, an API key, or a
network connection.

## Input

Pass a JSON file containing a `runs` array:

```json
{
  "runs": [
    {
      "id": "run-1",
      "wins": 10,
      "hero": "Dooley",
      "cards": ["Monitor Lizard", "Cog", "Chris Army Knife"]
    }
  ]
}
```

The command also accepts a top-level JSON array or one run object per JSONL
line. Use `--input -` to read the same formats from stdin. Additional fields in
a run record are ignored, so an exporter may retain its own metadata.

Each record must contain:

- `wins`: integer from 0 through 10;
- `hero`: non-empty hero name;
- `cards`: 1-64 non-empty card names.

The reader limits input to 64 MiB and 100,000 records. Card matching is
case-insensitive after trimming. Duplicate copies of one card inside a run
count once for combination support.

## Query

```powershell
bazaardb-cli ten-wins `
  --input .\examples\ten-win-runs.json `
  --hero Dooley `
  --card "Monitor Lizard" `
  --combination-size 2 `
  --min-runs 2 `
  --limit 20
```

Options:

| Option | Default | Meaning |
| --- | ---: | --- |
| `--hero NAME` | all | Keep one hero, using an exact case-insensitive match. |
| `--card NAME` | all | Return only combinations containing this card. |
| `--combination-size N` | `2` | Generate combinations of 2-5 distinct cards. |
| `--min-runs N` | `2` | Require the combination in at least this many runs. |
| `--limit N` | `20` | Return at most 1-1,000 ranked combinations. |

Only records with `wins=10` contribute. `support` is the combination's run
count divided by the number of ten-win runs remaining after the hero and card
filters. Results sort by run count descending and then card name ascending.

Use the global output option for automation or inspection:

```powershell
bazaardb-cli --output json ten-wins --input .\runs.json
bazaardb-cli --output jsonl ten-wins --input .\runs.json
bazaardb-cli --output table ten-wins --input .\runs.json
```

## Data-source boundary

`GameData.db` contains static card objects, not player-run outcomes. This CLI
does not obtain run data from BazaarDB or any game server. Supply only files
that you have the right to access and process. Do not distribute third-party or
personal data without the necessary rights or permission. See the repository
[NOTICE](../NOTICE.md).
