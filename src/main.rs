use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use bazaardb_cli::domain::{
    CacheDisposition, CacheMode, GetCardRequest, OUTPUT_SCHEMA_VERSION, SearchCardsRequest,
};
use bazaardb_cli::infrastructure::{
    GithubUpdater, catalog_cache_status, clear_catalog_cache, load_profile_snapshot,
    load_run_export, prune_catalog_cache,
};
use bazaardb_cli::profile::{
    ProfileRequest, generate_profile, load_supplement, render_markdown, write_dcc_knowledge,
};
use bazaardb_cli::server::{ServeConfig, loopback_socket};
use bazaardb_cli::{
    BazaarService, CacheStore, CanonicalGameIdentifier, CanonicalUuid, CardTier, CatalogService,
    GameDataGateway, GameDataGatewayConfig, ResolveBatchRequest, ResolveBatchResponse,
    ResolveCardRequest, ResolveJsonlRecord, ResolveMode, TenWinQuery, TenWinResult,
    analyze_ten_wins, detect_game_data_path,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use directories::ProjectDirs;
use serde::Serialize;
use serde_json::Value;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "bazaardb-cli", version, about)]
struct Cli {
    /// Data provider. Auto prefers the installed game's read-only GameData.db.
    #[arg(long, env = "BAZAARDB_PROVIDER", value_enum, default_value_t = ProviderArg::Auto, global = true)]
    provider: ProviderArg,

    /// Explicit path to the game's cached GameData.db.
    #[arg(long, env = "BAZAARDB_GAME_DATA", global = true)]
    game_data: Option<PathBuf>,

    #[arg(long, env = "BAZAARDB_CACHE_DIR", global = true)]
    cache_dir: Option<PathBuf>,

    #[arg(long, value_enum, default_value_t = CacheModeArg::Use, global = true)]
    cache_mode: CacheModeArg,

    #[arg(long, value_enum, default_value_t = OutputFormat::Json, global = true)]
    output: OutputFormat,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProviderArg {
    Auto,
    GameData,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CacheModeArg {
    Use,
    Refresh,
    Offline,
}

impl From<CacheModeArg> for CacheMode {
    fn from(value: CacheModeArg) -> Self {
        match value {
            CacheModeArg::Use => Self::Use,
            CacheModeArg::Refresh => Self::Refresh,
            CacheModeArg::Offline => Self::Offline,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Json,
    Jsonl,
    Table,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Search the installed game's local card catalog.
    Search(SearchArgs),
    /// Fetch one complete card by its name.
    Get(GetArgs),
    /// Resolve 1-64 canonical template UUIDs at explicit tiers.
    Resolve(ResolveArgs),
    /// Find frequent card combinations in a local export of ten-win runs.
    TenWins(TenWinsArgs),
    /// Generate an evidence-fenced gameplay handbook from the installed snapshot.
    Profile(ProfileArgs),
    /// List every endpoint supported by the configured provider.
    Endpoints,
    /// Inspect or maintain the local response cache.
    Cache(CacheArgs),
    /// Expose the card catalog through a read-only loopback HTTP API.
    Serve(ServeArgs),
    /// Check for or install a newer GitHub Release binary.
    Update(UpdateArgs),
}

#[derive(Debug, Args)]
struct ProfileArgs {
    /// Canonical hero identifier. Matching against card Heroes is exact.
    #[arg(long)]
    hero: String,

    /// Explicit season label matched exactly against the local seasons table.
    #[arg(long)]
    season_label: Option<String>,

    /// Optional local, schema-versioned supplement. This command never fetches its URLs.
    #[arg(long, value_name = "PATH")]
    supplement: Option<PathBuf>,

    /// Optional local JSON or JSONL run export used only as ten-win evidence.
    #[arg(long, value_name = "PATH")]
    runs: Option<PathBuf>,

    #[arg(long, value_enum, default_value_t = ProfileFormat::Json)]
    format: ProfileFormat,

    /// Profile knowledge root where playbooks/index.json is merged for dcc-cua.
    #[arg(long, value_name = "DIR")]
    dcc_knowledge_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProfileFormat {
    Json,
    Markdown,
}

#[derive(Debug, Args, Clone)]
struct SearchArgs {
    query: Option<String>,

    #[arg(long, default_value = "all")]
    category: String,

    #[arg(long, default_value_t = 0)]
    page: u32,

    #[arg(long, default_value_t = 25)]
    limit: u32,

    #[arg(long, default_value = "Auto")]
    sort_by: String,

    #[arg(long, default_value = "ascending")]
    order: String,

    #[arg(long)]
    show_unobtainable: bool,

    #[arg(long)]
    all: bool,

    #[arg(long, default_value_t = 8)]
    concurrency: usize,

    #[arg(long, default_value_t = 100)]
    max_pages: usize,
}

impl From<SearchArgs> for SearchCardsRequest {
    fn from(value: SearchArgs) -> Self {
        Self {
            query: value.query,
            category: value.category,
            page: value.page,
            limit: value.limit,
            sort_by: value.sort_by,
            order: value.order,
            show_unobtainable: value.show_unobtainable,
        }
    }
}

#[derive(Debug, Args)]
struct GetArgs {
    name: String,
}

#[derive(Debug, Args)]
struct TenWinsArgs {
    /// JSON or JSONL run export. Use - to read stdin.
    #[arg(long, value_name = "PATH")]
    input: PathBuf,

    /// Include only runs for this hero (case-insensitive exact match).
    #[arg(long)]
    hero: Option<String>,

    /// Include only combinations containing this card.
    #[arg(long)]
    card: Option<String>,

    /// Number of distinct cards in each combination.
    #[arg(long, default_value_t = 2)]
    combination_size: usize,

    /// Minimum number of matching runs containing a combination.
    #[arg(long, default_value_t = 2)]
    min_runs: usize,

    /// Maximum number of combinations to return.
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

#[derive(Debug, Args)]
struct ResolveArgs {
    /// Fail the whole batch on incomplete data, or return explicit partial results.
    #[arg(long, value_enum, default_value_t = ResolveModeArg::Strict)]
    mode: ResolveModeArg,

    /// Include the complete source card JSON in each resolved result.
    #[arg(long)]
    include_raw_template: bool,

    /// Include every enchantment definition instead of one requested exact definition.
    #[arg(long)]
    include_all_enchantments: bool,

    /// One or more TEMPLATE_UUID@TIER[#ENCHANTMENT_ID] values, preserving input order.
    #[arg(value_name = "TEMPLATE_UUID@TIER[#ENCHANTMENT_ID]", required = true, num_args = 1..=64)]
    requests: Vec<ResolveSpec>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ResolveModeArg {
    Strict,
    Partial,
}

impl From<ResolveModeArg> for ResolveMode {
    fn from(value: ResolveModeArg) -> Self {
        match value {
            ResolveModeArg::Strict => Self::Strict,
            ResolveModeArg::Partial => Self::Partial,
        }
    }
}

#[derive(Debug, Clone)]
struct ResolveSpec {
    template_id: CanonicalUuid,
    tier: CardTier,
    enchantment_id: Option<CanonicalGameIdentifier>,
}

impl FromStr for ResolveSpec {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let (card, enchantment_id) = value
            .split_once('#')
            .map_or((value, None), |(card, enchantment)| {
                (card, Some(enchantment))
            });
        let (template_id, tier) = card
            .split_once('@')
            .context("resolve value must use TEMPLATE_UUID@TIER[#ENCHANTMENT_ID]")?;
        Ok(Self {
            template_id: template_id.parse()?,
            tier: tier.parse()?,
            enchantment_id: enchantment_id.map(str::parse).transpose()?,
        })
    }
}

#[derive(Debug, Args)]
struct CacheArgs {
    #[command(subcommand)]
    command: CacheCommand,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    Status,
    Prune,
    Clear {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
struct ServeArgs {
    query: Option<String>,

    #[arg(long, default_value = "all")]
    category: String,

    #[arg(long, default_value_t = 7878)]
    port: u16,

    #[arg(long, default_value_t = 300)]
    refresh_seconds: u64,

    #[arg(long, default_value_t = 25)]
    limit: u32,
}

#[derive(Debug, Args)]
struct UpdateArgs {
    /// Only report whether a newer release exists.
    #[arg(long)]
    check: bool,

    /// Do not ask for confirmation before replacing the executable.
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Serialize)]
struct Envelope<T> {
    schema_version: &'static str,
    command: &'static str,
    source: &'static str,
    cache: Option<CacheSummary>,
    data: T,
}

#[derive(Debug, Serialize)]
struct CacheSummary {
    requests: usize,
    dispositions: BTreeMap<String, usize>,
}

impl CacheSummary {
    fn from_dispositions(values: impl IntoIterator<Item = CacheDisposition>) -> Self {
        let mut dispositions = BTreeMap::new();
        let mut requests = 0;
        for value in values {
            *dispositions.entry(value.to_string()).or_insert(0) += 1;
            requests += 1;
        }
        Self {
            requests,
            dispositions,
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        let response = serde_json::json!({
            "schema_version": OUTPUT_SCHEMA_VERSION,
            "success": false,
            "error": {
                "message": error.to_string(),
                "causes": error.chain().skip(1).map(ToString::to_string).collect::<Vec<_>>(),
            }
        });
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&response).unwrap_or_else(|_| error.to_string())
        );
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .try_init()
        .ok();
    let cli = Cli::parse();

    match cli.command {
        Command::Endpoints => {
            return print_value(
                &Envelope {
                    schema_version: OUTPUT_SCHEMA_VERSION,
                    command: "endpoints",
                    source: "bazaardb-cli",
                    cache: None,
                    data: serde_json::json!({
                        "endpoints": [
                            {
                                "name": "search_cards",
                                "commands": ["search"],
                                "categories": ["all", "items", "skills", "merchants", "trainers", "monsters", "events"]
                            },
                            {"name": "get_card", "commands": ["get"]},
                            {
                                "name": "resolve_catalog",
                                "commands": ["resolve"],
                                "batch": {"minimum": 1, "maximum": 64},
                                "requires": "game-data"
                            },
                            {
                                "name": "analyze_ten_win_combinations",
                                "commands": ["ten-wins"],
                                "source": "local JSON or JSONL run export",
                                "authentication": "none"
                            },
                            {
                                "name": "generate_gameplay_profile",
                                "commands": ["profile"],
                                "source": "local GameData.db plus explicit local evidence files",
                                "network": false
                            }
                        ],
                        "providers": {
                            "auto": "Require and use the installed game's read-only GameData.db",
                            "game-data": "Read the installed game's local SQLite card catalog without an API key"
                        }
                    }),
                },
                cli.output,
            );
        }
        Command::TenWins(args) => return execute_ten_wins(args, cli.output),
        Command::Update(args) => return execute_update(args),
        _ => {}
    }

    let cache_path = cache_path(cli.cache_dir.as_deref())?;
    let catalog_cache_dir = cache_path
        .parent()
        .context("response cache path has no parent")?
        .join("catalog");
    let cache = CacheStore::open(&cache_path)?;
    if let Command::Cache(args) = cli.command {
        return execute_cache(cache, &catalog_cache_dir, args, cli.output).await;
    }

    let services = create_services(&cli, cache, catalog_cache_dir)?;
    let service = services.cards.clone();
    let source = services.source;

    match cli.command {
        Command::Search(args) => {
            let all = args.all;
            let concurrency = args.concurrency;
            let max_pages = args.max_pages;
            let request = SearchCardsRequest::from(args);
            let (result, cache) = service
                .search(request, all, concurrency, max_pages, cli.cache_mode.into())
                .await?;
            if matches!(cli.output, OutputFormat::Jsonl | OutputFormat::Table) {
                return print_cards(&result.cards, cli.output);
            }
            print_value(
                &Envelope {
                    schema_version: OUTPUT_SCHEMA_VERSION,
                    command: "search",
                    source,
                    cache: Some(CacheSummary::from_dispositions(cache)),
                    data: result,
                },
                cli.output,
            )
        }
        Command::Get(args) => {
            let (card, cache) = service
                .get_card(&GetCardRequest { name: args.name }, cli.cache_mode.into())
                .await?;
            print_value(
                &Envelope {
                    schema_version: OUTPUT_SCHEMA_VERSION,
                    command: "get",
                    source,
                    cache: Some(CacheSummary::from_dispositions([cache])),
                    data: card,
                },
                cli.output,
            )
        }
        Command::Resolve(args) => {
            let catalog = services
                .catalog
                .context("resolve requires the game-data provider; use --provider game-data")?;
            let request = ResolveBatchRequest {
                requests: args
                    .requests
                    .into_iter()
                    .map(|request| ResolveCardRequest {
                        template_id: request.template_id,
                        tier: request.tier,
                        enchantment_id: request.enchantment_id,
                    })
                    .collect(),
                mode: args.mode.into(),
                include_raw_template: args.include_raw_template,
                include_all_enchantments: args.include_all_enchantments,
            };
            let response = catalog.resolve(&request).await?;
            print_resolve(&response, cli.output)
        }
        Command::Profile(args) => execute_profile(&services, args).await,
        Command::Serve(args) => {
            if !(5..=86_400).contains(&args.refresh_seconds) {
                bail!("refresh-seconds must be between 5 and 86400");
            }
            let request = SearchCardsRequest {
                query: args.query,
                category: args.category,
                limit: args.limit,
                ..SearchCardsRequest::default()
            };
            request.validate()?;
            let catalog = services
                .catalog
                .context("serve requires the game-data provider")?;
            bazaardb_cli::server::serve(
                service,
                catalog,
                ServeConfig {
                    listen: loopback_socket(args.port),
                    request,
                    refresh_interval: Duration::from_secs(args.refresh_seconds),
                },
            )
            .await
        }
        Command::Cache(_) | Command::Endpoints | Command::TenWins(_) | Command::Update(_) => {
            unreachable!()
        }
    }
}

fn execute_ten_wins(args: TenWinsArgs, output: OutputFormat) -> Result<()> {
    let runs = load_run_export(&args.input)?;
    let result = analyze_ten_wins(
        &runs,
        &TenWinQuery {
            hero: args.hero,
            card: args.card,
            combination_size: args.combination_size,
            min_runs: args.min_runs,
            limit: args.limit,
        },
    )?;
    print_ten_wins(&result, output)
}

struct SelectedServices {
    cards: BazaarService,
    catalog: Option<CatalogService>,
    source: &'static str,
    database_path: PathBuf,
}

fn create_services(
    cli: &Cli,
    cache: CacheStore,
    catalog_cache_dir: PathBuf,
) -> Result<SelectedServices> {
    let provider = match cli.provider {
        ProviderArg::GameData => ProviderSelection::GameData(
            cli.game_data
                .clone()
                .or_else(detect_game_data_path)
                .context(
                    "GameData.db was not found; launch The Bazaar once or pass --game-data PATH",
                )?,
        ),
        ProviderArg::Auto => ProviderSelection::GameData(
            cli.game_data
                .clone()
                .or_else(detect_game_data_path)
                .context(
                    "GameData.db was not found; launch The Bazaar once or pass --game-data PATH",
                )?,
        ),
    };

    match provider {
        ProviderSelection::GameData(database_path) => {
            let gateway = Arc::new(GameDataGateway::new(GameDataGatewayConfig {
                database_path: database_path.clone(),
                catalog_cache_dir,
                cache,
            })?);
            tracing::debug!(path = %gateway.database_path().display(), "using local GameData.db provider");
            Ok(SelectedServices {
                cards: BazaarService::new(gateway.clone()),
                catalog: Some(CatalogService::new(gateway)),
                source: "local/GameData.db",
                database_path,
            })
        }
    }
}

async fn execute_profile(services: &SelectedServices, args: ProfileArgs) -> Result<()> {
    let catalog = services
        .catalog
        .as_ref()
        .context("profile requires the game-data provider")?;
    let identity = catalog.status().await?.identity;
    let database_path = services.database_path.clone();
    let snapshot =
        tokio::task::spawn_blocking(move || load_profile_snapshot(&database_path, identity))
            .await
            .context("profile snapshot task failed")??;
    let supplement = args
        .supplement
        .as_deref()
        .map(load_supplement)
        .transpose()?;
    let runs = args.runs.as_deref().map(load_run_export).transpose()?;
    let profile = generate_profile(
        snapshot,
        &ProfileRequest {
            hero: args.hero,
            season_label: args.season_label,
        },
        supplement,
        runs.as_deref(),
    )?;
    if let Some(directory) = args.dcc_knowledge_dir.as_deref() {
        let path = write_dcc_knowledge(&profile, directory)?;
        tracing::info!(path = %path.display(), "wrote dcc-cua knowledge playbook");
    }
    match args.format {
        ProfileFormat::Json => println!("{}", serde_json::to_string_pretty(&profile)?),
        ProfileFormat::Markdown => print!("{}", render_markdown(&profile)),
    }
    Ok(())
}

enum ProviderSelection {
    GameData(PathBuf),
}

async fn execute_cache(
    cache: CacheStore,
    catalog_cache_dir: &std::path::Path,
    args: CacheArgs,
    output: OutputFormat,
) -> Result<()> {
    let data = match args.command {
        CacheCommand::Status => serde_json::json!({
            "responses": cache.status().await?,
            "catalog": catalog_cache_status(catalog_cache_dir)?,
        }),
        CacheCommand::Prune => {
            let responses = cache.prune(now_epoch_seconds()?).await?;
            let catalog = prune_catalog_cache(catalog_cache_dir)?;
            serde_json::json!({"responses": {"removed": responses}, "catalog": catalog})
        }
        CacheCommand::Clear { yes } => {
            if !yes {
                bail!("cache clear requires --yes");
            }
            let responses = cache.clear().await?;
            let catalog = clear_catalog_cache(catalog_cache_dir)?;
            serde_json::json!({"responses": {"removed": responses}, "catalog": catalog})
        }
    };
    print_value(
        &Envelope {
            schema_version: OUTPUT_SCHEMA_VERSION,
            command: "cache",
            source: "local",
            cache: None,
            data,
        },
        output,
    )
}

fn execute_update(args: UpdateArgs) -> Result<()> {
    let updater = GithubUpdater::new(
        "loonghao",
        "bazaardb-cli",
        "bazaardb-cli",
        env!("CARGO_PKG_VERSION"),
    );
    let check = updater.check()?;
    if args.check {
        println!("{}", serde_json::to_string_pretty(&check)?);
        return Ok(());
    }
    if !check.update_available {
        println!("{}", serde_json::to_string_pretty(&updater.install()?)?);
        return Ok(());
    }
    if check.asset.is_none() || !check.checksum_available {
        bail!("latest release is missing the target archive or SHA256SUMS");
    }
    if !args.yes {
        print!(
            "Update bazaardb-cli from {} to {}? [Y/n] ",
            check.current_version, check.latest_version
        );
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "" | "y" | "yes"
        ) {
            println!("Update cancelled.");
            return Ok(());
        }
    }
    println!("{}", serde_json::to_string_pretty(&updater.install()?)?);
    Ok(())
}

fn cache_path(override_dir: Option<&std::path::Path>) -> Result<PathBuf> {
    let directory = if let Some(path) = override_dir {
        path.to_path_buf()
    } else {
        ProjectDirs::from("gg", "loonghao", "bazaardb-cli")
            .context("failed to resolve a platform cache directory")?
            .cache_dir()
            .to_path_buf()
    };
    Ok(directory.join("responses.redb"))
}

fn print_value<T: Serialize>(value: &T, output: OutputFormat) -> Result<()> {
    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(value)?),
        OutputFormat::Jsonl => println!("{}", serde_json::to_string(value)?),
        OutputFormat::Table => println!("{}", serde_json::to_string_pretty(value)?),
    }
    Ok(())
}

fn print_cards(cards: &[Value], output: OutputFormat) -> Result<()> {
    match output {
        OutputFormat::Jsonl => {
            for card in cards {
                println!("{}", serde_json::to_string(card)?);
            }
        }
        OutputFormat::Table => {
            println!("NAME\tTYPE\tSIZE\tTIER");
            for card in cards {
                println!(
                    "{}\t{}\t{}\t{}",
                    field(card, &["name", "Name", "title"]),
                    field(card, &["type", "Type", "cardType"]),
                    field(card, &["size", "Size"]),
                    field(
                        card,
                        &[
                            "base_tier",
                            "baseTier",
                            "BaseTier",
                            "StartingTier",
                            "startingTier",
                        ],
                    ),
                );
            }
        }
        OutputFormat::Json => unreachable!(),
    }
    Ok(())
}

fn print_ten_wins(result: &TenWinResult, output: OutputFormat) -> Result<()> {
    match output {
        OutputFormat::Json => print_value(
            &Envelope {
                schema_version: OUTPUT_SCHEMA_VERSION,
                command: "ten-wins",
                source: "local-run-export",
                cache: None,
                data: result,
            },
            output,
        ),
        OutputFormat::Jsonl => {
            for combination in &result.combinations {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "schema_version": OUTPUT_SCHEMA_VERSION,
                        "command": "ten-wins",
                        "source": "local-run-export",
                        "matchedRuns": result.matched_runs,
                        "combinationSize": result.combination_size,
                        "combination": combination,
                    }))?
                );
            }
            Ok(())
        }
        OutputFormat::Table => {
            println!("RUNS\tSUPPORT\tCARDS");
            for combination in &result.combinations {
                println!(
                    "{}\t{:.4}\t{}",
                    combination.runs,
                    combination.support,
                    combination.cards.join(" + ")
                );
            }
            Ok(())
        }
    }
}

fn print_resolve(response: &ResolveBatchResponse, output: OutputFormat) -> Result<()> {
    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(response)?),
        OutputFormat::Jsonl => {
            for result in &response.results {
                println!(
                    "{}",
                    serde_json::to_string(&ResolveJsonlRecord {
                        identity: &response.identity,
                        result,
                        authority: bazaardb_cli::INSPECTION_AUTHORITY,
                        authorizes_action: false,
                    })?
                );
            }
        }
        OutputFormat::Table => {
            println!("TEMPLATE_ID\tTIER\tFOUND\tCOMPLETE\tNAME");
            for result in &response.results {
                let name = result
                    .template
                    .as_ref()
                    .and_then(|template| template.name.clone())
                    .unwrap_or_else(|| "-".to_owned());
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    result.template_id, result.tier, result.found, result.complete, name
                );
            }
        }
    }
    Ok(())
}

fn field(value: &Value, names: &[&str]) -> String {
    let direct = names
        .iter()
        .find_map(|name| value.get(name))
        .and_then(Value::as_str)
        .map(str::to_owned);
    direct
        .or_else(|| {
            names.contains(&"name").then(|| {
                value
                    .pointer("/Localization/Title/Text")
                    .and_then(Value::as_str)
                    .unwrap_or("-")
                    .to_owned()
            })
        })
        .unwrap_or_else(|| "-".to_owned())
        .replace(['\t', '\n', '\r'], " ")
}

fn now_epoch_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}
