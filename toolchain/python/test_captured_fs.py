"""Owned input-view controls; application code is never executed."""
import hashlib
import importlib.util
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("captured_fs", Path(__file__).with_name("captured_fs.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
CapturedFileSystem = module.CapturedFileSystem


def inputs(base="/owned"):
    roots = {name: base + "/" + name[1:-1] for name in ("{runtime}", "{compiler}", "{source}")}
    files = {"{runtime}/cache.pyc": b"\x00\xff\x80", "{compiler}/stub.pyi": b"def f() -> int: ...\n",
             "{source}/pkg/main.py": b"raise RuntimeError('application executed')\n"}
    directories = {"{runtime}": {"cache.pyc": "file"}, "{compiler}": {"stub.pyi": "file"},
                   "{source}": {"pkg": "directory", "empty": "directory"},
                   "{source}/pkg": {"main.py": "file"}, "{source}/empty": {}}
    return roots, files, directories


class CapturedInputs(unittest.TestCase):
    def test_no_host_reads_and_relocation_has_identical_observations(self):
        ledgers = []
        for base in ("/not-present", "/other path é"):
            roots, files, directories = inputs(base)
            with patch("builtins.open", side_effect=AssertionError("host open")), patch(
                    "os.stat", side_effect=AssertionError("host stat")), patch(
                    "os.listdir", side_effect=AssertionError("host listdir")):
                view = CapturedFileSystem(roots, files, directories)
                self.assertEqual(view.read(base + "/runtime/cache.pyc"), b"\x00\xff\x80")
                self.assertEqual(view.listdir(base + "/source"), ["empty", "pkg"])
                self.assertIsNone(view.stat_or_none(base + "/source/absent/deep.py"))
                self.assertIsNone(view.stat_or_none(base + "/source/pkg/Main.py"))
                metadata = view.stat_or_none(base + "/source/pkg/main.py")
                self.assertTrue(stat.S_ISREG(metadata.st_mode))
                self.assertEqual(metadata.st_size, len(files["{source}/pkg/main.py"]))
                self.assertEqual((metadata.st_atime, metadata.st_mtime, metadata.st_ctime), (1, 1, 1))
                ledgers.append(view.ledger())
        self.assertEqual(ledgers[0], ledgers[1])
        self.assertIn(["read", "{runtime}/cache.pyc", hashlib.sha256(b"\x00\xff\x80").hexdigest()], ledgers[0])

    def test_original_changes_and_aliases_cannot_change_captured_answers(self):
        with tempfile.TemporaryDirectory() as directory:
            roots, files, directories = inputs(directory)
            view = CapturedFileSystem(roots, files, directories)
            source = Path(roots["{source}"])
            (source / "pkg").mkdir(parents=True)
            (source / "pkg/main.py").write_bytes(b"different")
            (source / "added.py").write_bytes(b"added")
            alias = Path(directory) / "alias"
            alias.symlink_to(source, target_is_directory=True)
            self.assertEqual(view.read(str(source / "pkg/main.py")), files["{source}/pkg/main.py"])
            self.assertIsNone(view.stat_or_none(str(source / "added.py")))
            self.assertEqual(view.listdir(str(source)), ["empty", "pkg"])
            with self.assertRaisesRegex(ValueError, "undeclared"):
                view.read(str(alias / "pkg/main.py"))

    def test_negative_queries_are_closed_and_errors_preserve_kind(self):
        view = CapturedFileSystem(*inputs())
        self.assertEqual(view.listdir("/owned/source/empty"), [])
        for operation in (view.read, view.listdir):
            with self.assertRaises(FileNotFoundError):
                operation("/owned/source/missing")
            with self.assertRaises(NotADirectoryError):
                operation("/owned/source/pkg/main.py/child")
        with self.assertRaises(IsADirectoryError):
            view.read("/owned/source/pkg")
        with self.assertRaises(NotADirectoryError):
            view.listdir("/owned/source/pkg/main.py")
        for path in ("/usr/local/lib/mypy", "/owned/source-other", "/owned/source/../../escape"):
            for name in ("read", "listdir", "stat_or_none"):
                refused = CapturedFileSystem(*inputs())
                with self.assertRaisesRegex(ValueError, "undeclared"):
                    getattr(refused, name)(path)
                with self.assertRaisesRegex(ValueError, "already refused"):
                    refused.ledger()
                with self.assertRaisesRegex(ValueError, "already refused"):
                    refused.read("/owned/runtime/cache.pyc")
        self.assertIn(["read", "{source}/missing", "absent"], view.ledger())

    def test_inventories_must_be_connected_complete_and_normalized(self):
        roots, files, directories = inputs()
        for changed in ({**directories, "{source}/pkg": {}},
                        {**directories, "{source}": {"pkg": "directory"}},
                        {path: entries for path, entries in directories.items() if path != "{source}/pkg"},
                        {**directories, "{source}/orphan/deep": {}},
                        {**directories, "{source}/pkg/main.py": {}}):
            with self.assertRaises(ValueError):
                CapturedFileSystem(roots, files, changed)
        for path in ("{source}/../escape", "{source}//x", "{source}/./x", "{source}/x\\y", "{source}/x\0y"):
            with self.assertRaises(ValueError):
                CapturedFileSystem(roots, {**files, path: b""}, directories)
        for source in ("/owned/runtime", "/owned/runtime/child", "/owned", "/", "relative"):
            with self.assertRaises(ValueError):
                CapturedFileSystem({**roots, "{source}": source}, files, directories)

    def test_inputs_and_returned_lists_do_not_expose_mutable_storage(self):
        roots, files, directories = inputs()
        view = CapturedFileSystem(roots, files, directories)
        files["{runtime}/cache.pyc"] = b"changed"
        directories["{source}"]["added.py"] = "file"
        roots["{source}"] = "/changed"
        names = view.listdir("/owned/source")
        names.clear()
        self.assertEqual(view.listdir("/owned/source"), ["empty", "pkg"])
        self.assertEqual(view.read("/owned/runtime/cache.pyc"), b"\x00\xff\x80")
        with self.assertRaises(TypeError):
            view.directories["{source}"]["added"] = "file"

    def test_empty_directory_additions_change_inventory_observations(self):
        roots, files, directories = inputs()
        before = CapturedFileSystem(roots, files, directories)
        before.listdir("/owned/source")
        directories["{source}"]["new"] = "directory"
        directories["{source}/new"] = {}
        after = CapturedFileSystem(roots, files, directories)
        self.assertEqual(after.listdir("/owned/source"), ["empty", "new", "pkg"])
        self.assertNotEqual(before.ledger(), after.ledger())

    def test_observation_budget_counts_repeats_and_ledger_has_stable_order(self):
        view = CapturedFileSystem(*inputs(), max_observations=2)
        for _ in range(2):
            self.assertIsNone(view.stat_or_none("/owned/source/missing"))
        self.assertEqual(view.ledger(), [["stat", "{source}/missing", "absent"]])
        with self.assertRaisesRegex(ValueError, "observation budget"):
            view.stat_or_none("/owned/source/missing")
        with self.assertRaisesRegex(ValueError, "already refused"):
            view.ledger()
        for invalid in (0, -1, True, 100001):
            with self.assertRaises(ValueError):
                CapturedFileSystem(*inputs(), max_observations=invalid)


if __name__ == "__main__":
    unittest.main()
