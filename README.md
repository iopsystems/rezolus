# Rezolus

Rezolus captures and exports high resolution data about systems performance. It
sets itself apart from other telemetry agents by using:

* **eBPF**: Uses eBPF on Linux to instrument individual events and aggregate
  them into distributions. Rezolus will report on things like block io size and
  latency distributions, system call latency, TCP segment sizes, and more. By
  using eBPF we are able to instrument low-level events efficiently and provide
  new insights into system performance and workload characteristics.

* **Perf Events**: On x86_64 we support gathering data from the CPU using
  performance counters. Rezolus gathers information about instructions being
  retired, the number of CPU cyles, and fine-grained CPU frequency data. This
  helps expose how efficient workload execution is on the hardware.

## Overview

Rezolus is designed to produce high resolution systems performance telemetry. It
has a collection of samplers which instrument various aspects of systems
performance including CPU, GPU, task scheduling, system calls, TCP, block IO,
and more.

All of Rezolus's sampler focus on capturing key signals that can be used to
understand how the workload is running on the underlying system. These insights
are useful for understanding where bottlenecks and optimization opportunities
might be. With high frequency sampling and eBPF, Rezolus can also provide
insights into your workload itself like what typical block and network IO sizes
are, the number and type of system calls being executed, and if there's spikes
in utilization metrics.

Rezolus provides valuable data about systems performance and can be used to root
cause production performance issues, capture better data in test environments,
and provide signals for optimization efforts.

### Configuration

Rezolus uses a TOML configuration. See `config.toml` in this project for an
example config file.

### Dashboard

If you are running Prometheus and Grafana for collecting and visualizing
metrics, the `dashboard.json` file is an example Grafana dashboard that
demonstrates some ways to use the collected data. This can help you get started
on your own dashboards.

## Getting Help

Join our [Discord server][discord] to ask questions and have discussions.

If you have a problem using Rezolus or a question about Rezolus that you can't
find an answer to, please open a
[new issue on GitHub][new issue]

## Building

Rezolus is built using the Rust toolchain. If you do not have the Rust toolchain
installed, please see [rust-lang.org][rust-lang.org] to get started with Rust.

### Build Dependencies

Rust >= 1.70.0

#### Linux

A minimum kernel version of 5.5 is required. The following distributions should
work:

* Debian: Bullseye and newer
* Ubuntu: 20.10 and newer
* Red Hat: RHEL 9 and newer
* Amazon Linux: AL2 w/ 5.10 or newer, AL2023
* Any rolling-release distro: Arch, Gentoo, ...

In addition to the base dependencies, the following are needed:

* clang >= 11.0
* libelf-dev >= 0.183
* make >= 4.3
* pkg-config >= 0.29.2

Debian and Ubuntu users can install all the required dependencies for a default
build with:

```bash
sudo apt install clang libelf-dev make pkg-config
```

### Steps

* clone this repository or transfer the contents of the repository to your build
  machine
* change directory into the repository root
* run `cargo build` in release mode

```bash
git clone https://github.com/iopsystems/rezolus
cd rezolus
cargo build --release
```

### Configuration

See the [config.toml file][config] for an example configuration with
explanations for the various options.

### Installation

You can either manually install Rezolus and register it with your init system
(eg systemd) or if you're using Debian or Ubuntu you can build a package for
Rezolus using `dpkg-buildpackage -b` in the repository root. Note: you will need
both `devscripts` and `jq` installed to generate the package.

### Running

You may also run Rezolus manually after building from source. In the repository
root you can run:

```bash
sudo target/release/rezolus config.toml
```

## Contributing

To contribute to Rezolus first check if there are any open pull requests or
issues related to the bugfix or feature you wish to contribute. If there is not,
please start by opening a [new issue on GitHub][new issue] to either report the
bug or get feedback on a new feature. This will allow one of the maintainers to
confirm the bug and provide early input on new features.

Once you're ready to contribute some changes, the workflow is:
* [create a fork][create a fork] of this repository
* clone your fork and create a new feature branch
* make your changes and write a helpful commit message
* push your feature branch to your fork
* open a [new pull request][new pull request]

## License

Rezolus is dual-licensed under the [Apache License v2.0][license apache] and the
[MIT License][license mit], unless otherwise specified.

Detailed licensing information can be found in the
[COPYRIGHT document][copyright]

[config]: https://github.com/iopsystems/rezolus/blob/main/config.toml
[copyright]: https://github.com/iopsystems/rezolus/blob/main/COPYRIGHT
[create a fork]: https://github.com/iopsystems/rpc-perf/fork
[discord]: https://discord.gg/YC5GDsH4dG
[license apache]: https://github.com/iopsystems/rezolus/blob/main/LICENSE-APACHE
[license mit]: https://github.com/iopsystems/rezolus/blob/main/LICENSE-MIT
[new issue]: https://github.com/iopsystems/rezolus/issues/new
[new pull request]: https://github.com/iopsystems/rpc-perf/compare
[rust-lang.org]: https://www.rust-lang.org/
