#!/usr/bin/env bash
# Build seraphim-VERSION-vendor.tar.xz, the crate bundle seraphim.spec uses
# as Source1, from the release archive of tag vVERSION.
#
#   extra/rpm/make-vendor-tarball.sh 0.1.1 [OUTDIR]
#
# Attach the result to the GitHub release:
#   gh release upload v0.1.1 seraphim-0.1.1-vendor.tar.xz
set -euo pipefail

version=${1:?usage: $0 VERSION [OUTDIR]}
outdir=$(realpath "${2:-.}")
url=https://github.com/PercyThomas1127/Seraphim-terminal-emulator

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

curl -fsSL "$url/archive/refs/tags/v$version.tar.gz" | tar xz -C "$work"
cd "$work/Seraphim-terminal-emulator-$version"
cargo vendor --quiet --locked --versioned-dirs vendor >/dev/null

# Fixed order, owner and times so the same tag always gives the same bytes.
tar --sort=name --mtime='1970-01-01 00:00Z' --owner=0 --group=0 --numeric-owner \
  -cJf "$outdir/seraphim-$version-vendor.tar.xz" vendor
echo "$outdir/seraphim-$version-vendor.tar.xz"
