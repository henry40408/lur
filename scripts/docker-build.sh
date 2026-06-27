#!/usr/bin/env bash
# Build the multi-arch lur image with docker buildx.
#
#   ./scripts/docker-build.sh                       # build linux/amd64,linux/arm64 (validate both, no output)
#   LOAD=true PLATFORMS=linux/amd64 ./scripts/docker-build.sh   # single-arch, loaded into local docker to run
#   PUSH=true TAG=v1.2.3 ./scripts/docker-build.sh   # build + push the multi-arch manifest to $REGISTRY
#
# Env knobs (all optional): REGISTRY, IMAGE, TAG, PLATFORMS, PUSH, LOAD.
set -euo pipefail

REGISTRY="${REGISTRY:-ghcr.io/henry40408}"
IMAGE="${IMAGE:-lur}"
TAG="${TAG:-dev}"
PLATFORMS="${PLATFORMS:-linux/amd64,linux/arm64}"
PUSH="${PUSH:-false}"
LOAD="${LOAD:-false}"

ref="${REGISTRY}/${IMAGE}:${TAG}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# A dedicated builder instance keeps the multi-arch cache off the default one.
docker buildx inspect lur-builder >/dev/null 2>&1 || docker buildx create --name lur-builder >/dev/null

# Stamp `lur --version` with TAG (so a `TAG=v1.2.3` build reports v1.2.3); the
# default "dev" tag yields the same "dev" placeholder build.rs falls back to.
args=(buildx build --builder lur-builder --platform "$PLATFORMS" -t "$ref" --build-arg "LUR_VERSION=${TAG}")
if [ "$PUSH" = "true" ]; then
  args+=(--push)            # multi-arch manifest can only be exported by pushing
elif [ "$LOAD" = "true" ]; then
  args+=(--load)            # --load supports a single platform only
fi
args+=("$root")

echo "+ docker ${args[*]}"
exec docker "${args[@]}"
