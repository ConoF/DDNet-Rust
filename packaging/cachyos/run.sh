#!/usr/bin/env bash
set -euo pipefail
here="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
export DDNET_DATA_DIR="$here/data"
if [[ "$#" -eq 0 ]]; then
  exec "$here/ddnet-rust" --browse-servers
fi
if [[ "$1" == "--offline" ]]; then
  shift
  exec "$here/ddnet-rust" --play-offline "$here/data/maps/ctf1.map" "$@"
fi
exec "$here/ddnet-rust" "$@"
