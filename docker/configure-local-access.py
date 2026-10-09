import ipaddress
import json
import os
from pathlib import Path
from urllib.parse import urlsplit


origins = {}
for value in os.environ.get("BUCKYOS_LOCAL_HTTP_ORIGINS", "").split(","):
    origin = value.strip()
    if not origin:
        continue
    parsed = urlsplit(origin)
    if (
        parsed.scheme not in {"http", "https"}
        or parsed.username is not None
        or parsed.password is not None
        or parsed.path
        or parsed.query
        or parsed.fragment
        or parsed.hostname is None
    ):
        raise ValueError("Local access entries must be HTTP(S) IP origins")
    address = ipaddress.ip_address(parsed.hostname)
    authority = f"[{address}]" if address.version == 6 else str(address)
    port = parsed.port
    if port is not None:
        if not 1 <= port <= 65535:
            raise ValueError("Invalid local access port")
        if port != (80 if parsed.scheme == "http" else 443):
            authority = f"{authority}:{port}"
    if origin != f"{parsed.scheme}://{authority}":
        raise ValueError("Local access entries must use canonical IP origins")
    origins[origin] = True

path = Path(os.environ["BUCKYOS_ROOT"]) / "etc" / "local_access_origins.json"
temporary = path.with_suffix(".json.tmp")
temporary.write_text(json.dumps(origins, indent=2) + "\n")
temporary.replace(path)
