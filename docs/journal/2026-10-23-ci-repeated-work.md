# Reduce repeated CI work

The first CI pass removes duplicate execution and tool compilation while
retaining Ubuntu 22.04, Ubuntu 24.04 and macOS coverage. It is based on
`bffcf842`, independently of the cgroup memory and blocking-worker PRs.

## Evidence

In [run 40057643677](https://github.com/iopsystems/rezolus/actions/runs/40057643677),
the Ubuntu 24.04 debug job ran the native unit and integration tests twice.
The step named `doctests` used unrestricted `cargo test`: it started at
03:21:32 UTC and the next step started at 03:24:10, about 158 seconds later.
The same job had already run those tests in its `--tests --bins` step. Its
four documentation-test targets contained zero examples, but should still
run so future documentation tests are covered.

The audit job compiled cargo-audit 0.22.2 from 03:17:01 to 03:19:12, about
132 seconds, after requesting a read-only `check-ubuntu-latest` cache that
the check job did not write (its undefined matrix axis expanded to `check-`).
Audit does not need compiled project dependencies.

The WASM job already had wasm-pack 0.14.0 in its warm cache: its install
check took less than a second. Switching that tool to a prebuilt download
primarily avoids source compilation on cold caches; it is not a claimed
warm-cache speedup. These baseline observations are not measurements of
the new workflow's total elapsed time.

## Changes and coverage

- Restrict the documentation step to `cargo test --doc`. Keep the first
  native test pass, the separate native viewer tests, the all-targets build
  and both smoke tests. The smoke steps use the agent already produced by
  the all-targets build instead of invoking Cargo again.
- Install pinned cargo-audit 0.22.2 and wasm-pack 0.14.0 binaries through
  the existing `taiki-e/install-action@v2`, with source fallback disabled.
  Audit no longer restores an agent build cache. CI and Pages use the same
  wasm-pack version, retaining the binaryen version expected by build.sh.
- Share dependency caches between the two Clippy jobs, with clippy-deny
  as the writer, and between WASM validation and Pages. Match the Rust
  environment in both WASM jobs because rust-cache hashes it. Set native
  release overrides before computing the native cache key. Cache keys still
  include rust-cache's platform, compiler and dependency/environment hashes.
- Cancel a superseded run of the same PR. Push runs have distinct groups
  so new main/release commits do not cancel earlier release validation.
- Make `verify all tests pass` wait for audit and wasm-viewer too, and reject
  skipped prerequisites as well as failures and cancellations. Preserve the
  existing native check names, including their formerly implicit trailing
  separator, for compatibility with required status checks.

The local review exchange found test isolation and timing issues on the
sampler stack. This pass keeps the integration suite and its failure gates;
it neither adds retries nor hides timing-sensitive tests. The earlier
`hindsight_dump` failure during a second suite invocation illustrates why
duplicating a suite is not a useful substitute for deliberate stress testing.

Runner changes, reduced OS coverage, feature/target reduction, a consolidated
Clippy report/gate job and a separate stress-test tier are deferred until
the cheaper workflow provides a new baseline. The SARIF reporting and deny
jobs retain their current independent failure behavior.

## Validation

`actionlint` 1.7.12 passes for both modified workflows (ShellCheck is not
installed locally). The exact doctest selection, with `--offline` added,
passes for dashboard, report-save, rez and systeminfo; there are currently
zero runnable examples. Locked Cargo metadata and whitespace checks pass.
The pinned tools have published Linux x86_64 release assets. Hosted cache
hits, installer execution and the unchanged platform matrix still need the
PR run; no end-to-end speedup is claimed before that measurement.

Installer and cache behavior follow the upstream
[install-action documentation](https://github.com/taiki-e/install-action#usage)
and [rust-cache documentation](https://github.com/Swatinem/rust-cache#example-usage).
