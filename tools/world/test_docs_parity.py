#!/usr/bin/env python3
"""官网那三颗模块与随包网格，必须和钱包里的是**同一份字节**。

为什么要有这个闸：官网世界页现在直接 `import('./earth-map.js')` 画整颗地球，代码是从
`desktop-wallet/ui/` 复制进 `docs/` 的（GitHub Pages 只发 `docs/`，相对路径 import 不出这个目录）。
复制体没有闸就一定会漂：改了 `ui/earth-grid.js` 的档位而 `docs/` 那份没跟上，官网念出的
2,097,152 / 615,081 / 金环位置就全是**旧网格**的数，而页面看起来一切正常——这种错不报错，
只让人信一张过期的地球。

负对照不是装饰：脚本自己造一份"改了一个字节"的副本，要求检查**必须**翻红。
只跑"两边相等"的正例，等于没验过比较逻辑本身。

    python3 tools/world/test_docs_parity.py
"""
import hashlib
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
UI = os.path.join(ROOT, "desktop-wallet", "ui")
DOCS = os.path.join(ROOT, "docs")

SHARED = ["earth-grid.js", "earth-local.js", "earth-map.js", "earth-global-h.bin"]

ok = 0
bad = 0


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def check(name, cond, detail=""):
    global ok, bad
    if cond:
        ok += 1
        print("  ok   %s%s" % (name, (" — " + detail) if detail else ""))
    else:
        bad += 1
        print("  FAIL %s%s" % (name, (" — " + detail) if detail else ""))


print("docs/ 与 desktop-wallet/ui/ 逐字节对拍：")
for name in SHARED:
    a, b = os.path.join(UI, name), os.path.join(DOCS, name)
    if not (os.path.exists(a) and os.path.exists(b)):
        check(name, False, "文件缺失（%s / %s）" % (os.path.exists(a), os.path.exists(b)))
        continue
    ha, hb = sha(a), sha(b)
    check(name, ha == hb, "%s… %d B" % (ha[:12], os.path.getsize(b)))

print("官网不许依赖 three.js：")
for name in [n for n in SHARED if n.endswith(".js")]:
    src = open(os.path.join(DOCS, name), encoding="utf-8").read()
    # 只认** import 语句**：文件头注释里"不用依赖 three.js"这种话，按字符串找会误伤。
    check("%s 不 import planet-view" % name,
          not re.search(r"^\s*import[^;]*['\"]\./planet-view\.js", src, re.M))
    check("%s 不 import three" % name,
          not re.search(r"^\s*import[^;]*three", src, re.M))

print("网格口径住在 earth-grid.js（不是 planet-view.js）：")
grid = open(os.path.join(DOCS, "earth-grid.js"), encoding="utf-8").read()
for label, pat in [("GW", "GW = 2048"), ("GH", "GH = 1024"), ("SEA_CODES", "SEA_CODES = 128"),
                   ("unquantize", "export function unquantize"), ("cellOf", "export function cellOf")]:
    check("earth-grid.js 定义 %s" % label, pat in grid)
pv = open(os.path.join(UI, "planet-view.js"), encoding="utf-8").read()
check("planet-view.js 从 earth-grid.js 引口径", "from './earth-grid.js'" in pv)
check("planet-view.js 不再自己定义 GW", not re.search(r"^export const GW", pv, re.M))
mjs = open(os.path.join(DOCS, "earth-map.js"), encoding="utf-8").read()
check("earth-map.js 不再重复定义 cellOf", not re.search(r"^const cellOf", mjs, re.M))

print("负对照：改一个字节必须翻红")
orig = open(os.path.join(UI, "earth-grid.js"), encoding="utf-8").read()
mutated = orig.replace("export const GW = 2048", "export const GW = 2049", 1)
if mutated == orig:
    check("变异样本构造成功", False, "原文里找不到 GW = 2048，这条负对照等于没跑")
else:
    copy = os.path.join(DOCS, "earth-grid.js")
    # 复原用**它自己原来**的字节，不用 ui 那份：两边本来就该相等（上面刚验过），
    # 万一不等，用 ui 覆盖过去等于闸悄悄改了被测对象，下一次跑就成了假绿。
    bak = open(copy, encoding="utf-8").read()
    try:
        open(copy, "w", encoding="utf-8").write(mutated)
        check("docs 那份被改坏时检查翻红", sha(os.path.join(UI, "earth-grid.js")) != sha(copy))
    finally:
        open(copy, "w", encoding="utf-8").write(bak)
    check("复原后与 ui 那份又逐字节相同", sha(copy) == sha(os.path.join(UI, "earth-grid.js")))

print()
print("%d 项通过，%d 项失败" % (ok, bad))
sys.exit(1 if bad else 0)
