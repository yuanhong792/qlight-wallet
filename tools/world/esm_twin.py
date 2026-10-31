#!/usr/bin/env python3
"""把 Node 回归闸（CJS）转成能在沙箱里 import 的 ESM 孪生。

这台机器上没有 node 二进制，`node tools/world/test_x.js` 跑不了；node-repl 沙箱又禁 `require`。
CJS 原件留在仓库当正经闸（哪天有 node 就直接跑），这里派生一份孪生跑同一批断言。
改完闸必须重新生成孪生再跑，否则跑的是旧版。

用法：python3 tools/world/esm_twin.py [tools/world/test_x.js] [输出.mjs]
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
SRC = sys.argv[1] if len(sys.argv) > 1 else "tools/world/test_roam_v1.js"
OUT = sys.argv[2] if len(sys.argv) > 2 else "/tmp/gate_roam.mjs"

t = open(os.path.join(ROOT, SRC), encoding="utf-8").read()

# head 里带 JS 的 `\n` 转义，所以替换串必须走 lambda：re.sub 的模板会把 `\n` 当真换行吃掉，
# 字符串字面量当场被截断成语法错误。
HEAD = (
    "const __dirname = %r;\n"
    "globalThis.__sink = globalThis.nodeRepl\n"
    "  ? (s) => nodeRepl.write(String(s))\n"
    "  : (s) => process.stdout.write(String(s));\n"
    "globalThis.console = { log: (...a) => __sink(a.join(' ') + '\\n'),"
    " warn: (...a) => __sink(a.join(' ') + '\\n'),"
    " error: (...a) => __sink(a.join(' ') + '\\n') };\n"
    "const global = globalThis;\n"
) % HERE
t = re.sub(r"^'use strict';$", lambda m: m.group(0) + "\n" + HEAD, t, count=1, flags=re.M)

t = re.sub(r"^const (fs|path) = require\('(\w+)'\);$",
           lambda m: f"import {m.group(1)} from 'node:{m.group(2)}';", t, flags=re.M)
t = t.replace("require('fs')", "(await import('node:fs')).default")
t = t.replace("global.window = global;", "globalThis.window = globalThis;")
# 沙箱的 globalThis.setTimeout 是 non-writable/non-configurable，桩打不上。
# 孪生里跳过并打一行提示：原件语义不变，只是那一批延后回调在孪生里会真跑到下一个 tick。
t = t.replace("global.setTimeout = () => 0;",
              "__sink('\\n[孪生提示] 沙箱禁写 setTimeout，此桩跳过（原件仍生效）\\n');")
t = re.sub(r"require\(path\.join\((?:ROOT|UI), '([^']+)'\)\);",
           lambda m: f"await import('file://{ROOT}/desktop-wallet/ui/{m.group(1)}');", t)
t = t.replace("process.exit(fail ? 1 : 0)", "if (fail) throw new Error('gate failed')")
t = t.replace("process.exit(1)", "throw new Error('gate failed')")

open(OUT, "w", encoding="utf-8").write(t)
print(f"{SRC} -> {OUT}  ({len(t):,} 字符)")
