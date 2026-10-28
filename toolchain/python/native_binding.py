"""One captured-input native IR engine; parent owns runtime/OS attestation."""
import base64
import encodings.raw_unicode_escape
import encodings.unicode_escape
import hashlib
import os
import pathlib
import unicodedata
import mypy.modulefinder
import probe
from captured_fs import CapturedFileSystem
from captured_fscache import CapturedCache
from compiler_read_scope import CompilerReadScope
from package_profile import module_paths
from mypy.build import BuildSource
from mypy.options import Options
from mypy.nodes import FuncDef
from mypyc.options import CompilerOptions
from mypyc.codegen.emitmodule import parse_and_typecheck, compile_modules_to_ir
from mypyc.irbuild.mapper import Mapper
from mypyc.errors import Errors
from mypyc.irbuild.visitor import IRBuilderVisitor
from mypyc.irbuild.ll_builder import LowLevelIRBuilder
from mypyc.ir.ops import Call


stack = []
captured = []
original_visit = IRBuilderVisitor.visit_call_expr
original_add = LowLevelIRBuilder.add


def observe_call(self, expr):
    stack.append((self.builder, expr))
    try:
        return original_visit(self, expr)
    finally:
        stack.pop()


def observe_op(self, op):
    if isinstance(op, Call) and stack:
        builder, expr = stack[-1]
        if self is builder.builder:
            captured.append((builder.module_name, builder.fn_info.fitem, expr, op))
    return original_add(self, op)


IRBuilderVisitor.visit_call_expr = observe_call
LowLevelIRBuilder.add = observe_op


def capture_sources(roots, operator_files, operator_directories, entries):
    contents = dict(operator_files)
    directories = {path: dict(children) for path, children in operator_directories.items()}
    directories["{source}"] = {}
    files = {}
    for entry in entries:
        path = entry["path"]
        relative = pathlib.PurePosixPath(path)
        if (relative.is_absolute() or ".." in relative.parts or str(relative) != path
                or not path.endswith((".py", ".pyi")) or path in files):
            raise ValueError("unique relative Python input path required")
        raw = base64.b64decode(entry["source_base64"], validate=True)
        if hashlib.sha256(raw).hexdigest() != entry["sha256"]:
            raise ValueError("source content mismatch")
        files[path] = raw
    for path, raw in files.items():
        address = "{source}/" + path
        contents[address] = raw
        parent, name = address.rsplit("/", 1)
        directories.setdefault(parent, {})[name] = "file"
        while parent != "{source}":
            grandparent, name = parent.rsplit("/", 1)
            directories.setdefault(grandparent, {})[name] = "directory"
            parent = grandparent
    return CapturedFileSystem(roots, contents, directories)


def run(view, profile):
    captured.clear()
    files = {}
    for address in view.files:
        if address.startswith("{source}/"):
            raw = view.read(view.roots["{source}"] + "/" + address.removeprefix("{source}/"))
            encoding, _ = probe.tokenize.detect_encoding(probe.io.BytesIO(raw).readline)
            if encoding != "utf-8":
                raise ValueError("native source requires UTF-8 without a BOM")
            files[address.removeprefix("{source}/")] = raw.decode("utf-8")
    scratch = pathlib.Path(view.roots["{source}"])
    paths = module_paths(files, profile)
    def typeshed_paths(data_dir, pyversion, custom_typeshed_dir):
        if pyversion != (3, 14) or custom_typeshed_dir is not None:
            view.refuse("unexpected compiler search policy")
        return [view.roots["{compiler}"] + "/mypy/typeshed/" + relative for relative in
                ("stdlib", "stubs/mypy-extensions", "stubs/librt")]
    original_paths = mypy.modulefinder.default_lib_path
    mypy.modulefinder.default_lib_path = typeshed_paths
    try:
        return compile_query(view, files, scratch, paths, profile)
    finally:
        mypy.modulefinder.default_lib_path = original_paths


def compile_query(view, files, scratch, paths, profile):
    with CompilerReadScope(view):
        sources, parsed = [], {}
        for module, path in sorted(paths.items()):
            source = files[path]
            raw = source.encode("utf-8")
            destination = scratch / path
            parsed[module] = probe.parse_file({
                "path": path, "source_base64": base64.b64encode(raw).decode(),
                "sha256": hashlib.sha256(raw).hexdigest(),
            })
            if path.endswith(".py"):
                sources.append(BuildSource(str(destination), module, source))
        options = Options()
        options.python_version = (3, 14)
        options.python_executable = None
        options.no_site_packages = True
        options.plugins = []
        options.config_file = None
        options.export_types = True
        options.preserve_asts = True
        options.incremental = False
        options.cache_dir = os.devnull
        options.sqlite_cache = False
        search_roots = [str(scratch if root == "." else scratch / root)
                        for root in profile["source_roots"]]
        options.mypy_path = search_roots[1:]
        options.namespace_packages = profile["namespace_packages"]
        options.explicit_package_bases = profile["namespace_packages"]
        options.follow_imports = "skip"
        options.ignore_missing_imports = True
        options.native_parser = False
        for source in sources:
            options.per_module_options[source.module] = {"mypyc": True}
        compiler = CompilerOptions(python_version=(3, 14), target_dir=str(scratch / "build"))
        groups = [(sources, None)]
        mapper = Mapper({source.module: None for source in sources})
        errors = Errors(options)
        result = parse_and_typecheck(sources, options, compiler, groups,
                                     fscache=CapturedCache(view), alt_lib_path=search_roots[0])
        try:
            modules = compile_modules_to_ir(result, mapper, compiler, errors)
            if errors.num_errors:
                raise RuntimeError("compiler diagnostics")
            target_defs = {id(decl): node for node, decl in mapper.func_to_decl.items()
                           if isinstance(node, FuncDef)}
            implemented = {id(fn.decl) for module in modules.values() for fn in module.functions}
            counts = {}
            for module, caller, expr, op in captured:
                key = (module, id(caller), id(expr))
                counts[key] = counts.get(key, 0) + 1
            records, evidence = [], []
            for module, caller, expr, op in captured:
                if counts[(module, id(caller), id(expr))] != 1 or not isinstance(caller, FuncDef):
                    continue
                target = target_defs.get(id(op.fn))
                if (target is None or id(op.fn) not in implemented or op.fn.class_name is not None
                        or target.is_generator or target.is_coroutine):
                    continue
                source_file = parsed[module]
                matching = [call for call in source_file["calls"]
                            if call["span"]["start_line"] == expr.line
                            and call["evaluation_context"] == "body"]
                raw = files[paths[module]].encode("utf-8")
                lines = probe.re.split(rb"(?<=\n)|(?<=\r)(?!\n)", raw)
                # The selected mypy Python parser preserves CPython UTF-8 byte columns.
                offset = sum(len(line) for line in lines[:expr.line - 1]) + expr.column
                if not isinstance(expr.end_line, int) or expr.end_line <= 0 or expr.end_column < 0:
                    continue
                end_offset = sum(len(line) for line in lines[:expr.end_line - 1]) + expr.end_column
                matching = [call for call in matching
                            if call["span"]["start_byte"] == offset
                            and call["span"]["end_byte"] == end_offset]
                owners = [definition for definition in source_file["definitions"]
                          if definition["span"]["start_line"] == caller.line
                          and definition["name"] == caller.name]
                target_module = op.fn.module_name
                if target_module not in parsed:
                    continue
                targets = [definition for definition in parsed[target_module]["definitions"]
                           if definition["span"]["start_line"] == target.line
                           and definition["name"] == target.name
                           and not definition["decorators"] and not definition["declaration_only"]]
                if len(matching) != 1 or len(owners) != 1 or len(targets) != 1:
                    continue
                call = matching[0]
                if call["caller"] != owners[0]["qualified_name"]:
                    continue
                records.append([paths[module], expr.line, call["callee"],
                                paths[target_module], targets[0]["qualified_name"]])
                evidence.append({"path": paths[module], "call": call,
                                 "caller": owners[0], "target_path": paths[target_module],
                                 "target": targets[0], "mechanism": "direct_native_ir_call"})
            evidence.sort(key=lambda item: (item["path"], item["call"]["span"]["start_byte"]))
            eligible = {(item["path"], item["call"]["span"]["start_byte"],
                         item["call"]["span"]["end_byte"]) for item in evidence}
            if len(eligible) != len(evidence):
                raise ValueError("duplicate native source call evidence")
            observations = []
            for module, source_file in parsed.items():
                path = paths[module]
                for call in source_file["calls"]:
                    key = (path, call["span"]["start_byte"], call["span"]["end_byte"])
                    if key in eligible:
                        status, reason = "eligible_candidate", "direct_native_ir_call"
                    elif path.endswith(".pyi"):
                        status, reason = "withheld", "declaration_only_source"
                    elif call["evaluation_context"] != "body" or call["caller"] is None:
                        status, reason = "withheld", "unsupported_evaluation_context"
                    else:
                        status, reason = "withheld", "no_unique_supported_native_body"
                    observations.append({"path": path, "call": call,
                                         "status": status, "reason": reason})
            observations.sort(key=lambda item: (item["path"], item["call"]["span"]["start_byte"],
                                                 item["call"]["span"]["end_byte"]))
            return {"calls": sorted(records), "evidence": evidence, "observations": observations}
        finally:
            result.manager.metastore.close()
