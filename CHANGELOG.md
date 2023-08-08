## [Unreleased]

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

[unreleased]: https://github.com/iopsystems/rezolus/compare/v3.3.2...HEAD
[3.3.2]: https://github.com/iopsystems/rezolus/compare/v3.3.1...v3.3.2
[3.3.1]: https://github.com/iopsystems/rezolus/compare/v3.3.0...v3.3.1
[3.3.0]: https://github.com/iopsystems/rezolus/compare/v3.2.0...v3.3.0
[3.2.0]: https://github.com/iopsystems/rezolus/compare/v3.1.0...v3.2.0
[3.1.0]: https://github.com/iopsystems/rezolus/compare/v3.0.0...v3.1.0
[3.0.0]: https://github.com/iopsystems/rezolus/releases/tag/v3.0.0
