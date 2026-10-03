"""LLDB callbacks for the temporary macOS fork diagnostics.

Loaded with `command script import`. Every callback returns False, so the inferior never stops:
the callbacks only log, with wall-clock times that the MCP driver's log shares.

- `alloc_once_hit`: first `_os_alloc_once` call for each slot of libplatform's
  `_os_alloc_once_table`, with the stack. The first call for slot 0
  (OS_ALLOC_ONCE_KEY_LIBSYSTEM_NOTIFY) is libnotify's one-time global initialization.
- `notify_hit`: the first calls into libsystem_notify.dylib.
- `fork_hit` / `spawn_hit`: process creation by fork or posix_spawn. The first fork disables the
  two breakpoints above: a forked child inherits their trap instructions and would die on them.
- `ptrace_hit`: skips `ptrace(PT_DENY_ATTACH)`, which the worker calls in `main`.
"""

import time

import lldb

# Libsystem alloc_once_private.h (apple-oss-distributions/Libsystem, main).
SLOT_NAMES = {
    0: "LIBSYSTEM_NOTIFY", 1: "LIBXPC", 2: "LIBSYSTEM_C", 3: "LIBSYSTEM_INFO",
    4: "LIBSYSTEM_NETWORK", 5: "LIBCACHE", 6: "LIBCOMMONCRYPTO", 7: "LIBDISPATCH",
    8: "LIBDYLD", 9: "LIBKEYMGR", 10: "LIBLAUNCH", 11: "LIBMACHO", 12: "OS_TRACE",
    13: "LIBSYSTEM_BLOCKS", 14: "LIBSYSTEM_MALLOC", 15: "LIBSYSTEM_PLATFORM",
    16: "LIBSYSTEM_PTHREAD", 17: "LIBSYSTEM_STATS", 18: "LIBSECINIT",
    19: "LIBSYSTEM_CORESERVICES", 20: "LIBSYSTEM_SYMPTOMS", 21: "LIBSYSTEM_PLATFORM_ASL",
    22: "LIBSYSTEM_FEATUREFLAGS",
}
PT_DENY_ATTACH = 31

_state = {"table": None, "seen": set(), "forks": 0, "spawns": 0, "notify": 0, "traced": set()}


def _now():
    return "%.6f" % time.time()


def _arg0(frame):
    for name in ("x0", "rdi"):
        reg = frame.FindRegister(name)
        if reg.IsValid():
            return reg.GetValueAsUnsigned()
    return None


def _stack(thread, limit=60):
    lines = []
    for index in range(min(thread.GetNumFrames(), limit)):
        frame = thread.GetFrameAtIndex(index)
        module = frame.GetModule().GetFileSpec().GetFilename() or "?"
        name = frame.GetFunctionName()
        if not name and frame.GetSymbol().IsValid():
            name = frame.GetSymbol().GetName()
        lines.append("    #%-2d %s`%s" % (index, module, name or "0x%x" % frame.GetPC()))
    return "\n".join(lines)


def _thread(thread):
    return "tid=%d name=%s" % (thread.GetThreadID(), thread.GetName() or "-")


def _table(target):
    if _state["table"] is None:
        for name in ("_os_alloc_once_table", "__os_alloc_once_table"):
            symbols = target.FindSymbols(name)
            for index in range(symbols.GetSize()):
                symbol = symbols.GetContextAtIndex(index).GetSymbol()
                address = symbol.GetStartAddress().GetLoadAddress(target)
                if address != lldb.LLDB_INVALID_ADDRESS:
                    _state["table"] = address
                    return address
    return _state["table"]


def _remember(bp_loc):
    # Breakpoints whose code a forked child may run; disabled at the first fork, because a
    # child inherits their trap instructions with the parent's memory and dies on them.
    _state["traced"].add(bp_loc.GetBreakpoint().GetID())


def alloc_once_hit(frame, bp_loc, internal_dict):
    _remember(bp_loc)
    thread = frame.GetThread()
    table = _table(thread.GetProcess().GetTarget())
    slot = _arg0(frame)
    if table is None or slot is None:
        print("ALLOC-ONCE unresolved table=%s slot=%s" % (table, slot), flush=True)
        return False
    index = (slot - table) // 16
    if index in _state["seen"]:
        return False
    _state["seen"].add(index)
    print("ALLOC-ONCE-FIRST t=%s slot=%d (%s) %s\n%s" % (
        _now(), index, SLOT_NAMES.get(index, "?"), _thread(thread), _stack(thread)), flush=True)
    return False


def notify_hit(frame, bp_loc, internal_dict):
    _remember(bp_loc)
    _state["notify"] += 1
    count = _state["notify"]
    if count <= 25:
        thread = frame.GetThread()
        print("NOTIFY-CALL #%d t=%s fn=%s %s\n%s" % (
            count, _now(), frame.GetFunctionName(), _thread(thread), _stack(thread, 40)), flush=True)
    return False


def fork_hit(frame, bp_loc, internal_dict):
    _state["forks"] += 1
    count = _state["forks"]
    thread = frame.GetThread()
    if count == 1 and _state["traced"]:
        target = thread.GetProcess().GetTarget()
        for bp_id in sorted(_state["traced"]):
            target.FindBreakpointByID(bp_id).SetEnabled(False)
        print("TRACE-OFF t=%s breakpoints %s disabled before the first fork" % (
            _now(), sorted(_state["traced"])), flush=True)
    if count <= 12:
        print("FORK #%d t=%s %s\n%s" % (count, _now(), _thread(thread), _stack(thread, 30)), flush=True)
    else:
        print("FORK #%d t=%s %s" % (count, _now(), _thread(thread)), flush=True)
    return False


def spawn_hit(frame, bp_loc, internal_dict):
    _state["spawns"] += 1
    count = _state["spawns"]
    thread = frame.GetThread()
    print("POSIX-SPAWN #%d t=%s fn=%s %s\n%s" % (
        count, _now(), frame.GetFunctionName(), _thread(thread), _stack(thread, 20)), flush=True)
    return False


def ptrace_hit(frame, bp_loc, internal_dict):
    if _arg0(frame) != PT_DENY_ATTACH:
        return False
    thread = frame.GetThread()
    error = thread.ReturnFromFrame(frame, frame.EvaluateExpression("(int)0"))
    if error.Fail():
        link = frame.FindRegister("lr").GetValueAsUnsigned()
        frame.FindRegister("x0").SetValueFromCString("0", lldb.SBError())
        frame.SetPC(link)
    print("PTRACE PT_DENY_ATTACH skipped t=%s (%s)" % (
        _now(), "returned" if error.Success() else error.GetCString()), flush=True)
    return False
