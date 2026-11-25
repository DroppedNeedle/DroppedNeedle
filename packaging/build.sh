#!/usr/bin/env bash
# Builds the .deb and the .rpm from a release's files.
#
#   packaging/build.sh <arch> <version> <server-dir> <web-ui-dir> <out-dir>
#
# <arch> is amd64 or arm64. <version> is the release tag, such as v3.2.0 or
# v3.2.0-beta.1; a pre-release becomes 3.2.0~beta.1, so it sorts before
# 3.2.0. <server-dir> holds the two binaries, LICENSE and NOTICES-server.txt,
# and <web-ui-dir> holds static/ and NOTICES-web-ui.txt, as in the release
# tarballs. The packages are written to <out-dir>.
set -euo pipefail

if [[ $# -ne 5 ]]; then
    echo "usage: $0 <arch> <version> <server-dir> <web-ui-dir> <out-dir>" >&2
    exit 2
fi
arch=$1 tag=$2 server=$3 webui=$4 out=$5

case "$arch" in
    amd64 | arm64) ;;
    *) echo "unknown arch '$arch': amd64 or arm64" >&2; exit 2 ;;
esac
if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$ ]]; then
    echo "'$tag' is not a version tag (v1.2.3 or v1.2.3-beta.1)" >&2
    exit 2
fi
version=${tag#v}
version=${version/-/\~}

for file in "$server/droppedneedle" "$server/droppedneedle-tool" "$server/LICENSE" \
    "$server/NOTICES-server.txt" "$webui/static/index.html" "$webui/NOTICES-web-ui.txt"; do
    if [[ ! -f "$file" ]]; then
        echo "missing $file" >&2
        exit 1
    fi
done

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# nfpm, pinned by version and checksum, for the machine doing the build.
nfpm_version=2.49.0
case "$(uname -m)" in
    x86_64) nfpm_arch=x86_64 nfpm_sha256=2afaa5ffb5e3493402e31e7c67a5a1fb42133e3f66fc315ca0051934125561c7 ;;
    aarch64) nfpm_arch=arm64 nfpm_sha256=d4eee71839e92c68cbd3b101640bad32d2856fc551acae0635be9118e48c9d9c ;;
    *) echo "no pinned nfpm for $(uname -m)" >&2; exit 1 ;;
esac
nfpm_tar="nfpm_${nfpm_version}_Linux_${nfpm_arch}.tar.gz"
curl -fsSL --retry 3 -o "$work/$nfpm_tar" \
    "https://github.com/goreleaser/nfpm/releases/download/v${nfpm_version}/${nfpm_tar}"
echo "$nfpm_sha256  $work/$nfpm_tar" | sha256sum -c --quiet -
tar -xzf "$work/$nfpm_tar" -C "$work" nfpm

# The folder nfpm.yaml's source paths are relative to.
stage="$work/stage"
mkdir -p "$stage/packaging/scripts"
cp -R "$server" "$stage/server"
cp -R "$webui" "$stage/web-ui"
cp "$here/droppedneedle-tool" "$here/droppedneedle.env" "$here/sysusers.conf" "$stage/packaging/"
cp "$here"/scripts/*.sh "$stage/packaging/scripts/"
sed "s|@COMMIT_TAG@|$tag|" "$here/droppedneedle.service.in" > "$stage/packaging/droppedneedle.service"

mkdir -p "$out"
out=$(cd "$out" && pwd)
for packager in deb rpm; do
    (cd "$stage" && VERSION=$version ARCH=$arch "$work/nfpm" package \
        --config "$here/nfpm.yaml" --packager "$packager" --target "$out/")
done
ls -l "$out"
