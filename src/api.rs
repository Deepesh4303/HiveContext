use crate::chunker::{Chunker, ContextManifest};
use crate::embedder::EmbedderClient;
use crate::p2p::SwarmNode;
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub swarm: Arc<SwarmNode>,
    pub upstream_llm_url: Option<String>,
    pub embedder: Arc<EmbedderClient>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct StoreContextRequest {
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub summary: String,
    pub content: String,
    #[serde(default)]
    pub chunk_size: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoreContextResponse {
    pub cid: String,
    pub chunks_count: usize,
    pub total_size: usize,
    pub title: String,
    pub has_embedding: bool,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub tag: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeerInfoResponse {
    pub peer_id: String,
    pub addr: String,
    pub last_seen_secs_ago: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatCompletionRequest {
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub context_query: Option<String>,
    pub stream: Option<bool>,
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        // --- P2P Swarm Inter-Node Endpoints ---
        .route("/p2p/ping", get(p2p_ping))
        .route("/p2p/manifests", get(p2p_manifests))
        .route("/p2p/manifest/:cid", get(p2p_get_manifest))
        .route("/p2p/chunk/:hash", get(p2p_get_chunk))
        .route("/p2p/announce", post(p2p_announce_manifest))
        // --- Local Client / AI Inference Endpoints ---
        .route("/v1/context/store", post(store_context))
        .route("/v1/context/search", get(search_context))
        .route("/v1/context/fetch/:cid", get(fetch_context))
        .route("/v1/swarm/peers", get(list_peers))
        .route("/v1/swarm/stats", get(get_stats))
        .route("/v1/chat/completions", post(chat_completions_proxy))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

// ==================== P2P Handlers ====================

async fn p2p_ping(State(state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "peer_id": state.swarm.peer_id,
        "status": "online"
    }))
}

async fn p2p_manifests(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> impl IntoResponse {
    let query_vec = if !query.q.trim().is_empty() {
        state.embedder.embed(&query.q).await
    } else {
        None
    };

    let manifests = state
        .swarm
        .storage
        .search_manifests(&query.q, query.tag.as_deref(), query_vec.as_deref(), 0.3);
    Json(manifests)
}

async fn p2p_get_manifest(
    State(state): State<AppState>,
    Path(cid): Path<String>,
) -> Result<Json<ContextManifest>, StatusCode> {
    state
        .swarm
        .storage
        .get_manifest(&cid)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn p2p_get_chunk(
    State(state): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Response, StatusCode> {
    match state.swarm.storage.get_chunk(&hash) {
        Ok(Some(data)) => Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/octet-stream")],
            data,
        )
            .into_response()),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn p2p_announce_manifest(
    State(state): State<AppState>,
    Json(manifest): Json<ContextManifest>,
) -> StatusCode {
    let _ = state.swarm.storage.save_manifest(&manifest);
    StatusCode::ACCEPTED
}

// ==================== Client / AI Inference Handlers ====================

async fn store_context(
    State(state): State<AppState>,
    Json(payload): Json<StoreContextRequest>,
) -> Result<Json<StoreContextResponse>, (StatusCode, String)> {
    let chunk_size = payload.chunk_size.unwrap_or(0);

    // Compute semantic embedding for the title + summary + tags
    let embed_text = format!("{} {} {}", payload.title, payload.summary, payload.tags.join(" "));
    let embedding = state.embedder.embed(&embed_text).await;
    let has_embedding = embedding.is_some();

    let (manifest, chunks) = Chunker::create_manifest(
        &payload.title,
        payload.tags,
        &payload.summary,
        payload.content.as_bytes(),
        chunk_size,
        embedding,
    );

    // Save chunks to local storage
    for chunk in &chunks {
        state
            .swarm
            .storage
            .save_chunk(chunk)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    // Save manifest locally
    state
        .swarm
        .storage
        .save_manifest(&manifest)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Announce to all active swarm peers
    state.swarm.announce_manifest_to_peers(&manifest).await;

    info!(
        "Stored context '{}' (CID: {}) with {} chunks (embedded: {})",
        manifest.title,
        manifest.cid,
        chunks.len(),
        has_embedding
    );

    Ok(Json(StoreContextResponse {
        cid: manifest.cid,
        chunks_count: chunks.len(),
        total_size: manifest.total_size,
        title: manifest.title,
        has_embedding,
    }))
}

async fn search_context(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> impl IntoResponse {
    let query_vec = if !query.q.trim().is_empty() {
        state.embedder.embed(&query.q).await
    } else {
        None
    };

    let results = state
        .swarm
        .query_swarm_manifests(&query.q, query_vec.as_deref())
        .await;
    Json(results)
}

async fn fetch_context(
    State(state): State<AppState>,
    Path(cid): Path<String>,
) -> Result<Response, (StatusCode, String)> {
    // 1. Check if manifest is known
    let manifest = match state.swarm.storage.get_manifest(&cid) {
        Some(m) => m,
        None => {
            // Ask swarm for manifest
            let found = state.swarm.query_swarm_manifests("", None).await;
            match found.into_iter().find(|m| m.cid == cid) {
                Some(m) => m,
                None => return Err((StatusCode::NOT_FOUND, "Context CID not found in swarm".into())),
            }
        }
    };

    // 2. Swarm fetch all chunks and reconstruct markdown
    match state.swarm.swarm_fetch_document(&manifest).await {
        Ok(bytes) => {
            let markdown = String::from_utf8_lossy(&bytes).to_string();
            Ok((
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
                markdown,
            )
                .into_response())
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Swarm fetch failed: {}", e),
        )),
    }
}

async fn list_peers(State(state): State<AppState>) -> impl IntoResponse {
    let peers: Vec<PeerInfoResponse> = state
        .swarm
        .active_peers()
        .into_iter()
        .map(|p| PeerInfoResponse {
            peer_id: p.peer_id,
            addr: p.addr.to_string(),
            last_seen_secs_ago: p.last_seen.elapsed().as_secs(),
        })
        .collect();

    Json(peers)
}

async fn get_stats(State(state): State<AppState>) -> impl IntoResponse {
    let stats = state.swarm.storage.stats();
    Json(stats)
}

/// AI Inference Proxy: intercepts chat prompts, injects matching hive context, and forwards to LLM
async fn chat_completions_proxy(
    State(state): State<AppState>,
    Json(mut req): Json<ChatCompletionRequest>,
) -> Result<Response, (StatusCode, String)> {
    // 1. Determine search query from request
    let query_term = req.context_query.clone().unwrap_or_else(|| {
        req.messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default()
    });

    // 2. Compute query embedding for semantic search
    let query_vec = if !query_term.trim().is_empty() {
        state.embedder.embed(&query_term).await
    } else {
        None
    };

    // 3. Check hive swarm for matching context with Hybrid Ranking (BM25 + Cosine)
    let matches = state
        .swarm
        .query_swarm_manifests(&query_term, query_vec.as_deref())
        .await;

    let mut injected_context = String::new();
    if let Some(best_match) = matches.first() {
        if let Ok(bytes) = state.swarm.swarm_fetch_document(best_match).await {
            let context_text = String::from_utf8_lossy(&bytes);
            injected_context = format!(
                "\n\n--- [HIVE CACHE CONTEXT: {}] ---\n{}\n--- [END CONTEXT] ---\n",
                best_match.title, context_text
            );
        }
    }

    // 4. Inject context into system message
    if !injected_context.is_empty() {
        if let Some(system_msg) = req.messages.iter_mut().find(|m| m.role == "system") {
            system_msg.content.push_str(&injected_context);
        } else {
            req.messages.insert(
                0,
                ChatMessage {
                    role: "system".into(),
                    content: format!("You are an AI assistant with access to relevant past team context:{}", injected_context),
                },
            );
        }
    }

    // 5. If an upstream LLM (like Ollama or OpenAI) is configured, forward the request
    if let Some(upstream) = &state.upstream_llm_url {
        let client = reqwest::Client::new();
        let target_url = format!("{}/v1/chat/completions", upstream.trim_end_matches('/'));

        match client.post(&target_url).json(&req).send().await {
            Ok(upstream_res) => {
                let status = StatusCode::from_u16(upstream_res.status().as_u16())
                    .unwrap_or(StatusCode::OK);
                let body = upstream_res
                    .text()
                    .await
                    .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
                return Ok((
                    status,
                    [(header::CONTENT_TYPE, "application/json")],
                    body,
                )
                    .into_response());
            }
            Err(e) => {
                return Err((
                    StatusCode::BAD_GATEWAY,
                    format!("Failed connecting to upstream LLM: {}", e),
                ));
            }
        }
    }

    // Otherwise, return the augmented prompt preview so the caller can inspect the injected context
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string_pretty(&serde_json::json!({
            "status": "augmented_without_upstream",
            "injected_context_found": !injected_context.is_empty(),
            "augmented_messages": req.messages
        }))
        .unwrap(),
    )
        .into_response())
}
