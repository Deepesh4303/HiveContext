use crate::chunker::{Chunk, Chunker, ContextManifest};
use crate::storage::StorageEngine;
use anyhow::{bail, Context, Result};
use dashmap::DashMap;
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

pub const DEFAULT_UDP_BEACON_PORT: u16 = 43888;
const BEACON_INTERVAL_SECS: u64 = 4;
const PEER_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerBeacon {
    pub peer_id: String,
    pub http_port: u16,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub peer_id: String,
    pub addr: SocketAddr, // IP + http_port
    pub last_seen: Instant,
}

pub struct SwarmNode {
    pub peer_id: String,
    pub http_port: u16,
    pub storage: Arc<StorageEngine>,
    pub peers: DashMap<String, DiscoveredPeer>,
    client: reqwest::Client,
}

impl SwarmNode {
    pub fn new(peer_id: String, http_port: u16, storage: Arc<StorageEngine>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();

        Self {
            peer_id,
            http_port,
            storage,
            peers: DashMap::new(),
            client,
        }
    }

    /// Manually register a peer (e.g. for localhost testing or fixed IP setups)
    pub fn add_manual_peer(&self, addr: SocketAddr) {
        let key = addr.to_string();
        self.peers.insert(
            key.clone(),
            DiscoveredPeer {
                peer_id: format!("manual-{}", key),
                addr,
                last_seen: Instant::now(),
            },
        );
        info!("Added manual peer: {}", addr);
    }

    /// Returns a list of currently active peers that haven't timed out
    pub fn active_peers(&self) -> Vec<DiscoveredPeer> {
        let now = Instant::now();
        let timeout = Duration::from_secs(PEER_TIMEOUT_SECS);

        self.peers
            .iter()
            .filter(|r| now.duration_since(r.value().last_seen) < timeout)
            .map(|r| r.value().clone())
            .collect()
    }

    /// Background task: Listens for incoming UDP beacons on LAN
    pub async fn start_udp_listener(self: Arc<Self>, udp_port: u16) -> Result<()> {
        let bind_addr = format!("0.0.0.0:{}", udp_port);
        let socket = UdpSocket::bind(&bind_addr)
            .await
            .with_context(|| format!("Failed to bind UDP listener on {}", bind_addr))?;

        socket.set_broadcast(true)?;
        info!("UDP discovery listener running on {}", bind_addr);

        let mut buf = [0u8; 1024];
        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, src)) => {
                    if let Ok(beacon) = serde_json::from_slice::<PeerBeacon>(&buf[..len]) {
                        if beacon.peer_id != self.peer_id {
                            let mut peer_addr = src;
                            peer_addr.set_port(beacon.http_port);

                            self.peers.insert(
                                beacon.peer_id.clone(),
                                DiscoveredPeer {
                                    peer_id: beacon.peer_id,
                                    addr: peer_addr,
                                    last_seen: Instant::now(),
                                },
                            );
                            debug!("Updated peer {} at {}", src, peer_addr);
                        }
                    }
                }
                Err(e) => {
                    warn!("UDP discovery receive error: {}", e);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    /// Background task: Periodically broadcasts UDP beacon on LAN
    pub async fn start_udp_broadcaster(self: Arc<Self>, udp_port: u16) -> Result<()> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.set_broadcast(true)?;
        let target_addr = format!("255.255.255.255:{}", udp_port);

        info!("UDP beacon broadcaster active targeting {}", target_addr);

        loop {
            let beacon = PeerBeacon {
                peer_id: self.peer_id.clone(),
                http_port: self.http_port,
                timestamp: chrono::Utc::now().timestamp(),
            };

            if let Ok(payload) = serde_json::to_vec(&beacon) {
                let _ = socket.send_to(&payload, &target_addr).await;
            }

            tokio::time::sleep(Duration::from_secs(BEACON_INTERVAL_SECS)).await;
        }
    }

    /// Queries all active peers to search for matching context manifests
    pub async fn query_swarm_manifests(
        &self,
        query: &str,
        query_vec: Option<&[f32]>,
    ) -> Vec<ContextManifest> {
        let peers = self.active_peers();
        let mut tasks = Vec::new();

        for peer in peers {
            let client = self.client.clone();
            let url = format!("http://{}/p2p/manifests?q={}", peer.addr, query);
            tasks.push(async move {
                if let Ok(res) = client.get(&url).send().await {
                    if let Ok(manifests) = res.json::<Vec<ContextManifest>>().await {
                        return manifests;
                    }
                }
                Vec::new()
            });
        }

        let results = join_all(tasks).await;
        let mut aggregated = self.storage.search_manifests(query, None, query_vec, 0.3);
        for list in results {
            for m in list {
                if !aggregated.iter().any(|existing| existing.cid == m.cid) {
                    aggregated.push(m);
                }
            }
        }

        // Re-rank combined results with HybridRanker if query is provided
        if !query.trim().is_empty() || query_vec.is_some() {
            let ranker = crate::similarity::HybridRanker::new(0.3);
            aggregated.sort_by(|a, b| {
                let text_a = format!("{} {} {}", a.title, a.summary, a.tags.join(" "));
                let text_b = format!("{} {} {}", b.title, b.summary, b.tags.join(" "));
                let kw_a = crate::similarity::HybridRanker::keyword_score(query, &text_a);
                let kw_b = crate::similarity::HybridRanker::keyword_score(query, &text_b);
                let (score_a, _) = ranker.combine_score(kw_a, query_vec, a.embedding.as_deref());
                let (score_b, _) = ranker.combine_score(kw_b, query_vec, b.embedding.as_deref());
                score_b.partial_cmp(&score_a).unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        aggregated
    }

    /// Fetches a single chunk from a specific peer
    pub async fn fetch_chunk_from_peer(&self, peer_addr: SocketAddr, hash: &str) -> Option<Chunk> {
        let url = format!("http://{}/p2p/chunk/{}", peer_addr, hash);
        if let Ok(res) = self.client.get(&url).send().await {
            if res.status().is_success() {
                if let Ok(bytes) = res.bytes().await {
                    let received_data = bytes.to_vec();
                    let computed_hash = Chunker::hash_bytes(&received_data);
                    if computed_hash == hash {
                        return Some(Chunk {
                            hash: hash.to_string(),
                            size: received_data.len(),
                            data: received_data,
                        });
                    } else {
                        warn!("Received corrupted chunk {} from {}", hash, peer_addr);
                    }
                }
            }
        }
        None
    }

    /// BitTorrent-style parallel swarming: downloads missing chunks across all available peers
    pub async fn swarm_fetch_document(&self, manifest: &ContextManifest) -> Result<Vec<u8>> {
        let mut missing_chunks: Vec<String> = Vec::new();
        let mut assembled_chunks: Vec<Option<Chunk>> = vec![None; manifest.chunk_hashes.len()];

        // 1. Identify which chunks are already in local storage
        for (i, hash) in manifest.chunk_hashes.iter().enumerate() {
            if let Ok(Some(data)) = self.storage.get_chunk(hash) {
                assembled_chunks[i] = Some(Chunk {
                    hash: hash.clone(),
                    size: data.len(),
                    data,
                });
            } else {
                missing_chunks.push(hash.clone());
            }
        }

        info!(
            "Fetching document {} ({} chunks total, {} missing)",
            manifest.cid,
            manifest.chunk_hashes.len(),
            missing_chunks.len()
        );

        if !missing_chunks.is_empty() {
            let active_peers = self.active_peers();
            if active_peers.is_empty() {
                bail!("Cannot fetch missing chunks: no active swarm peers available");
            }

            // Distribute requests across peers (round-robin / parallel swarm)
            let mut download_tasks = Vec::new();
            for (idx, hash) in missing_chunks.into_iter().enumerate() {
                let peer = active_peers[idx % active_peers.len()].clone();
                let this = self;
                download_tasks.push(async move {
                    (hash.clone(), this.fetch_chunk_from_peer(peer.addr, &hash).await)
                });
            }

            let results = join_all(download_tasks).await;
            for (hash, maybe_chunk) in results {
                if let Some(chunk) = maybe_chunk {
                    // Save to local storage for future seeding
                    let _ = self.storage.save_chunk(&chunk);

                    // Place in correct index in assembled_chunks
                    if let Some(pos) = manifest.chunk_hashes.iter().position(|h| h == &hash) {
                        assembled_chunks[pos] = Some(chunk);
                    }
                } else {
                    bail!("Failed to download required chunk {} from swarm", hash);
                }
            }
        }

        // 2. Also save the manifest locally so this node can seed it
        let _ = self.storage.save_manifest(manifest);

        // 3. Assemble and verify all chunks
        let complete_chunks: Vec<Chunk> = assembled_chunks
            .into_iter()
            .collect::<Option<Vec<Chunk>>>()
            .context("Incomplete chunks after swarm download")?;

        let reconstructed = Chunker::assemble_chunks(manifest, &complete_chunks)?;
        info!(
            "Successfully assembled document {} ({} bytes)",
            manifest.cid,
            reconstructed.len()
        );
        Ok(reconstructed)
    }

    /// Announces a new manifest to all active peers immediately
    pub async fn announce_manifest_to_peers(&self, manifest: &ContextManifest) {
        let peers = self.active_peers();
        for peer in peers {
            let client = self.client.clone();
            let url = format!("http://{}/p2p/announce", peer.addr);
            let m = manifest.clone();
            tokio::spawn(async move {
                let _ = client.post(&url).json(&m).send().await;
            });
        }
    }
}
