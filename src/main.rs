
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use tracing::{info, warn};

use klaar::cache;
use klaar::config;
use klaar::db;
use klaar::embedder;
use klaar::mcp;
use klaar::tools;
use klaar::utils;

use config::GlobalConfig;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
use embedder::Embedder;

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "klaar",
    version = env!("CARGO_PKG_VERSION"),
    about = "Universal AI coding agent optimizer — MCP server for Google Antigravity, Claude Code, and any MCP-compatible environment"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP server over stdio (for use in mcp_config.json)
    Serve,

    /// Install klaar: copy binary and patch AI config files
    Install {
        /// Initialise .klaar/targets.toml in the given project directory
        #[arg(long, value_name = "DIR")]
        project: Option<String>,
    },

    /// Run pre-push checks directly from the command line
    Check {
        /// Path to the project root (default: current directory)
        #[arg(long, default_value = ".")]
        path: String,

        /// Target name to check (e.g. "production")
        #[arg(long, short)]
        target: String,
    },
    
    /// View token savings and ROI metrics
    Stats {
        /// Optional: Timeframe filter (e.g. 'day', 'week', 'month')
        #[arg(long)]
        timeframe: Option<String>,
        
        /// Optional: Filter metrics to a specific project
        #[arg(long)]
        project: Option<String>,
    },

    /// Dump all stored memories
    Dump {
        /// Optional: Filter memories by project (e.g. full path)
        #[arg(long)]
        project: Option<String>,
    },

    /// Run token-optimized grep
    Grep {
        /// Search pattern
        query: String,

        /// Path to search (default: current directory)
        #[arg(long, short, default_value = ".")]
        path: String,

        /// Match case-insensitively
        #[arg(long, short, default_value_t = true)]
        case_insensitive: bool,

        /// Optional glob filter (e.g. "*.rs")
        #[arg(long, short)]
        glob: Option<String>,
    },

    /// Run noise-filtered directory listing
    Ls {
        #[arg(long)]
        path: String,
        #[arg(long, default_value = "2")]
        depth: usize,
    },
    FindSymbol {
        #[arg(long)]
        path: String,
        #[arg(long)]
        symbol: String,
    },
    Read {
        #[arg(long)]
        path: String,
        #[arg(long, default_value = "skeleton")]
        mode: String,
    },
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    // Logging must go to stderr — stdout is the MCP JSON-RPC channel
    let subscriber = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            std::env::var("KLAAR_LOG")
                .unwrap_or_else(|_| "klaar=info".to_string()),
        )
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve => cmd_serve().await?,
        Commands::Install { project } => cmd_install(project).await?,
        Commands::Check { path, target } => cmd_check(&path, &target).await?,
        Commands::Stats { timeframe, project } => cmd_stats(timeframe, project).await?,
        Commands::Dump { project } => cmd_dump(project).await?,
        Commands::Grep { query, path, case_insensitive, glob } => {
            let results = tools::search::search(&query, &path, case_insensitive, glob.as_deref())?;
            println!("{}", results);
        }
        Commands::Ls { path, depth } => {
            let results = tools::ls::list_dir(&path, Some(depth))?;
            println!("{}", results);
        }
        Commands::FindSymbol { path, symbol } => {
            let index = tools::index::build_index(&path);
            if let Some(locations) = index.symbols.get(&symbol) {
                println!("Found {} matches for `{}`:", locations.len(), symbol);
                for loc in locations {
                    println!("  {} → {}:L{}", loc.kind, loc.file, loc.line);
                }
            } else {
                println!("Symbol `{}` not found.", symbol);
            }
        }
        Commands::Read { path, mode } => {
            let config = GlobalConfig::load()?;
            let (mut content, saved_bytes, _) = tools::read::surgical_read(&path, &mode, &Arc::new(config))?;
            
            if mode == "skeleton" {
                let project_root = crate::utils::find_project_root(&path);
                tools::standards::inject_standards(&mut content, &path, project_root.as_deref());
            }

            println!("--- OUTPUT (Saved {} bytes) ---", saved_bytes);
            println!("{}", content);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// `klaar serve` — MCP stdio server loop
// ---------------------------------------------------------------------------

async fn cmd_serve() -> Result<()> {
    info!("klaar MCP server starting (stdio transport)");

    let global = GlobalConfig::load()?;
    let db_path = global.resolved_db_path();
    let db = db::init(&db_path).await?;
    let db = Arc::new(db);
    let config = Arc::new(global);
    
    // Initialize caching layer
    let cache = Arc::new(cache::SearchCache::new());
    let watcher = Arc::new(tokio::sync::Mutex::new(cache::watcher::FileWatcher::new(cache.clone())?));

    // Lazy-load the embedding model in the background
    let embedder = Arc::new(tokio::sync::RwLock::new(None));
    let embedder_clone = embedder.clone();
    tokio::spawn(async move {
        // CPU-bound model parsing happens blocking
        if let Some(e) = tokio::task::spawn_blocking(Embedder::try_init).await.unwrap_or(None) {
            *embedder_clone.write().await = Some(Arc::new(e));
        }
    });

    // R6-L2: Background purge of stale co_changes and ROI entries every 6 hours
    {
        let db_purge = db.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
            interval.tick().await; // skip first immediate tick
            loop {
                interval.tick().await;
                if let Err(e) = crate::db::purge_stale_entries(&db_purge).await {
                    warn!("Background purge failed: {}", e);
                }
            }
        });
    }

    let request_semaphore = Arc::new(tokio::sync::Semaphore::new(8));

    use tokio::io::AsyncBufReadExt;
    
    let stdin = tokio::io::stdin();
    let _stdout = std::io::stdout(); // std::io::stdout is okay for background blocking writes
    
    let mut reader = tokio::io::BufReader::new(stdin).lines();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(100);

    // Stdout serialization task
    tokio::spawn(async move {
        while let Some(serialized) = rx.recv().await {
            use std::io::Write;
            let mut stdout = std::io::stdout();
            if let Err(e) = writeln!(stdout, "{}", serialized) {
                tracing::error!("Fatal: Failed to write to stdout: {}. Shutting down.", e);
                std::process::exit(1);
            }
        }
    });

    while let Ok(Some(line)) = reader.next_line().await {
        if line.trim().is_empty() {
            continue;
        }

        let db_clone = db.clone();
        let config_clone = config.clone();
        let embedder_clone = embedder.clone();
        let tx_clone = tx.clone();
        
        // Output task spawned concurrently
        let cache_clone = cache.clone();
        let watcher_clone = watcher.clone();
        
        if let Ok(permit) = request_semaphore.clone().acquire_owned().await {
            tokio::spawn(async move {
                let _permit = permit;
                if let Some(response) = handle_message(&line, &db_clone, &config_clone, &embedder_clone, &cache_clone, &watcher_clone).await {
                    match serde_json::to_string(&response) {
                        Ok(s) => {
                            if let Err(e) = tx_clone.send(s).await {
                                tracing::error!("Fatal: failed to send response to stdout writer channel: {}. Shutting down.", e);
                                std::process::exit(1);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to serialize response: {}", e);
                        }
                    }
                }
            });
        }
    }

    tracing::info!("klaar MCP server shutting down");
    Ok(())
}

/// Handle one JSON-RPC message.
/// Returns `Some(response)` for requests, `None` for notification messages
/// (which require no response per the JSON-RPC 2.0 spec).
async fn handle_message(
    raw: &str,
    db: &Arc<db::KlaarDb>,
    config: &Arc<GlobalConfig>,
    embedder: &Arc<tokio::sync::RwLock<Option<Arc<Embedder>>>>,
    cache: &Arc<cache::SearchCache>,
    watcher: &Arc<tokio::sync::Mutex<cache::watcher::FileWatcher>>,
) -> Option<Value> {
    let req: mcp::RpcRequest = match serde_json::from_str(raw) {
        Ok(r) => r,
        Err(e) => {
            warn!("Malformed JSON-RPC input: {}", e);
            let v = serde_json::to_value(mcp::RpcResponse::err(
                None,
                -32700,
                "Parse error",
                Some(e.to_string()),
            ))
            .unwrap_or(json!({"jsonrpc":"2.0","error":{"code":-32700,"message":"Parse error"}}));
            return Some(v);
        }
    };

    info!("→ method: {}", req.method);

    // F5: JSON-RPC 2.0 notifications have no id and must not receive a response
    let id = match req.id.clone() {
        Some(val) => Some(val),
        None => return None,
    };

    let response = match req.method.as_str() {
        "initialize" => handle_initialize(id),
        "tools/list" => handle_tools_list(id),
        "tools/call" => handle_tools_call(id, &req.params, db, config, embedder, cache, watcher).await,
        "ping" => mcp::RpcResponse::ok(id, json!({})),
        other => {
            warn!("Unknown method: {}", other);
            mcp::RpcResponse::method_not_found(id, other)
        }
    };

    Some(
        serde_json::to_value(response)
            .unwrap_or_else(|_| json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"Serialization error"}})),
    )
}

fn handle_initialize(id: Option<Value>) -> mcp::RpcResponse {
    mcp::RpcResponse::ok(
        id,
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "klaar",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

fn handle_tools_list(id: Option<Value>) -> mcp::RpcResponse {
    mcp::RpcResponse::ok(id, mcp::tools::tool_list())
}

async fn handle_tools_call(
    id: Option<Value>,
    params: &Value,
    db: &Arc<db::KlaarDb>,
    config: &Arc<GlobalConfig>,
    embedder: &Arc<tokio::sync::RwLock<Option<Arc<Embedder>>>>,
    cache: &Arc<cache::SearchCache>,
    watcher: &Arc<tokio::sync::Mutex<cache::watcher::FileWatcher>>,
) -> mcp::RpcResponse {
    let name = match params["name"].as_str() {
        Some(n) => n,
        None => {
            return mcp::RpcResponse::invalid_params(id, "Missing 'name' in tools/call params")
        }
    };

    let args = &params["arguments"];
    
    // Auto-register project for file watching on access, but only if the path is within allowed_roots
    if let Some(path_str) = args["project_path"].as_str().or_else(|| args["file_path"].as_str()) {
        let path = std::path::Path::new(path_str);
        if crate::config::check_path_allowed(path, &config.allowed_roots, "auto_watch", "allowed_roots", config.deny_when_unconfigured).is_ok() {
            if let Some(root) = crate::utils::find_project_root(path_str) {
                let mut w = watcher.lock().await;
                if let Err(e) = w.watch(&root) {
                    tracing::error!("Failed to register directory watcher for root '{}': {}", root, e);
                }
            }
        }
    }

    let result = mcp::tools::dispatch(name, args, db, config, embedder, cache).await;
    mcp::RpcResponse::ok(id, result)
}

// Moved to utils.rs

// ---------------------------------------------------------------------------
// `klaar install`
// ---------------------------------------------------------------------------

async fn cmd_install(project: Option<String>) -> Result<()> {
    if let Some(dir) = project {
        return init_project_config(&dir);
    }

    println!("klaar installer");
    println!("───────────────");

    // Detect and patch Google Antigravity config
    patch_antigravity_config();

    // Detect and patch Claude Code config
    patch_claude_config();

    println!();
    println!("Done. Restart your AI environment to pick up the new MCP server.");
    println!("Tips:");
    println!("  • Run `klaar check --target production` to test pre-push checks");
    println!("  • Run `klaar install --project .` to init .klaar/targets.toml in a project");

    Ok(())
}

fn current_binary_path() -> String {
    std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("/usr/local/bin/klaar"))
        .to_string_lossy()
        .to_string()
}

fn mcp_entry() -> Value {
    json!({
        "command": current_binary_path(),
        "args": ["serve"],
        "env": {}
    })
}

fn patch_antigravity_config() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            println!("⚠️  Google Antigravity: Could not determine home directory");
            return;
        }
    };
    let candidates = [
        home.join(".gemini").join("antigravity").join("mcp_config.json"), // real AG location
        home.join(".gemini").join("settings.json"),
        home.join(".config").join("antigravity").join("settings.json"),
    ];

    for path in &candidates {
        if path.exists() {
            match patch_mcp_config(path, "klaar") {
                Ok(_) => {
                    println!("✅ Google Antigravity: patched {}", path.display());
                    return;
                }
                Err(e) => {
                    println!("⚠️  Google Antigravity: failed to patch {} — {}", path.display(), e);
                }
            }
        }
    }
    println!("ℹ️  Google Antigravity config not found (checked ~/.gemini/settings.json and ~/.config/antigravity/settings.json)");
}

fn patch_claude_config() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            println!("⚠️  Claude Desktop: Could not determine home directory");
            return;
        }
    };
    let candidates = [
        home.join(".config").join("claude").join("claude_desktop_config.json"),
        home.join("Library")
            .join("Application Support")
            .join("Claude")
            .join("claude_desktop_config.json"),
    ];

    for path in &candidates {
        if path.exists() {
            match patch_mcp_config(path, "klaar") {
                Ok(_) => {
                    println!("✅ Claude Code: patched {}", path.display());
                    return;
                }
                Err(e) => println!("⚠️  Claude Code: failed to patch {} — {}", path.display(), e),
            }
        }
    }
    println!("ℹ️  Claude Code config not found");
    println!("   (checked ~/.config/claude/ and ~/Library/Application Support/Claude/)");
}

fn patch_mcp_config(config_path: &std::path::Path, server_name: &str) -> Result<()> {
    let raw = std::fs::read_to_string(config_path)?;
    let mut cfg: Value = serde_json::from_str(&raw)?;

    // Ensure mcpServers key exists
    if cfg.get("mcpServers").is_none() {
        cfg["mcpServers"] = json!({});
    }

    cfg["mcpServers"][server_name] = mcp_entry();

    let updated = serde_json::to_string_pretty(&cfg)?;

    // 1. Create a backup of the original config file
    let mut backup_path = config_path.to_path_buf();
    backup_path.set_extension("json.bak");
    if let Err(e) = std::fs::copy(config_path, &backup_path) {
        tracing::warn!("Failed to create config backup at {}: {}", backup_path.display(), e);
    }

    // 2. Write to a temporary file in the same directory to guarantee atomic rename
    let temp_path = config_path.with_extension("json.tmp");
    std::fs::write(&temp_path, updated)?;

    // 3. Atomically rename the temp file to the target path
    if let Err(e) = std::fs::rename(&temp_path, config_path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(e.into());
    }

    Ok(())
}

fn init_project_config(dir: &str) -> Result<()> {
    let root = std::path::Path::new(dir);
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let klaar_dir = root.join(".klaar");
    let config_path = klaar_dir.join("targets.toml");

    if config_path.exists() {
        println!("ℹ️  .klaar/targets.toml already exists. No changes made.");
        return Ok(());
    }

    std::fs::create_dir_all(&klaar_dir)?;

    // Detect language by checking for common project files
    let template = if root.join("Cargo.toml").exists() {
        let name = canonical_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("my-project");
        config::rust_template(name)
    } else if root.join("package.json").exists() {
        let name = canonical_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("my-project");
        config::typescript_template(name)
    } else {
        let name = canonical_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("my-project");
        config::mixed_template(name)
    };

    std::fs::write(&config_path, template)?;
    println!("✅ Created {}", config_path.display());
    println!("   Edit it to configure your pre-push checks and deploy commands.");

    Ok(())
}

// ---------------------------------------------------------------------------
// `klaar check`
// ---------------------------------------------------------------------------

async fn cmd_check(path: &str, target: &str) -> Result<()> {
    let config = Arc::new(GlobalConfig::load()?);
    let result = tools::gatekeeper::pre_push_check(path, target, &config).await?;

    if result.passed {
        println!("✅ All checks passed for target '{}'", result.target);
    } else {
        println!("❌ Checks FAILED for target '{}'", result.target);
    }

    for check in &result.checks {
        let icon = if check.passed { "  ✅" } else { "  ❌" };
        println!("{} {}", icon, check.command);
        if !check.output.trim().is_empty() && !check.passed {
            println!("{}", check.output.trim());
        }
    }

    if !result.passed {
        std::process::exit(1);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Native Stats/ROI Display
// ---------------------------------------------------------------------------

async fn cmd_stats(timeframe: Option<String>, project: Option<String>) -> Result<()> {
    let global = config::GlobalConfig::load()?;
    let db_path = global.resolved_db_path();
    let db = db::init(&db_path).await?;
    
    let temporal_constraint = match timeframe.as_deref() {
        Some("day") => Some(chrono::Utc::now() - chrono::Duration::days(1)),
        Some("week") => Some(chrono::Utc::now() - chrono::Duration::days(7)),
        Some("month") => Some(chrono::Utc::now() - chrono::Duration::days(28)),
        Some(other) => {
            eprintln!("Unknown timeframe: {}. Use 'day', 'week', or 'month'.", other);
            return Ok(());
        }
        None => None, // all time
    };

    println!("Fetching token optimization statistics...\n");

    let stats = db::get_stats(&db, temporal_constraint, project.clone()).await?;

    if stats.is_empty() {
        println!("No token savings recorded for the specified filters.");
        return Ok(());
    }

    // Print Header
    println!("{:<25} | {:<25} | {:>15} | {:>15} | {:>8}", "AGENT", "PROJECT", "TOKENS SAVED", "TOTAL TOKENS", "% SAVED");
    println!("{:-<25}-+-{:-<25}-+-{:-<15}-+-{:-<15}-+-{:-<8}", "", "", "", "", "");

    let mut total_saved: usize = 0;
    let mut total_usage_sum: usize = 0;

    for row in stats {
        let agent = row.agent_id.unwrap_or_else(|| "Unknown".to_string());
        
        let pct = if row.total_usage > 0 {
            (row.total_saved as f64 / row.total_usage as f64) * 100.0
        } else {
            0.0
        };

        println!("{:<25} | {:<25} | {:>15} | {:>15} | {:>7.1}%", agent, row.project, row.total_saved, row.total_usage, pct);
        
        total_saved += row.total_saved;
        total_usage_sum += row.total_usage;
    }

    println!("{:-<25}-+-{:-<25}-+-{:-<15}-+-{:-<15}-+-{:-<8}", "", "", "", "", "");
    
    let total_pct = if total_usage_sum > 0 {
        (total_saved as f64 / total_usage_sum as f64) * 100.0
    } else {
        0.0
    };
    
    println!("{:<25} | {:<25} | {:>15} | {:>15} | {:>7.1}%", "TOTAL", "", total_saved, total_usage_sum, total_pct);

    Ok(())
}

// ---------------------------------------------------------------------------
// `klaar dump` — Dumps all memories
// ---------------------------------------------------------------------------

async fn cmd_dump(project_filter: Option<String>) -> Result<()> {
    let global = GlobalConfig::load()?;
    let db_path = global.resolved_db_path();
    let db = db::init(&db_path).await?;
    
    let records: Vec<db::MemoryRecord> = if let Some(proj) = project_filter {
        let mut res = db
            .query("SELECT * FROM memory WHERE project = $proj LIMIT 101")
            .bind(("proj", proj))
            .await?;
        res.take(0)?
    } else {
        let mut res = db.query("SELECT * FROM memory LIMIT 101").await?;
        res.take(0)?
    };

    for rec in records.iter().take(100) {
        println!("{:#?}", rec);
    }
    if records.len() > 100 {
        println!("\n[Notice] Capped output at 100 memories to prevent heap memory exhaustion. Use direct query queries or filter options to retrieve specific records.");
    }
    Ok(())
}
