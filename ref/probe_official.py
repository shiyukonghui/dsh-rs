#!/usr/bin/env python3
"""Probe the official DeepSeek Harness web API and dump responses for comparison."""
import json, sys, urllib.request

BASE = "http://127.0.0.1:3222"

METHODS = [
    "host.describe", "session.list", "session.models", "session.rename",
    "session.fork", "workspace.list", "skill.list", "agentPreset.list",
    "commands.list", "settings.describe", "credentials.describe",
    "llm.providers", "llm.models", "goal.get", "subagent.list",
    "session.history", "session.search", "session.cancel",
    "session.updateQueue", "workspace.archiveSession",
]

for i, m in enumerate(METHODS):
    body = json.dumps({"type": "client-request", "rpcId": f"r{i}", "method": m, "payload": {}}).encode()
    req = urllib.request.Request(f"{BASE}/api/{m}", data=body, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            data = resp.read().decode()
    except Exception as e:
        data = f"<error: {e}>"
    fn = "official_" + m.replace(".", "_") + ".txt"
    with open(fn, "w", encoding="utf-8") as f:
        f.write(f"===== {m} =====\n{data}\n")
    print(f"{m}: {len(data)} bytes")
print("DONE")
