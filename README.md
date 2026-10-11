# HiveContext: BitTorrent-Inspired P2P Context Caching with Semantic Search

A lightweight, decentralized, and quota-bounded peer-to-peer storage hive designed for AI prompt and context caching. Nearby inference workers and developer machines swarm-cache and share context documents over local networks with **Dense Semantic Vector Embeddings** and **Hybrid Ranking**.

---

## ⚡ Core Concepts

1. **Content Addressing (BLAKE3):** Every `.md` context or prompt transcript is split into deterministic chunks (e.g. 64 KB) and assigned a root Content ID (CID).
2. **Dense Vector Embeddings & Hybrid Search (NEW):**
   - Automatically generates 384-dimensional vector embeddings for stored contexts using `SentenceTransformer("all-MiniLM-L6-v2")`.
   - Manifests carry both text metadata and vector embeddings.
   - **Hybrid Ranker** merges BM25 keyword overlap with Cosine Similarity ($Score = \alpha \cdot BM25 + (1 - \alpha) \cdot Cosine$).
   - **Solves Vocabulary Mismatch:** Person B can ask a completely generic question with **zero keyword overlap** (e.g. *"latency bottleneck in data tier"*), and still discover Person A's specific post-mortem (*"Redis timeout incident"*).
3. **BitTorrent-Style Swarm Downloading:** When a node needs a past context, it asks the local swarm and downloads chunks in parallel from multiple online peers.
4. **Bounded Peer Quota & LRU Eviction:** Each peer commits a configurable storage budget (e.g. 500 MB). When the quota is exceeded, least-recently-accessed chunks are evicted.
5. **Zero-Configuration LAN Discovery:** Nodes discover peers automatically using UDP broadcast beacons or explicit `--peer` flags.
6. **AI Inference Proxy:** An OpenAI/Ollama-compatible endpoint (`/v1/chat/completions`) that intercepts incoming prompts, semantically matches relevant past context from the swarm, injects the retrieved `.md` into the system prompt, and forwards to your local LLM.

---

## 🚀 Quick Start

### 1. Build the Binary
```bash
cargo build --release
```
The executable will be located at `target/release/hivecontext.exe`.

### 2. (Optional) Run the Embedding Service
To enable dense vector search, start the FastAPI embedding service from `D:\Vector Embed\core`:
```bash
python -m uvicorn embedding_service.app:app --host 127.0.0.1 --port 8000
```
*(If the embedding service is not running, HiveContext automatically falls back to keyword matching.)*

---

## 🖥️ Usage & CLI Commands

### 1. Start the Hive Daemon
Start a peer node on port 7071 with vector search enabled:
```bash
hivecontext daemon --port 7071 --quota-mb 500 --data-dir ./hive_data --embedding-url http://127.0.0.1:8000
```

To connect manually to another peer:
```bash
hivecontext daemon --port 7072 --data-dir ./hive_data_2 --peer 127.0.0.1:7071 --embedding-url http://127.0.0.1:8000
```

To enable forwarding to an upstream local LLM (like Ollama):
```bash
hivecontext daemon --port 7071 --upstream-llm http://localhost:11434
```

### 2. Store a Markdown Context File
Ingest, embed, chunk, and announce:
```bash
hivecontext store --file ./examples/redis_incident_context.md \
  --title "Redis Incident Post-Mortem" \
  --tags "redis,cache,incident" \
  --summary "Post-mortem analysis of cache cluster timeout"
```

Output:
```
Successfully stored and announced context to hive!
  Title:         Redis Incident Post-Mortem
  CID:           6f841029ece8b404ef4f50071c0c90ad269ee79dfc335fdb97fcbeb6fb8901e1
  Chunks:        5
  Total Size:    1083 bytes
  Vector Embed:  Generated (384-dim)
```

### 3. Search the Hive Swarm (Semantic Query)
Search across all discovered peers even without exact keywords:
```bash
hivecontext search "unresponsive latency bottleneck in data tier"
```

### 4. Fetch and Reconstruct Markdown from Swarm
```bash
hivecontext fetch 6f841029ece8b404ef4f50071c0c90ad269ee79dfc335fdb97fcbeb6fb8901e1 --out ./restored.md
```

### 5. Inspect Swarm Peers & Storage Stats
```bash
hivecontext peers
hivecontext stats
```

---

## 🔌 REST API Reference

| Endpoint | Method | Description |
| :--- | :--- | :--- |
| `/v1/context/store` | `POST` | Ingests `.md`, embeds vector, chunks with BLAKE3, and announces to peers. |
| `/v1/context/search?q=...` | `GET` | Searches manifests across swarm using Hybrid Ranking (Cosine + BM25). |
| `/v1/context/fetch/:cid` | `GET` | Swarm-downloads chunks and returns assembled markdown content. |
| `/v1/swarm/peers` | `GET` | Returns list of currently active discovered peers. |
| `/v1/swarm/stats` | `GET` | Returns storage quota usage and chunk counts. |
| `/v1/chat/completions` | `POST` | AI Context Proxy: semantically matches hive context and injects into system prompt. |
| `/p2p/chunk/:hash` | `GET` | Serves binary chunk bytes to requesting peers. |
| `/p2p/manifests` | `GET` | Returns local manifests for peer queries. |

---

## 🧪 Automated Tests

1. **Basic Swarm & Reassembly Test:**
   ```bash
   python tests/test_swarm_e2e.py
   ```
2. **Semantic Vector Search Test (Zero Keyword Overlap):**
   ```bash
   python tests/test_semantic_search_e2e.py
   ```
