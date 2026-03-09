#!/usr/bin/env bash
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
  echo "please run as root: sudo ./scripts/uninstall-systemd.sh" >&2
  exit 1
fi

systemctl disable --now gpu-usage-ntfy || true
rm -f /etc/systemd/system/gpu-usage-ntfy.service
systemctl daemon-reload
systemctl reset-failed gpu-usage-ntfy || true

echo "removed systemd unit /etc/systemd/system/gpu-usage-ntfy.service"
echo "kept /etc/gpu-usage-ntfy.env intact"
