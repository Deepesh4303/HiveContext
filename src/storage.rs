use crate::chunker::{Chunk, ContextManifest};
use crate::similarity::HybridRanker;
use anyhow::{Context, Result};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageStats {
    pub total_chunks: usize,
    pub total_manifests: usize,
    pub used_bytes: u64,
    pub max_quota_bytes: u64,
}

pub struct StorageEngine {
    data_dir: PathBuf,
    chunks_dir: PathBuf,
    manifests_dir: PathBuf,
    max_quota_bytes: u64,
    // Cached manifests for rapid query filtering
    manifest_index: DashMap<String, ContextManifest>,
    // Track chunk access for LRU eviction (chunk_hash, size)
    chunk_access_log: Mutex<VecDeque<String>>,
    // Track total chunk bytes
    used_bytes: Mutex<u64>,
}

impl StorageEngine {
    pub fn new<P: AsRef<Path>>(data_dir: P, max_quota_bytes: u64) -> Result<Self> {
        let root = data_dir.as_ref().to_path_buf();
        let chunks_dir = root.join("chunks");
        let manifests_dir = root.join("manifests");

        fs::create_dir_all(&chunks_dir)
            .with_context(|| format!("Failed to create chunks dir {:?}", chunks_dir))?;
        fs::create_dir_all(&manifests_dir)
            .with_context(|| format!("Failed to create manifests dir {:?}", manifests_dir))?;

        let engine = Self {
            data_dir: root,
            chunks_dir,
            manifests_dir,
            max_quota_bytes,
            manifest_index: DashMap::new(),
            chunk_access_log: Mutex::new(VecDeque::new()),
            used_bytes: Mutex::new(0),
        };

        engine.reindex_disk()?;
        Ok(engine)
    }

    /// Scans the disk on startup to populate the in-memory index and tally disk usage
    fn reindex_disk(&self) -> Result<()> {
        let mut total_bytes = 0u64;

        // Scan chunks
        if let Ok(entries) = fs::read_dir(&self.chunks_dir) {
            let mut access_log = self.chunk_access_log.lock().unwrap();
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata() {
                    if metadata.is_file() {
                        total_bytes += metadata.len();
                        if let Some(name) = entry.file_name().to_str() {
                            if let Some(hash) = name.strip_suffix(".chunk") {
                                access_log.push_back(hash.to_string());
                            }
                        }
                    }
                }
            }
        }

        *self.used_bytes.lock().unwrap() = total_bytes;

        // Scan manifests
        if let Ok(entries) = fs::read_dir(&self.manifests_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        if let Ok(manifest) = serde_json::from_str::<ContextManifest>(&content) {
                            self.manifest_index.insert(manifest.cid.clone(), manifest);
                        }
                    }
                }
            }
        }

        info!(
            "Storage initialized at {:?}. Manifests: {}, Chunks: {}, Used: {} KB / {} MB quota",
            self.data_dir,
            self.manifest_index.len(),
            self.chunk_access_log.lock().unwrap().len(),
            total_bytes / 1024,
            self.max_quota_bytes / (1024 * 1024)
        );

        Ok(())
    }

    /// Saves a chunk to disk, verifying that total quota is respected
    pub fn save_chunk(&self, chunk: &Chunk) -> Result<()> {
        let chunk_path = self.chunks_dir.join(format!("{}.chunk", chunk.hash));
        if chunk_path.exists() {
            // Already present, just mark it as recently accessed
            self.mark_chunk_accessed(&chunk.hash);
            return Ok(());
        }

        // Write chunk file
        fs::write(&chunk_path, &chunk.data)
            .with_context(|| format!("Failed writing chunk {:?}", chunk_path))?;

        let size = chunk.data.len() as u64;
        {
            let mut used = self.used_bytes.lock().unwrap();
            *used += size;
        }

        self.mark_chunk_accessed(&chunk.hash);
        self.enforce_quota()?;
        Ok(())
    }

    /// Checks if a chunk is present locally
    #[allow(dead_code)]
    pub fn has_chunk(&self, hash: &str) -> bool {
        self.chunks_dir.join(format!("{}.chunk", hash)).exists()
    }

    /// Loads a chunk from disk
    pub fn get_chunk(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        let chunk_path = self.chunks_dir.join(format!("{}.chunk", hash));
        if !chunk_path.exists() {
            return Ok(None);
        }

        let data = fs::read(&chunk_path)
            .with_context(|| format!("Failed reading chunk {:?}", chunk_path))?;
        self.mark_chunk_accessed(hash);
        Ok(Some(data))
    }

    /// Saves a context manifest (indexed by CID)
    pub fn save_manifest(&self, manifest: &ContextManifest) -> Result<()> {
        let manifest_path = self.manifests_dir.join(format!("{}.json", manifest.cid));
        let serialized = serde_json::to_string_pretty(manifest)?;
        fs::write(&manifest_path, serialized)
            .with_context(|| format!("Failed writing manifest {:?}", manifest_path))?;

        self.manifest_index.insert(manifest.cid.clone(), manifest.clone());
        Ok(())
    }

    /// Gets a manifest by CID
    pub fn get_manifest(&self, cid: &str) -> Option<ContextManifest> {
        self.manifest_index.get(cid).map(|r| r.value().clone())
    }

    /// Lists all known manifests stored locally
    #[allow(dead_code)]
    pub fn list_manifests(&self) -> Vec<ContextManifest> {
        self.manifest_index.iter().map(|r| r.value().clone()).collect()
    }

    /// Searches and ranks manifests using hybrid BM25/keyword and vector similarity
    pub fn search_manifests(
        &self,
        query: &str,
        tag_filter: Option<&str>,
        query_vec: Option<&[f32]>,
        alpha: f32,
    ) -> Vec<ContextManifest> {
        let ranker = HybridRanker::new(alpha);
        let tag_filter_lower = tag_filter.map(|t| t.to_lowercase());

        let mut scored: Vec<(ContextManifest, f32)> = self
            .manifest_index
            .iter()
            .filter_map(|r| {
                let m = r.value();
                if let Some(ref required_tag) = tag_filter_lower {
                    if !m.tags.iter().any(|t| t.to_lowercase() == *required_tag) {
                        return None;
                    }
                }

                if query.trim().is_empty() {
                    return Some((m.clone(), 1.0));
                }

                let target_text = format!("{} {} {}", m.title, m.summary, m.tags.join(" "));
                let kw_score = HybridRanker::keyword_score(query, &target_text);
                let (final_score, _) = ranker.combine_score(
                    kw_score,
                    query_vec,
                    m.embedding.as_deref(),
                );

                if final_score > 0.05 || (query_vec.is_none() && kw_score > 0.0) {
                    Some((m.clone(), final_score))
                } else {
                    None
                }
            })
            .collect();

        // Sort by highest score first
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().map(|(m, _)| m).collect()
    }

    /// Evicts oldest accessed chunks if current storage exceeds quota
    fn enforce_quota(&self) -> Result<()> {
        let mut used = self.used_bytes.lock().unwrap();
        if *used <= self.max_quota_bytes {
            return Ok(());
        }

        let mut access_log = self.chunk_access_log.lock().unwrap();
        while *used > self.max_quota_bytes && !access_log.is_empty() {
            if let Some(oldest_hash) = access_log.pop_front() {
                let path = self.chunks_dir.join(format!("{}.chunk", oldest_hash));
                if let Ok(metadata) = fs::metadata(&path) {
                    let size = metadata.len();
                    if fs::remove_file(&path).is_ok() {
                        *used = used.saturating_sub(size);
                        debug!("Evicted chunk {} ({} bytes) due to quota limit", oldest_hash, size);
                    }
                }
            }
        }

        if *used > self.max_quota_bytes {
            warn!("Storage still exceeds quota after eviction. Current: {} bytes", *used);
        }

        Ok(())
    }

    fn mark_chunk_accessed(&self, hash: &str) {
        let mut log = self.chunk_access_log.lock().unwrap();
        // Remove if previously logged and push to back (most recently used)
        if let Some(pos) = log.iter().position(|h| h == hash) {
            log.remove(pos);
        }
        log.push_back(hash.to_string());
    }

    pub fn stats(&self) -> StorageStats {
        StorageStats {
            total_chunks: self.chunk_access_log.lock().unwrap().len(),
            total_manifests: self.manifest_index.len(),
            used_bytes: *self.used_bytes.lock().unwrap(),
            max_quota_bytes: self.max_quota_bytes,
        }
    }
}
