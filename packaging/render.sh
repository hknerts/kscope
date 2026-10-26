#!/usr/bin/env bash
# Renders the Homebrew formula, krew manifest and Scoop manifest for a
# published release, with the checksums from its .sha256 assets.
#
#   packaging/render.sh 1.7.0 dist/
#
# Writes dist/kscope.rb, dist/kscope.yaml (krew) and dist/kscope.json (Scoop).
# The release workflow runs this and attaches the three files to the release.
set -euo pipefail

version=${1:?usage: $0 VERSION OUTDIR}
out=${2:?usage: $0 VERSION OUTDIR}
version=${version#v}
repo=${KSCOPE_REPO:-hknerts/kscope}
here=$(cd "$(dirname "$0")" && pwd)

targets=(
  x86_64-unknown-linux-gnu:tar.gz
  aarch64-unknown-linux-gnu:tar.gz
  x86_64-apple-darwin:tar.gz
  aarch64-apple-darwin:tar.gz
  x86_64-pc-windows-msvc:zip
)

subst=(-e "s|@VERSION@|$version|g")
for entry in "${targets[@]}"; do
  target=${entry%%:*} ext=${entry#*:}
  url="https://github.com/$repo/releases/download/$version/kscope-$target.$ext.sha256"
  sha=$(curl -sSfL --retry 3 "$url" | awk '{print tolower($1); exit}')
  if ! [[ $sha =~ ^[0-9a-f]{64}$ ]]; then
    echo "no valid checksum at $url" >&2
    exit 1
  fi
  subst+=(-e "s|@SHA256_${target}@|$sha|g")
done

mkdir -p "$out"
sed "${subst[@]}" "$here/homebrew/kscope.rb.in"  > "$out/kscope.rb"
sed "${subst[@]}" "$here/krew/kscope.yaml.in"    > "$out/kscope.yaml"
sed "${subst[@]}" "$here/scoop/kscope.json.in"   > "$out/kscope.json"

if grep -n '@[A-Z0-9_a-z-]*@' "$out"/kscope.{rb,yaml,json}; then
  echo "unrendered placeholders left" >&2
  exit 1
fi
echo "rendered $version into $out"
