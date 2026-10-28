"""Owned controls for captured compiler I/O, with no application execution."""
import _io
import ast
import builtins
import importlib.util
import io
import os
from pathlib import Path
import posix
import sys
import types
import unittest


def owned_module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CapturedFileSystem = owned_module("captured_fs").CapturedFileSystem
CompilerReadScope = owned_module("compiler_read_scope").CompilerReadScope


def view():
    return CapturedFileSystem(
        {"{runtime}": "/owned/runtime", "{compiler}": "/owned/compiler", "{source}": "/owned/source"},
        {"{compiler}/VERSIONS": b"builtins: 3.14\n", "{source}/sample.py": b"raise RuntimeError('executed')\n"},
        {"{runtime}": {}, "{compiler}": {"VERSIONS": "file"}, "{source}": {"sample.py": "file"}})


class CompilerReads(unittest.TestCase):
    def test_captured_operations_and_disabled_cache_do_not_need_host_files(self):
        captured = view()
        original = (os.stat, posix.stat, builtins.open, io.open, _io.open, os.getcwd)
        with CompilerReadScope(captured):
            with open("/owned/compiler/VERSIONS") as file:
                self.assertEqual(file.read(), "builtins: 3.14\n")
            with Path("/owned/source/sample.py").open("rb") as file:
                self.assertIsInstance(ast.parse(file.read()), ast.Module)
            self.assertEqual(os.stat("sample.py").st_mtime, 1)
            self.assertEqual(posix.listdir("/owned/source"), ["sample.py"])
            self.assertEqual(os.getcwd(), "/owned/source")
            self.assertFalse(os.path.isdir(os.devnull))
        self.assertEqual(original, (os.stat, posix.stat, builtins.open, io.open, _io.open, os.getcwd))
        self.assertIn(["cache", "{disabled-cache}", "disabled"], captured.ledger())

    def refusal(self, operation):
        captured = view()
        original = os.stat
        with self.assertRaises(ValueError):
            with CompilerReadScope(captured):
                try:
                    operation()
                except ValueError:
                    pass
        self.assertIs(os.stat, original)
        with self.assertRaisesRegex(ValueError, "already refused"):
            captured.ledger()

    def test_ambient_reads_and_unsupported_directory_queries_refuse(self):
        for operation in (lambda: open("/ambient/config"),
                          lambda: os.stat("/ambient/config"),
                          lambda: posix.listdir("/ambient"),
                          lambda: os.scandir("/owned/source"),
                          lambda: posix.open("/owned/source/sample.py", os.O_RDONLY),
                          lambda: open("/owned/source/sample.py", "w"),
                          lambda: open("/owned/source/sample.py", opener=lambda *args: 0)):
            self.refusal(operation)

    def test_saved_open_and_fileio_cannot_bypass_the_audit_hook(self):
        saved_open = builtins.open
        for operation in (lambda: saved_open("/ambient/config", "rb"),
                          lambda: _io.FileIO("/ambient/config", "rb")):
            self.refusal(operation)

    def test_descriptor_and_nontext_queries_poison_the_ledger(self):
        for path in (0, None, b"/owned/source/sample.py"):
            for operation in (lambda: open(path, "rb"),
                              lambda: os.stat(path),
                              lambda: os.listdir(path)):
                self.refusal(operation)

    def test_ast_data_is_allowed_and_execution_is_refused(self):
        code = compile("raise RuntimeError('application executed')", "<owned application>", "exec")
        self.refusal(lambda: exec(code))
        self.refusal(lambda: types.FunctionType(code, {}))

    def test_late_imports_and_module_graph_mutations_refuse(self):
        self.refusal(lambda: __import__("xml.dom.minidom"))
        try:
            self.refusal(lambda: sys.modules.__setitem__("owned_unapproved_module", types.ModuleType("owned_unapproved_module")))
        finally:
            sys.modules.pop("owned_unapproved_module", None)

    def test_scope_is_restored_when_compiler_raises_and_cannot_be_reused(self):
        captured = view()
        scope = CompilerReadScope(captured)
        original = os.stat
        with self.assertRaises(RuntimeError):
            with scope:
                raise RuntimeError("owned compiler failure")
        self.assertIs(os.stat, original)
        with self.assertRaisesRegex(ValueError, "single-use"):
            with scope:
                pass

    def test_disabled_cache_cannot_hide_an_earlier_refusal(self):
        captured = view()
        with self.assertRaisesRegex(ValueError, "already refused"):
            with CompilerReadScope(captured):
                try:
                    os.stat("/ambient/config")
                except ValueError:
                    pass
                os.path.isdir(os.devnull)


if __name__ == "__main__":
    unittest.main()
