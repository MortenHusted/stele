"""Owned package ambiguity controls; no compiler or application imports."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("package_profile", Path(__file__).with_name("package_profile.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
module_paths = module.module_paths


def profile(roots, namespaces=True):
    return {"source_roots": roots, "namespace_packages": namespaces, "external_stub_roots": []}


class PackageProfileTests(unittest.TestCase):
    def test_repository_paths_and_declared_root_order(self):
        files = {"src/pkg/__init__.py": b"", "src/pkg/a.py": b"", "lib/b.pyi": b""}
        self.assertEqual(module_paths(files, profile(["lib", "src"], False)),
                         {"b": "lib/b.pyi", "pkg": "src/pkg/__init__.py", "pkg.a": "src/pkg/a.py"})

    def test_namespace_policy_is_explicit(self):
        files = {"src/pkg/a.py": b""}
        self.assertEqual(module_paths(files, profile(["src"])), {"pkg.a": "src/pkg/a.py"})
        with self.assertRaisesRegex(ValueError, "initializer"):
            module_paths(files, profile(["src"], False))

    def test_competing_roots_never_choose_a_winner(self):
        for roots in (["src", "src"], [".", "src"], ["src", "src/nested"]):
            with self.subTest(roots=roots), self.assertRaisesRegex(ValueError, "overlapping"):
                module_paths({}, profile(roots))
        with self.assertRaisesRegex(ValueError, "ambiguous"):
            module_paths({"src/a.py": b"", "lib/a.py": b""}, profile(["src", "lib"]))

    def test_module_package_and_declaration_collisions(self):
        for files in ({"a.py": b"", "a.pyi": b""},
                      {"a.py": b"", "a/__init__.py": b""},
                      {"a.py": b"", "a/b.py": b""}):
            with self.subTest(files=files), self.assertRaises(ValueError):
                module_paths(files, profile(["."]))

    def test_namespace_splits_do_not_extend_regular_packages(self):
        self.assertEqual(module_paths({"src/pkg/a.py": b"", "lib/pkg/b.py": b""},
                                      profile(["src", "lib"])),
                         {"pkg.b": "lib/pkg/b.py", "pkg.a": "src/pkg/a.py"})
        for suffix in (".py", ".pyi"):
            with self.subTest(suffix=suffix), self.assertRaisesRegex(ValueError, "span source roots"):
                module_paths({"src/pkg/__init__" + suffix: b"", "lib/pkg/child.py": b""},
                             profile(["src", "lib"]))

    def test_undeclared_and_invalid_inputs_refuse(self):
        with self.assertRaisesRegex(ValueError, "declared root"):
            module_paths({"other/a.py": b""}, profile(["src"]))
        for roots in ([], [""], ["/src"], ["../src"], ["src/"], ["src\\pkg"]):
            with self.subTest(roots=roots), self.assertRaises(ValueError):
                module_paths({}, profile(roots))
        with self.assertRaisesRegex(ValueError, "module name"):
            module_paths({"src/__init__.py": b""}, profile(["src"]))
        with self.assertRaisesRegex(ValueError, "external stub"):
            module_paths({}, {**profile(["."]), "external_stub_roots": ["stubs"]})
        with self.assertRaisesRegex(ValueError, "explicit package"):
            module_paths({}, {**profile(["."]), "plugins": []})


if __name__ == "__main__":
    unittest.main()
