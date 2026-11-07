#!/usr/bin/env bash
# Reproduce data/basemap/world.pmtiles: the whole world, down to a chosen zoom,
# cut from the newest Protomaps daily planet build. Idempotent: skips work if
# the output already exists.
#
# Override via environment, e.g.:  MAXZOOM=8 ./scripts/fetch-basemap.sh
#   MAXZOOM deepest zoom to include. World sizes: 7 is 190 MB, 8 is 560 MB,
#           9 is 1.6 GB (the default), 10 is 3.8 GB. The map magnifies the
#           deepest zoom when you look closer.
#   BBOX    west,south,east,north to cut a region instead of the world
#   BUILD   build date YYYYMMDD     (default: newest in builds.json)
#   OUT     output path             (default: data/basemap/world.pmtiles)
#   FORCE=1 re-extract even if OUT exists
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BBOX="${BBOX:-}"
MAXZOOM="${MAXZOOM:-9}"
OUT="${OUT:-$ROOT/data/basemap/world.pmtiles}"
BIN_DIR="$ROOT/tools/bin"

# 1. Locate or download the pmtiles CLI (go-pmtiles).
if command -v pmtiles >/dev/null 2>&1; then
  PMTILES="$(command -v pmtiles)"
elif [ -x "$BIN_DIR/pmtiles" ]; then
  PMTILES="$BIN_DIR/pmtiles"
else
  case "$(uname -s)" in Darwin) os=Darwin ;; Linux) os=Linux ;; *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;; esac
  case "$(uname -m)" in arm64|aarch64) arch=arm64 ;; x86_64|amd64) arch=x86_64 ;; *) echo "unsupported arch: $(uname -m)" >&2; exit 1 ;; esac
  echo "Downloading pmtiles CLI (${os}_${arch}) into $BIN_DIR"
  # Darwin assets are .zip, Linux are .tar.gz (asset names differ slightly).
  url="$(curl -fsSL https://api.github.com/repos/protomaps/go-pmtiles/releases/latest \
    | grep -o "https://[^\"]*_${os}_${arch}\.\(zip\|tar\.gz\)" | head -n1)"
  [ -n "$url" ] || { echo "no release asset found for ${os}_${arch}" >&2; exit 1; }
  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
  curl -fsSL "$url" -o "$tmp/archive"
  case "$url" in
    *.zip) unzip -q "$tmp/archive" pmtiles -d "$tmp" ;;
    *)     tar -xzf "$tmp/archive" -C "$tmp" pmtiles ;;
  esac
  mkdir -p "$BIN_DIR"
  mv "$tmp/pmtiles" "$BIN_DIR/pmtiles"
  chmod +x "$BIN_DIR/pmtiles"
  PMTILES="$BIN_DIR/pmtiles"
fi

# 2. Find the newest Protomaps build.
if [ -z "${BUILD:-}" ]; then
  BUILD="$(curl -fsSL https://build-metadata.protomaps.dev/builds.json \
    | grep -o '"key":"[0-9]\{8\}\.pmtiles"' | grep -o '[0-9]\{8\}' | sort | tail -n1)"
  [ -n "$BUILD" ] || { echo "could not determine newest build" >&2; exit 1; }
fi
SRC="https://build.protomaps.com/${BUILD}.pmtiles"

# 3. Extract (only the needed byte ranges are downloaded).
if [ -s "$OUT" ] && [ "${FORCE:-0}" != 1 ]; then
  echo "$OUT already exists; set FORCE=1 to rebuild."
  exit 0
fi
mkdir -p "$(dirname "$OUT")"
echo "Extracting $SRC ${BBOX:+bbox=$BBOX }maxzoom=$MAXZOOM -> $OUT"
rm -f "$OUT.tmp"
if [ -n "$BBOX" ]; then
  "$PMTILES" extract "$SRC" "$OUT.tmp" --bbox="$BBOX" --maxzoom="$MAXZOOM"
else
  "$PMTILES" extract "$SRC" "$OUT.tmp" --maxzoom="$MAXZOOM"
fi
mv "$OUT.tmp" "$OUT"
"$PMTILES" show "$OUT"
