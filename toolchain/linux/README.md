# Linux runtime feasibility

Linux support for the guarded C++, JS/TS and Python launchers is pending.
Existing portable syntax and macOS behavior remain unchanged.

`guard_probe.py` is an independent real-kernel mechanism test, not a production
sandbox or a semantic launcher. Its labels froze before implementation in
`guard_expectations.json`. Run it on Linux with `python3 -I -S -B guard_probe.py`;
the interpreter runs only this trusted helper and synthetic inputs. No application
modules, packages or repository corpus are loaded.

The parent creates synthetic files, runs an owned child with a deadline and
closed inherited descriptors, checks unchanged original bytes/membership and
cleans up. The child stacks Landlock filesystem restrictions and a seccomp filter
and checks allowed pinned reads/private writes and denied unowned reads/writes,
truncation, creation, unlink/rename/link, unapproved execution and TCP/UDP/Unix
socket creation. Socket creation succeeds before the child filter is installed.
Missing kernel mechanisms or mismatched results fail; there is no degraded pass.

The filter rejects other syscall architectures, x32 syscall numbers and io_uring
entry points. This does not establish complete production syscall containment:
inherited descriptors, descendant cleanup, capability state, real tool startup,
exact executable allowlists, loaded runtime bytes, input races and mapped-image
attestation still require their own controls. No canonical calls are enabled by
this probe, and passed kernel checks do not constitute adapter/platform acceptance.

## Runtime byte-pin prerequisite

`runtime_expectations.json` freezes separate descriptor and live-process control
labels before either implementation. `codemap::linux_runtime` implements only the
**descriptor** group: canonical unique files and inode identities, exact byte
pins, ELF64 little-endian x86-64/AArch64 loadable images, explicit mapped data,
logical roles, and an anonymous-execution policy included in identity. Retained
handles plus byte and inode/metadata rechecks reject replacement and mutation.
Physical paths do not enter identity. Darwin manifests and fingerprints use their
existing schema without changes.

The ignored `linux_operator_runtime_capture_relocation_and_drift` test consumes
an explicit `CODEMAP_LINUX_RUNTIME_MANIFEST` prepared by the operator. It validates
real runtime files, relocation and mutation without executing application code.
It must be explicitly run on each native Linux architecture; its presence in the
workspace suite is not execution evidence.

The live-binding labels are **pending**, not passed by descriptor tests. Runtime
byte pins alone prove neither process ownership nor loaded-memory identity. No
Linux semantic launcher is enabled by this prerequisite. Guard enforcement,
procfs binding, input/read closure, descendants, deadlines and unchanged adapter
oracles remain separate required gates in the portability plan.

## Owned process inspection prerequisite

`linux_process::OwnedLinuxChild` binds the child at spawn to a pidfd and an
actual procfs directory handle. It accepts no cached/reaped `Child` and exposes
no reusable wait operation. `StoppedLinuxChild` borrows that sealed owner. It checks parent, process group and birth
identity; requests a stop of the thread group and confirms it through the kernel;
checks every observed thread;
and reads bounded reports and selected memory through the retained directory.
Each witness read checks the stopped state before and after. Explicit resume
checks its result; dropping an inspection attempts resume so a read failure does
not intentionally leave the child stopped. Termination consumes ownership and kills the owned group before reaping.
The transport supervisor still owns query deadlines and guard enforcement.

The explicit ignored `owned_stop_memory_resume_and_failure_controls` test uses
operator-system `/usr/bin/sleep` solely as a kernel control. It
checks owned stop/memory/resume, read-failure resume, process replacement, exited
process and foreign-group refusal, and rejection after an external resume. Run
it as an ordinary user on native Linux; a skipped test is not verification.

This supplies process witnesses only. It does not admit a mapped image, compare
executable memory to pinned ELF bytes, enforce a guard, or emit Linux semantic
facts. `memory_expectations.json` freezes the additional complete image-admission
controls; the unimplemented controls remain pending. The next unit binds these
kernel witnesses to exact file/data/vDSO pins and executable-memory comparisons,
then integrates guarded transport before any adapter's acceptance run.

## File-backed image witness

`CapturedLinuxRuntime::verify_file_backed_images` binds every file-backed map to
a retained pin, the kernel-reported device/inode and its `map_files` readlink.
Required executable/library pins must be mapped. Each ELF mapping must correspond
to a load segment at one consistent bias; executable loads require complete page
coverage, private read-only permissions and exact memory bytes, including loader
zero-filled tails. Bounded proc reports, executable-memory work and repeated
stopped-process/file checks fail on unavailable or changed witnesses.

`file_mapping_expectations.json` froze this unit's parser and live labels before
implementation. Compile only the trusted fixture during operator preparation:
`cc -O0 -static -Wall -Wextra -Werror toolchain/linux/file_mapping_fixture.c -o /tmp/codemap-file-fixture`.
Run `CODEMAP_LINUX_FILE_FIXTURE=/tmp/codemap-file-fixture cargo +1.99.0 test -p codemap --test linux_file_images -- --include-ignored`
as an ordinary user on each native architecture. The controls verify private
text tampering with unchanged disk bytes, writable executable pages, extra/deleted/alias
data, missing images, replaced/changed pins, unreadable memory and execution
outside ELF loads. `unreadable_memory` exercises the direct memory witness with
an unmapped address; it does not claim a guarded transport failure test.

This method deliberately covers file-backed mappings only. A successful return
leaves vDSO/vsyscall, anonymous executable memory, source/input closure, guards,
descendants and transport deadlines unproven. The broader frozen runtime and
memory matrices remain pending as complete admission suites. No adapter calls
this witness or enables Linux semantic facts yet.

## Prepared process guard prerequisite

`LinuxGuard::prepare` retains a Landlock ABI 3 ruleset and a native seccomp
allowlist before fork. `OwnedLinuxChild::spawn_guarded` installs these restrictions
before executing the selected pinned executable, closes inherited descriptors at
exec, and uses pipes for standard input/output/error. The child must have an
ordinary identity and no capabilities. The current policy requires anonymous
execution to be denied; a Node JIT policy still needs separate implementation
and controls.

Read permissions cover explicit runtime/input files and directory enumeration;
writes cover only fresh private state. Input permissions do not authenticate
source bytes. The syscall policy permits threads, restricts signals and resource
limits to the child, and denies process creation, network sockets, io_uring,
process-group escape, auxiliary-vector rewriting, executable anonymous mappings,
and descriptor operations that can signal other processes. Unknown syscalls are
denied; missing kernel restrictions fail launch.

`process_guard_expectations.json` froze the control labels before implementation.
Compile the trusted helper during operator preparation:
`cc -O0 -static -pthread -Wall -Wextra -Werror toolchain/linux/process_guard_fixture.c -o /tmp/codemap-guard-fixture`.
Run `CODEMAP_LINUX_GUARD_FIXTURE=/tmp/codemap-guard-fixture cargo +1.99.0 test -p codemap --test linux_guard -- --include-ignored`
as an ordinary user on each native Linux architecture. The control also runs
`auxv_rewrite_probe.py` as a trusted unguarded baseline and proves failed guard
installation leaves no child. It loads no application code. Classification comes
from fixture observations and is compared with the independent frozen oracle.

This guard is a launch prerequisite. Complete kernel-image admission, source
closure, bounded query pipes/deadlines, cleanup and real tool startup remain
required before an adapter can emit Linux semantic facts.

## Kernel and anonymous executable-image admission

`LinuxKernelPins` supplies independent operator vDSO size/SHA pins and an explicit
vsyscall policy. `verify_complete_images` requires the sealed guarded launch,
checks bounded saved auxiliary-vector bytes, binds its vDSO address to exactly
one read-only private executable kernel mapping, validates native ELF structure
and exact memory bytes, and verifies the complete file-backed image closure.
Other anonymous executable mappings are refused. x86-64 permits only the
explicitly selected fixed emulated vsyscall page; AArch64 requires absence.
Witnesses and file identities are rechecked before success. The combined image
fingerprint includes file pins, kernel pins and guard policy.

Guarded launch takes `LinuxLaunch` data and constructs the command internally
with a cleared environment. It accepts no opaque `Command` or caller callbacks.
A compile-fail control covers that boundary; an owned child executing through
an earlier callback on the unguarded API must fail image admission.

`kernel_image_expectations.json` froze the independent metadata and live labels.
Compile the trusted operator fixture:
`cc -O0 -static -Wall -Wextra -Werror toolchain/linux/kernel_image_fixture.c -o /tmp/codemap-kernel-fixture`.
Run the metadata controls with
`cargo +1.99.0 test -p codemap --lib linux_kernel_images` and the actual process
controls with
`CODEMAP_LINUX_KERNEL_FIXTURE=/tmp/codemap-kernel-fixture cargo +1.99.0 test -p codemap --test linux_kernel_images -- --include-ignored`
on both native architectures as an ordinary user. The fixture obtains operator
pins in a separate trusted process. Live controls include wrong pins, refusal of
unguarded children, and parent-injected vDSO memory changes with unchanged disk
bytes, followed by successful restoration.

Image admission still leaves source/read closure, bounded transport and adapter
acceptance open. No adapter enables Linux semantic facts through this API yet;
anonymous-execution extensions remain unaccepted.

## Sealed I/O ownership and cleanup

`OwnedLinuxChild::take_stdio` hands off the guarded child's three pipes once,
without exposing its `Child` or a reusable wait operation. `verify_live` checks
the retained kernel handles and birth/parent/group identity without reaping.
The consuming termination path removes signaling ownership, kills the group and
selected child before its first reap, and polls for exit within two seconds.
An expired budget transfers the child to a wait-only worker and returns an error;
that worker never signals a cached PID or group.

`process_io_expectations.json` froze this subunit's controls. Compile the trusted
fixture:
`cc -O0 -static -Wall -Wextra -Werror toolchain/linux/process_io_fixture.c -o /tmp/codemap-io-fixture`.
Run
`CODEMAP_LINUX_IO_FIXTURE=/tmp/codemap-io-fixture cargo +1.99.0 test -p codemap --test linux_process_io -- --include-ignored`
and
`cargo +1.99.0 test -p codemap --lib linux_process -- --include-ignored`
as an ordinary user on both native architectures. Controls observe actual full
pipe blocking, stopped and exited children, retained zombie ownership and reap
completion. The expired private wait-budget control exercises the wait-only
handoff; it does not simulate an uninterruptible kernel task.

Pipe ownership alone does not supply protocol byte budgets or query deadlines.
Bounded I/O and shared LSP routing, source closure and adapter acceptance still
follow this prerequisite. Linux semantic facts remain gated.

## Bounded protocol pipes

`LinuxPipes` converts the sealed stdio handoff to nonblocking owned pipes.
Reads and writes carry one absolute deadline across partial transfers and
readiness polling. LSP framing reuses the shared decoder with explicit limits:
headers at most 8 KiB and bodies at most 4 MiB. Outbound serialization is capped
before any header is written. The existing unbounded decoder policy retains
its accepted behavior for current semantic clients.

`protocol_expectations.json` froze 25 controls before implementation. Compile
the trusted fixture:
`cc -O0 -static -Wall -Wextra -Werror toolchain/linux/protocol_fixture.c -o /tmp/codemap-protocol-fixture`.
Run
`CODEMAP_LINUX_PROTOCOL_FIXTURE=/tmp/codemap-protocol-fixture cargo +1.99.0 test -p codemap --test linux_protocol -- --include-ignored`
and the shared decoder compatibility unit test as an ordinary user on both
native architectures. Controls cover fragmented and buffered frames, malformed
or oversized input, truncated output, closed pipes, stalled reads and writes,
and capped outbound serialization. The backpressure control independently
observes bulk bytes delivered into the non-reading child's pipe before timeout.

These primitives do not yet integrate the shared transport's Linux process
variant, bounded routing queues, full request deadlines or image inspections.
Source closure and each adapter's acceptance remain pending.

## Owned shared LSP sessions

The canonical `lsp::Transport` has a Linux ownership variant, re-exported as
`LinuxLspTransport` for its independent controls. `spawn_linux` prepares its guard
from the same retained runtime pins used by explicit stopped-process image
checkpoints. It accepts pure launch data and declared read permissions, with no
caller command callbacks or arbitrary server-request hooks. Unsupported server
requests receive a deterministic JSON-RPC error.

Linux request deadlines cover registration, writer-lock acquisition, serialization,
partial writes and response waits. Pending requests and unsolicited messages have
capacity limits. Frame and wire-byte totals persist separately in each direction;
stderr has its own cumulative byte cap. Limits may be reduced, never enlarged
beyond the named policy's maxima. Pipe cancellation interrupts readiness waits.
Failure retains its first cause, marks cancellation and attempts cleanup without
blocking on the owner lock. An active process lease retains cleanup responsibility
until release; releasing it retries cancelled-child cleanup after unlocking.

`lsp_transport_expectations.json` froze the original controls and additional review/verification
controls before their implementations. Compile the trusted fixtures:
`cc -O0 -static -Wall -Wextra -Werror toolchain/linux/lsp_transport_fixture.c -o /tmp/codemap-lsp-fixture`
and the kernel-image fixture above. Run
`CODEMAP_LINUX_LSP_FIXTURE=/tmp/codemap-lsp-fixture CODEMAP_LINUX_KERNEL_FIXTURE=/tmp/codemap-kernel-fixture cargo +1.99.0 test -p codemap --lib frozen_owned_linux_lsp_controls -- --include-ignored`
as an ordinary user on both native architectures. Observations cover concurrent
reply routing, internal replies, input/output budgets, held writer/process locks,
retained ownership followed by actual reaping, malformed replies, cancellation,
image tampering with unchanged disk bytes and shutdown/drop/failure cleanup.

The transport enables no Linux semantic facts. Adapters must bracket protocol
phases with image checks, authenticate source/read closure, include the transport
policy fingerprint in identity and pass their unchanged acceptance gates.
