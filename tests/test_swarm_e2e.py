import subprocess
import time
import requests
import json
import os
import shutil
import sys

def main():
    print("=== HiveContext End-to-End P2P Swarm Test ===")
    
    bin_path = r"D:\Peer2P\target\debug\hivecontext.exe"
    data_dir_a = r"D:\Peer2P\data_test_node_a"
    data_dir_b = r"D:\Peer2P\data_test_node_b"
    sample_file = r"D:\Peer2P\examples\redis_incident_context.md"

    # Cleanup any previous test data
    for d in [data_dir_a, data_dir_b]:
        if os.path.exists(d):
            shutil.rmtree(d, ignore_errors=True)

    # 1. Start Node A on port 7071
    print("[1] Starting Node A (Port 7071)...")
    proc_a = subprocess.Popen([
        bin_path, "daemon",
        "--port", "7071",
        "--udp-port", "0", # disabled for deterministic localhost test
        "--data-dir", data_dir_a,
        "--quota-mb", "100"
    ])

    # 2. Start Node B on port 7072, peered with Node A
    print("[2] Starting Node B (Port 7072, Peered with Node A)...")
    proc_b = subprocess.Popen([
        bin_path, "daemon",
        "--port", "7072",
        "--udp-port", "0",
        "--data-dir", data_dir_b,
        "--quota-mb", "100",
        "--peer", "127.0.0.1:7071"
    ])

    time.sleep(2) # Give daemons time to bind

    try:
        # Check Node A status
        res_a = requests.get("http://127.0.0.1:7071/v1/swarm/stats")
        print(f"Node A initial stats: {res_a.json()}")

        # Check Node B status
        res_b = requests.get("http://127.0.0.1:7072/v1/swarm/peers")
        print(f"Node B active peers: {res_b.json()}")

        # 3. Store the markdown file into Node A
        print("\n[3] Storing markdown context into Node A...")
        with open(sample_file, "r", encoding="utf-8") as f:
            md_content = f.read()

        store_payload = {
            "title": "Redis Timeout Incident Post-Mortem",
            "tags": ["redis", "incident", "cache", "p2p"],
            "summary": "Post-mortem analysis of cache cluster timeout and P2P migration",
            "content": md_content,
            "chunk_size": 256 # smaller chunks to test multi-chunk swarming
        }

        store_res = requests.post("http://127.0.0.1:7071/v1/context/store", json=store_payload)
        stored_data = store_res.json()
        print(f"Node A Stored: CID={stored_data['cid']}, Chunks={stored_data['chunks_count']}, Bytes={stored_data['total_size']}")

        # 4. Search from Node B across the swarm
        print("\n[4] Querying context from Node B (query='redis')...")
        search_res = requests.get("http://127.0.0.1:7072/v1/context/search?q=redis")
        matches = search_res.json()
        print(f"Node B found {len(matches)} match(es) from swarm: {[m['title'] for m in matches]}")
        assert len(matches) > 0, "Node B should discover Node A's manifest"

        # 5. Swarm-fetch from Node B
        cid = stored_data['cid']
        print(f"\n[5] Node B swarm-fetching document CID {cid}...")
        fetch_res = requests.get(f"http://127.0.0.1:7072/v1/context/fetch/{cid}")
        retrieved_text = fetch_res.text
        print(f"Node B successfully assembled {len(retrieved_text)} chars from swarm.")
        assert retrieved_text == md_content, "Reconstructed content must match original exactly"
        print(" Verification: Assembled content matches original byte-for-byte!")

        # 6. Verify Node B now has chunks cached locally
        stats_b = requests.get("http://127.0.0.1:7072/v1/swarm/stats").json()
        print(f"Node B local stats after swarm fetch: {stats_b}")
        assert stats_b['total_chunks'] == stored_data['chunks_count'], "Node B should have cached all chunks"

        # 7. Test AI Chat Completions Context Injection Proxy
        print("\n[7] Testing AI Chat Completion Proxy on Node B...")
        chat_payload = {
            "messages": [
                {"role": "user", "content": "What was the root cause of the redis timeout incident?"}
            ],
            "context_query": "redis timeout"
        }
        chat_res = requests.post("http://127.0.0.1:7072/v1/chat/completions", json=chat_payload)
        chat_data = chat_res.json()
        print("AI Proxy Response:")
        print(json.dumps(chat_data, indent=2))
        assert chat_data.get("injected_context_found") is True, "AI proxy should find and inject hive context"

        print("\n=== ALL TESTS PASSED SUCCESSFULLY! ===")

    finally:
        # Terminate test daemons
        proc_a.terminate()
        proc_b.terminate()
        proc_a.wait()
        proc_b.wait()
        print("Test daemons terminated.")

if __name__ == "__main__":
    main()
