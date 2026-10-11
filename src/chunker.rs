use anyhow::{bail, Result};
use blake3::Hasher;
use serde::{Deserialize, Serialize};

/// Default chunk size: 64 KB (optimal for prompt segments, chat turns, and fast P2P swarming)
pub const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Chunk {
    pub hash: String,
    #[serde(skip_serializing, skip_deserializing)]
    pub data: Vec<u8>,
    pub size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextManifest {
    /// Root Content-ID (Blake3 over sorted chunk hashes)
    pub cid: String,
    pub title: String,
    pub tags: Vec<String>,
    pub summary: String,
    pub chunk_hashes: Vec<String>,
    pub total_size: usize,
    pub created_at: i64,
    /// 384-dimensional vector embedding for semantic search
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
}

pub struct Chunker;

impl Chunker {
    /// Computes the BLAKE3 hash of arbitrary bytes and returns a lowercase hex string
    pub fn hash_bytes(data: &[u8]) -> String {
        let mut hasher = Hasher::new();
        hasher.update(data);
        hasher.finalize().to_hex().to_string()
    }

    /// Splits input bytes into fixed-size content-addressed chunks
    pub fn chunk_data(data: &[u8], chunk_size: usize) -> Vec<Chunk> {
        let chunk_size = if chunk_size == 0 { DEFAULT_CHUNK_SIZE } else { chunk_size };
        data.chunks(chunk_size)
            .map(|slice| {
                let hash = Self::hash_bytes(slice);
                Chunk {
                    hash,
                    data: slice.to_vec(),
                    size: slice.len(),
                }
            })
            .collect()
    }

    /// Creates a complete ContextManifest from markdown/text content
    pub fn create_manifest(
        title: &str,
        tags: Vec<String>,
        summary: &str,
        content: &[u8],
        chunk_size: usize,
        embedding: Option<Vec<f32>>,
    ) -> (ContextManifest, Vec<Chunk>) {
        let chunks = Self::chunk_data(content, chunk_size);
        let chunk_hashes: Vec<String> = chunks.iter().map(|c| c.hash.clone()).collect();

        // Root CID is computed over concatenated chunk hashes
        let mut root_hasher = Hasher::new();
        for h in &chunk_hashes {
            root_hasher.update(h.as_bytes());
        }
        let cid = root_hasher.finalize().to_hex().to_string();

        let manifest = ContextManifest {
            cid,
            title: title.to_string(),
            tags,
            summary: summary.to_string(),
            chunk_hashes,
            total_size: content.len(),
            created_at: chrono::Utc::now().timestamp(),
            embedding,
        };

        (manifest, chunks)
    }

    /// Reconstructs full document bytes from an ordered sequence of verified chunks
    pub fn assemble_chunks(manifest: &ContextManifest, chunks: &[Chunk]) -> Result<Vec<u8>> {
        if chunks.len() != manifest.chunk_hashes.len() {
            bail!(
                "Chunk count mismatch: expected {}, got {}",
                manifest.chunk_hashes.len(),
                chunks.len()
            );
        }

        let mut output = Vec::with_capacity(manifest.total_size);
        for (i, expected_hash) in manifest.chunk_hashes.iter().enumerate() {
            let chunk = &chunks[i];
            if &chunk.hash != expected_hash {
                bail!(
                    "Chunk hash mismatch at index {}: expected {}, got {}",
                    i,
                    expected_hash,
                    chunk.hash
                );
            }
            output.extend_from_slice(&chunk.data);
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunking_and_reassembly() {
        let sample_text = "# Project Setup\nThis is a test prompt context file for the P2P swarm."
            .repeat(500); // ~37 KB
        let (manifest, chunks) = Chunker::create_manifest(
            "Test Setup",
            vec!["rust".into(), "p2p".into()],
            "Test summary",
            sample_text.as_bytes(),
            4 * 1024, // 4 KB chunks
            None,
        );

        assert!(!manifest.cid.is_empty());
        assert_eq!(chunks.len(), manifest.chunk_hashes.len());

        let reconstructed = Chunker::assemble_chunks(&manifest, &chunks).unwrap();
        assert_eq!(reconstructed, sample_text.as_bytes());
    }
}
