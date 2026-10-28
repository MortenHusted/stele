"""Pinned parser/scope probe. Source bytes are data; targets are never imported."""

import ast
import base64
import codecs
import hashlib
import io
import json
import re
import symtable
import sys
import tokenize


def source_spans(raw, tree):
    encoding, _ = tokenize.detect_encoding(io.BytesIO(raw).readline)
    text = raw.decode(encoding)
    codec = codecs.lookup(encoding).name
    bom = b"\xef\xbb\xbf" if codec == "utf-8-sig" and raw.startswith(b"\xef\xbb\xbf") else b""
    if codec == "utf-8-sig":
        codec = "utf-8"
    lines = re.split(r"(?<=\n)|(?<=\r)(?!\n)", text)
    line_starts = [0]
    for line in lines:
        line_starts.append(line_starts[-1] + len(line.encode("utf-8")))
    wanted = set()
    for node in ast.walk(tree):
        if all(isinstance(getattr(node, field, None), int)
               for field in ("lineno", "col_offset", "end_lineno", "end_col_offset")):
            wanted.add(line_starts[node.lineno - 1] + node.col_offset)
            wanted.add(line_starts[node.end_lineno - 1] + node.end_col_offset)
    offsets = {}
    utf8, original = 0, len(bom)
    encoded = bytearray(bom)
    for character in text:
        if utf8 in wanted:
            offsets[utf8] = original
        part = character.encode(codec)
        encoded.extend(part)
        original += len(part)
        utf8 += len(character.encode("utf-8"))
    if utf8 in wanted:
        offsets[utf8] = original
    if bytes(encoded) != raw or offsets.keys() != wanted:
        raise ValueError("source encoding cannot supply exact original byte spans")

    def span(node):
        first = line_starts[node.lineno - 1] + node.col_offset
        last = line_starts[node.end_lineno - 1] + node.end_col_offset
        return {"start_byte": offsets[first], "end_byte": offsets[last],
                "start_line": node.lineno, "end_line": node.end_lineno}

    return text, encoding, span


def scope_record(table):
    symbols = []
    for symbol in sorted(table.get_symbols(), key=lambda value: value.get_name()):
        symbols.append({"name": symbol.get_name(), "local": symbol.is_local(),
                        "global": symbol.is_global(), "nonlocal": symbol.is_nonlocal(),
                        "free": symbol.is_free(), "parameter": symbol.is_parameter(),
                        "assigned": symbol.is_assigned(), "imported": symbol.is_imported()})
    return {"name": table.get_name(), "line": table.get_lineno(),
            "kind": table.get_type().value, "symbols": symbols,
            "children": [scope_record(child) for child in table.get_children()]}


def parse_file(file, structural=False):
    path = file["path"]
    if not path.endswith((".py", ".pyi")):
        raise ValueError("explicit Python source path required")
    raw = base64.b64decode(file["source_base64"], validate=True)
    if len(raw) > 8 * 1024 * 1024 or hashlib.sha256(raw).hexdigest() != file["sha256"]:
        raise ValueError("source budget or content pin mismatch")
    tree = ast.parse(raw, filename=path, type_comments=True)
    compile(tree, path, "exec", dont_inherit=True)
    text, encoding, span = source_spans(raw, tree)
    table = symtable.symtable(text, path, "exec")
    definitions, calls, imports = [], [], []

    def spelling(node):
        location = span(node)
        codec = "utf-8" if codecs.lookup(encoding).name == "utf-8-sig" else encoding
        return raw[location["start_byte"]:location["end_byte"]].decode(codec)

    def expression(node):
        location = span(node)
        return {"spelling": spelling(node), "span": location,
                "sha256": hashlib.sha256(raw[location["start_byte"]:location["end_byte"]]).hexdigest()}

    def visit(node, scope, caller, context, owner=None):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            qualified = ".".join(scope + [node.name])
            function = not isinstance(node, ast.ClassDef)
            index = len(definitions)
            definition = {"name": node.name, "qualified_name": qualified,
                                "kind": type(node).__name__, "span": span(node),
                                "declaration_only": path.endswith(".pyi"),
                                "decorators": [spelling(value) for value in node.decorator_list]}
            if structural:
                definition.update(owner=owner, decorators=[expression(value) for value in node.decorator_list],
                                  bases=[expression(value) for value in node.bases] if not function else [])
            definitions.append(definition)
            for field, value in ast.iter_fields(node):
                if field == "body":
                    continue
                values = value if isinstance(value, list) else [value]
                for child in values:
                    if isinstance(child, ast.AST):
                        if field in ("returns", "type_params"):
                            visit(child, scope, None,
                                  "annotation" if field == "returns" else "type_parameter", index)
                        else:
                            visit(child, scope, caller, "definition_setup", index)
            for child in node.body:
                visit(child, scope + [node.name], qualified if function else None,
                      "body" if function else "class_body", index)
            return
        if isinstance(node, ast.arg):
            if node.annotation:
                visit(node.annotation, scope, None, "annotation", owner)
            return
        if isinstance(node, ast.AnnAssign):
            visit(node.target, scope, caller, context, owner)
            visit(node.annotation, scope, None, "annotation", owner)
            if node.value:
                visit(node.value, scope, caller, context, owner)
            return
        if isinstance(node, ast.TypeAlias):
            for child in node.type_params:
                visit(child, scope, None, "type_parameter", owner)
            visit(node.value, scope, None, "type_alias", owner)
            return
        if isinstance(node, ast.Lambda):
            for value in node.args.defaults + [value for value in node.args.kw_defaults if value]:
                visit(value, scope, caller, "definition_setup", owner)
            visit(node.body, scope, None, "lambda", owner)
            return
        if isinstance(node, (ast.ListComp, ast.SetComp, ast.DictComp, ast.GeneratorExp)):
            caller, context = None, "comprehension"
        if isinstance(node, ast.Call):
            calls.append({"span": span(node), "callee_span": span(node.func),
                          "callee": spelling(node.func), "callee_kind": type(node.func).__name__,
                          "caller": caller, "evaluation_context": context})
            if structural:
                calls[-1]["owner"] = owner
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            imports.append({"span": span(node), "spelling": spelling(node)})
            if structural:
                imports[-1]["owner"] = owner
        for child in ast.iter_child_nodes(node):
            visit(child, scope, caller, context, owner)

    visit(tree, [], None, "module_body")
    if structural:
        counts, ordinals = {}, {}
        for definition in definitions:
            key = (definition["owner"], definition["name"])
            counts[key] = counts.get(key, 0) + 1
        for definition in definitions:
            key = (definition["owner"], definition["name"])
            definition["ordinal"] = ordinals.get(key, 0)
            definition["ambiguous"] = counts[key] > 1
            ordinals[key] = definition["ordinal"] + 1
    return {"path": path, "sha256": file["sha256"], "encoding": encoding,
            "definitions": definitions, "calls": calls, "imports": imports,
            "scopes": scope_record(table)}


def main():
    if sys.implementation.name != "cpython" or sys.version_info[:3] != (3, 14, 7):
        raise ValueError("require exact operator CPython 3.14.7")
    data = sys.stdin.buffer.read(32 * 1024 * 1024 + 1)
    if len(data) > 32 * 1024 * 1024:
        raise ValueError("probe request budget exceeded")
    files = json.loads(data)["files"]
    if len({file["path"] for file in files}) != len(files):
        raise ValueError("duplicate source path")
    result = {"contract": "codemap-python-source-census/v1", "version": "3.14.7",
              "status": "Parser and scope witnesses only; no target or admission claims",
              "files": [parse_file(file) for file in sorted(files, key=lambda value: value["path"])]}
    output = json.dumps(result, sort_keys=True, separators=(",", ":"))
    if len(output.encode("utf-8")) > 32 * 1024 * 1024:
        raise ValueError("probe response budget exceeded")
    print(output)


if __name__ == "__main__":
    main()
