#!/usr/bin/env bash

set -euo pipefail

source_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
libexec_dir="${HOME}/.local/libexec/tally-fleet-gate"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/tally-fleet-gate"

install -d -m 0755 "$libexec_dir" "$unit_dir"
install -d -m 0700 "$config_dir"
install -m 0755 "$source_root/test/fleet-gate.sh" "$libexec_dir/fleet-gate.sh"
install -m 0755 "$source_root/test/fleet-gate-poll.sh" "$libexec_dir/fleet-gate-poll.sh"
install -m 0644 \
  "$source_root/contrib/systemd/tally-fleet-gate.service" \
  "$unit_dir/tally-fleet-gate.service"
install -m 0644 \
  "$source_root/contrib/systemd/tally-fleet-gate.timer" \
  "$unit_dir/tally-fleet-gate.timer"

systemctl --user daemon-reload
printf 'Installed tally-fleet-gate.service and tally-fleet-gate.timer.\n'
printf 'Create %s/github-token with mode 0600, then run:\n' "$config_dir"
printf '  systemctl --user enable --now tally-fleet-gate.timer\n'
