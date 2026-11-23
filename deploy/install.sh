#!/usr/bin/env bash
# Installs Card Benefits Tracker as rootless podman containers managed by
# systemd (Quadlet). Run on the Raspberry Pi, as the user that will own the
# service, from the repository root, after building or loading the image:
#
#   podman build -t card-benefits .        # or: podman load -i card-benefits.tar
#   ./deploy/install.sh
#
# Needs podman 4.4+ (Raspberry Pi OS "trixie" or newer) on a 64-bit OS.
# Safe to re-run: existing secrets and settings are kept.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
conf="$HOME/.config/card-benefits"
units="$HOME/.config/containers/systemd"

command -v podman >/dev/null || { echo "podman is not installed (sudo apt install podman)"; exit 1; }
version="$(podman version --format '{{.Client.Version}}')"
major="${version%%.*}"; rest="${version#*.}"; minor="${rest%%.*}"
if [ "$major" -lt 4 ] || { [ "$major" -eq 4 ] && [ "$minor" -lt 4 ]; }; then
  echo "podman $version is too old: Quadlet needs 4.4 or newer."; exit 1
fi
case "$(uname -m)" in
  aarch64|arm64|x86_64) ;;
  *) echo "A 64-bit OS is required (found $(uname -m))."; exit 1 ;;
esac
podman image exists localhost/card-benefits:latest || {
  echo "Image localhost/card-benefits:latest not found. Build it first: podman build -t card-benefits ."; exit 1; }

mkdir -p "$conf" "$units"

if ! podman secret exists cbt-db-password; then
  db_password="$(openssl rand -hex 24)"
  printf '%s' "$db_password" | podman secret create cbt-db-password -
  printf 'postgres://benefits:%s@card-benefits-db:5432/benefits' "$db_password" | podman secret create cbt-database-url -
  echo "Created database secrets."
fi
if ! podman secret exists cbt-encryption-key; then
  openssl rand -base64 32 | tr -d '\n' | podman secret create cbt-encryption-key -
  echo "Created the encryption key. Back it up (see README): without it, linked accounts must be re-linked."
fi
if ! podman secret exists cbt-admin-password; then
  while true; do
    read -r -s -p "Choose the app password (10+ characters): " pw; echo
    read -r -s -p "Again: " pw2; echo
    [ "$pw" = "$pw2" ] && [ "${#pw}" -ge 10 ] && break
    echo "Passwords must match and be at least 10 characters."
  done
  printf '%s' "$pw" | podman secret create cbt-admin-password -
  unset pw pw2
fi

if [ ! -f "$conf/app.env" ]; then
  read -r -p "Email address for sign-in and alerts: " email
  sed "s/^ADMIN_EMAIL=.*/ADMIN_EMAIL=$email/" "$here/app.env.example" > "$conf/app.env"
  chmod 600 "$conf/app.env"
  echo "Wrote $conf/app.env (add Plaid and SMTP settings there)."
fi

cp "$here"/quadlet/* "$units/"

# Keep the service running without an open login session, and across reboots.
loginctl enable-linger "$USER" 2>/dev/null || sudo loginctl enable-linger "$USER"

systemctl --user daemon-reload
systemctl --user restart card-benefits-db.service card-benefits.service

echo
echo "Started. Open http://$(hostname -I 2>/dev/null | awk '{print $1}'):8080 and sign in."
echo "Logs:   journalctl --user -u card-benefits -f"
echo "Status: systemctl --user status card-benefits card-benefits-db"
