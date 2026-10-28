"""Declared source roots for native queries; repository paths stay unchanged."""
import pathlib


def module_paths(files, profile):
    if type(profile) is not dict or set(profile) != {
            "source_roots", "namespace_packages", "external_stub_roots"}:
        raise ValueError("explicit package profile required")
    roots = profile["source_roots"]
    if type(roots) is not list or not roots or len(roots) > 100:
        raise ValueError("bounded ordered source roots required")
    if type(profile["namespace_packages"]) is not bool:
        raise ValueError("explicit namespace package policy required")
    if profile["external_stub_roots"] != []:
        raise ValueError("external stub roots unsupported by native query")
    for index, root in enumerate(roots):
        if (type(root) is not str or "\\" in root or "\0" in root
                or pathlib.PurePosixPath(root).is_absolute()
                or str(pathlib.PurePosixPath(root)) != root
                or ".." in pathlib.PurePosixPath(root).parts):
            raise ValueError("normalized repository source root required")
        for previous in roots[:index]:
            if (root == previous or "." in {root, previous}
                    or root.startswith(previous + "/") or previous.startswith(root + "/")):
                raise ValueError("overlapping source roots refused")
    paths = {}
    for path in sorted(files):
        owners = [root for root in roots if root == "." or path.startswith(root + "/")]
        if len(owners) != 1:
            raise ValueError("source must belong to exactly one declared root")
        root = owners[0]
        relative = path if root == "." else path.removeprefix(root + "/")
        parts = relative.removesuffix(".pyi").removesuffix(".py").split("/")
        if parts[-1] == "__init__":
            parts.pop()
        if not parts or any(not part.isidentifier() for part in parts):
            raise ValueError("unambiguous Python module name required")
        module = ".".join(parts)
        if module in paths:
            raise ValueError("ambiguous module input")
        if not profile["namespace_packages"]:
            parents = relative.split("/")[:-1]
            for length in range(1, len(parents) + 1):
                prefix = "/".join(parents[:length])
                prefix = prefix if root == "." else root + "/" + prefix
                if not any(prefix + "/__init__" + suffix in files for suffix in (".py", ".pyi")):
                    raise ValueError("regular package initializer required")
        paths[module] = path
    for module in paths:
        parts = module.split(".")
        for length in range(1, len(parts)):
            parent = paths.get(".".join(parts[:length]))
            if parent is not None:
                if not parent.endswith(("/__init__.py", "/__init__.pyi")):
                    raise ValueError("module and package collision")
                if not paths[module].startswith(parent.rsplit("/", 1)[0] + "/"):
                    raise ValueError("regular package cannot span source roots")
    return paths
