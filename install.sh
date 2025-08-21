#!/bin/bash
# Rezolus installer script
# 
# Usage:
#   curl -fsSL https://install.rezolus.com | bash
#   curl -fsSL https://install.rezolus.com | bash -s -- --disable-services
#   curl -fsSL https://install.rezolus.com | bash -s -- -y
#
set -e

# Default values
DISABLE_SERVICES=""
SKIP_CONFIRM=false

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --disable-services)
            DISABLE_SERVICES=true
            shift
            ;;
        -y|--yes)
            SKIP_CONFIRM=true
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --disable-services    Install but disable systemd services"
            echo "  -y, --yes            Skip confirmation prompt (default: services enabled)"
            echo "  -h, --help           Show this help message"
            echo ""
            echo "Examples:"
            echo "  # Interactive installation"
            echo "  curl -fsSL https://install.rezolus.com | bash"
            echo ""
            echo "  # Non-interactive installation with services enabled"
            echo "  curl -fsSL https://install.rezolus.com | bash -s -- -y"
            echo ""
            echo "  # Non-interactive installation with services disabled"
            echo "  curl -fsSL https://install.rezolus.com | bash -s -- -y --disable-services"
            exit 0
            ;;
        *)
            echo "Error: Unknown option $1" >&2
            echo "Use --help for usage information" >&2
            exit 1
            ;;
    esac
done

# Detect the operating system
OS_TYPE="$(uname -s)"

# Check for sudo access early
if [[ "$OS_TYPE" == "Linux" ]]; then
    echo "This installer requires sudo access to configure package repositories and install Rezolus"
    echo "You may be prompted for your password"
    echo ""
    
    # Test sudo access - this will prompt for password if needed
    if ! sudo -v; then
        echo "Error: This installer requires sudo access" >&2
        exit 1
    fi
    
    # Keep sudo alive for the duration of the script
    while true; do sudo -n true; sleep 60; kill -0 "$$" || exit; done 2>/dev/null &
fi

if [[ "$OS_TYPE" == "Darwin" ]]; then
    echo "Error: This installer is for Linux systems only" >&2
    echo "" >&2
    echo "For macOS, please use:" >&2
    echo "  brew install iopsystems/iop/rezolus" >&2
    echo "" >&2
    echo "Or install via Cargo:" >&2
    echo "  cargo install rezolus" >&2
    exit 1
fi

if [[ "$OS_TYPE" != "Linux" ]]; then
    echo "Error: Unsupported operating system: $OS_TYPE" >&2
    echo "This installer only supports Linux" >&2
    exit 1
fi

# Linux installation - detect the distribution and package manager
if [[ ! -f /etc/os-release ]]; then
    echo "Error: Cannot detect Linux distribution" >&2
    echo "Missing /etc/os-release file" >&2
    exit 1
fi

source /etc/os-release
DISTRO=$(echo "$ID" | tr '[:upper:]' '[:lower:]')
CODENAME="${VERSION_CODENAME:-}"
VERSION_ID="${VERSION_ID:-}"

echo "Detected distribution: $DISTRO"

# Determine package manager and repo name
if command -v apt &> /dev/null; then
    PACKAGE_MANAGER="apt"
    case "$DISTRO" in
        debian)
            case "$CODENAME" in
                trixie|bookworm|bullseye)
                    REPO_NAME="debian-${CODENAME}"
                    ;;
                *)
                    echo "Error: Unsupported Debian release: $CODENAME" >&2
                    echo "Supported releases: trixie, bookworm, bullseye" >&2
                    exit 1
                    ;;
            esac
            ;;
        ubuntu)
            case "$CODENAME" in
                focal|jammy|noble)
                    REPO_NAME="ubuntu-${CODENAME}"
                    ;;
                *)
                    echo "Error: Unsupported Ubuntu release: $CODENAME" >&2
                    echo "Supported releases: focal (20.04), jammy (22.04), noble (24.04)" >&2
                    exit 1
                    ;;
            esac
            ;;
        *)
            echo "Error: Unsupported APT-based distribution: $DISTRO" >&2
            echo "Supported distributions: Debian, Ubuntu" >&2
            exit 1
            ;;
    esac
elif command -v dnf &> /dev/null || command -v yum &> /dev/null; then
    # Prefer dnf over yum if both are available
    if command -v dnf &> /dev/null; then
        PACKAGE_MANAGER="dnf"
    else
        PACKAGE_MANAGER="yum"
    fi
    
    case "$DISTRO" in
        rocky)
            MAJOR_VERSION="${VERSION_ID%%.*}"
            if [[ "$MAJOR_VERSION" == "9" ]]; then
                REPO_NAME="rocky9"
            else
                echo "Error: Unsupported Rocky Linux version: $VERSION_ID" >&2
                echo "Supported versions: 9" >&2
                exit 1
            fi
            ;;
        amzn)
            if [[ "$VERSION_ID" == "2023" ]]; then
                REPO_NAME="al2023"
            else
                echo "Error: Unsupported Amazon Linux version: $VERSION_ID" >&2
                echo "Supported versions: 2023" >&2
                exit 1
            fi
            ;;
        *)
            echo "Error: Unsupported RPM-based distribution: $DISTRO" >&2
            echo "Supported distributions: Rocky Linux, Amazon Linux" >&2
            exit 1
            ;;
    esac
else
    echo "Error: No supported package manager found" >&2
    echo "This installer requires apt (Debian/Ubuntu) or dnf/yum (Rocky Linux/Amazon Linux)" >&2
    echo "" >&2
    echo "To install Rezolus without a package manager, use:" >&2
    echo "  cargo install rezolus" >&2
    exit 1
fi

echo "Using repository: $REPO_NAME"
echo "Package manager: $PACKAGE_MANAGER"

# Check if Rezolus is already installed
if command -v rezolus &> /dev/null; then
    echo ""
    echo "Rezolus is already installed (version: $(rezolus --version 2>&1 | head -1))"
    if [[ "$SKIP_CONFIRM" == "false" ]]; then
        if [[ -t 0 ]]; then
            read -p "Continue with reinstallation? [y/N]: " continue_install
        elif [[ -t 1 ]]; then
            read -p "Continue with reinstallation? [y/N]: " continue_install < /dev/tty
        else
            echo "Use -y flag to skip this confirmation" >&2
            exit 1
        fi
        
        if [[ ! "$continue_install" =~ ^[Yy]$ ]]; then
            echo "Installation cancelled"
            exit 0
        fi
    fi
fi

# If DISABLE_SERVICES wasn't set via CLI and we're not skipping confirmation, prompt
if [[ -z "$DISABLE_SERVICES" ]] && [[ "$SKIP_CONFIRM" == "false" ]]; then
    
    # Handle both interactive and piped installations
    if [[ -t 0 ]]; then
        # Interactive mode - stdin is available
        read -p "Enable Rezolus services for continuous metrics collection? [Y/n]: " enable_services
    elif [[ -t 1 ]]; then
        # Piped mode but stdout is available - read from /dev/tty directly
        read -p "Enable Rezolus services for continuous metrics collection? [Y/n]: " enable_services < /dev/tty
    else
        # No terminal available
        echo "Error: Unable to run interactively" >&2
        echo "" >&2
        echo "When piping this script, please run in a terminal that supports interaction" >&2
        echo "Or use command-line options:" >&2
        echo "  curl -fsSL https://install.rezolus.com | bash -s -- -y" >&2
        echo "  curl -fsSL https://install.rezolus.com | bash -s -- -y --disable-services" >&2
        exit 1
    fi

    # Default to yes if user just presses enter
    if [[ -z "$enable_services" ]] || [[ "$enable_services" =~ ^[Yy]$ ]]; then
        DISABLE_SERVICES=false
        echo "Services will be enabled"
        echo ""
    else
        DISABLE_SERVICES=true
        echo "Services will be disabled"
        echo ""
    fi
elif [[ -z "$DISABLE_SERVICES" ]]; then
    # -y was specified but --disable-services wasn't, default to enabling services
    DISABLE_SERVICES=false
fi

# Install based on package manager
case "$PACKAGE_MANAGER" in
    apt)
        echo "Adding Rezolus repository GPG signing key..."
        if ! curl -fsSL https://us-apt.pkg.dev/doc/repo-signing-key.gpg | sudo gpg --yes --dearmor -o /etc/apt/trusted.gpg.d/rezolus-archive-keyring.gpg; then
            echo "Error: Failed to add GPG signing key" >&2
            exit 1
        fi
        
        echo "Adding Rezolus APT repository..."
        echo "deb [arch=amd64] https://us-apt.pkg.dev/projects/rezolus ${REPO_NAME} main" | sudo tee /etc/apt/sources.list.d/rezolus.list > /dev/null
        
        echo "Updating package list..."
        sudo apt update
        
        echo "Installing Rezolus..."
        sudo apt install -y rezolus
        ;;
        
    dnf|yum)
        echo "Adding Rezolus YUM repository..."
        sudo tee /etc/yum.repos.d/rezolus.repo > /dev/null <<EOF
[rezolus]
name=Rezolus Repository
baseurl=https://us-yum.pkg.dev/projects/rezolus/${REPO_NAME}
enabled=1
gpgcheck=1
gpgkey=https://us-yum.pkg.dev/doc/repo-signing-key.gpg
EOF
        
        echo "Installing Rezolus..."
        sudo $PACKAGE_MANAGER install -y rezolus
        ;;
esac

echo ""
echo "Installation completed successfully"

if [[ "$DISABLE_SERVICES" == "true" ]]; then
    echo ""
    echo "Disabling services..."
    sudo systemctl disable --now rezolus.service
    sudo systemctl disable --now rezolus-exporter.service
    echo ""
    echo "Services have been disabled"
    echo ""
    echo "Run 'rezolus --help' for usage information"
    echo ""
    echo "To enable services later:"
    echo "  sudo systemctl enable --now rezolus"
    echo "  sudo systemctl enable --now rezolus-exporter"
    echo ""
    echo "Installation complete"
else
    echo ""
    echo "Enabling and starting services..."
    
    # Enable services in case they were previously disabled
    sudo systemctl enable rezolus.service rezolus-exporter.service
    
    # Start services if they're not already running
    sudo systemctl start rezolus.service rezolus-exporter.service
    
    # Give services a moment to start
    sleep 2
    
    echo "Checking service status..."
    
    # Check if services are running
    REZOLUS_STATUS=$(sudo systemctl is-active rezolus.service 2>/dev/null || echo "inactive")
    EXPORTER_STATUS=$(sudo systemctl is-active rezolus-exporter.service 2>/dev/null || echo "inactive")
    
    if [[ "$REZOLUS_STATUS" == "active" ]] && [[ "$EXPORTER_STATUS" == "active" ]]; then
        echo "Services are running successfully:"
        echo "  rezolus.service: active"
        echo "  rezolus-exporter.service: active"
        echo ""
        echo "Run 'rezolus --help' for usage information"
        echo ""
        echo "Installation complete"
    else
        echo "Warning: One or more services failed to start:" >&2
        if [[ "$REZOLUS_STATUS" != "active" ]]; then
            echo "  rezolus.service: $REZOLUS_STATUS" >&2
        fi
        if [[ "$EXPORTER_STATUS" != "active" ]]; then
            echo "  rezolus-exporter.service: $EXPORTER_STATUS" >&2
        fi
        echo ""
        echo "Check service logs for details:" >&2
        echo "  sudo journalctl -u rezolus.service -n 50" >&2
        echo "  sudo journalctl -u rezolus-exporter.service -n 50" >&2
        echo ""
        echo "Installation completed with errors - services not running"
    fi
fi