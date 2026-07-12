#!/bin/sh
set -eu

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root: curl -fsSL .../install.sh | sudo bash" >&2
  exit 1
fi

REPO="${KONNECTOR_GITHUB_REPO:-veliuysal/konnector}"
REPO="${REPO#https://github.com/}"
REPO="${REPO#http://github.com/}"
REPO="${REPO%.git}"

apt-get update
apt-get install -y curl ca-certificates libcap2-bin

if [ -n "${KONNECTOR_VERSION:-}" ]; then
  RELEASE_URL="https://api.github.com/repos/${REPO}/releases/tags/${KONNECTOR_VERSION}"
else
  RELEASE_URL="https://api.github.com/repos/${REPO}/releases/latest"
fi

RELEASE_JSON="$(curl -fsSL -H "Accept: application/vnd.github+json" "${RELEASE_URL}")"

DEB_URL="$(printf '%s\n' "${RELEASE_JSON}" | sed -n 's/.*"browser_download_url": "\([^"]*_amd64\.deb\)".*/\1/p' | head -1)"

if [ -z "${DEB_URL}" ]; then
  echo "no .deb package found in ${RELEASE_URL}" >&2
  exit 1
fi

TMP="$(mktemp /tmp/konnector.XXXXXX.deb)"
curl -fsSL -o "${TMP}" "${DEB_URL}"
apt-get install -y "${TMP}"
rm -f "${TMP}"

if [ -x /opt/konnector/current/konnector ]; then
  setcap cap_net_bind_service=+ep /opt/konnector/current/konnector || true
elif [ -x /usr/bin/konnector ]; then
  setcap cap_net_bind_service=+ep /usr/bin/konnector || true
fi

echo "Konnector installed."
command -v konnector >/dev/null && konnector status || true
