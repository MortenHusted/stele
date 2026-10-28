"""Run the owned parser on data, without importing or executing corpus modules."""
import base64
import hashlib
import json
from pathlib import Path
import runpy
import sys

if sys.implementation.name != "cpython" or sys.version_info[:3] != (3, 14, 7):
    raise ValueError("require exact CPython 3.14.7")
probe, request_path, root = sys.argv[1:]
parse = runpy.run_path(probe)["parse_file"]
request = json.loads(Path(request_path).read_text())
rejected = []
for file in request:
    path = file["path"]
    raw = Path(root, path).read_bytes()
    if hashlib.sha256(raw).hexdigest() != file["sha256"]:
        raise ValueError("source hash differs from tracked input")
    try:
        parse({**file, "source_base64": base64.b64encode(raw).decode("ascii")})
    except SyntaxError:
        rejected.append(path)
print(json.dumps({"rejected": sorted(rejected), "kept": len(request) - len(rejected)}))
