import subprocess
import time
import requests
import json
import os
import shutil
import sys

def main():
    print("=== HiveContext Semantic Vector Search Integration Test ===")
    
    bin_path = r"D:\Peer2P\target\debug\hivecontext.exe"
    data_dir_a = r"D:\Peer2P\data_test_sem_a"
    data_dir_b = r"D:\Peer2P\data_test_sem_b"
    sample_file = r"D:\Peer2P\examples\redis_incident_context.md"
    embed_dir = r"D:\Vector Embed\core"

    # Cleanup any previous test data
    for d in [data_dir_a, data_dir_b]:
        if os.path.exists(d):
            shutil.rmtree(d, ignore_errors=True)

    # 1. Start Embedding Service on port 8000
    print("[1] Starting Embedding Service from D:\\Vector Embed (Port 8000)...")
    proc_embed = subprocess.Popen([
        sys.executable, "-m", "uvicorn", "embedding_service.app:app",
        "--host", "127.0.0.1", "--port", "8000"
    ], cwd=embed_dir)

    # Wait for embedding service to load model
    print("Waiting for SentenceTransformer model to load...")
    for _ in range(30):
        try:
            r = requests.get("http://127.0.0.1:8000/", timeout=1)
            if r.status_code == 200:
                print("Embedding Service is ONLINE!")
                break
        except Exception:
            pass
        time.sleep(1)

    # 2. Start Node A (Port 7071)
    print("\n[2] Starting HiveContext Node A (Port 7071)...")
    proc_a = subprocess.Popen([
        bin_path, "daemon",
        "--port", "7071",
        "--udp-port", "0",
        "--data-dir", data_dir_a,
        "--quota-mb", "100",
        "--embedding-url", "http://127.0.0.1:8000"
    ])

    # 3. Start Node B (Port 7072, Peered with Node A)
    print("[3] Starting HiveContext Node B (Port 7072, Peered with Node A)...")
    proc_b = subprocess.Popen([
        bin_path, "daemon",
        "--port", "7072",
        "--udp-port", "0",
        "--data-dir", data_dir_b,
        "--quota-mb", "100",
        "--peer", "127.0.0.1:7071",
        "--embedding-url", "http://127.0.0.1:8000"
    ])

    time.sleep(3)

    try:
        # 4. Store Context into Node A
        print("\n[4] Storing Markdown Context into Node A...")
        with open(sample_file, "r", encoding="utf-8") as f:
            md_content = f.read()

        store_payload = {
            "title": "Redis Timeout Incident Post-Mortem",
            "tags": ["redis", "incident", "cache"],
            "summary": "Post-mortem analysis of cache cluster timeout and P2P migration",
            "content": md_content,
            "chunk_size": 256
        }

        store_res = requests.post("http://127.0.0.1:7071/v1/context/store", json=store_payload)
        stored_data = store_res.json()
        print(f"Node A Stored: CID={stored_data['cid']}, Vector Generated={stored_data['has_embedding']}")
        assert stored_data["has_embedding"] is True, "Vector embedding must be generated"

        # 5. SEMANTIC TEST: Person B asks a completely generic question with ZERO keyword overlap!
        generic_query = "unresponsive latency bottleneck in the data tier"
        print(f"\n[5] Person B queries with zero keyword overlap: '{generic_query}'...")
        search_res = requests.get(f"http://127.0.0.1:7072/v1/context/search?q={generic_query}")
        matches = search_res.json()
        
        print(f"Search results count: {len(matches)}")
        for m in matches:
            print(f"  -> Found: '{m['title']}' (CID: {m['cid'][:16]}...) [Vector Embed={m['embedding'] is not None}]")

        assert len(matches) > 0, "Semantic vector search MUST find the context even with zero keyword overlap!"
        print("\n SUCCESS: Semantic search bridged generic query to specific incident post-mortem!")

        # 6. Test AI Prompt Injection with Generic Query
        print("\n[6] Testing AI Chat Completion Proxy with generic query...")
        chat_payload = {
            "messages": [
                {"role": "user", "content": "How do we fix the unresponsive latency bottleneck in the data tier?"}
            ],
            "context_query": generic_query
        }
        chat_res = requests.post("http://127.0.0.1:7072/v1/chat/completions", json=chat_payload)
        chat_data = chat_res.json()
        print("AI Proxy Response:")
        print(json.dumps(chat_data, indent=2))
        assert chat_data.get("injected_context_found") is True, "AI Proxy should inject the semantically matched context"

        print("\n=== ALL SEMANTIC INTEGRATION TESTS PASSED! ===")

    finally:
        print("\nCleaning up processes...")
        proc_a.terminate()
        proc_b.terminate()
        proc_embed.terminate()
        proc_a.wait()
        proc_b.wait()
        proc_embed.wait()
        print("Done.")

if __name__ == "__main__":
    main()
