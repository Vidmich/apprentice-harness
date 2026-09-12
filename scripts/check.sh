#!/usr/bin/env sh
# Local equivalent of the CI checks. Usage: sh scripts/check.sh
set -eu
cd "$(dirname "$0")/.."
just check
