"""Owned interpreter/parser guard control; never imports application code."""
import base64
import errno
import hashlib
import importlib.util
import json
from pathlib import Path
import socket
import subprocess
import sys

if sys.implementation.name != "cpython" or sys.version_info[:3] != (3, 14, 7):
    raise ValueError("exact operator CPython 3.14.7 required")
request = json.loads(Path(sys.argv[1]).read_text())
spec = importlib.util.spec_from_file_location("owned_source_probe", Path(__file__).with_name("probe.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)
Path(request["ready"]).write_text("ready")
if sys.stdin.buffer.read(1) != b"1":
    raise ValueError("guard query handshake")


def denied(operation):
    try:
        operation()
    except OSError as error:
        if error.errno in (errno.EPERM, errno.EACCES):
            return True
        raise
    return False


raw = (b"def target(x):\n    return x + 1\n"
       b"def caller():\n    return target(1)\n"
       b"raise RuntimeError('application executed')\n")
parsed = probe.parse_file({"path": "fixture.py", "source_base64": base64.b64encode(raw).decode(),
                           "sha256": hashlib.sha256(raw).hexdigest()})
with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as connection:
    network = denied(lambda: connection.connect(("127.0.0.1", request["port"])))
write = denied(lambda: Path(request["outside"]).write_text("changed"))
execute = denied(lambda: subprocess.run(["/usr/bin/true"], check=True, timeout=5))
assert parsed["calls"][0]["caller"] == "caller"
Path(request["done"]).write_text(json.dumps({"network": network, "write": write,
                                           "execute": execute, "source_parser": True}, sort_keys=True))
if sys.stdin.buffer.read(1) != b"2":
    raise ValueError("guard exit handshake")
