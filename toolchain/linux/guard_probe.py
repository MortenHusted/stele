"""Linux kernel feasibility controls; this never enables canonical extraction."""
import ctypes
import errno
import json
import os
import pathlib
import platform
import socket
import subprocess
import sys
import tempfile


class Ruleset(ctypes.Structure):
    _fields_ = [('handled_access_fs', ctypes.c_uint64)]


class PathRule(ctypes.Structure):
    _pack_ = 1
    _fields_ = [('allowed_access', ctypes.c_uint64), ('parent_fd', ctypes.c_int)]


class Filter(ctypes.Structure):
    _fields_ = [('code', ctypes.c_ushort), ('jt', ctypes.c_ubyte),
                ('jf', ctypes.c_ubyte), ('k', ctypes.c_uint32)]


class Program(ctypes.Structure):
    _fields_ = [('len', ctypes.c_ushort), ('filter', ctypes.POINTER(Filter))]


def require_syscall(value, operation):
    if value < 0:
        raise OSError(ctypes.get_errno(), operation)
    return value


def restrict(root, minimum_abi):
    arch, network_calls = {
        'aarch64': (0xc00000b7, (198, 199)),
        'x86_64': (0xc000003e, (41, 53)),
    }[platform.machine()]
    libc = ctypes.CDLL(None, use_errno=True)
    libc.syscall.restype = ctypes.c_long
    libc.prctl.argtypes = [ctypes.c_int] + [ctypes.c_ulong] * 4
    abi = require_syscall(libc.syscall(444, 0, 0, 1), 'Landlock ABI')
    if abi < minimum_abi:
        raise RuntimeError('required Landlock filesystem controls are unavailable')
    attr = Ruleset((1 << 15) - 1)
    fd = require_syscall(libc.syscall(444, ctypes.byref(attr), ctypes.sizeof(attr), 0),
                         'create Landlock ruleset')
    read = 1 << 2
    write = (1 << 1) | (1 << 4) | (1 << 5) | (1 << 7) | (1 << 8) | (1 << 14)
    try:
        for path, rights in [(root / 'pinned', read),
                             (root / 'state', read | (1 << 3) | write)]:
            pathfd = os.open(path, os.O_PATH | os.O_CLOEXEC)
            try:
                rule = PathRule(rights, pathfd)
                require_syscall(libc.syscall(445, fd, 1, ctypes.byref(rule), 0),
                                'add Landlock path rule')
            finally:
                os.close(pathfd)
        require_syscall(libc.prctl(38, 1, 0, 0, 0), 'no_new_privs')
        require_syscall(libc.syscall(446, fd, 0), 'enforce Landlock')
    finally:
        os.close(fd)
    instructions = [Filter(0x20, 0, 0, 4), Filter(0x15, 1, 0, arch),
                    Filter(0x06, 0, 0, 0x80000000), Filter(0x20, 0, 0, 0),
                    Filter(0x35, 0, 1, 0x40000000), Filter(0x06, 0, 0, 0x80000000)]
    # io_uring must not bypass syscall-level socket restrictions.
    for number in (*network_calls, 425, 426, 427):
        instructions.extend([Filter(0x15, 0, 1, number),
                             Filter(0x06, 0, 0, 0x00050001)])
    instructions.append(Filter(0x06, 0, 0, 0x7fff0000))
    filters = (Filter * len(instructions))(*instructions)
    program = Program(len(filters), filters)
    require_syscall(libc.prctl(22, 2, ctypes.addressof(program), 0, 0),
                    'enforce seccomp')
    return abi


def denied(action, expected_errno):
    try:
        value = action()
        if isinstance(value, int):
            os.close(value)
        elif hasattr(value, 'close'):
            value.close()
        elif isinstance(value, tuple):
            for item in value:
                item.close()
        return False
    except OSError as error:
        return error.errno == expected_errno


def child(root, minimum_abi):
    # Demonstrate that socket creation works before this process's own filter.
    for family, kind in [(socket.AF_INET, socket.SOCK_STREAM),
                         (socket.AF_INET, socket.SOCK_DGRAM),
                         (socket.AF_UNIX, socket.SOCK_STREAM)]:
        socket.socket(family, kind).close()
    for endpoint in socket.socketpair():
        endpoint.close()
    baseline = os.posix_spawn('/usr/bin/true', ['/usr/bin/true'], {})
    if os.waitpid(baseline, 0) != (baseline, 0):
        raise RuntimeError('baseline executable did not complete successfully')
    abi = restrict(root, minimum_abi)
    result = {'pinned_read_allowed': (root / 'pinned').read_bytes() == b'original'}
    (root / 'state' / 'allowed').write_bytes(b'private')
    result['private_write_allowed'] = True
    actions = {
        'unowned_read_denied': lambda: open(root / 'outside', 'rb'),
        'pinned_write_denied': lambda: open(root / 'pinned', 'wb'),
        'outside_write_denied': lambda: open(root / 'outside', 'wb'),
        'readonly_truncate_denied': lambda: os.open(root / 'pinned', os.O_RDONLY | os.O_TRUNC),
        'outside_create_denied': lambda: open(root / 'new', 'wb'),
        'outside_unlink_denied': lambda: os.unlink(root / 'outside'),
        'outside_rename_denied': lambda: os.rename(root / 'outside', root / 'renamed'),
        'cross_hierarchy_link_denied': lambda: os.link(root / 'pinned', root / 'state' / 'link'),
        'unowned_exec_denied': lambda: os.posix_spawn('/usr/bin/true', ['/usr/bin/true'], {}),
    }
    result.update({name: denied(action, errno.EXDEV if name == 'cross_hierarchy_link_denied'
                               else errno.EACCES) for name, action in actions.items()})
    network = {
        'tcp_socket_denied': lambda: socket.socket(socket.AF_INET, socket.SOCK_STREAM),
        'udp_socket_denied': lambda: socket.socket(socket.AF_INET, socket.SOCK_DGRAM),
        'unix_socket_denied': lambda: socket.socket(socket.AF_UNIX, socket.SOCK_STREAM),
        'socketpair_denied': socket.socketpair,
    }
    result.update({name: denied(action, errno.EPERM) for name, action in network.items()})
    return {'landlock_abi': abi, 'architecture': platform.machine(), 'controls': result}


def main():
    if sys.platform != 'linux':
        raise RuntimeError('this feasibility control requires a real Linux host')
    if len(sys.argv) == 4 and sys.argv[1] == '--child':
        print(json.dumps(child(pathlib.Path(sys.argv[2]), int(sys.argv[3])), sort_keys=True))
        return
    if len(sys.argv) != 1:
        raise RuntimeError('unexpected arguments')
    oracle = json.loads(pathlib.Path(__file__).with_name('guard_expectations.json').read_text())
    if oracle['contract'] != 'codemap-linux-guard-feasibility/v1':
        raise RuntimeError('unsupported control contract')
    with tempfile.TemporaryDirectory(prefix='codemap-linux-guard-') as temporary:
        root = pathlib.Path(temporary)
        (root / 'state').mkdir()
        for name in ['pinned', 'outside']:
            (root / name).write_bytes(b'original')
        os.link(root / 'pinned', root / 'state' / 'baseline-link')
        (root / 'state' / 'baseline-link').unlink()
        output = subprocess.check_output([sys.executable, '-I', '-S', '-B', __file__,
                                          '--child', temporary, str(oracle['minimum_landlock_abi'])],
                                         close_fds=True, timeout=30)
        result = json.loads(output)
        if result['controls'] != oracle['expected']:
            raise RuntimeError(('kernel controls differ from frozen expectations', result))
        if any((root / name).read_bytes() != b'original' for name in ['pinned', 'outside']):
            raise RuntimeError('a denied operation changed original bytes')
        if any((root / name).exists() for name in ['new', 'renamed', 'state/link']):
            raise RuntimeError('a denied operation changed filesystem membership')
        if (root / 'state' / 'allowed').read_bytes() != b'private':
            raise RuntimeError('private state bytes differ')
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()
