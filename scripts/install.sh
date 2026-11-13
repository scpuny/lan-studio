#!/usr/bin/env bash
# Lan Studio lan-mdns installer / upgrader (F-I05 / F-I06 / F-I04).
#
#   curl -fsSL <release>/install.sh | sudo bash
#   sudo ./scripts/install.sh --upgrade          # keep config, credentials, logs
#   sudo ./scripts/install.sh --from ./dist      # use a local release directory
#   sudo ./scripts/install.sh --url <tarball>   # download a release tarball
#   sudo ./scripts/install.sh --no-service      # binary + data only, no systemd
#
# Upgrade NEVER overwrites: config.yaml, auth.toml, logs/, history/.
set -euo pipefail

BIN="lan-mdns"
PREFIX="${LAN_STUDIO_PREFIX:-/opt/lan-studio}"
DATA="${LAN_STUDIO_DATA:-/var/lib/lan-studio}"
UPGRADE=0
NO_SERVICE=0
SOURCE_DIR=""
TARBALL_URL="${LAN_STUDIO_RELEASE_URL:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

usage() {
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --upgrade)    UPGRADE=1; shift ;;
        --no-service) NO_SERVICE=1; shift ;;
        --prefix)     PREFIX="$2"; shift 2 ;;
        --data)       DATA="$2"; shift 2 ;;
        --from)       SOURCE_DIR="$2"; shift 2 ;;
        --url)        TARBALL_URL="$2"; shift 2 ;;
        -h|--help)    usage 0 ;;
        *) echo "ERROR: unknown argument: $1" >&2; usage 1 ;;
    esac
done

if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: installer needs root (try: sudo $0 $*)" >&2
    exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

SRC_BIN=""
if [ -n "$TARBALL_URL" ]; then
    echo "==> downloading $TARBALL_URL"
    curl -fsSL "$TARBALL_URL" -o "$TMP/release.tar.gz"
    tar -xzf "$TMP/release.tar.gz" -C "$TMP"
    SRC_BIN="$(find "$TMP" -type f -name "$BIN" | head -n 1)"
elif [ -n "$SOURCE_DIR" ]; then
    SRC_BIN="$SOURCE_DIR/$BIN"
elif [ -x "$ROOT_DIR/target/release/$BIN" ]; then
    SRC_BIN="$ROOT_DIR/target/release/$BIN"
else
    echo "ERROR: no binary found. Use --from <dir>, --url <tarball>, or run" >&2
    echo "       cargo build --release -p lan-mdns first." >&2
    exit 1
fi

if [ -z "$SRC_BIN" ] || [ ! -f "$SRC_BIN" ]; then
    echo "ERROR: $BIN not found in the selected source" >&2
    exit 1
fi
chmod +x "$SRC_BIN" 2>/dev/null || true
"$SRC_BIN" --version >/dev/null

if ! id -u lanstudio >/dev/null 2>&1; then
    if command -v useradd >/dev/null 2>&1; then
        useradd --system --no-create-home --shell /usr/sbin/nologin lanstudio || true
    fi
fi

install -d -m 0755 "$PREFIX" "$DATA" "$DATA/logs" "$DATA/history"

if [ "$UPGRADE" -eq 1 ]; then
    echo "==> upgrading binary (configuration, credentials and logs are preserved)"
    if [ -x "$PREFIX/$BIN" ]; then
        cp -a "$PREFIX/$BIN" "$PREFIX/$BIN.bak"
    fi
else
    echo "==> installing $BIN to $PREFIX"
fi
install -m 0755 "$SRC_BIN" "$PREFIX/$BIN"

# The reference template is refreshed on every run, but an existing user
# config is NEVER replaced (F-E09). config.yaml may contain admin_token.
"$PREFIX/$BIN" --print-config > "$DATA/config.template.yaml"
if [ ! -f "$DATA/config.yaml" ]; then
    cp "$DATA/config.template.yaml" "$DATA/config.yaml"
    chmod 0600 "$DATA/config.yaml"
    echo "==> generated $DATA/config.yaml from the embedded template"
else
    echo "==> kept existing $DATA/config.yaml"
fi
chown -R lanstudio:lanstudio "$DATA" 2>/dev/null || true

if [ "$NO_SERVICE" -eq 0 ] && command -v systemctl >/dev/null 2>&1; then
    UNIT_SRC="$ROOT_DIR/deploy/lan-mdns.service"
    if [ -f "$UNIT_SRC" ]; then
        sed -e "s#/opt/lan-studio#$PREFIX#g" \
            -e "s#/var/lib/lan-studio#$DATA#g" \
            -e "s#/var/log/lan-studio#$DATA/logs#g" \
            "$UNIT_SRC" > /etc/systemd/system/lan-mdns.service
        systemctl daemon-reload
        systemctl enable lan-mdns.service >/dev/null 2>&1 || true
        if [ "$UPGRADE" -eq 1 ]; then
            systemctl restart lan-mdns.service >/dev/null 2>&1 || true
        else
            systemctl start lan-mdns.service >/dev/null 2>&1 || true
        fi
        echo "==> systemd unit installed: systemctl status lan-mdns"
    fi
fi

echo "==> validating configuration"
LAN_STUDIO_CONFIG="$DATA/config.yaml" "$PREFIX/$BIN" --validate || true
echo "==> done. Useful commands:"
echo "    LAN_STUDIO_CONFIG=$DATA/config.yaml $PREFIX/$BIN --healthcheck"
echo "    curl http://127.0.0.1:8080/healthz"
echo "    $0 --upgrade      # future upgrades keep all user data"
