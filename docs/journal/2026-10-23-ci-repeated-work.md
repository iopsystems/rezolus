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
  and both smoke tests. The smoke steps retain plain builds: the local review
  caught that `--all-targets` unifies dev-dependency features such as
  `rez/test-support` into the earlier binary. Both debug and release smoke
  tests should exercise the plain-build artifact.
- Install pinned cargo-audit 0.22.2 and wasm-pack 0.14.0 binaries through
  the existing `taiki-e/install-action@v2`, with source fallback disabled.
  Audit no longer restores an agent build cache. CI, Pages and the tag-time
  viewer release share `.github/actions/setup-wasm`, so the shipping bundle
  uses the same wasm-pack pin, Rust environment and dependency cache as CI.
  The pin retains the binaryen version expected by build.sh. Pages also
  rebuilds when either shared setup action changes.
- Share dependency caches between the two Clippy jobs, with clippy-deny
  as the writer even on lint failure (`cache-on-failure: true`), and across
  the three WASM build paths. Match the Rust environment in those jobs
  because rust-cache hashes it. Set native
  release overrides before computing the native cache key. Cache keys still
  include rust-cache's platform, compiler and dependency/environment hashes.
- Cancel a superseded run of the same PR. Push runs have distinct groups
  so new main/release commits do not cancel earlier release validation.
- Make `verify all tests pass` wait for wasm-viewer too, and reject
  skipped prerequisites as well as failures and cancellations. Preserve the
  existing native check names, including their formerly implicit trailing
  separator, for compatibility with required status checks; renaming requires
  coordinating the main/release branch rulesets. Audit remains its independent
  status, preserving the existing advisory policy. The initial revision added
  it to the aggregate gate; review caught that this also made a new advisory
  or database fetch outage block the aggregate status, so that change was
  withdrawn. Advisory database caching remains separate work.

The cleaned-up `check` cache key intentionally replaces `check-`, costing one
cold population. The version is alpha.65 against main's alpha.64, as required
by repository policy. PR #1537 also proposes alpha.65 and #1542 is stacked at
alpha.66. No merge order is assumed: after either independent PR lands, the
other must refresh main and bump again, then the sampler stack must be rebased
and its revision checked. Identical version edits alone will not force a
merge conflict, so a clean merge is not evidence that the revision is current.

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

`actionlint` 1.7.12 passes for all three modified workflows (ShellCheck is not
installed locally). The exact doctest selection, with `--offline` added,
passes for dashboard, report-save, rez and systeminfo; there are currently
zero runnable examples. Locked Cargo metadata and whitespace checks pass.
The pinned tools have published Linux x86_64 release assets. The initial
[hosted run 40062328051](https://github.com/iopsystems/rezolus/actions/runs/40062328051)
passed every job, including both tool installers and the platform matrix.
The review fixes need a new hosted run. Pages and tag-time release execution,
cross-workflow cache hits and cache saving after a lint failure have not been
observed in that PR run. The composite's steps also pass actionlint when
expanded into a temporary workflow, and its shell snippets pass `bash -n`.

Installer and cache behavior follow the upstream
[install-action documentation](https://github.com/taiki-e/install-action#usage)
and [rust-cache documentation](https://github.com/Swatinem/rust-cache#example-usage).
