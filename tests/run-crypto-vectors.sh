#!/usr/bin/env bash
set -euo pipefail

echo "[T1.4] Vectores criptográficos (debug)"
cargo test -p meridian-crypto

echo "[T1.4] Vectores criptográficos (release)"
cargo test -p meridian-crypto --release

echo "[OK] T1.4 vectores criptográficos"
