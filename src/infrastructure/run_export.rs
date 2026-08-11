use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::ten_wins::RunRecord;

const MAX_EXPORT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct WrappedRuns {
    runs: Vec<RunRecord>,
}

pub fn load_run_export(path: &Path) -> Result<Vec<RunRecord>> {
    let bytes = if path == Path::new("-") {
        read_limited(io::stdin().lock()).context("failed to read run export from stdin")?
    } else {
        let file = File::open(path)
            .with_context(|| format!("failed to open run export at {}", path.display()))?;
        read_limited(file)
            .with_context(|| format!("failed to read run export at {}", path.display()))?
    };
    parse_run_export(&bytes)
}

pub fn parse_run_export(bytes: &[u8]) -> Result<Vec<RunRecord>> {
    if bytes.is_empty() {
        bail!("run export is empty");
    }
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        return match value {
            serde_json::Value::Array(_) => {
                serde_json::from_value(value).context("run export array contains an invalid record")
            }
            serde_json::Value::Object(ref object) if object.contains_key("runs") => {
                serde_json::from_value::<WrappedRuns>(value)
                    .map(|wrapped| wrapped.runs)
                    .context("run export wrapper contains invalid records")
            }
            serde_json::Value::Object(_) => serde_json::from_value(value)
                .map(|run| vec![run])
                .context("run export contains an invalid record"),
            _ => bail!("run export must be a JSON array, an object with runs, or JSONL records"),
        };
    }

    let text = std::str::from_utf8(bytes).context("run export is not valid UTF-8")?;
    let mut runs = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let run = serde_json::from_str::<RunRecord>(line)
            .with_context(|| format!("run export JSONL record {} is invalid", index + 1))?;
        runs.push(run);
    }
    if runs.is_empty() {
        bail!("run export contains no records");
    }
    Ok(runs)
}

fn read_limited(mut reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take((MAX_EXPORT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_EXPORT_BYTES {
        bail!("run export exceeds the {MAX_EXPORT_BYTES} byte limit");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wrapped_array_array_and_jsonl() {
        let wrapped =
            parse_run_export(br#"{"runs":[{"wins":10,"hero":"Dooley","cards":["Cog"]}]}"#).unwrap();
        let array = parse_run_export(br#"[{"wins":10,"hero":"Dooley","cards":["Cog"]}]"#).unwrap();
        let jsonl = parse_run_export(
            b"{\"wins\":10,\"hero\":\"Dooley\",\"cards\":[\"Cog\"]}\n{\"wins\":9,\"hero\":\"Mak\",\"cards\":[\"Athanor\"]}\n",
        )
        .unwrap();
        assert_eq!(wrapped.len(), 1);
        assert_eq!(array.len(), 1);
        assert_eq!(jsonl.len(), 2);
    }
}
