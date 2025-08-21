# Installation Guide

This guide covers all available methods for installing Rezolus on supported platforms.

## Prerequisites

- **Linux kernel 5.8+** - Required for eBPF-based samplers (CPU, network, block I/O metrics)
- **Root/sudo access** - Required for eBPF programs and system metric collection
- **systemd** - For service management (optional)

## Quick Install (recommended)

The install script will add our repositories and install Rezolus using your
package manager.

**Supported distributions:**
- Debian: 13 (trixie/stable), 12 (bookworm/oldstable), 11 (bullseye/oldoldstable)
- Ubuntu: 20.04 (focal), 22.04 (jammy), 24.04 (noble)
- Rocky Linux: 9
- Amazon Linux: 2023

```bash
curl -fsSL https://install.rezolus.com | bash
```

## macOS

For macOS users, install via Homebrew:

```bash
brew install iopsystems/iop/rezolus
```

Or build from source following the instructions below.

## Package Repository

If you prefer to add our repositories yourself, you may follow the distribution
specific instructions:

### Debian/Ubuntu

```bash
# Add repository and install
source /etc/os-release
DISTRO=$(echo "$ID" | tr '[:upper:]' '[:lower:]')
CODENAME="$VERSION_CODENAME"
curl -fsSL https://us-apt.pkg.dev/doc/repo-signing-key.gpg | sudo gpg --dearmor -o /etc/apt/trusted.gpg.d/rezolus-archive-keyring.gpg
echo "deb [arch=amd64] https://us-apt.pkg.dev/projects/rezolus ${DISTRO}-${CODENAME} main" | sudo tee /etc/apt/sources.list.d/rezolus.list
sudo apt update && sudo apt install rezolus
```

### AmazonLinux/RockyLinux

```bash
# Add repository and install
source /etc/os-release
case "$ID" in
    rocky) REPO_NAME="rocky9" ;;
    amzn) REPO_NAME="al2023" ;;
esac
sudo tee /etc/yum.repos.d/rezolus.repo <<EOF
[rezolus]
name=Rezolus Repository
baseurl=https://us-yum.pkg.dev/projects/rezolus/${REPO_NAME}
enabled=1
gpgcheck=1
gpgkey=https://us-yum.pkg.dev/doc/repo-signing-key.gpg
EOF
sudo dnf install rezolus
```

## Direct Package Download

If you can't add our repository but still wish to use a packaged release, you
may download `.deb` or `.rpm` packages from our [releases page](https://github.com/iopsystems/rezolus/releases/latest)
and install with:

Note: most users will not need the dbgsym packages that contain just debug
symbols.

```bash
# Debian/Ubuntu
sudo dpkg -i rezolus_*.deb

# Rocky/Amazon Linux  
sudo rpm -i rezolus-*.rpm
```

## Building from Source

If you need to build Rezolus from source, you can either build the binary directly or create distribution packages.

### Building the Binary

To build just the Rezolus binary without packaging:

```bash
# Clone the repository
git clone https://github.com/iopsystems/rezolus.git
cd rezolus

# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# Build the release binary
cargo build --release

# The binary will be at ./target/release/rezolus
./target/release/rezolus --version

# Install system-wide (optional)
sudo cp target/release/rezolus /usr/local/bin/
sudo mkdir -p /etc/rezolus
sudo cp config/*.toml /etc/rezolus/

# Install systemd services (optional)
sudo cp debian/rezolus.rezolus.service /etc/systemd/system/rezolus.service
sudo cp debian/rezolus.rezolus-exporter.service /etc/systemd/system/rezolus-exporter.service
sudo cp debian/rezolus.rezolus-hindsight.service /etc/systemd/system/rezolus-hindsight.service
sudo systemctl daemon-reload

# Enable and start services (optional)
sudo systemctl enable --now rezolus
sudo systemctl enable --now rezolus-exporter
```

## Building Packages from Source

If you need to create distribution packages, you can build them on their respective build hosts.

### Building Debian/Ubuntu Packages

On a Debian or Ubuntu system, you can build `.deb` packages using standard Debian build tools:

```bash
# Clone the repository
git clone https://github.com/iopsystems/rezolus.git
cd rezolus

# Install build dependencies
sudo apt update
sudo apt install -y build-essential dpkg-dev devscripts curl

# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# Install additional build dependencies from debian/control
sudo apt build-dep -y .

# Build the packages
dpkg-buildpackage -b -us -uc

# Packages will be in the parent directory
ls -la ../*.deb
```

### Building RPM Packages

On a Rocky Linux or Amazon Linux system, you can build `.rpm` packages:

```bash
# Clone the repository
git clone https://github.com/iopsystems/rezolus.git
cd rezolus

# Install build dependencies
sudo dnf install -y gcc rpm-build jq rsync

# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# Build the release binary
cargo build --release

# Create RPM build environment
mkdir -p ./target/rpmbuild/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

# Prepare the spec file
cp rpm/rezolus.spec.template ./target/rpmbuild/SPECS/rezolus.spec
VERSION=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
sed -i "s/@NAME@/rezolus/g" ./target/rpmbuild/SPECS/rezolus.spec
sed -i "s/@VERSION@/$VERSION/g" ./target/rpmbuild/SPECS/rezolus.spec
sed -i "s/@RELEASE@/1/g" ./target/rpmbuild/SPECS/rezolus.spec
sed -i "s/@ARCH@/$(uname -m)/g" ./target/rpmbuild/SPECS/rezolus.spec

# Copy source files to the build directory (excluding target)
rsync -av --exclude='target/' --exclude='.git/' . ./target/rpmbuild/SOURCES/

# Build the RPM
rpmbuild --define "_topdir $(pwd)/target/rpmbuild" -bb ./target/rpmbuild/SPECS/rezolus.spec

# Packages will be in ./target/rpmbuild/RPMS/
ls -la ./target/rpmbuild/RPMS/*/*.rpm
```

## Post-Installation

### Verify Installation

Check that Rezolus is installed and running:

```bash
# Check version
rezolus --version

# Check service status (if using systemd)
sudo systemctl status rezolus
sudo systemctl status rezolus-exporter

# View metrics (exporter endpoint)
curl http://localhost:4242/metrics
```

### Configuration

Configuration files are located in `/etc/rezolus/`:

- `agent.toml` - Main agent configuration (samplers, intervals)
- `exporter.toml` - Prometheus exporter settings
- `hindsight.toml` - Hindsight service configuration

To modify which samplers are enabled or adjust collection intervals, edit `/etc/rezolus/agent.toml` and restart the service:

```bash
sudo systemctl restart rezolus
```

### Troubleshooting

If services fail to start, check the logs:

```bash
# View service logs
sudo journalctl -u rezolus -n 50
sudo journalctl -u rezolus-exporter -n 50

# Check kernel version (for eBPF support)
uname -r

# Verify eBPF is available
ls /sys/fs/bpf/
```
