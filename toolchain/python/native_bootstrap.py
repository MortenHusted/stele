"""Owned compiler bootstrap control; application modules are never imported."""
import ast
import encodings.raw_unicode_escape
import encodings.unicode_escape
import hashlib
import json
import os
from pathlib import Path
import sys
import unicodedata


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
compiler = Path(config["compiler"])
if Path(sys.executable) != runtime / "bin/python3.14" or Path(sys.prefix) != runtime:
    raise ValueError("selected private runtime required")
expected_paths = [runtime / "lib/python314.zip", runtime / "lib/python3.14",
                  runtime / "lib/python3.14/lib-dynload"]
if list(map(Path, sys.path)) != expected_paths or expected_paths[0].exists():
    raise ValueError("ambient interpreter path refused")
if (runtime / "lib/python3.14/site-packages").exists():
    raise ValueError("ambient site packages refused")

def address(path):
    path = Path(path)
    for root, logical in ((runtime, "{runtime}"), (compiler, "{compiler}")):
        if path.is_relative_to(root):
            return logical + "/" + str(path.relative_to(root))
    if path.parent == Path(__file__).parent and path.name in config["helpers"]:
        return "{helper}/" + path.name
    raise ValueError("ambient bootstrap module refused")

contents = {}
for logical, digest in config["files"].items():
    root, relative = logical.split("/", 1)
    path = {"{runtime}": runtime, "{compiler}": compiler}[root] / relative
    raw = path.read_bytes()
    if path.stat().st_mtime_ns != 1_000_000_000 or hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError("private operator bytes or modification time changed")
    contents[logical] = raw
for logical, children in config["directories"].items():
    root, _, relative = logical.partition("/")
    path = {"{runtime}": runtime, "{compiler}": compiler}[root] / relative
    actual = {entry.name: "directory" if entry.is_dir() else "file" for entry in path.iterdir()}
    if path.stat().st_mtime_ns != 1_000_000_000 or actual != children:
        raise ValueError("private operator directory inventory or modification time changed")
sys.path[:0] = [str(compiler), str(compiler / "dependencies")]
for name, pin in config["helpers"].items():
    if hashlib.sha256(Path(__file__).with_name(name).read_bytes()).hexdigest() != pin:
        raise ValueError("owned helper content mismatch")
sys.path.insert(0, str(Path(__file__).parent))
import native_binding
from mypyc.codegen.emitmodule import parse_and_typecheck, compile_modules_to_ir
from mypyc.irbuild.visitor import IRBuilderVisitor
from mypyc.irbuild.ll_builder import LowLevelIRBuilder
from mypy.version import __version__
if __version__ != "2.3.1":
    raise ValueError("exact compiler version required")
origins = {}
for name, module in sorted(sys.modules.items()):
    if name == "__main__":
        continue
    origin = getattr(getattr(module, "__spec__", None), "origin", None)
    if origin in {"built-in", "frozen", None}:
        origins[name] = origin
    else:
        logical = address(origin)
        if logical not in config["files"] and not (
                logical.startswith("{helper}/") and logical.removeprefix("{helper}/") in config["helpers"]):
            raise ValueError("unmanifested bootstrap module")
        origins[name] = logical
publish(config["ready"], json.dumps({"version": list(sys.version_info[:3]),
                                   "compiler": __version__, "origins": origins}, sort_keys=True).encode())
if sys.stdin.buffer.read(1) != b"1":
    raise ValueError("bootstrap query handshake")
request_bytes = sys.stdin.buffer.readline(32 * 1024 * 1024 + 1)
if len(request_bytes) > 32 * 1024 * 1024 or not request_bytes.endswith(b"\n"):
    raise ValueError("native query request budget")
request = json.loads(request_bytes)
view = native_binding.capture_sources(
    {"{runtime}": str(runtime), "{compiler}": str(compiler),
     "{source}": str(Path(__file__).parent / "query")},
    contents, config["directories"], request["files"])
result = native_binding.run(view, request["package_profile"])
result["read_ledger"] = view.ledger()
result["canonical_admission"] = False
outside = Path(config["outside"])
for operation in (outside.read_bytes, outside.stat, lambda: list(os.scandir(outside.parent))):
    try:
        operation()
    except PermissionError:
        pass
    else:
        raise ValueError("undeclared OS read guard failed")
try:
    (runtime / "write-control").write_text("unapproved")
except PermissionError:
    pass
else:
    raise ValueError("operator root write guard failed")
output = json.dumps(result, sort_keys=True, separators=(",", ":")).encode()
if len(output) > 32 * 1024 * 1024:
    raise ValueError("native query response budget")
publish(config["done"], output)
if sys.stdin.buffer.read(1) != b"2":
    raise ValueError("bootstrap exit handshake")
