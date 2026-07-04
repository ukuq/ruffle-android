#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/../.." && pwd)"

cd "$repo_dir/../ruffle/web"
npm ci
npm run build --workspace=ruffle-core
npm run build --workspace=ruffle-selfhosted
