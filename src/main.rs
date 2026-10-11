mod api;
mod chunker;
mod embedder;
mod p2p;
mod similarity;
mod storage;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use p2p::SwarmNode;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use storage::StorageEngine;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

#[derive(Parser)]
#[command(name = "hivecontext")]
#[command(about = "BitTorrent-inspired P2P prompt and context-caching hive with Semantic Vector Search")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the P2P context caching daemon
    Daemon {
        /// HTTP and P2P server port
        #[arg(short, long, default_value_t = 7071)]
        port: u16,

        /// UDP discovery broadcast port (0 to disable)
        #[arg(long, default_value_t = p2p::DEFAULT_UDP_BEACON_PORT)]
        udp_port: u16,

        /// Root data directory for chunks and manifests
        #[arg(short, long, default_value = "./hive_data")]
        data_dir: PathBuf,

        /// Maximum storage quota allocated for this peer in MB
        #[arg(short, long, default_value_t = 500)]
        quota_mb: u64,

        /// Initial manual peer addresses to connect to (e.g. 127.0.0.1:7072)
        #[arg(long)]
        peer: Vec<SocketAddr>,

        /// Upstream LLM endpoint to forward augmented chat completions to (e.g. http://localhost:11434)
        #[arg(long)]
        upstream_llm: Option<String>,

        /// Embedding service endpoint for semantic vector search (e.g. http://127.0.0.1:8000)
        #[arg(long, default_value = "http://127.0.0.1:8000")]
        embedding_url: Option<String>,
    },

    /// Store a markdown context file into the local hive and announce to swarm
    Store {
        /// Path to the markdown file
        #[arg(short, long)]
        file: PathBuf,

        /// Title for this context document
        #[arg(short, long)]
        title: String,

        /// Comma-separated tags (e.g. "rust,auth,redis")
        #[arg(long, default_value = "")]
        tags: String,

        /// Brief summary of what this context covers
        #[arg(short, long, default_value = "")]
        summary: String,

        /// Target local daemon port
        #[arg(short, long, default_value_t = 7071)]
        daemon_port: u16,
    },

    /// Search the hive swarm for relevant context manifests
    Search {
        /// Query string (matches title, summary, tags, or semantic meaning)
        query: String,

        /// Target local daemon port
        #[arg(short, long, default_value_t = 7071)]
        daemon_port: u16,
    },

    /// Fetch and assemble a markdown context from the swarm by CID
    Fetch {
        /// Root Content ID (CID) of the context
        cid: String,

        /// Output file path (prints to stdout if omitted)
        #[arg(short, long)]
        out: Option<PathBuf>,

        /// Target local daemon port
        #[arg(short, long, default_value_t = 7071)]
        daemon_port: u16,
    },

    /// List active discovered swarm peers on the network
    Peers {
        /// Target local daemon port
        #[arg(short, long, default_value_t = 7071)]
        daemon_port: u16,
    },

    /// Show local storage stats and quota usage
    Stats {
        /// Target local daemon port
        #[arg(short, long, default_value_t = 7071)]
        daemon_port: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("Setting default subscriber failed");

    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon {
            port,
            udp_port,
            data_dir,
            quota_mb,
            peer,
            upstream_llm,
            embedding_url,
        } => {
            run_daemon(port, udp_port, data_dir, quota_mb, peer, upstream_llm, embedding_url).await?;
        }

        Commands::Store {
            file,
            title,
            tags,
            summary,
            daemon_port,
        } => {
            run_store(file, title, tags, summary, daemon_port).await?;
        }

        Commands::Search { query, daemon_port } => {
            run_search(&query, daemon_port).await?;
        }

        Commands::Fetch {
            cid,
            out,
            daemon_port,
        } => {
            run_fetch(&cid, out, daemon_port).await?;
        }

        Commands::Peers { daemon_port } => {
            run_peers(daemon_port).await?;
        }

        Commands::Stats { daemon_port } => {
            run_stats(daemon_port).await?;
        }
    }

    Ok(())
}

async fn run_daemon(
    port: u16,
    udp_port: u16,
    data_dir: PathBuf,
    quota_mb: u64,
    peers: Vec<SocketAddr>,
    upstream_llm: Option<String>,
    embedding_url: Option<String>,
) -> Result<()> {
    let peer_id = format!("node-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let max_quota_bytes = quota_mb * 1024 * 1024;

    info!("Starting HiveContext Node: {}", peer_id);
    info!("Storage quota: {} MB | Data directory: {:?}", quota_mb, data_dir);

    let storage = Arc::new(StorageEngine::new(data_dir, max_quota_bytes)?);
    let swarm = Arc::new(SwarmNode::new(peer_id.clone(), port, storage.clone()));

    let embedder = Arc::new(embedder::EmbedderClient::new(embedding_url.clone()));
    if embedder.is_available().await {
        info!("Semantic Vector Engine active via embedding service: {:?}", embedding_url);
    } else {
        info!("Embedding service at {:?} not reachable; hybrid ranker will use keyword search with vector fallback", embedding_url);
    }

    // Add manual peers
    for p in peers {
        swarm.add_manual_peer(p);
    }

    // Start UDP discovery if enabled
    if udp_port > 0 {
        let swarm_listener = swarm.clone();
        tokio::spawn(async move {
            if let Err(e) = swarm_listener.start_udp_listener(udp_port).await {
                tracing::warn!("UDP listener failed: {}", e);
            }
        });

        let swarm_broadcaster = swarm.clone();
        tokio::spawn(async move {
            if let Err(e) = swarm_broadcaster.start_udp_broadcaster(udp_port).await {
                tracing::warn!("UDP broadcaster failed: {}", e);
            }
        });
    }

    let state = api::AppState {
        swarm,
        upstream_llm_url: upstream_llm,
        embedder,
    };

    let router = api::create_router(state);
    let bind_addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;

    info!("HiveContext daemon HTTP/P2P listening on http://{}", bind_addr);
    info!("API Endpoints:");
    info!("  - POST http://localhost:{}/v1/context/store", port);
    info!("  - GET  http://localhost:{}/v1/context/search?q=...", port);
    info!("  - GET  http://localhost:{}/v1/context/fetch/:cid", port);
    info!("  - GET  http://localhost:{}/v1/swarm/peers", port);
    info!("  - POST http://localhost:{}/v1/chat/completions (AI context proxy)", port);

    axum::serve(listener, router).await?;
    Ok(())
}

async fn run_store(
    file: PathBuf,
    title: String,
    tags: String,
    summary: String,
    daemon_port: u16,
) -> Result<()> {
    let content = fs::read_to_string(&file)
        .with_context(|| format!("Failed to read context file {:?}", file))?;

    let tag_list: Vec<String> = tags
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/v1/context/store", daemon_port);

    let payload = api::StoreContextRequest {
        title,
        tags: tag_list,
        summary,
        content,
        chunk_size: None,
    };

    let res = client.post(&url).json(&payload).send().await
        .with_context(|| format!("Failed to connect to HiveContext daemon at port {}", daemon_port))?;

    if res.status().is_success() {
        let resp: api::StoreContextResponse = res.json().await?;
        println!("Successfully stored and announced context to hive!");
        println!("  Title:         {}", resp.title);
        println!("  CID:           {}", resp.cid);
        println!("  Chunks:        {}", resp.chunks_count);
        println!("  Total Size:    {} bytes", resp.total_size);
        println!("  Vector Embed:  {}", if resp.has_embedding { "Generated (384-dim)" } else { "None" });
    } else {
        eprintln!("Error from daemon: {}", res.text().await?);
    }

    Ok(())
}

async fn run_search(query: &str, daemon_port: u16) -> Result<()> {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/v1/context/search?q={}", daemon_port, query);

    let res = client.get(&url).send().await
        .with_context(|| format!("Failed to connect to HiveContext daemon at port {}", daemon_port))?;

    if res.status().is_success() {
        let manifests: Vec<chunker::ContextManifest> = res.json().await?;
        if manifests.is_empty() {
            println!("No matching context found in swarm for query: '{}'", query);
        } else {
            println!("Found {} context(s) across the hive:\n", manifests.len());
            for m in manifests {
                println!("  CID:      {}", m.cid);
                println!("  Title:    {}", m.title);
                println!("  Tags:     [{}]", m.tags.join(", "));
                println!("  Summary:  {}", m.summary);
                println!("  Size:     {} bytes ({} chunks)", m.total_size, m.chunk_hashes.len());
                println!("  Vector:   {}", if m.embedding.is_some() { "Present (384-dim)" } else { "None" });
                println!("  ---");
            }
        }
    } else {
        eprintln!("Error: {}", res.text().await?);
    }

    Ok(())
}

async fn run_fetch(cid: &str, out: Option<PathBuf>, daemon_port: u16) -> Result<()> {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/v1/context/fetch/{}", daemon_port, cid);

    let res = client.get(&url).send().await
        .with_context(|| format!("Failed to connect to HiveContext daemon at port {}", daemon_port))?;

    if res.status().is_success() {
        let markdown = res.text().await?;
        if let Some(path) = out {
            fs::write(&path, &markdown)
                .with_context(|| format!("Failed to write output file {:?}", path))?;
            println!("Assembled context saved to {:?}", path);
        } else {
            println!("{}", markdown);
        }
    } else {
        eprintln!("Error fetching CID: {}", res.text().await?);
    }

    Ok(())
}

async fn run_peers(daemon_port: u16) -> Result<()> {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/v1/swarm/peers", daemon_port);

    let res = client.get(&url).send().await
        .with_context(|| format!("Failed to connect to HiveContext daemon at port {}", daemon_port))?;

    if res.status().is_success() {
        let peers: Vec<api::PeerInfoResponse> = res.json().await?;
        if peers.is_empty() {
            println!("No peers currently connected. (Ensure other nodes have UDP discovery or --peer set)");
        } else {
            println!("Active Swarm Peers ({}):\n", peers.len());
            for p in peers {
                println!("  Peer ID:   {}", p.peer_id);
                println!("  Address:   {}", p.addr);
                println!("  Last seen: {}s ago", p.last_seen_secs_ago);
                println!("  ---");
            }
        }
    } else {
        eprintln!("Error: {}", res.text().await?);
    }

    Ok(())
}

async fn run_stats(daemon_port: u16) -> Result<()> {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/v1/swarm/stats", daemon_port);

    let res = client.get(&url).send().await
        .with_context(|| format!("Failed to connect to HiveContext daemon at port {}", daemon_port))?;

    if res.status().is_success() {
        let stats: storage::StorageStats = res.json().await?;
        println!("Local Hive Node Stats:");
        println!("  Total Chunks:     {}", stats.total_chunks);
        println!("  Total Manifests:  {}", stats.total_manifests);
        println!("  Quota Used:       {:.2} KB / {:.2} MB", 
            stats.used_bytes as f64 / 1024.0, 
            stats.max_quota_bytes as f64 / (1024.0 * 1024.0)
        );
    } else {
        eprintln!("Error: {}", res.text().await?);
    }

    Ok(())
}
