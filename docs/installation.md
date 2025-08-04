# Using Rezolus in Production

A user's guide to deploying and using Rezolus in production environments.

## Installation

There are a few ways to install Rezolus. Ultimately, the goal is to get
some combination of the Rezolus Agent running on the hosts you want to observe.

If you plan on consuming Rezolus metrics with your own observability stack, you
will want to run the Rezolus Exporter which exposes metrics on a Prometheus
compatible HTTP endpoint.

You may also want to use some combination of the Rezolus Recorder and Hindsight
service for gathering metrics artifacts. Recorder based workflows are commonly
used for ad-hoc collection. Hindsight is designed for "after-the-fact"
collection of telemetry artifacts.

See the [README] for an overview of the operating modes.

In any case, Rezolus is designed as a single binary that contains the
functionalitity for all of the operating modes. This keeps deployment simple and
allows for easily using any Rezolus functionality through a single tool.

### Quick Install (recommended)

The install script will add our repositories and install Rezolus using your
package manager.

**Supported distributions:**
- Debian: 12 (bookworm/stable), 11 (bullseye/oldstable)
- Ubuntu: 20.04 (focal), 22.04 (jammy), 24.04 (noble)
- Rocky Linux: 9
- Amazon Linux: 2023

```bash
curl -fsSL https://install.rezolus.com | bash
```

### Package Repository

If you prefer to add our repositories yourself, you may follow the distribution
specific instructions:

#### Debian/Ubuntu

```bash
# Add repository and install
source /etc/os-release
DISTRO=$(echo "$ID" | tr '[:upper:]' '[:lower:]')
CODENAME="$VERSION_CODENAME"
curl -fsSL https://us-apt.pkg.dev/doc/repo-signing-key.gpg | sudo gpg --dearmor -o /etc/apt/trusted.gpg.d/gcp-artifact-registry.gpg
echo "deb [arch=amd64] https://us-apt.pkg.dev/projects/rezolus ${DISTRO}-${CODENAME} main" | sudo tee /etc/apt/sources.list.d/rezolus.list
sudo apt update && sudo apt install rezolus
```

#### AmazonLinux/RockyLinux

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

### Direct Package Download

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

