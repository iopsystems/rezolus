#!/usr/bin/env python3
"""Write golden.json: what a released rezolus answers on the archives here.

Usage: golden.py REZOLUS_BINARY

Runs REZOLUS_BINARY's `mcp` server over stdio and asks its `query` tool each
query in QUERIES on a copy of each archive in ARCHIVES (reading a `.dendro`
can leave SQLite's `-shm` and `-wal` files beside it). Each result is kept as
its series' labels and their (timestamp, value) points. The binary must be
the release that wrote the archives (see README.md).
"""

import json
import pathlib
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
ARCHIVES = ["v5.29.2.rez", "v5.29.2.dendro"]
QUERIES = [
    "cpu_cores",
    "rezolus_memory_usage_resident_set_size",
    "rate(cpu_usage[1s])",
    "sum by (state) (irate(cpu_usage[5s]))",
    "irate(blockio_operations[5s])",
    "rate(rezolus_cpu_usage[1s])",
]


def ask(binary, archive):
    with tempfile.TemporaryDirectory() as scratch:
        copy = pathlib.Path(scratch) / archive
        shutil.copyfile(HERE / archive, copy)
        requests = [
            {"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
        ]
        for i, q in enumerate(QUERIES, 1):
            requests.append({
                "jsonrpc": "2.0", "id": i, "method": "tools/call",
                "params": {"name": "query",
                           "arguments": {"parquet_file": str(copy), "query": q}},
            })
        out = subprocess.run(
            [binary, "mcp"], input="".join(json.dumps(r) + "\n" for r in requests),
            capture_output=True, text=True, check=True,
        ).stdout
    responses = {}
    for line in out.splitlines():
        r = json.loads(line)
        responses[r.get("id")] = r
    results = {}
    for i, q in enumerate(QUERIES, 1):
        text = responses[i]["result"]["content"][0]["text"]
        matrix = json.loads(text)
        results[q] = sorted(
            ({"labels": s["metric"], "points": [[float(t), float(v)] for t, v in s["values"]]}
             for s in matrix["result"]),
            key=lambda s: sorted(s["labels"].items()),
        )
    return results


def main():
    binary = sys.argv[1]
    version = subprocess.run([binary, "--version"], capture_output=True, text=True,
                             check=True).stdout.strip()
    golden = {"written_by": version, "archives": {a: ask(binary, a) for a in ARCHIVES}}
    (HERE / "golden.json").write_text(json.dumps(golden, indent=1, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
