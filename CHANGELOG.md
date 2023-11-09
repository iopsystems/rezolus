## [Unreleased]

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

[unreleased]: https://github.com/iopsystems/rezolus/compare/v3.6.0...HEAD
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
