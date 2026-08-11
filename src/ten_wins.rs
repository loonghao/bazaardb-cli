use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

const MAX_RUNS: usize = 100_000;
const MAX_CARDS_PER_RUN: usize = 64;
const MAX_CARD_NAME_BYTES: usize = 200;
const MAX_RUN_ID_BYTES: usize = 200;
const MAX_COMBINATIONS_PER_RUN: usize = 100_000;
const MAX_TOTAL_COMBINATIONS: usize = 5_000_000;

#[derive(Debug, Clone, Deserialize)]
pub struct RunRecord {
    #[serde(default)]
    pub id: Option<String>,
    pub wins: u8,
    pub hero: String,
    pub cards: Vec<String>,
}

impl RunRecord {
    pub fn validate(&self) -> Result<()> {
        if let Some(id) = self.id.as_deref() {
            if id.trim().is_empty() {
                bail!("run id must not be empty when present");
            }
            if id.len() > MAX_RUN_ID_BYTES {
                bail!("run id must not exceed {MAX_RUN_ID_BYTES} bytes");
            }
        }
        if self.wins > 10 {
            bail!("run wins must be between 0 and 10");
        }
        if self.hero.trim().is_empty() {
            bail!("run hero is required");
        }
        if !(1..=MAX_CARDS_PER_RUN).contains(&self.cards.len()) {
            bail!("each run must contain between 1 and {MAX_CARDS_PER_RUN} cards");
        }
        for card in &self.cards {
            let name = card.trim();
            if name.is_empty() {
                bail!("run card names must not be empty");
            }
            if name.len() > MAX_CARD_NAME_BYTES {
                bail!("run card names must not exceed {MAX_CARD_NAME_BYTES} bytes");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TenWinQuery {
    pub hero: Option<String>,
    pub card: Option<String>,
    pub combination_size: usize,
    pub min_runs: usize,
    pub limit: usize,
}

impl TenWinQuery {
    pub fn validate(&self) -> Result<()> {
        if !(2..=5).contains(&self.combination_size) {
            bail!("combination-size must be between 2 and 5");
        }
        if self.min_runs == 0 {
            bail!("min-runs must be at least 1");
        }
        if !(1..=1_000).contains(&self.limit) {
            bail!("limit must be between 1 and 1000");
        }
        if self
            .hero
            .as_deref()
            .is_some_and(|hero| hero.trim().is_empty())
        {
            bail!("hero must not be empty");
        }
        if self
            .card
            .as_deref()
            .is_some_and(|card| card.trim().is_empty())
        {
            bail!("card must not be empty");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenWinCombination {
    pub cards: Vec<String>,
    pub runs: usize,
    pub support: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenWinFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hero: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    pub min_runs: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenWinResult {
    pub input_runs: usize,
    pub ten_win_runs: usize,
    pub matched_runs: usize,
    pub combination_size: usize,
    pub filters: TenWinFilters,
    pub combinations: Vec<TenWinCombination>,
}

pub fn analyze_ten_wins(runs: &[RunRecord], query: &TenWinQuery) -> Result<TenWinResult> {
    query.validate()?;
    if runs.len() > MAX_RUNS {
        bail!("run export exceeds the {MAX_RUNS} record limit");
    }
    let mut run_ids = BTreeSet::new();
    for run in runs {
        run.validate()?;
        if let Some(id) = run.id.as_deref().map(str::trim)
            && !run_ids.insert(id)
        {
            bail!("duplicate run id: {id}");
        }
    }

    let hero_filter = query.hero.as_deref().map(normalize);
    let card_filter = query.card.as_deref().map(normalize);
    let ten_win_runs = runs.iter().filter(|run| run.wins == 10).count();
    let mut matched_runs = 0_usize;
    let mut total_expansion = 0_usize;
    let mut display_names = BTreeMap::<String, String>::new();
    let mut counts = HashMap::<Vec<String>, usize>::new();
    for run in runs.iter().filter(|run| run.wins == 10).filter(|run| {
        hero_filter
            .as_deref()
            .is_none_or(|hero| normalize(&run.hero) == hero)
    }) {
        let cards = normalized_cards(&run.cards);
        if card_filter
            .as_deref()
            .is_some_and(|card| !cards.contains_key(card))
        {
            continue;
        }
        matched_runs += 1;
        let expansion = card_filter.as_deref().map_or_else(
            || combination_count(cards.len(), query.combination_size),
            |_| combination_count(cards.len().saturating_sub(1), query.combination_size - 1),
        );
        total_expansion = total_expansion.saturating_add(expansion);
        if expansion > MAX_COMBINATIONS_PER_RUN || total_expansion > MAX_TOTAL_COMBINATIONS {
            bail!(
                "requested combination expansion exceeds the safety limit; reduce combination-size or filter the runs"
            );
        }
        for (key, display) in &cards {
            display_names
                .entry(key.clone())
                .and_modify(|current| {
                    if display < current {
                        current.clone_from(display);
                    }
                })
                .or_insert_with(|| display.clone());
        }
        count_combinations(
            &cards,
            query.combination_size,
            card_filter.as_deref(),
            &mut counts,
        );
    }

    let denominator = matched_runs as f64;
    let mut combinations = counts
        .into_iter()
        .filter(|(_, count)| *count >= query.min_runs)
        .map(|(cards, runs)| TenWinCombination {
            cards: cards
                .iter()
                .map(|key| {
                    display_names
                        .get(key)
                        .cloned()
                        .unwrap_or_else(|| key.clone())
                })
                .collect(),
            runs,
            support: if denominator == 0.0 {
                0.0
            } else {
                runs as f64 / denominator
            },
        })
        .collect::<Vec<_>>();
    combinations.sort_by(|left, right| {
        right
            .runs
            .cmp(&left.runs)
            .then_with(|| left.cards.cmp(&right.cards))
    });
    combinations.truncate(query.limit);

    Ok(TenWinResult {
        input_runs: runs.len(),
        ten_win_runs,
        matched_runs,
        combination_size: query.combination_size,
        filters: TenWinFilters {
            hero: query.hero.as_deref().map(str::trim).map(str::to_owned),
            card: query.card.as_deref().map(str::trim).map(str::to_owned),
            min_runs: query.min_runs,
        },
        combinations,
    })
}

fn normalized_cards(cards: &[String]) -> BTreeMap<String, String> {
    let mut unique = BTreeMap::new();
    for card in cards {
        let display = card.trim().to_owned();
        let key = normalize(&display);
        unique
            .entry(key)
            .and_modify(|current: &mut String| {
                if display < *current {
                    current.clone_from(&display);
                }
            })
            .or_insert(display);
    }
    unique
}

fn normalize(value: &str) -> String {
    value.trim().to_lowercase()
}

fn count_combinations(
    cards: &BTreeMap<String, String>,
    size: usize,
    required_card: Option<&str>,
    counts: &mut HashMap<Vec<String>, usize>,
) {
    let keys = cards.keys().cloned().collect::<Vec<_>>();
    if let Some(required_card) = required_card {
        let other_keys = keys
            .into_iter()
            .filter(|key| key != required_card)
            .collect::<Vec<_>>();
        for_each_combination(&other_keys, size - 1, |partial| {
            let mut combination = partial.to_vec();
            let insert_at = combination
                .binary_search_by(|candidate| candidate.as_str().cmp(required_card))
                .unwrap_or_else(|index| index);
            combination.insert(insert_at, required_card.to_owned());
            *counts.entry(combination).or_insert(0) += 1;
        });
    } else {
        for_each_combination(&keys, size, |combination| {
            *counts.entry(combination.to_vec()).or_insert(0) += 1;
        });
    }
}

#[cfg(test)]
fn combinations(values: &[String], size: usize) -> Vec<Vec<String>> {
    let mut output = Vec::new();
    for_each_combination(values, size, |combination| {
        output.push(combination.to_vec());
    });
    output
}

fn for_each_combination(values: &[String], size: usize, mut visit: impl FnMut(&[String])) {
    let mut current = Vec::with_capacity(size);
    visit_combinations(values, size, 0, &mut current, &mut visit);
}

fn combination_count(values: usize, size: usize) -> usize {
    if size > values {
        return 0;
    }
    let size = size.min(values - size);
    (0..size)
        .try_fold(1_usize, |count, index| {
            count
                .checked_mul(values - index)
                .map(|value| value / (index + 1))
        })
        .unwrap_or(usize::MAX)
}

fn visit_combinations(
    values: &[String],
    size: usize,
    start: usize,
    current: &mut Vec<String>,
    visit: &mut impl FnMut(&[String]),
) {
    if current.len() == size {
        visit(current);
        return;
    }
    let remaining = size - current.len();
    if values.len().saturating_sub(start) < remaining {
        return;
    }
    for index in start..=values.len() - remaining {
        current.push(values[index].clone());
        visit_combinations(values, size, index + 1, current, visit);
        current.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combination_generation_is_stable_at_boundaries() {
        assert!(combinations(&[], 2).is_empty());
        assert!(combinations(&["a".into()], 2).is_empty());
        assert_eq!(
            combinations(&["a".into(), "b".into(), "c".into()], 2),
            vec![
                vec!["a".to_owned(), "b".to_owned()],
                vec!["a".to_owned(), "c".to_owned()],
                vec!["b".to_owned(), "c".to_owned()],
            ]
        );
    }

    #[test]
    fn normalized_cards_deduplicate_case_insensitively() {
        let cards =
            normalized_cards(&["Athanor".into(), " athanor ".into(), "Broken Bottle".into()]);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards["athanor"], "Athanor");
    }

    #[test]
    fn rejects_excessive_combination_expansion() {
        let run = RunRecord {
            id: Some("run-1".into()),
            wins: 10,
            hero: "Dooley".into(),
            cards: (0..30).map(|index| format!("Card {index}")).collect(),
        };
        let error = analyze_ten_wins(
            &[run],
            &TenWinQuery {
                hero: None,
                card: None,
                combination_size: 5,
                min_runs: 1,
                limit: 20,
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("requested combination expansion exceeds")
        );
    }

    #[test]
    fn filtered_queries_prune_unrelated_combinations_before_the_safety_limit() {
        let run = RunRecord {
            id: Some("run-1".into()),
            wins: 10,
            hero: "Dooley".into(),
            cards: (0..30).map(|index| format!("Card {index}")).collect(),
        };

        let result = analyze_ten_wins(
            &[run],
            &TenWinQuery {
                hero: None,
                card: Some("Card 0".into()),
                combination_size: 5,
                min_runs: 1,
                limit: 1_000,
            },
        )
        .unwrap();

        assert_eq!(result.matched_runs, 1);
        assert_eq!(result.combinations.len(), 1_000);
        assert!(
            result
                .combinations
                .iter()
                .all(|combination| combination.cards.iter().any(|card| card == "Card 0"))
        );
    }
}
