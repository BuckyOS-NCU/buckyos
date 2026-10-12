#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
container="${BUCKYOS_CONTAINER:-buckyos}"
image="${BUCKYOS_AIOS_IMAGE:-buckyos-aios-local:latest-amd64}"
context="$(docker exec "$container" mktemp -d /tmp/buckyos-aios-runtime.XXXXXX)"
cleanup() {
  docker exec "$container" python3 -c 'import pathlib,shutil,sys; p=pathlib.Path(sys.argv[1]); assert p.parent == pathlib.Path("/tmp") and p.name.startswith("buckyos-aios-runtime."); shutil.rmtree(p)' "$context"
}
trap cleanup EXIT

docker exec "$container" sh -eu -c '
  mkdir -p "$1/opendan" "$1/sdk"
  cp -a /opt/buckyos/bin/opendan/. "$1/opendan/"
  cp -a /opt/buckyos/libexec/buckyos-tool/dist/. "$1/sdk/"
' sh "$context"
if [[ -n "${BUCKYOS_OPENDAN_BINARY:-}" ]]; then
  if [[ ! -f "$BUCKYOS_OPENDAN_BINARY" || ! -x "$BUCKYOS_OPENDAN_BINARY" ]]; then
    printf '%s\n' 'BUCKYOS_OPENDAN_BINARY must name an executable file' >&2
    exit 1
  fi
  docker cp "$BUCKYOS_OPENDAN_BINARY" "$container:$context/opendan/opendan"
fi
docker cp "$repo_root/docker/aios-runtime.Dockerfile" "$container:$context/Dockerfile"
docker cp "$repo_root/publish/aios/aicc-tool" "$container:$context/aicc-tool"
docker cp "$repo_root/publish/aios/entrypoint.sh" "$container:$context/entrypoint.sh"
docker cp "$repo_root/src/tools/buckyos-agent" "$container:$context/buckyos-agent"
docker exec "$container" python3 -c '
import json,pathlib,sys
p=pathlib.Path(sys.argv[1])/"buckyos-agent/deno.json"
d=json.loads(p.read_text())
d.setdefault("imports",{})["buckyos"]="./vendor/buckyos-websdk/node.mjs"
p.write_text(json.dumps(d,indent=2)+"\n")
' "$context"
docker exec "$container" docker build -t "$image" "$context"
docker exec "$container" docker image inspect "$image" --format '{{.Id}}'
