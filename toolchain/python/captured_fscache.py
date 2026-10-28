"""Mypy cache bound to captured data; raw compiler I/O still needs enforcement."""
from mypy.fscache import FileSystemCache
from mypy.util import hash_digest


class CapturedCache(FileSystemCache):
    def __init__(self, view):
        super().__init__()
        self.view = view

    def set_package_root(self, package_root):
        if package_root:
            self.view.refuse("fabricated package roots refused")
        super().set_package_root(package_root)

    def stat_or_none(self, path):
        return self.view.stat_or_none(path)

    def listdir(self, path):
        return self.view.listdir(path)

    def read(self, path):
        data = self.view.read(path)
        self.hash_cache[path] = hash_digest(data)
        return data
