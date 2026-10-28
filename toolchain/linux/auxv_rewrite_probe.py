import ctypes as c
import json
import os
import struct
from pathlib import Path

assert os.geteuid() != 0
status = Path("/proc/self/status").read_text()
assert next(line.split()[1] for line in status.splitlines() if line.startswith("CapEff:")) == "0000000000000000"
lib = c.CDLL(None, use_errno=True)
lib.prctl.argtypes = [c.c_int, c.c_ulong, c.c_ulong, c.c_ulong, c.c_ulong]
lib.prctl.restype = c.c_int
lib.sbrk.argtypes = [c.c_long]
lib.sbrk.restype = c.c_void_p
assert lib.prctl(38, 1, 0, 0, 0) == 0
names = ["start_code", "end_code", "start_data", "end_data", "start_brk", "brk", "start_stack", "arg_start", "arg_end", "env_start", "env_end", "auxv"]
class MM(c.Structure):
    _fields_ = [(name, c.c_uint64) for name in names] + [("auxv_size", c.c_uint32), ("exe_fd", c.c_uint32)]
assert c.sizeof(MM) == 104
original = Path("/proc/self/auxv").read_bytes()
pairs = list(struct.iter_unpack("=QQ", original))
assert sum(tag == 33 for tag, value in pairs) == 1
changed = b"".join(struct.pack("=QQ", tag, 0x12345000 if tag == 33 else value) for tag, value in pairs)
buffer = c.create_string_buffer(changed, len(changed))
restore = c.create_string_buffer(original, len(original))
fields = Path("/proc/self/stat").read_text().rsplit(") ", 1)[1].split()
info = MM(int(fields[23]), int(fields[24]), int(fields[42]), int(fields[43]), int(fields[44]), 0, int(fields[25]), int(fields[45]), int(fields[46]), int(fields[47]), int(fields[48]), c.addressof(buffer), len(changed), 0xffffffff)
info.brk = lib.sbrk(0)
result = lib.prctl(35, 14, c.addressof(info), c.sizeof(info), 0)
assert result == 0, f"unprivileged map mutation unavailable errno={c.get_errno()}"
observed = Path("/proc/self/auxv").read_bytes()
assert observed == changed, "successful call did not rewrite saved auxv"
info.auxv = c.addressof(restore)
info.brk = lib.sbrk(0)
assert lib.prctl(35, 14, c.addressof(info), c.sizeof(info), 0) == 0
assert Path("/proc/self/auxv").read_bytes() == original
print(json.dumps({"ordinary_no_capabilities": True, "no_new_privileges": True, "saved_auxv_rewritten": True, "saved_auxv_restored": True}))
