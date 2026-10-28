"""Owned source-only bootstrap; supplied application bytes are never executed."""

import hashlib
import importlib
import json
import os
from pathlib import Path
import sys
import unicodedata
import warnings


def publish(path, data):
    path = Path(path)
    pending = path.with_name(path.name + ".pending")
    pending.write_bytes(data)
    pending.replace(path)


if (sys.implementation.name != "cpython" or sys.version_info[:3] != (3, 14, 7)
        or not sys.flags.isolated or not sys.flags.no_site or not sys.dont_write_bytecode):
    raise ValueError("exact isolated CPython 3.14.7 required")
config = json.loads(Path(sys.argv[1]).read_bytes())
runtime = Path(config["runtime"])
if Path(sys.executable) != runtime / "bin/python3.14" or Path(sys.prefix) != runtime:
    raise ValueError("selected private parser runtime required")
expected_paths = [runtime / "lib/python314.zip", runtime / "lib/python3.14",
                  runtime / "lib/python3.14/lib-dynload"]
if list(map(Path, sys.path)) != expected_paths or expected_paths[0].exists():
    raise ValueError("ambient parser import path refused")
if (runtime / "lib/python3.14/site-packages").exists():
    raise ValueError("ambient parser site packages refused")
for logical, digest in config["files"].items():
    if not logical.startswith("{runtime}/"):
        raise ValueError("parser compiler inputs refused")
    path = runtime / logical.removeprefix("{runtime}/")
    if path.stat().st_mtime_ns != 1_000_000_000 or hashlib.sha256(path.read_bytes()).hexdigest() != digest:
        raise ValueError("private parser runtime bytes or metadata changed")
for logical, children in config["directories"].items():
    if logical != "{runtime}" and not logical.startswith("{runtime}/"):
        raise ValueError("parser compiler directory refused")
    path = runtime if logical == "{runtime}" else runtime / logical.removeprefix("{runtime}/")
    actual = {entry.name: "directory" if entry.is_dir() else "file" for entry in path.iterdir()}
    if path.stat().st_mtime_ns != 1_000_000_000 or actual != children:
        raise ValueError("private parser directory inventory or metadata changed")
for name, pin in config["helpers"].items():
    if hashlib.sha256(Path(__file__).with_name(name).read_bytes()).hexdigest() != pin:
        raise ValueError("owned parser helper content mismatch")
sys.path.insert(0, str(Path(__file__).parent))
import probe

# Windows-only codecs are absent from this Darwin execution profile.
for logical in sorted(config["files"]):
    prefix = "{runtime}/lib/python3.14/encodings/"
    if logical.startswith(prefix) and logical.endswith(".py"):
        name = logical.removeprefix(prefix).removesuffix(".py")
        if "/" not in name and name not in {"__init__", "mbcs", "oem"}:
            importlib.import_module("encodings." + name)

warnings.filterwarnings("ignore", category=SyntaxWarning)


def origins():
    result = {}
    for name, module in sorted(sys.modules.items()):
        if name == "__main__":
            continue
        origin = getattr(getattr(module, "__spec__", None), "origin", None)
        if origin in {"built-in", "frozen", None}:
            result[name] = origin
        else:
            path = Path(origin)
            if path.is_relative_to(runtime):
                logical = "{runtime}/" + str(path.relative_to(runtime))
                if logical not in config["files"]:
                    raise ValueError("unmanifested parser runtime module")
            elif path.parent == Path(__file__).parent and path.name in config["helpers"]:
                logical = "{helper}/" + path.name
            else:
                raise ValueError("ambient parser module refused")
            result[name] = logical
    return result


before = origins()
modules = dict(sys.modules)
publish(config["ready"], json.dumps({"contract": "python-parser-ready/1",
                                    "version": list(sys.version_info[:3]),
                                    "origins": before}, sort_keys=True).encode())
pending = str(Path(config["done"]).with_name(Path(config["done"]).name + ".pending"))


def audit(event, args):
    if event == "open":
        if args[0] != pending or args[1] != "w":
            raise ValueError("parser query filesystem access refused")
    elif event == "os.rename":
        if args[:2] != (pending, config["done"]):
            raise ValueError("parser query rename refused")
    elif (event in {"import", "exec", "sys.settrace", "sys.setprofile"}
          or event.startswith(("socket.", "subprocess.", "ctypes.", "os."))):
        raise ValueError("parser query executable operation refused")


sys.addaudithook(audit)
if sys.stdin.buffer.read(1) != b"1":
    raise ValueError("parser query handshake")
raw = sys.stdin.buffer.readline(32 * 1024 * 1024 + 1)
if len(raw) > 32 * 1024 * 1024 or not raw.endswith(b"\n"):
    raise ValueError("parser request budget exceeded")
request = json.loads(raw)
if set(request) not in ({"files"}, {"files", "declarations"}) or not isinstance(request["files"], list):
    raise ValueError("parser source-only request required")
files = request["files"]
if len(files) > 100_000 or len({file["path"] for file in files}) != len(files):
    raise ValueError("parser source inventory budget or uniqueness failed")
declarations = request.get("declarations", [])
if (not isinstance(declarations, list) or any(not isinstance(path, str) for path in declarations)
        or declarations != sorted(set(declarations))
        or any(not path.endswith(".pyi") for path in declarations)
        or not set(declarations) <= {file["path"] for file in files}):
    raise ValueError("explicit unique declaration inputs required")
if any(not file["path"].endswith(".py") and file["path"] not in declarations for file in files):
    raise ValueError("parser source inventory excludes unselected declaration inputs")
result = {"contract": "codemap-python-source-census/v2", "version": "3.14.7",
          "status": "Parser and scope witnesses only; no target or admission claims",
          "files": [probe.parse_file(file, structural=True) for file in sorted(files, key=lambda value: value["path"])]}
if (before != origins() or sys.modules.keys() != modules.keys()
        or any(sys.modules[name] is not module for name, module in modules.items())):
    raise ValueError("parser module closure changed")
output = json.dumps(result, sort_keys=True, separators=(",", ":")).encode()
if len(output) > 32 * 1024 * 1024:
    raise ValueError("parser response budget exceeded")
publish(config["done"], output)
if sys.stdin.buffer.read(1) != b"2":
    raise ValueError("parser exit handshake")
