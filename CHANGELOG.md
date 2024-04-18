## [Unreleased]

## [3.14.2] - 2024-04-18

## Fixed

- CPU usage for soft and hard irq was incorrectly reported. (#236)

## [3.14.1] - 2024-04-16

## Fixed

- CPU usage reporting via BPF would report CPU as always idle on some systems.
  (#233)

## [3.14.0] - 2024-04-03

## Changed

- metriken crates updated which changes the msgpack output. (#224)

## Fixed

- Dependency updates to address RUSTSEC-2024-0332.

## [3.13.0] - 2024-04-01

## Changed

- Memory sampler was reporting memory usage stats in KiB, but with bytes for the
  unit metadata. This change corrects the sampler to report memory usage in
  bytes. This fix is disruptive as it will cause the memory stats to change.
  (#222)

## [3.12.0] - 2024-03-28

## Added

- MacOS cpu usage sampling. (#203)
- Metric unit annotations are added and exposed as metadata.
- Logs version number on startup. (#213)

## Fixed

- Incorrect summary stats (percentiles) were reported in version 3.10.2, 3.10.3,
  and 3.11.0. (#216)

## [3.11.0] - 2024-03-25

## Changed

- Refactored the scheduler and syscall BPF samplers to reduce overheads. (#193
  #195)

## Added

- BlockIO thoughput and operation metrics using BPF. (#198)
- Network throughput and packet metrics using BPF. (#200)

## Fixed

- Online CPU detection for CPU usage sampler needed a trimmed string. (#194)

## [3.10.3] - 2024-03-20

## Fixed

- Fixes an incorrect calculation of the number of online CPUs in the BPF-based
  CPU usage sampler.

## [3.10.2] - 2024-03-20

## Fixed

- Fixes a panic in the CPU perf event sampler due to a divide-by-zero. This
  occurs when there are no active perf event groups. (#185)

## [3.10.1] - 2024-03-20

## Fixed

- Fixes per-CPU idle time accounting in the BPF-based sampler. Starting in
  release 3.9.0 these metrics incorrectly report no idle time. (#181)

## [3.10.0] - 2024-03-19

## Added

- Additional system information fields including kernel version, CPU frequency
  details, network queues, and IRQ affinity. (#100)

## Fixed

- Fixes a panic on some systems when perf counter initialization has failed.
  This bug was introduced in 3.9.0. (#175)
- Fixes CPU idle time accounting in the BPF-based sampler. In 3.9.0 the sampler
  incorrectly reports no idle time. (#176)

## [3.9.0] - 2024-03-15

## Added

- CPU usage metrics are now collected via BPF when available. (#165)
- Perf event sampler can now initialize when only some counters are available.
  (#168)

## [3.8.0] - 2024-03-04

## Added

- Allows Rezolus to run on MacOS though sampler support is limited.
- Provides msgpack exposition format as a more efficient exposition format.

## Fixed

- Updates of various direct dependencies.

## [3.7.0] - 2023-12-21

### Added

- Optional compression for HTTP exposition. (#128)
- Additional GPU metrics for utilization and energy consumption. (#138)

### Fixed

- Duplicate metric name in Rezolus sampler. (#134)

## [3.6.1] - 2023-11-30

### Fixed

- Fixed incorrect type annotation for CPU metrics (frequency, ipkc, ipus). (#98)
- Fixed under-reported TCP retransmits. (#121)
- Fixed TCP segment metrics. (#123)

## [3.6.0] - 2023-10-26

### Added

- Allow configuration of individual samplers in the config file. This allows
  each sampler to be individually enabled/disabled and have its collection
  intervals adjusted.
- TCP connection state sampler which tracks the number of tcp connections in
  each state.
- Rezolus sampler which monitors resource utilization of Rezolus itself.
- Optional exposition of histogram buckets on the Prometheus/OpenTelemetry
  endpoint.
- Track latencies for each group of syscalls to help understand the breakdown of
  total syscall latency.

### Fixed

- Corrected a length check of the mmap'd histogram regions. This fix enables the
  fast path for reading histogram data into userspace.

## [3.5.0] - 2023-10-16

### Changed

- Updated `metriken` and replaced heatmaps with histograms. This reduces runtime
  resource utilization.

## [3.4.0] - 2023-10-10

### Changed

- Moved to fetching multiple percentiles at once to reduce overhead.
- Refactor of the hardware info sampler into a separate crate to allow reuse and
  make improvements to that sampler.

### Fixed

- Update `warp` to address RUSTSEC-2023-0065.

## [3.3.3] - 2023-08-08

### Added

- Packaging support for `aarch64`

### Fixed

- Updated dependencies to pull-in fixes and improvements.

## [3.3.2] - 2023-08-08

### Fixed

- Fixed hardware info and cpu samplers on platforms which do not expose either
  die or node information in the topology, which may happen on ARM.
- Fixed BPF program generation to restore compatibility with clang 11.

## [3.3.1] - 2023-08-07

### Fixed

- Fixed path inconsistency in Debian packaging.

## [3.3.0] - 2023-08-02

### Added

- Added BTF type definitions for aarch64 target architecture.

### Fixed

- Update dependencies to reduce overhead and pull-in bugfixes.
- Documentation improvements.

## [3.2.0] - 2023-07-26

### Added

- Added a TCP packet latency sampler to measure the latency from packet being
  received to being processed by the userspace application.
- Added per-device metrics for GPU sampler.

## [3.1.0] - 2023-07-26

### Added

- Added per-CPU metrics for usage, frequency, and perf counters.
- Added BPF to the set of default features.

## [3.0.0] - 2023-07-25

### Changed

- Rewritten implementation of Rezolus using libbpf-rs and perf-event2 to provide
  a more modern approach to BPF and Perf Event instrumentation. 

[unreleased]: https://github.com/iopsystems/rezolus/compare/v3.14.2...HEAD
[3.14.2]: https://github.com/iopsystems/rezolus/compare/v3.14.1...v3.14.2
[3.14.1]: https://github.com/iopsystems/rezolus/compare/v3.14.0...v3.14.1
[3.14.0]: https://github.com/iopsystems/rezolus/compare/v3.13.0...v3.14.0
[3.13.0]: https://github.com/iopsystems/rezolus/compare/v3.12.0...v3.13.0
[3.12.0]: https://github.com/iopsystems/rezolus/compare/v3.11.0...v3.12.0
[3.11.0]: https://github.com/iopsystems/rezolus/compare/v3.10.3...v3.11.0
[3.10.3]: https://github.com/iopsystems/rezolus/compare/v3.10.2...v3.10.3
[3.10.2]: https://github.com/iopsystems/rezolus/compare/v3.10.1...v3.10.2
[3.10.1]: https://github.com/iopsystems/rezolus/compare/v3.10.0...v3.10.1
[3.10.0]: https://github.com/iopsystems/rezolus/compare/v3.9.0...v3.10.0
[3.9.0]: https://github.com/iopsystems/rezolus/compare/v3.8.0...v3.9.0
[3.8.0]: https://github.com/iopsystems/rezolus/compare/v3.7.0...v3.8.0
[3.7.0]: https://github.com/iopsystems/rezolus/compare/v3.6.1...v3.7.0
[3.6.1]: https://github.com/iopsystems/rezolus/compare/v3.6.0...v3.6.1
[3.6.0]: https://github.com/iopsystems/rezolus/compare/v3.5.0...v3.6.0
[3.5.0]: https://github.com/iopsystems/rezolus/compare/v3.4.0...v3.5.0
[3.4.0]: https://github.com/iopsystems/rezolus/compare/v3.3.3...v3.4.0
[3.3.3]: https://github.com/iopsystems/rezolus/compare/v3.3.2...v3.3.3
[3.3.2]: https://github.com/iopsystems/rezolus/compare/v3.3.1...v3.3.2
[3.3.1]: https://github.com/iopsystems/rezolus/compare/v3.3.0...v3.3.1
[3.3.0]: https://github.com/iopsystems/rezolus/compare/v3.2.0...v3.3.0
[3.2.0]: https://github.com/iopsystems/rezolus/compare/v3.1.0...v3.2.0
[3.1.0]: https://github.com/iopsystems/rezolus/compare/v3.0.0...v3.1.0
[3.0.0]: https://github.com/iopsystems/rezolus/releases/tag/v3.0.0
