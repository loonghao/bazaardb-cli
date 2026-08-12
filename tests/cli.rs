use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::{Connection, params};
use serde_json::json;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::Duration;
use tempfile::TempDir;

const EAGLE_ID: &str = "0022c409-c839-41e8-8022-65a407457dfe";
const MERCHANT_ID: &str = "1022c409-c839-41e8-8022-65a407457dfe";
const UNIBOU_ID: &str = "7317d6a2-adea-442c-9e97-7f7bbf64ae99";

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bazaardb-cli"))
}

#[test]
fn profile_reads_multiple_tables_and_does_not_invent_ten_win_evidence() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    let knowledge = directory.path().join("knowledge");
    create_profile_game_data(&database);
    let output = command()
        .args([
            "--provider",
            "game-data",
            "--game-data",
            database.to_str().unwrap(),
            "--cache-dir",
            cache.to_str().unwrap(),
            "profile",
            "--hero",
            "Pygmalien",
            "--season-label",
            "Season 1",
            "--knowledge-root",
            knowledge.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["season"]["verified"], true);
    assert_eq!(value["rules"][0]["victoriesToWin"], 10);
    assert_eq!(value["heroPool"]["always"].as_array().unwrap().len(), 1);
    assert_eq!(
        value["archetypes"]["piggles"]["core"][0]["name"],
        "Piggles Launcher"
    );
    assert_eq!(
        value["levelUpChoices"][0]["eligibleGroups"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(value["tenWinEvidence"]["available"], false);
    assert_eq!(value["tenWinEvidence"]["inputRuns"], 0);
    let profile_root = knowledge.join("the-bazaar");
    let index: serde_json::Value =
        serde_json::from_slice(&fs::read(profile_root.join("index.json")).unwrap()).unwrap();
    assert_eq!(index["schemaVersion"], 2);
    let entry = &index["documents"][0];
    assert_eq!(entry["selectors"]["hero"], "Pygmalien");
    assert_eq!(entry["selectors"]["archetype"], "piggles");
    assert_eq!(entry["identities"]["content-version"], "5.0.0");
    let document: serde_json::Value = serde_json::from_slice(
        &fs::read(profile_root.join(entry["path"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    assert_eq!(document["fences"], entry["identities"]);
    assert_eq!(document["evidence"]["tenWin"]["status"], "unavailable");
}

#[test]
fn sqlite_row_id_is_authoritative_and_payload_conflicts_fail_closed() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    let card_without_id = json!({
        "InternalName": "No Payload Id",
        "Type": "Item",
        "Version": "1.0.0",
        "StartingTier": "Bronze",
        "Size": "Small",
        "Tags": [],
        "Tiers": {
            "Bronze": {"Attributes": {}},
            "Silver": {"Attributes": {}}
        }
    });
    create_single_card_game_data(&database, EAGLE_ID, &card_without_id);
    let output = resolve_process(&database, &cache);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["results"][0]["templateId"], EAGLE_ID);
    assert_eq!(value["results"][0]["found"], true);
    assert_eq!(
        value["results"][0]["template"]["payloadIdConsistency"],
        "absent"
    );

    fs::remove_file(&database).unwrap();
    fs::remove_dir_all(&cache).unwrap();
    let conflicting = json!({
        "Id": MERCHANT_ID,
        "InternalName": "Conflicting Payload Id",
        "Type": "Item",
        "Version": "1.0.0",
        "StartingTier": "Bronze",
        "Size": "Small",
        "Tags": [],
        "Tiers": {"Bronze": {"Attributes": {}}}
    });
    create_single_card_game_data(&database, EAGLE_ID, &conflicting);
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_bazaardb-cli"))
        .args([
            "--provider",
            "game-data",
            "--game-data",
            database.to_str().unwrap(),
            "--cache-dir",
            cache.to_str().unwrap(),
            "resolve",
            "--mode",
            "partial",
            &format!("{EAGLE_ID}@Bronze"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["results"][0]["found"], true);
    assert_eq!(value["results"][0]["complete"], false);
    assert!(
        value["results"][0]["malformed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|detail| detail.as_str().unwrap().contains("payloadId:mismatch"))
    );

    let search = command()
        .args([
            "--provider",
            "game-data",
            "--game-data",
            database.to_str().unwrap(),
            "--cache-dir",
            cache.to_str().unwrap(),
            "search",
            "Conflicting Payload Id",
        ])
        .output()
        .unwrap();
    assert!(
        search.status.success(),
        "{}",
        String::from_utf8_lossy(&search.stderr)
    );
    let search: serde_json::Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search["data"]["cards"][0]["templateId"], EAGLE_ID);
    assert_eq!(
        search["data"]["cards"][0]["payloadIdConsistency"],
        "mismatch"
    );
    assert_eq!(search["data"]["cards"][0]["complete"], false);
}

#[test]
fn wal_commits_invalidate_cross_process_snapshot_generation() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    create_game_data(&database);
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();

    let first = resolve_process(&database, &cache);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["results"][0]["attributes"]["values"]["Damage"], 20);
    let main_before = fs::read(&database).unwrap();

    let body = writer
        .query_row("SELECT Data FROM cards WHERE Id = ?1", [EAGLE_ID], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .unwrap();
    let mut card: serde_json::Value = serde_json::from_slice(&body).unwrap();
    card["Tiers"]["Silver"]["Attributes"]["Damage"] = json!(99);
    writer
        .execute(
            "UPDATE cards SET Data = ?1 WHERE Id = ?2",
            params![serde_json::to_vec(&card).unwrap(), EAGLE_ID],
        )
        .unwrap();
    assert_eq!(main_before, fs::read(&database).unwrap());
    assert!(database.with_extension("db-wal").exists());

    let second = resolve_process(&database, &cache);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(first["databaseSha256"], second["databaseSha256"]);
    assert_ne!(first["contentId"], second["contentId"]);
    assert_eq!(second["results"][0]["attributes"]["values"]["Damage"], 99);
}

#[test]
fn reader_shm_churn_does_not_invalidate_catalog_snapshot() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    create_game_data(&database);
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch(
            "CREATE TABLE reader_marker (value INTEGER); INSERT INTO reader_marker VALUES (1);",
        )
        .unwrap();

    let first = resolve_process(&database, &cache);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_json: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let main_before = fs::read(&database).unwrap();
    let wal_path = database.with_extension("db-wal");
    let wal_before = fs::read(&wal_path).unwrap();
    let shm_path = database.with_extension("db-shm");
    let mut shm = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&shm_path)
        .unwrap();
    let position = shm.metadata().unwrap().len() - 1;
    shm.seek(SeekFrom::Start(position)).unwrap();
    let mut byte = [0_u8; 1];
    shm.read_exact(&mut byte).unwrap();
    shm.seek(SeekFrom::Start(position)).unwrap();
    shm.write_all(&byte).unwrap();
    shm.flush().unwrap();

    assert_eq!(main_before, fs::read(&database).unwrap());
    assert_eq!(wal_before, fs::read(&wal_path).unwrap());
    let second = resolve_process(&database, &cache);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(String::from_utf8_lossy(&second.stderr).contains("outcome=\"hit\""));
    let second_json: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(first_json["contentId"], second_json["contentId"]);
}

#[test]
fn compact_projection_required_and_optional_shapes_are_honest() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    let malformed = json!({
        "Id": EAGLE_ID,
        "InternalName": "Malformed Projection",
        "Type": 42,
        "Version": [],
        "StartingTier": "Bronze",
        "Tags": "Item",
        "Tooltips": {},
        "Tiers": {"Bronze": {"Attributes": "wrong"}}
    });
    create_single_card_game_data(&database, EAGLE_ID, &malformed);
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_bazaardb-cli"))
        .args([
            "--provider",
            "game-data",
            "--game-data",
            database.to_str().unwrap(),
            "--cache-dir",
            cache.to_str().unwrap(),
            "resolve",
            "--mode",
            "partial",
            &format!("{EAGLE_ID}@Bronze"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let result = &value["results"][0];
    assert_eq!(result["complete"], false);
    assert!(
        result["missing"]
            .as_array()
            .unwrap()
            .contains(&json!("template.size"))
    );
    for expected in [
        "template.type:expected_string",
        "template.tags:expected_array",
        "template.version:expected_string",
        "template.tooltips:expected_array",
        "tiers.Bronze.attributes:expected_object",
    ] {
        assert!(
            result["malformed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|detail| detail == expected)
        );
    }
}

fn create_game_data(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch("CREATE TABLE cards (Id TEXT NOT NULL PRIMARY KEY, Data BLOB NOT NULL);")
        .unwrap();
    let cards = [
        json!({
            "$type": "TCardItem",
            "Id": EAGLE_ID,
            "InternalName": "Eagle Talisman",
            "Type": "Item",
            "StartingTier": "Bronze",
            "Size": "Small",
            "Tags": ["Loot"],
            "HiddenTags": ["CritReference"],
            "SpawningEligibility": "Always",
            "Localization": {
                "Title": {"Text": "Eagle Talisman"},
                "Tooltips": [
                    {"Content": {"Text": "When you sell this, gain 5 Gold"}},
                    {"Content": {"Text": "At the start of each day, get a Small item"}}
                ]
            },
            "Tiers": {
                "Bronze": {
                    "Attributes": {"AmmoMax": 5, "Damage": 10},
                    "AbilityIds": ["bronze-ability"]
                },
                "Silver": {
                    "Attributes": {"Damage": 20},
                    "AbilityIds": ["silver-ability"],
                    "AuraIds": ["silver-aura"]
                },
                "Gold": {
                    "Attributes": {},
                    "AbilityIds": ["scalar-ability"],
                    "AuraIds": ["scalar-aura"]
                }
            },
            "Abilities": {
                "bronze-ability": {"kind": "bronze"},
                "silver-ability": {"kind": "silver"},
                "scalar-ability": 7
            },
            "Auras": {
                "silver-aura": {"kind": "silver"},
                "scalar-aura": []
            },
            "Enchantments": {
                "Fiery": {"Damage": 10},
                "all": {"Damage": 11},
                "not_requested": {"Damage": 12},
                "Scalar": 13,
                "Broken": null
            },
            "future_field": {"preserved": true}
        }),
        json!({
            "$type": "TCardEncounterEvent",
            "Id": MERCHANT_ID,
            "InternalName": "Aila",
            "Type": "EventEncounter",
            "StartingTier": "Bronze",
            "Size": "Medium",
            "Tags": ["Merchant"],
            "SpawningEligibility": "Always",
            "Localization": {"Title": {"Text": "Aila"}}
            ,"Tiers": {"Bronze": {"Attributes": {}}}
        }),
    ];
    for card in cards {
        connection
            .execute(
                "INSERT INTO cards (Id, Data) VALUES (?1, ?2)",
                params![
                    card["Id"].as_str().unwrap(),
                    serde_json::to_vec(&card).unwrap()
                ],
            )
            .unwrap();
    }
}

fn create_profile_game_data(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE cards (Id TEXT NOT NULL PRIMARY KEY, Data BLOB NOT NULL);
             CREATE TABLE game_modes (Id TEXT NOT NULL PRIMARY KEY, Data BLOB NOT NULL);
             CREATE TABLE level_ups (Id INTEGER NOT NULL PRIMARY KEY, Data BLOB NOT NULL);
             CREATE TABLE seasons (Id INTEGER NOT NULL PRIMARY KEY, Data BLOB NOT NULL);",
        )
        .unwrap();
    let launcher = json!({
        "Id": EAGLE_ID,
        "Version": "5.0.0",
        "InternalName": "Piggles Launcher",
        "Type": "Item",
        "Heroes": ["Pygmalien"],
        "StartingTier": "Bronze",
        "Size": "Medium",
        "Tags": ["Toy"],
        "SpawningEligibility": "Always",
        "Localization": {
            "Title": {"Text": "Piggles Launcher"},
            "Tooltips": [{"Content": {"Text": "Charge an adjacent Small item"}}]
        },
        "Tiers": {"Bronze": {"Attributes": {"Damage": 10}}}
    });
    let wrong_hero = json!({
        "Id": MERCHANT_ID,
        "Version": "5.0.0",
        "InternalName": "Wrong Case",
        "Type": "Item",
        "Heroes": ["pygmalien"],
        "StartingTier": "Bronze",
        "Size": "Small",
        "Tags": [],
        "SpawningEligibility": "Always",
        "Tiers": {"Bronze": {"Attributes": {}}}
    });
    for card in [launcher, wrong_hero] {
        connection
            .execute(
                "INSERT INTO cards (Id, Data) VALUES (?1, ?2)",
                params![
                    card["Id"].as_str().unwrap(),
                    serde_json::to_vec(&card).unwrap()
                ],
            )
            .unwrap();
    }
    let game_mode = json!({
        "Id": "base",
        "Version": "5.0.0",
        "InternalName": "Base GameMode",
        "VictoriesToWin": 10,
        "NumberOfDays": 10,
        "HoursInADay": 6
    });
    connection
        .execute(
            "INSERT INTO game_modes (Id, Data) VALUES (?1, ?2)",
            params!["base", serde_json::to_vec(&game_mode).unwrap()],
        )
        .unwrap();
    let level_up = json!({
        "Level": 1,
        "Version": "5.0.0",
        "Rewards": {"Groups": [{
            "Filters": [{"Ids": [EAGLE_ID]}],
            "SelectionMethod": "Random",
            "Prerequisites": [{"Conditions": {
                "$type": "TRunConditionalPlayerHero",
                "Heroes": ["Pygmalien"],
                "Operator": "Any"
            }}]
        }]}
    });
    connection
        .execute(
            "INSERT INTO level_ups (Id, Data) VALUES (?1, ?2)",
            params![1, serde_json::to_vec(&level_up).unwrap()],
        )
        .unwrap();
    let season = json!({"Id": 2, "Version": "5.0.0", "InternalName": "Season 1"});
    connection
        .execute(
            "INSERT INTO seasons (Id, Data) VALUES (?1, ?2)",
            params![2, serde_json::to_vec(&season).unwrap()],
        )
        .unwrap();
}

fn mutate_game_data(path: &Path) {
    let connection = Connection::open(path).unwrap();
    let body = connection
        .query_row("SELECT Data FROM cards WHERE Id = ?1", [EAGLE_ID], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .unwrap();
    let mut card: serde_json::Value = serde_json::from_slice(&body).unwrap();
    card["catalogMutation"] = json!("x".repeat(4096));
    connection
        .execute(
            "UPDATE cards SET Data = ?1 WHERE Id = ?2",
            params![serde_json::to_vec(&card).unwrap(), EAGLE_ID],
        )
        .unwrap();
}

fn create_single_card_game_data(path: &Path, row_id: &str, card: &serde_json::Value) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch("CREATE TABLE cards (Id TEXT NOT NULL PRIMARY KEY, Data BLOB NOT NULL);")
        .unwrap();
    connection
        .execute(
            "INSERT INTO cards (Id, Data) VALUES (?1, ?2)",
            params![row_id, serde_json::to_vec(card).unwrap()],
        )
        .unwrap();
}

fn create_sixty_four_card_game_data(path: &Path) -> Vec<String> {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch("CREATE TABLE cards (Id TEXT NOT NULL PRIMARY KEY, Data BLOB NOT NULL);")
        .unwrap();
    let ids = (0..64)
        .map(|index| format!("00000000-0000-0000-0001-{index:012x}"))
        .collect::<Vec<_>>();
    for (index, id) in ids.iter().enumerate() {
        let card = json!({
            "Id": id,
            "InternalName": format!("Batch Card {index}"),
            "Type": "Item",
            "Version": "1.0.0",
            "StartingTier": "Bronze",
            "Size": "Small",
            "Tags": ["Fixture"],
            "Localization": {
                "Title": {"Text": format!("Batch Card {index}")},
                "Tooltips": [{"Content": {"Text": format!("Tooltip {index}")}}]
            },
            "Tiers": {"Bronze": {"Attributes": {"Damage": index}}}
        });
        connection
            .execute(
                "INSERT INTO cards (Id, Data) VALUES (?1, ?2)",
                params![id, serde_json::to_vec(&card).unwrap()],
            )
            .unwrap();
    }
    ids
}

fn resolve_process(database: &Path, cache: &Path) -> std::process::Output {
    ProcessCommand::new(env!("CARGO_BIN_EXE_bazaardb-cli"))
        .env("RUST_LOG", "bazaardb_cli::catalog_cache=info")
        .args([
            "--provider",
            "game-data",
            "--game-data",
            database.to_str().unwrap(),
            "--cache-dir",
            cache.to_str().unwrap(),
            "resolve",
            &format!("{EAGLE_ID}@Silver"),
        ])
        .output()
        .unwrap()
}

fn catalog_snapshot(cache: &Path) -> std::path::PathBuf {
    fs::read_dir(cache.join("catalog"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "snapshot")
        })
        .expect("catalog snapshot was not created")
}

fn replace_snapshot_header(path: &Path, field: &str, replacement: &str) {
    let mut bytes = fs::read(path).unwrap();
    let length = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
    let header = std::str::from_utf8(&bytes[20..20 + length]).unwrap();
    let current = if field == "resolverVersion" {
        "1.2.0"
    } else {
        "2.0.0"
    };
    let old = format!("\"{field}\":\"{current}\"");
    let new = format!("\"{field}\":\"{replacement}\"");
    assert_eq!(old.len(), new.len());
    let updated = header.replacen(&old, &new, 1);
    assert_ne!(header, updated, "snapshot header field was not found");
    bytes[20..20 + length].copy_from_slice(updated.as_bytes());
    fs::write(path, bytes).unwrap();
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn available_loopback_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn serve_exposes_read_only_state_with_etag_on_loopback() {
    let cache = TempDir::new().unwrap();
    let database = cache.path().join("GameData.db");
    create_game_data(&database);

    let port = available_loopback_port();
    let child = ProcessCommand::new(env!("CARGO_BIN_EXE_bazaardb-cli"))
        .args([
            "--provider",
            "game-data",
            "--game-data",
            database.to_str().unwrap(),
            "--cache-dir",
            cache.path().to_str().unwrap(),
            "serve",
            "eagle",
            "--port",
            &port.to_string(),
            "--refresh-seconds",
            "300",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = ChildGuard(child);
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/v1/state");
    let response = {
        let mut response = None;
        for _ in 0..50 {
            if let Ok(candidate) = client.get(&url).send().await
                && candidate.status().is_success()
            {
                response = Some(candidate);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        response.expect("read-only state server did not start")
    };
    let etag = response.headers()[reqwest::header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    let value: serde_json::Value = response.json().await.unwrap();
    assert_eq!(value["schema_version"], "1.0.0");
    assert_eq!(value["tick"], 1);
    assert_eq!(value["data"]["cards"][0]["name"], "Eagle Talisman");

    let cached = client
        .get(url)
        .header(reqwest::header::IF_NONE_MATCH, etag)
        .send()
        .await
        .unwrap();
    assert_eq!(cached.status(), reqwest::StatusCode::NOT_MODIFIED);
}

#[test]
fn endpoints_and_cache_status_do_not_require_credentials() {
    let cache = TempDir::new().unwrap();
    command()
        .arg("endpoints")
        .assert()
        .success()
        .stdout(predicate::str::contains("search_cards"))
        .stdout(predicate::str::contains("get_card"));

    command()
        .args([
            "--cache-dir",
            cache.path().to_str().unwrap(),
            "cache",
            "status",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"entries\": 0"));
}

#[test]
fn public_cli_exposes_only_local_data_sources() {
    let help = command().arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(!help.contains("parse"));
    assert!(!help.contains("api-base"));
    assert!(!help.contains("api-key"));

    let endpoints = command().arg("endpoints").output().unwrap();
    assert!(endpoints.status.success());
    let endpoints: serde_json::Value = serde_json::from_slice(&endpoints.stdout).unwrap();
    assert_eq!(
        endpoints["data"]["providers"],
        json!({
            "auto": "Require and use the installed game's read-only GameData.db",
            "game-data": "Read the installed game's local SQLite card catalog without an API key"
        })
    );
}

#[test]
fn ten_wins_aggregates_card_combinations_from_a_local_export() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("runs.json");
    fs::write(
        &input,
        serde_json::to_vec(&json!({
            "runs": [
                {
                    "id": "run-1",
                    "wins": 10,
                    "hero": "Dooley",
                    "cards": ["Monitor Lizard", "Cog", "Chris Army Knife"]
                },
                {
                    "id": "run-2",
                    "wins": 10,
                    "hero": "Dooley",
                    "cards": ["Cog", "Monitor Lizard", "C.O.R.A"]
                },
                {
                    "id": "run-3",
                    "wins": 9,
                    "hero": "Dooley",
                    "cards": ["Monitor Lizard", "Cog"]
                },
                {
                    "id": "run-4",
                    "wins": 10,
                    "hero": "Vanessa",
                    "cards": ["Monitor Lizard", "Chris Army Knife"]
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let output = command()
        .args([
            "ten-wins",
            "--input",
            input.to_str().unwrap(),
            "--hero",
            "dooley",
            "--combination-size",
            "2",
            "--min-runs",
            "2",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["command"], "ten-wins");
    assert_eq!(value["source"], "local-run-export");
    assert_eq!(value["data"]["inputRuns"], 4);
    assert_eq!(value["data"]["tenWinRuns"], 3);
    assert_eq!(value["data"]["matchedRuns"], 2);
    assert_eq!(value["data"]["combinationSize"], 2);
    assert_eq!(value["data"]["combinations"].as_array().unwrap().len(), 1);
    assert_eq!(
        value["data"]["combinations"][0]["cards"],
        json!(["Cog", "Monitor Lizard"])
    );
    assert_eq!(value["data"]["combinations"][0]["runs"], 2);
    assert_eq!(value["data"]["combinations"][0]["support"], 1.0);
}

#[test]
fn ten_wins_deduplicates_cards_within_each_run_and_filters_by_card() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("runs.jsonl");
    fs::write(
        &input,
        concat!(
            "{\"id\":\"run-1\",\"wins\":10,\"hero\":\"Mak\",\"cards\":[\"Athanor\",\"Broken Bottle\",\"Athanor\"]}\n",
            "{\"id\":\"run-2\",\"wins\":10,\"hero\":\"Mak\",\"cards\":[\"Broken Bottle\",\"Athanor\"]}\n",
            "{\"id\":\"run-3\",\"wins\":10,\"hero\":\"Mak\",\"cards\":[\"Athanor\",\"Energy Potion\"]}\n"
        ),
    )
    .unwrap();

    let output = command()
        .args([
            "ten-wins",
            "--input",
            input.to_str().unwrap(),
            "--card",
            "athanor",
            "--combination-size",
            "2",
            "--min-runs",
            "2",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["matchedRuns"], 3);
    assert_eq!(value["data"]["combinations"].as_array().unwrap().len(), 1);
    assert_eq!(
        value["data"]["combinations"][0]["cards"],
        json!(["Athanor", "Broken Bottle"])
    );
    assert_eq!(value["data"]["combinations"][0]["runs"], 2);
}

#[test]
fn ten_wins_rejects_an_invalid_combination_size() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("runs.json");
    fs::write(&input, r#"{"runs":[]}"#).unwrap();

    command()
        .args([
            "ten-wins",
            "--input",
            input.to_str().unwrap(),
            "--combination-size",
            "1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "combination-size must be between 2 and 5",
        ));
}

#[test]
fn ten_wins_rejects_duplicate_run_ids() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("runs.json");
    fs::write(
        &input,
        r#"{"runs":[{"id":"same","wins":10,"hero":"Mak","cards":["A","B"]},{"id":"same","wins":10,"hero":"Mak","cards":["A","B"]}]}"#,
    )
    .unwrap();

    command()
        .args(["ten-wins", "--input", input.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("duplicate run id: same"));
}

#[test]
fn game_data_provider_queries_without_credentials_and_reuses_response_cache() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    create_game_data(&database);

    let arguments = [
        "--provider",
        "game-data",
        "--game-data",
        database.to_str().unwrap(),
        "--cache-dir",
        cache.to_str().unwrap(),
        "search",
        "eagle",
        "--category",
        "items",
    ];
    command()
        .args(arguments)
        .assert()
        .success()
        .stdout(predicate::str::contains("local/GameData.db"))
        .stdout(predicate::str::contains("Eagle Talisman"))
        .stdout(predicate::str::contains("payloadIdConsistency"))
        .stdout(predicate::str::contains("\"miss\""));

    command()
        .args(arguments)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"hit\""));

    command()
        .args([
            "--provider",
            "game-data",
            "--game-data",
            database.to_str().unwrap(),
            "--cache-dir",
            cache.to_str().unwrap(),
            "--cache-mode",
            "offline",
            "get",
            "Eagle Talisman",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Eagle Talisman"))
        .stdout(predicate::str::contains("\"offline\""));
}

#[test]
fn resolve_is_compact_stable_and_enchantment_aware() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    create_game_data(&database);

    let output = command()
        .args([
            "--provider",
            "game-data",
            "--game-data",
            database.to_str().unwrap(),
            "--cache-dir",
            cache.to_str().unwrap(),
            "resolve",
            &format!("{EAGLE_ID}@Silver#Fiery"),
            &format!("{MERCHANT_ID}@Bronze"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["results"][0]["templateId"], EAGLE_ID);
    assert_eq!(value["results"][1]["templateId"], MERCHANT_ID);
    assert_eq!(value["results"][0]["attributes"]["values"]["AmmoMax"], 5);
    assert_eq!(value["results"][0]["attributes"]["values"]["Damage"], 20);
    assert_eq!(value["results"][0]["enchantments"]["ids"], json!(["Fiery"]));
    assert_eq!(
        value["results"][0]["templateContentId"],
        value["results"][0]["template"]["templateContentId"]
    );
    assert_eq!(
        value["results"][0]["template"]["tooltips"]["values"],
        json!([
            "When you sell this, gain 5 Gold",
            "At the start of each day, get a Small item"
        ])
    );
    assert_eq!(
        value["results"][1]["enchantments"]["status"],
        "not_requested"
    );
    assert!(value["results"][0].get("rawTemplate").is_none());
    assert!(
        value["results"][0]["resolutionKey"]
            .as_str()
            .unwrap()
            .contains("selector/exact/Fiery")
    );

    command()
        .args([
            "--provider",
            "game-data",
            "--game-data",
            database.to_str().unwrap(),
            "--cache-dir",
            cache.to_str().unwrap(),
            "resolve",
            &format!("{EAGLE_ID}@Silver#Unknown"),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown enchantment"));

    let jsonl = command()
        .args([
            "--provider",
            "game-data",
            "--game-data",
            database.to_str().unwrap(),
            "--cache-dir",
            cache.to_str().unwrap(),
            "--output",
            "jsonl",
            "resolve",
            &format!("{EAGLE_ID}@Silver#Fiery"),
        ])
        .output()
        .unwrap();
    assert!(jsonl.status.success());
    let record: serde_json::Value = serde_json::from_slice(&jsonl.stdout).unwrap();
    assert_eq!(record["authority"], "inspection_only");
    assert_eq!(record["authorizesAction"], false);
}

#[test]
fn compact_catalog_does_not_bundle_third_party_identifiers() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    let card = json!({
        "Id": UNIBOU_ID,
        "InternalName": "Unibou",
        "Type": "Item",
        "Version": "1.0.0",
        "StartingTier": "Bronze",
        "Size": "Medium",
        "Tags": ["Friend"],
        "Localization": {"Title": {"Text": "Unibou"}},
        "Tiers": {"Bronze": {"Attributes": {"Shield": 10}}}
    });
    create_single_card_game_data(&database, UNIBOU_ID, &card);

    let output = command()
        .args([
            "--provider",
            "game-data",
            "--game-data",
            database.to_str().unwrap(),
            "--cache-dir",
            cache.to_str().unwrap(),
            "resolve",
            &format!("{UNIBOU_ID}@Bronze"),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(response.get("externalIdentitySchemaVersion").is_none());
    assert!(response.get("externalIdentityContentId").is_none());
    assert!(
        response["results"][0]["template"]
            .get("externalReferences")
            .is_none()
    );
    let serialized = String::from_utf8(output.stdout).unwrap();
    assert!(!serialized.contains("bazaardb.gg"));
    assert!(!serialized.contains("externalCardId"));
}

#[test]
fn sixty_four_card_compact_batch_stays_below_response_cap() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    let ids = create_sixty_four_card_game_data(&database);
    let mut arguments = vec![
        "--provider".to_owned(),
        "game-data".to_owned(),
        "--game-data".to_owned(),
        database.to_string_lossy().into_owned(),
        "--cache-dir".to_owned(),
        cache.to_string_lossy().into_owned(),
        "resolve".to_owned(),
    ];
    arguments.extend(ids.iter().map(|id| format!("{id}@Bronze")));
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_bazaardb-cli"))
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() < 8 * 1024 * 1024);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["results"].as_array().unwrap().len(), 64);
    assert!(
        value["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| result.get("rawTemplate").is_none()
                && result["templateContentId"].as_str().is_some())
    );
}

#[tokio::test]
async fn catalog_api_is_read_only_compact_and_fail_closed() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    create_game_data(&database);
    let port = available_loopback_port();
    let child = ProcessCommand::new(env!("CARGO_BIN_EXE_bazaardb-cli"))
        .args([
            "--provider",
            "game-data",
            "--game-data",
            database.to_str().unwrap(),
            "--cache-dir",
            cache.to_str().unwrap(),
            "serve",
            "--port",
            &port.to_string(),
            "--refresh-seconds",
            "300",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = ChildGuard(child);
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}/v1/catalog");
    let status = {
        let mut response = None;
        for _ in 0..50 {
            if let Ok(candidate) = client.get(format!("{base}/status")).send().await
                && candidate.status().is_success()
            {
                response = Some(candidate);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        response.expect("catalog API did not start")
    };
    assert_eq!(
        status.headers()[reqwest::header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    let status: serde_json::Value = status.json().await.unwrap();
    assert_eq!(status["authority"], "inspection_only");
    assert_eq!(status["authorizesAction"], false);
    assert_eq!(status["catalogSchemaVersion"], "2.0.0");
    assert_eq!(status["resolverVersion"], "1.2.0");
    assert!(status.get("externalIdentitySchemaVersion").is_none());
    assert!(status.get("externalIdentityContentId").is_none());
    assert!(
        !serde_json::to_string(&status)
            .unwrap()
            .contains(database.to_str().unwrap())
    );

    let search = client
        .get(format!("{base}/search"))
        .query(&[("query", "eagle"), ("category", "items")])
        .send()
        .await
        .unwrap();
    assert!(search.status().is_success());
    assert_eq!(
        search.headers()[reqwest::header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    let search: serde_json::Value = search.json().await.unwrap();
    assert_eq!(search["authority"], "inspection_only");
    assert_eq!(search["authorizesAction"], false);
    assert_eq!(search["cards"][0]["name"], "Eagle Talisman");
    assert!(search["cards"][0].get("future_field").is_none());
    let searched_template_content_id = search["cards"][0]["templateContentId"].clone();
    assert_eq!(
        search["cards"][0]["tooltips"]["values"][0],
        "When you sell this, gain 5 Gold"
    );

    let resolved = client
        .post(format!("{base}/resolve"))
        .json(&json!({
            "requests": [{
                "templateId": EAGLE_ID,
                "tier": "Silver",
                "enchantmentId": "Fiery"
            }]
        }))
        .send()
        .await
        .unwrap();
    assert!(resolved.status().is_success());
    assert_eq!(
        resolved.headers()[reqwest::header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    let resolved: serde_json::Value = resolved.json().await.unwrap();
    assert_eq!(resolved["authority"], "inspection_only");
    assert_eq!(resolved["authorizesAction"], false);
    assert!(resolved["results"][0].get("rawTemplate").is_none());
    assert_eq!(
        resolved["results"][0]["enchantments"]["ids"],
        json!(["Fiery"])
    );
    assert_eq!(
        resolved["results"][0]["templateContentId"],
        searched_template_content_id
    );

    let distinct_tuples = client
        .post(format!("{base}/resolve"))
        .json(&json!({
            "requests": [
                {"templateId": EAGLE_ID, "tier": "Bronze"},
                {"templateId": EAGLE_ID, "tier": "Silver"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(distinct_tuples.status().is_success());

    let sentinel_ids = client
        .post(format!("{base}/resolve"))
        .json(&json!({
            "requests": [
                {"templateId": EAGLE_ID, "tier": "Bronze"},
                {"templateId": EAGLE_ID, "tier": "Bronze", "enchantmentId": "all"},
                {"templateId": EAGLE_ID, "tier": "Bronze", "enchantmentId": "not_requested"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(sentinel_ids.status().is_success());
    let sentinel_ids: serde_json::Value = sentinel_ids.json().await.unwrap();
    let keys = sentinel_ids["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["resolutionKey"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(keys.len(), 3);
    assert!(keys[0].ends_with("selector/not_requested"));
    assert!(keys[1].ends_with("selector/exact/all"));
    assert!(keys[2].ends_with("selector/exact/not_requested"));

    let include_all_valid = client
        .post(format!("{base}/resolve"))
        .json(&json!({
            "includeAllEnchantments": true,
            "requests": [{"templateId": MERCHANT_ID, "tier": "Bronze"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(include_all_valid.status().is_success());
    let include_all_valid: serde_json::Value = include_all_valid.json().await.unwrap();
    assert!(
        include_all_valid["results"][0]["resolutionKey"]
            .as_str()
            .unwrap()
            .ends_with("selector/all")
    );

    let include_all = client
        .post(format!("{base}/resolve"))
        .json(&json!({
            "includeAllEnchantments": true,
            "requests": [{"templateId": EAGLE_ID, "tier": "Bronze"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        include_all.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );
    let include_all: serde_json::Value = include_all.json().await.unwrap();
    assert!(
        include_all["error"]["details"]["failures"][0]["malformed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|detail| detail.as_str().unwrap().contains("Scalar:expected_object"))
    );

    let malformed_components = client
        .post(format!("{base}/resolve"))
        .json(&json!({
            "requests": [{"templateId": EAGLE_ID, "tier": "Gold"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        malformed_components.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );
    let malformed_components: serde_json::Value = malformed_components.json().await.unwrap();
    let malformed = malformed_components["error"]["details"]["failures"][0]["malformed"]
        .as_array()
        .unwrap();
    assert!(malformed.iter().any(|detail| {
        detail
            .as_str()
            .unwrap()
            .contains("abilities:scalar-ability:expected_object")
    }));
    assert!(malformed.iter().any(|detail| {
        detail
            .as_str()
            .unwrap()
            .contains("auras:scalar-aura:expected_object")
    }));

    let malformed_selected_enchantment = client
        .post(format!("{base}/resolve"))
        .json(&json!({
            "requests": [{
                "templateId": EAGLE_ID,
                "tier": "Silver",
                "enchantmentId": "Scalar"
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        malformed_selected_enchantment.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );

    for query in ["sortBy=bogus", "limti=1"] {
        let invalid = client
            .get(format!("{base}/search?{query}"))
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
        assert_eq!(
            invalid.headers()[reqwest::header::CACHE_CONTROL],
            "no-store, max-age=0"
        );
        let invalid: serde_json::Value = invalid.json().await.unwrap();
        assert_eq!(invalid["error"]["code"], "invalid_request");
        assert_eq!(invalid["authorizesAction"], false);
    }

    let unknown = client
        .post(format!("{base}/resolve"))
        .json(&json!({
            "requests": [{
                "templateId": EAGLE_ID,
                "tier": "Silver",
                "enchantmentId": "Unknown"
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        unknown.headers()[reqwest::header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    let unknown: serde_json::Value = unknown.json().await.unwrap();
    assert_eq!(unknown["error"]["code"], "unknown_enchantment");
    assert_eq!(unknown["authorizesAction"], false);

    let malformed_enchantment = client
        .post(format!("{base}/resolve"))
        .json(&json!({
            "requests": [{
                "templateId": EAGLE_ID,
                "tier": "Silver",
                "enchantmentId": "Broken"
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        malformed_enchantment.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );
    let malformed_enchantment: serde_json::Value = malformed_enchantment.json().await.unwrap();
    assert_eq!(malformed_enchantment["error"]["code"], "resolve_incomplete");

    let duplicate = client
        .post(format!("{base}/resolve"))
        .json(&json!({
            "requests": [
                {"templateId": EAGLE_ID, "tier": "Bronze"},
                {"templateId": EAGLE_ID, "tier": "Bronze"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        duplicate.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );
    let duplicate: serde_json::Value = duplicate.json().await.unwrap();
    assert_eq!(duplicate["error"]["code"], "duplicate_resolution");
}

#[test]
fn normalized_catalog_snapshot_hits_across_processes_and_changes_identity() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    create_game_data(&database);

    let cold = resolve_process(&database, &cache);
    assert!(
        cold.status.success(),
        "{}",
        String::from_utf8_lossy(&cold.stderr)
    );
    assert!(String::from_utf8_lossy(&cold.stderr).contains("outcome=\"miss_rebuilt\""));
    let cold_json: serde_json::Value = serde_json::from_slice(&cold.stdout).unwrap();

    let warm = resolve_process(&database, &cache);
    assert!(
        warm.status.success(),
        "{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let warm_trace = String::from_utf8_lossy(&warm.stderr);
    assert!(warm_trace.contains("outcome=\"hit\""));
    assert!(warm_trace.contains("database_bytes_read=0"));
    assert!(warm_trace.contains("sqlite_rows_read=0"));
    let warm_json: serde_json::Value = serde_json::from_slice(&warm.stdout).unwrap();
    assert_eq!(cold_json["contentId"], warm_json["contentId"]);

    mutate_game_data(&database);
    let changed = resolve_process(&database, &cache);
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    assert!(String::from_utf8_lossy(&changed.stderr).contains("outcome=\"miss_rebuilt\""));
    let changed_json: serde_json::Value = serde_json::from_slice(&changed.stdout).unwrap();
    assert_ne!(cold_json["contentId"], changed_json["contentId"]);
    assert_ne!(cold_json["databaseSha256"], changed_json["databaseSha256"]);
}

#[test]
fn template_digest_is_fenced_by_cross_template_static_definitions() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    create_game_data(&database);
    let connection = Connection::open(&database).unwrap();
    let eagle_body = connection
        .query_row("SELECT Data FROM cards WHERE Id = ?1", [EAGLE_ID], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .unwrap();
    let mut eagle: serde_json::Value = serde_json::from_slice(&eagle_body).unwrap();
    eagle["RelatedTemplateId"] = json!(MERCHANT_ID);
    connection
        .execute(
            "UPDATE cards SET Data = ?1 WHERE Id = ?2",
            params![serde_json::to_vec(&eagle).unwrap(), EAGLE_ID],
        )
        .unwrap();
    let first = resolve_process(&database, &cache);
    assert!(first.status.success());
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let first_digest = first["results"][0]["templateContentId"].clone();

    let dependency_body = connection
        .query_row(
            "SELECT Data FROM cards WHERE Id = ?1",
            [MERCHANT_ID],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .unwrap();
    let mut dependency: serde_json::Value = serde_json::from_slice(&dependency_body).unwrap();
    dependency["StaticDefinitionRevision"] = json!(2);
    connection
        .execute(
            "UPDATE cards SET Data = ?1 WHERE Id = ?2",
            params![serde_json::to_vec(&dependency).unwrap(), MERCHANT_ID],
        )
        .unwrap();
    let second = resolve_process(&database, &cache);
    assert!(second.status.success());
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_ne!(first["contentId"], second["contentId"]);
    assert_ne!(first_digest, second["results"][0]["templateContentId"]);
}

#[test]
fn catalog_cache_lifecycle_is_bounded_observable_and_clearable() {
    let directory = TempDir::new().unwrap();
    let cache = directory.path().join("cache");
    for generation in 0..5 {
        let database = directory.path().join(format!("generation-{generation}.db"));
        let card = json!({
            "Id": EAGLE_ID,
            "InternalName": "Lifecycle Fixture",
            "Type": "Item",
            "Version": "1.0.0",
            "StartingTier": "Bronze",
            "Size": "Small",
            "Tags": [],
            "Tiers": {"Bronze": {"Attributes": {"Generation": generation}}}
        });
        create_single_card_game_data(&database, EAGLE_ID, &card);
        let output = command()
            .args([
                "--provider",
                "game-data",
                "--game-data",
                database.to_str().unwrap(),
                "--cache-dir",
                cache.to_str().unwrap(),
                "resolve",
                &format!("{EAGLE_ID}@Bronze"),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let status = command()
        .args(["--cache-dir", cache.to_str().unwrap(), "cache", "status"])
        .output()
        .unwrap();
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert!(
        status["data"]["catalog"]["generationCount"]
            .as_u64()
            .unwrap()
            <= 3
    );
    assert!(status["data"]["catalog"]["snapshotBytes"].as_u64().unwrap() > 0);
    assert_eq!(status["data"]["catalog"]["maxGenerations"], 3);
    assert_eq!(
        status["data"]["catalog"]["maxBytes"],
        1024_u64 * 1024 * 1024
    );
    assert_eq!(
        status["data"]["catalog"]["retentionSeconds"],
        30_u64 * 24 * 60 * 60
    );

    let prune = command()
        .args(["--cache-dir", cache.to_str().unwrap(), "cache", "prune"])
        .output()
        .unwrap();
    assert!(prune.status.success());
    let prune: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert!(
        prune["data"]["catalog"]["remainingGenerations"]
            .as_u64()
            .unwrap()
            <= 3
    );

    command()
        .args([
            "--cache-dir",
            cache.to_str().unwrap(),
            "cache",
            "clear",
            "--yes",
        ])
        .assert()
        .success();
    let status = command()
        .args(["--cache-dir", cache.to_str().unwrap(), "cache", "status"])
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["data"]["catalog"]["generationCount"], 0);
    assert_eq!(status["data"]["catalog"]["snapshotBytes"], 0);
}

#[test]
fn corrupt_payload_schema_and_resolver_snapshots_rebuild() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("GameData.db");
    let cache = directory.path().join("cache");
    create_game_data(&database);
    assert!(resolve_process(&database, &cache).status.success());
    let snapshot = catalog_snapshot(&cache);

    fs::write(&snapshot, b"corrupt").unwrap();
    let corrupt = resolve_process(&database, &cache);
    assert!(
        corrupt.status.success(),
        "{}",
        String::from_utf8_lossy(&corrupt.stderr)
    );
    assert!(String::from_utf8_lossy(&corrupt.stderr).contains("outcome=\"miss_rebuilt\""));

    let mut bytes = fs::read(&snapshot).unwrap();
    let last = bytes.last_mut().unwrap();
    *last ^= 1;
    fs::write(&snapshot, bytes).unwrap();
    let payload = resolve_process(&database, &cache);
    assert!(
        payload.status.success(),
        "{}",
        String::from_utf8_lossy(&payload.stderr)
    );
    let payload_trace = String::from_utf8_lossy(&payload.stderr);
    assert!(payload_trace.contains("snapshot_payload_sha256_mismatch"));

    replace_snapshot_header(&snapshot, "catalogSchemaVersion", "9.9.9");
    let schema = resolve_process(&database, &cache);
    assert!(
        schema.status.success(),
        "{}",
        String::from_utf8_lossy(&schema.stderr)
    );
    assert!(String::from_utf8_lossy(&schema.stderr).contains("snapshot_catalog_schema_mismatch"));

    replace_snapshot_header(&snapshot, "resolverVersion", "9.9.9");
    let resolver = resolve_process(&database, &cache);
    assert!(
        resolver.status.success(),
        "{}",
        String::from_utf8_lossy(&resolver.stderr)
    );
    assert!(String::from_utf8_lossy(&resolver.stderr).contains("snapshot_resolver_mismatch"));
}
