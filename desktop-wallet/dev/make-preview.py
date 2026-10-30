#!/usr/bin/env python3
"""生成 dev/__preview.html：ui/index.html 的可打开副本 + 假 __TAURI__ 桩。

为什么这些文件住在 dev/ 而不是 ui/：`tauri.conf.json` 的 `frontendDist` 就是整个 `ui/`
目录，Cargo 编译时把里面**每一个**文件原样嵌进二进制——包括以 `__` 开头的对拍页。
一份能伪造整条链状态的假外壳跟着发行包一起发出去，不是"开发方便"，是包里躺着一个假的钱包。

    python3 desktop-wallet/dev/make-preview.py     # → desktop-wallet/dev/__preview.html
    python3 -m http.server 8899 --directory desktop-wallet   # 服务根要落在 desktop-wallet/

页面里所有相对引用都被改写成 `../ui/…`，`__stub.js` 留在本目录（它才是假的那一份）。
"""
import os
import re
import time

HERE = os.path.dirname(os.path.abspath(__file__))
UI = os.path.join(os.path.dirname(HERE), "ui")
OUT = os.path.join(HERE, "__preview.html")

rel = re.compile(r'\b(?P<k>src|href)="(?P<v>(?!https?:|#|data:|\.\.)[^"]+)"')


def to_ui(m):
    v = m.group("v")
    # 对拍页最大的假信号来自缓存：改了 ui/*.js、页面里那行 src 却没变，浏览器继续跑旧的那份，
    # 看上去就像"改了没效果"。用文件 mtime 当版本号——改了必然变，没改就不会白重画。
    stem, ext = os.path.splitext(v)
    if ext in (".js", ".css"):
        try:
            v = "%s?v=%d" % (v, int(os.path.getmtime(os.path.join(UI, v))))
        except OSError:
            pass
    return '%s="../ui/%s"' % (m.group("k"), v)


src = open(os.path.join(UI, "index.html"), encoding="utf-8").read()
page = rel.sub(to_ui, src)
stub = '<script src="__stub.js?v=%d"></script>\n' % int(time.time())
i18n_tag = re.search(r'<script src="\.\./ui/i18n\.js[^"]*"></script>', page)
if i18n_tag:
    page = page[:i18n_tag.start()] + stub + i18n_tag.group(0) + page[i18n_tag.end():]
else:
    raise SystemExit("没找到 i18n.js 的 script 标签，注入点变了——去 make-preview.py 里对齐 index.html")

open(OUT, "w", encoding="utf-8").write(page)
print("%s（桩版本号 %d；引用已改到 ../ui/）" % (OUT, int(time.time())))
