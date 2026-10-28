"""Immutable compiler input view; not complete runtime closure or admission."""
import errno
import hashlib
import os
import stat
from types import MappingProxyType


class CapturedFileSystem:
    def __init__(self, roots, files, directories, *, max_observations=100000):
        if set(roots) != {"{runtime}", "{compiler}", "{source}"}:
            raise ValueError("exact Python input roots required")
        self.roots = dict(roots)
        for root in self.roots.values():
            if (not isinstance(root, str) or root == "/" or not root.startswith("/")
                    or "\\" in root or "\0" in root or os.path.normpath(root) != root):
                raise ValueError("normalized absolute input root required")
        paths = list(self.roots.values())
        for index, first in enumerate(paths):
            for second in paths[:index]:
                if first == second or first.startswith(second + "/") or second.startswith(first + "/"):
                    raise ValueError("disjoint input roots required")
        if type(max_observations) is not int or not 0 < max_observations <= 100000:
            raise ValueError("observation budget required")
        self.limit = max_observations
        self.count = 0
        self.refused = False
        self.observations = set()
        self.files = {}
        self.directories = {}
        if len(files) + len(directories) > 100000:
            raise ValueError("input entry budget")
        total = 0
        for path, data in files.items():
            self.validate_address(path)
            limit = 8 if path.startswith("{source}/") else 128
            if path in roots or type(data) is not bytes or len(data) > limit * 1024 * 1024:
                raise ValueError("bounded captured file bytes required")
            total += len(data)
            if total > 512 * 1024 * 1024:
                raise ValueError("input byte budget")
            self.files[path] = data
        for path, entries in directories.items():
            self.validate_address(path)
            self.directories[path] = dict(entries)
        if set(self.files) & set(self.directories) or not set(roots) <= set(self.directories):
            raise ValueError("distinct files and complete root directories required")
        derived = {path: {} for path in self.directories}
        for path, kind in [(path, "file") for path in self.files] + [
                (path, "directory") for path in self.directories if path not in roots]:
            parent, name = path.rsplit("/", 1)
            if parent not in derived:
                raise ValueError("connected directory inventory required")
            derived[parent][name] = kind
        if derived != self.directories:
            raise ValueError("complete directory inventory required")
        self.roots = MappingProxyType(self.roots)
        self.files = MappingProxyType(self.files)
        self.directories = MappingProxyType({
            path: MappingProxyType(entries) for path, entries in self.directories.items()})
        self.inodes = MappingProxyType({path: index + 1 for index, path in enumerate(sorted(
            set(self.files) | set(self.directories)))})
        self.digests = MappingProxyType({path: hashlib.sha256(data).hexdigest()
                                        for path, data in self.files.items()})
        self.directory_digests = MappingProxyType({
            path: hashlib.sha256("\0".join(
                name + "\0" + kind for name, kind in sorted(entries.items())).encode()).hexdigest()
            for path, entries in self.directories.items()})

    def validate_address(self, path):
        if not isinstance(path, str) or "\\" in path or "\0" in path:
            raise ValueError("normalized logical input address required")
        parts = path.split("/")
        if parts[0] not in self.roots or any(part in {"", ".", ".."} for part in parts[1:]):
            raise ValueError("normalized logical input address required")

    def address(self, path):
        if self.refused:
            self.refuse("compiler filesystem already refused")
        if not isinstance(path, str) or not path.startswith("/") or "\0" in path:
            self.refuse("absolute compiler input path required")
        path = os.path.normpath(path)
        for logical, root in self.roots.items():
            if path == root or path.startswith(root.rstrip("/") + "/"):
                return logical + path[len(root):]
        self.refuse("undeclared compiler filesystem query")

    def refuse(self, message):
        self.refused = True
        raise ValueError(message)

    def observe(self, operation, address, result):
        self.count += 1
        if self.count > self.limit:
            self.refuse("compiler observation budget")
        self.observations.add((operation, address, result))

    def missing(self, address, path):
        parts = address.split("/")
        if any("/".join(parts[:index]) in self.files for index in range(1, len(parts))):
            return NotADirectoryError(errno.ENOTDIR, "captured parent is a file", path)
        return FileNotFoundError(errno.ENOENT, "absent from captured input", path)

    def stat_or_none(self, path):
        address = self.address(path)
        if address in self.files:
            kind, size = "file", len(self.files[address])
            mode = stat.S_IFREG | 0o444
        elif address in self.directories:
            kind, size = "directory", 0
            mode = stat.S_IFDIR | 0o555
        else:
            self.observe("stat", address, "absent")
            return None
        self.observe("stat", address, kind)
        return os.stat_result((mode, self.inodes[address], 1, 1, 0, 0, size, 1, 1, 1))

    def listdir(self, path):
        address = self.address(path)
        if address in self.directories:
            self.observe("listdir", address, self.directory_digests[address])
            return sorted(self.directories[address])
        self.observe("listdir", address, "file" if address in self.files else "absent")
        if address in self.files:
            raise NotADirectoryError(errno.ENOTDIR, "captured input is a file", path)
        raise self.missing(address, path)

    def read(self, path):
        address = self.address(path)
        if address in self.files:
            self.observe("read", address, self.digests[address])
            return self.files[address]
        self.observe("read", address, "directory" if address in self.directories else "absent")
        if address in self.directories:
            raise IsADirectoryError(errno.EISDIR, "captured input is a directory", path)
        raise self.missing(address, path)

    def ledger(self):
        if self.refused:
            self.refuse("compiler filesystem already refused")
        return [list(observation) for observation in sorted(self.observations)]
