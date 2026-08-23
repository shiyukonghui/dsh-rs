#!/usr/bin/env python3
"""dsh-code-runtime Python worker（M5-DESIGN §7.3 真实后端；协议由我们自设计，见 D-066）。

传输：stdin/stdout JSON-lines（宿主不可在本平台建额外 fd → 协议走 0/1，用户 print() 在
进程内捕获并经 `log` 帧按行回流；stderr 只承载引导级诊断）。帧：
  host->child : boot | run | reply{ok,value|message}
  child->host: boot_ack | call | log | done{value|error}
宿主视入站为敌：每帧字段被宿主 REBUILD 校验；本 worker 只做自己一侧。
"""

import json
import sys
import types

_BOOT_OUT = sys.stdout.buffer  # 协议写端（重定向前抓取）
_REAL_ERR = sys.stderr  # 引导致命诊断


def _send(obj):
    raw = json.dumps(obj, ensure_ascii=False, allow_nan=False).encode("utf-8")
    _BOOT_OUT.write(raw)
    _BOOT_OUT.write(b"\n")
    _BOOT_OUT.flush()


def _readline():
    line = sys.stdin.buffer.readline()
    if not line:
        raise EOFError("host closed stdin")
    return json.loads(line.decode("utf-8"))


def _fatal(msg):
    _REAL_ERR.write("dsh-python-worker fatal: %s\n" % msg)
    _REAL_ERR.flush()
    sys.exit(1)


class _LogCapture:
    """用户输出捕获：写缓冲按行回流为 `log` 帧（保证顺序、程序中断不丢已缓冲行）。"""

    def __init__(self, label):
        self._label = label
        self._pending = ""

    def write(self, s):
        self._pending += s
        while "\n" in self._pending:
            line, self._pending = self._pending.split("\n", 1)
            self._emit(line)

    def flush(self):
        if self._pending:
            self._emit(self._pending)
            self._pending = ""

    def _emit(self, line):
        _send({"type": "log", "text": "%s%s" % (self._label, line)})


_STDOUT_CAP = _LogCapture("")
_STDERR_CAP = _LogCapture("")


def _make_binding(ns_global, name, next_id, reject):
    """程序调用的绑定代理：发 call、阻塞等 reply（单线程，逐 id 匹配）。"""

    def _call(args):
        cid = next(next_id)
        _send({"type": "call", "id": cid, "global": ns_global, "name": name, "args": args})
        while True:
            frame = _readline()
            if frame.get("type") == "reply" and frame.get("id") == cid:
                if frame.get("ok"):
                    return frame.get("value")
                raise reject(name, frame.get("message", "call failed"))
            raise RuntimeError("unexpected host frame while awaiting reply: %r" % (frame,))

    _call.__name__ = name
    return _call


def _install(binding_ns, next_id):
    ns_global = binding_ns["global"]
    fns = {}
    reject = _plain_reject

    for mname in binding_ns.get("names") or []:
        fns[mname] = _make_binding(ns_global, mname, next_id, None)  # reject patched below
    # 先造拒绝器（错误类），再造绑定（闭包需引用 reject）
    ec = binding_ns.get("error_class")
    if ec:
        cls = types.new_class(ec["name"], (Exception,), {})
        prop = ec["member_name_property"]

        def _typed_reject(member_name, message, _cls=cls, _prop=prop):
            inst = _cls(message)
            setattr(inst, _prop, member_name)
            return inst

        reject = _typed_reject
    for mname in list(fns.keys()):
        fns[mname] = _make_binding(ns_global, mname, next_id, reject)

    obj = types.SimpleNamespace(**fns)
    return ns_global, obj


def _plain_reject(member_name, message):
    return RuntimeError("[%s] %s" % (member_name, message))


def _run_program(code, globals_dict):
    """把程序体作为同步函数体执行（python 后端无顶层 await；返回值即完成值）。
    空/无 return → 返回 None（宿主持「无完成值即缺省」语义）。"""
    body = ["    " + l if l else "" for l in code.split("\n")]
    wrapped = "def __dsh_main__():\n" + "\n".join(body) + "\n"
    exec(compile(wrapped, "<dsh-program>", "exec"), globals_dict)
    fn = globals_dict["__dsh_main__"]
    return fn()


def _settle_done(value):
    try:
        _send({"type": "done", "value": value})
    except (TypeError, ValueError):
        _send({"type": "done", "error": {"kind": "invalid-output",
                                         "message": "completion value is not JSON-serializable"}})


def main():
    try:
        boot = _readline()
    except EOFError:
        _fatal("no boot frame")
    if boot.get("type") != "boot":
        _fatal("expected boot frame, got %r" % boot)
    next_id = iter(range(1, 1 << 53))  # 足够；host 幂等判重

    globals_dict = {"__builtins__": __builtins__}
    try:
        for binding_ns in boot.get("namespaces") or []:
            ns_global, obj = _install(binding_ns, next_id)
            globals_dict[ns_global] = obj
    except Exception as e:
        _fatal("bad namespace: %r" % (e,))

    # 引导成功：用户输出改走捕获（协议保持干净）
    sys.stdout = _STDOUT_CAP
    sys.stderr = _STDERR_CAP
    _send({"type": "boot_ack"})

    try:
        run = _readline()
    except (EOFError, json.JSONDecodeError) as e:
        _fatal("expected run frame: %r" % (e,))
    if run.get("type") != "run":
        _fatal("expected run frame, got %r" % run)
    code = run.get("code")

    try:
        value = _run_program(code, globals_dict)
        _settle_done(value)
    except BaseException as e:  # noqa: BLE001 程序异常/语法错误 → exception 失败
        _send({"type": "done", "error": {"kind": "exception",
                                         "message": "%s: %s" % (type(e).__name__, e)}})


if __name__ == "__main__":
    main()
