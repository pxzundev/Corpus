#!/usr/bin/env python3
"""Smoke-test the MCP server over stdio: handshake, tool discovery, tool calls.

  python3 scripts/mcp-smoke.py [path-to-corpus-mcp] ["query"]

Exits non-zero if the server does not complete the MCP handshake, does not
advertise both tools, or returns an error result.
"""
import json
import re
import subprocess
import sys

BIN = sys.argv[1] if len(sys.argv) > 1 else "target/debug/corpus-mcp"
QUERY = sys.argv[2] if len(sys.argv) > 2 else "what does the document say about procedures and requirements"

proc = subprocess.Popen(
    [BIN],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
    bufsize=1,
)


def send(message):
    proc.stdin.write(json.dumps(message) + "\n")
    proc.stdin.flush()


def expect(msg_id, timeout=240):
    """Read frames until the reply to msg_id arrives; stderr is read separately."""
    import select

    deadline = timeout
    while True:
        ready, _, _ = select.select([proc.stdout], [], [], deadline)
        if not ready:
            raise SystemExit(f"FAIL: no reply to id {msg_id} within {timeout}s")
        line = proc.stdout.readline()
        if not line:
            raise SystemExit(f"FAIL: server closed stdout; stderr:\n{proc.stderr.read()}")
        message = json.loads(line)
        if message.get("id") == msg_id:
            return message


send(
    {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "mcp-smoke", "version": "0"},
        },
    }
)
initialized = expect(1)
info = initialized["result"]
print(f"handshake ok: server={info['serverInfo']['name']} protocol={info['protocolVersion']}")
assert info["serverInfo"]["name"] == "corpus", "server reports a name other than corpus"

send({"jsonrpc": "2.0", "method": "notifications/initialized"})

send({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
tools = expect(2)["result"]["tools"]
names = sorted(tool["name"] for tool in tools)
print(f"tools advertised: {names}")
assert names == ["list_documents", "search_docs"], f"unexpected tools: {names}"
schema = next(t for t in tools if t["name"] == "search_docs")["inputSchema"]
print(f"search_docs params: {sorted(schema['properties'])} required={schema.get('required')}")
assert "query" in schema["properties"], "query parameter missing from schema"

send(
    {
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {"name": "search_docs", "arguments": {"query": QUERY, "k": 2}},
    }
)
call = expect(3)
if call["result"].get("isError"):
    raise SystemExit(f"FAIL: tool returned an error: {call['result']}")
body = call["result"]["content"][0]["text"]
print(f"\n--- search_docs({QUERY!r}, k=2) ---\n{body[:600]}")
assert "[" in body and "|" in body, "citation format missing from results"

# filename filter: every citation must come from the requested document.
# The target comes from the search above, so this works against any corpus.
CITATION = re.compile(r"^\[(.+) p\.\d+ \|")
matched = [CITATION.match(line) for line in body.splitlines() if line.startswith("[")]
assert any(matched), f"no citations to derive a filter target from:\n{body[:300]}"
FILTERED_FILE = next(m for m in matched if m).group(1)
print(f"\nfilter target: {FILTERED_FILE}")
send(
    {
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "search_docs",
            "arguments": {
                "query": QUERY,
                "k": 3,
                "filename": FILTERED_FILE,
            },
        },
    }
)
filtered = expect(4)["result"]
assert not filtered.get("isError"), f"filtered search failed: {filtered}"
filtered_body = filtered["content"][0]["text"]
citations = [line for line in filtered_body.splitlines() if line.startswith("[")]
assert citations, "filename filter returned no citations"
for citation in citations:
    assert FILTERED_FILE in citation, f"filter leaked another document: {citation}"
print(f"filename filter ok: {len(citations)} citations, all from the requested document")

send({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "list_documents", "arguments": {}}})
listing = expect(5)["result"]["content"][0]["text"]
print(f"\n--- list_documents ---\n{listing[:200]}")

proc.stdin.close()
proc.wait(timeout=30)
print("\nMCP smoke test passed")
