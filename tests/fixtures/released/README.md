# Archives written by a released rezolus

`tests/released_archives.rs` reads these with the current build and compares
its answers with `golden.json`. Do not regenerate them with the current
build: they stand for what users already have on disk.

## `v5.29.2.rez`, `v5.29.2.dendro`

Written on 2026-11-06 by rezolus 5.29.2, built from the `v5.29.2` tag, on
macOS (Apple M4 Max). One agent (`rezolus agent.toml`, the tag's
`config/agent.toml` with `listen = "127.0.0.1:14241"` and the `drivehealth`
sampler disabled, since its labels carry the drive's serial number) was
recorded by two `record` runs at the same time:

```
rezolus record --duration 15s --interval 1s http://127.0.0.1:14241 -o v5.29.2.rez
rezolus record --duration 15s --interval 1s http://127.0.0.1:14241 -o v5.29.2.dendro
```

Each holds one recording (`host=macbook.localdomain`, `source=rezolus`) of 16
rows per table. Data comes from `cpu_usage`, `blockio_requests` and
`rezolus_rusage`. The archives also hold 20 `unattributed/*` histogram
streams with no data, so they cannot test histogram reads; that needs a
recording from a Linux agent. The `.rez` is a v3 archive. Both archives hold
the same row timestamps, and their values differ at three of them, so a test
that reads the wrong archive fails.

## `golden.json`

What 5.29.2's MCP `query` tool answered on each archive. Regenerate it, only
with a 5.29.2 binary, by `python3 golden.py path/to/rezolus-5.29.2`.

The queries are ones 6.0 answers the same way: gauges, `irate`, and `rate`
over a range of one interval. `tests/released_archives.rs` lists them too and
checks golden.json holds exactly those. 5.x answered `rate(x[r])` with the rate over
one step for any `r`; 6.0 answers with the average rate over `[t - r, t]`.
