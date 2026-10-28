"""Parser compatibility tests; no binding-admission implementation is exercised."""

import base64
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


HELPER = Path(__file__).with_name("probe.py")


def source(raw, path="fixture.py"):
    return {"path": path, "source_base64": base64.b64encode(raw).decode("ascii"),
            "sha256": hashlib.sha256(raw).hexdigest()}


def run(files, startup_traps=False):
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        request, output, error = [root / name for name in ("request", "output", "error")]
        request.write_bytes(json.dumps({"files": files}).encode())
        environment = {}
        if startup_traps:
            trap = "raise RuntimeError('startup hook executed')\n"
            (root / "sitecustomize.py").write_text(trap)
            (root / "usercustomize.py").write_text(trap)
            (root / "json.py").write_text(trap)
            (root / "startup.py").write_text(trap)
            environment = {"PYTHONPATH": str(root), "PYTHONSTARTUP": str(root / "startup.py")}
        with request.open("rb") as stdin, output.open("wb") as stdout, error.open("wb") as stderr:
            result = subprocess.run([sys.executable, "-I", "-S", "-B", str(HELPER)],
                                    stdin=stdin, stdout=stdout, stderr=stderr,
                                    cwd=root, env=environment, timeout=20, check=False)
        return result.returncode, output.read_bytes(), error.read_bytes()


class ParserTests(unittest.TestCase):
    def parsed(self, raw, path="fixture.py"):
        status, output, error = run([source(raw, path)])
        self.assertEqual(status, 0, error.decode())
        return json.loads(output)["files"][0]

    def test_original_byte_spans_across_encodings_and_newlines(self):
        texts = ["# café\ndef outer():\n    return target('🙂')\n",
                 "# coding: latin-1\ndef outer():\n    return target('é')\n",
                 "def outer():\n    return target('é') # type: ignore\n"]
        for index, text in enumerate(texts):
            encoding = "latin-1" if index == 1 else "utf-8"
            for newline in ("\n", "\r\n", "\r"):
                raw = text.replace("\n", newline).encode(encoding)
                for bom in ([b"", b"\xef\xbb\xbf"] if encoding == "utf-8" else [b""]):
                    with self.subTest(index=index, newline=newline, bom=bom):
                        data = bom + raw
                        call = self.parsed(data)["calls"][0]
                        span = call["callee_span"]
                        self.assertEqual(data[span["start_byte"]:span["end_byte"]], b"target")
                        self.assertEqual(span["start_line"], 3 if index < 2 else 2)
                        self.assertEqual(call["caller"], "outer")

    def test_modern_syntax_and_scope_are_observations(self):
        raw = ("type Alias[T] = list[T]\n"
               "def outer[T](value: T):\n"
               "    def target():\n        return 1\n"
               "    text = t'value: {value}'\n"
               "    try:\n        return target()\n"
               "    except ValueError, TypeError:\n        return None\n").encode()
        file = self.parsed(raw)
        self.assertEqual([value["qualified_name"] for value in file["definitions"]],
                         ["outer", "outer.target"])
        self.assertEqual(file["calls"][0]["caller"], "outer")
        self.assertNotIn("target", file["calls"][0])
        self.assertTrue(file["scopes"]["children"])

    def test_nested_execution_contexts_preserve_ownership(self):
        raw = ("def outer(value=setup()):\n"
               "    @decorate()\n    def inner(value=default()):\n        body()\n"
               "    lambda: deferred()\n"
               "    [item() for item in iterable()]\n"
               "    direct()\n").encode()
        calls = {value["callee"]: value for value in self.parsed(raw)["calls"]}
        self.assertEqual(calls["setup"]["caller"], None)
        self.assertEqual(calls["default"]["caller"], "outer")
        self.assertEqual(calls["decorate"]["evaluation_context"], "definition_setup")
        self.assertEqual(calls["body"]["caller"], "outer.inner")
        self.assertEqual(calls["deferred"]["evaluation_context"], "lambda")
        self.assertEqual(calls["item"]["caller"], None)
        self.assertEqual(calls["direct"]["caller"], "outer")

    def test_source_is_never_executed_and_stub_is_declaration_only(self):
        raw = b"import missing_application_plugin\nraise RuntimeError('executed')\ndef declared():\n    target()\n"
        file = self.parsed(raw, "fixture.pyi")
        self.assertTrue(file["definitions"][0]["declaration_only"])
        self.assertEqual(len(file["imports"]), 1)

    def test_isolation_flags_ignore_startup_and_import_path_injection(self):
        status, output, error = run([source(b"import missing_application_plugin\n")],
                                    startup_traps=True)
        self.assertEqual(status, 0, error.decode())
        self.assertEqual(json.loads(output)["files"][0]["imports"][0]["spelling"],
                         "import missing_application_plugin")

    def test_annotations_and_type_aliases_do_not_claim_body_execution(self):
        raw = ("type Alias[T: bound()] = factory()\n"
               "def outer(value: annotation() = default()) -> result():\n"
               "    local: local_type() = body()\n").encode()
        calls = {value["callee"]: value for value in self.parsed(raw)["calls"]}
        for name in ("annotation", "result", "local_type"):
            self.assertEqual(calls[name]["evaluation_context"], "annotation")
            self.assertIsNone(calls[name]["caller"])
        self.assertEqual(calls["factory"]["evaluation_context"], "type_alias")
        self.assertEqual(calls["bound"]["evaluation_context"], "type_parameter")
        self.assertEqual(calls["body"]["caller"], "outer")

    def test_unicode_normalization_does_not_replace_original_spelling(self):
        raw = "def outer():\n    K()\n    target('x\u2028y')\n".encode()
        calls = self.parsed(raw)["calls"]
        self.assertEqual(calls[0]["callee"], "K")
        self.assertEqual(calls[1]["span"]["start_line"], 3)

    def test_rejects_malformed_inputs(self):
        for raw in (b"def broken(:\n", b"return 1\n", b"# coding: missing-codec\n", b"\x00"):
            with self.subTest(raw=raw):
                self.assertNotEqual(run([source(raw)])[0], 0)
        file = source(b"target()\n")
        file["sha256"] = "0" * 64
        self.assertNotEqual(run([file])[0], 0)
        self.assertNotEqual(run([source(b"pass"), source(b"pass")])[0], 0)
        self.assertNotEqual(run([source(b"pass", "fixture.js")])[0], 0)

    def test_file_order_is_byte_deterministic(self):
        files = [source(b"def caller():\n    target()\n", "b.py"), source(b"pass\n", "a.py")]
        first = run(files)
        second = run(list(reversed(files)))
        self.assertEqual(first[0], 0)
        self.assertEqual(first[1], second[1])

    def test_unknown_code_can_rewrite_a_caller_local_binding(self):
        def replacement():
            return "replacement"

        def unknown_operation():
            sys._getframe(1).f_locals["target"] = replacement

        def caller():
            def target():
                return "original"
            unknown_operation()
            return target()

        self.assertEqual(caller(), "replacement")


if __name__ == "__main__":
    unittest.main()
