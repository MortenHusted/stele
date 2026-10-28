"""Captured compiler I/O phase; requires separate OS/runtime attestation."""
import _io
import builtins
import io
import os
import posix
import stat
import sys


class CompilerReadScope:
    def __init__(self, view):
        self.view = view
        self.cwd = view.roots["{source}"]
        self.active = False
        self.used = False
        self.originals = []

    def snapshot_modules(self):
        return {name: (module, getattr(module, "__spec__", None),
                       getattr(getattr(module, "__spec__", None), "origin", None))
                for name, module in sys.modules.items()}

    def audit(self, event, arguments):
        if self.active and (event.startswith("socket.") or event in {
                "open", "os.listdir", "os.scandir", "exec", "import",
                "os.fork", "os.forkpty", "os.exec", "os.posix_spawn", "os.system",
                "subprocess.Popen", "ctypes.dlopen", "mmap.__new__",
                "function.__new__", "code.__new__", "marshal.loads", "marshal.load",
                "pickle.find_class", "sys.settrace", "sys.setprofile"}):
            self.view.refuse("uncaptured compiler operation: " + event)

    def path(self, path):
        try:
            path = os.fspath(path)
        except (TypeError, ValueError):
            self.view.refuse("text compiler input path required")
        if not isinstance(path, str):
            self.view.refuse("text compiler input path required")
        return os.path.normpath(path if path.startswith("/") else self.cwd + "/" + path)

    def deny(self, *arguments, **keywords):
        self.view.refuse("unsupported compiler filesystem operation")

    def metadata(self, path, *, dir_fd=None, follow_symlinks=True):
        if dir_fd is not None:
            self.view.refuse("compiler directory descriptors refused")
        path = self.path(path)
        result = self.view.stat_or_none(path)
        if result is None:
            raise self.view.missing(self.view.address(path), path)
        return result

    def listdir(self, path="."):
        return self.view.listdir(self.path(path))

    def isdir(self, path):
        path = self.path(path)
        # Mypy uses this exact sentinel to disable metadata storage. This is
        # policy, not a lookup of the host's character device.
        if path == os.devnull:
            self.view.address(self.cwd)
            self.view.observe("cache", "{disabled-cache}", "disabled")
            return False
        result = self.view.stat_or_none(path)
        return result is not None and stat.S_ISDIR(result.st_mode)

    def open(self, path, mode="r", buffering=-1, encoding=None, errors=None,
             newline=None, closefd=True, opener=None):
        if mode not in {"r", "rt", "rb"} or opener is not None or not closefd:
            self.view.refuse("compiler file writes and open callbacks refused")
        if buffering != -1 or encoding not in {None, "utf-8"} or errors not in {None, "strict"}:
            self.view.refuse("unsupported compiler read options")
        if mode == "rb" and (encoding is not None or errors is not None or newline is not None):
            self.view.refuse("binary compiler read options refused")
        data = self.view.read(self.path(path))
        buffer = io.BytesIO(data)
        if mode == "rb":
            return buffer
        return io.TextIOWrapper(buffer, encoding="utf-8", errors="strict", newline=newline)

    def replace(self, module, name, value):
        self.originals.append((module, name, getattr(module, name)))
        setattr(module, name, value)

    def __enter__(self):
        if self.used:
            self.view.refuse("compiler read scope is single-use")
        self.used = True
        sys.addaudithook(self.audit)
        self.modules = self.snapshot_modules()
        for module in (os, posix):
            self.replace(module, "stat", self.metadata)
            self.replace(module, "lstat", self.metadata)
            self.replace(module, "listdir", self.listdir)
            self.replace(module, "getcwd", lambda: self.cwd)
            for name in ("scandir", "open", "fstat", "access", "readlink", "chdir", "fchdir",
                         "mkdir", "unlink", "remove", "rename", "replace", "rmdir",
                         "chmod", "chown", "link", "symlink"):
                self.replace(module, name, self.deny)
        self.replace(os.path, "isdir", self.isdir)
        for module in (builtins, io, _io):
            self.replace(module, "open", self.open)
        for module in (io, _io):
            self.replace(module, "open_code", self.deny)
        self.active = True
        return self

    def __exit__(self, exception_type, exception, traceback):
        try:
            if self.snapshot_modules() != self.modules:
                self.view.refuse("compiler module graph changed")
            self.view.ledger()
        finally:
            self.active = False
            for module, name, original in reversed(self.originals):
                setattr(module, name, original)
            self.originals.clear()
        return False
