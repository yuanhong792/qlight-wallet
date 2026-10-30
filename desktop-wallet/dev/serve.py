#!/usr/bin/env python3
"""对拍用的静态服务器：每条响应都不许缓存。

为什么不用 `python3 -m http.server`：`make-preview.py` 只能给 index.html 里写死的
`src`/`href` 打 `?v=<mtime>`，而 app.js 是**动态 import** 那些模块的
（earth-map.js → earth-local.js）。动态 import 的 URL 没有版本号，浏览器一发
条件请求拿回 304，页面就跑在半新半旧的模块图上：`earth-map.js` 要 `BEACH_M`，
缓存里的 `earth-local.js` 还没导出它——报"模块没这个导出"，看着像代码改坏了，
其实是缓存。对拍的证据必须可复现，所以这里不留缓存这一档。

    python3 desktop-wallet/dev/serve.py 8791      # 根落在 desktop-wallet/
"""
import functools
import http.server
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class NoCache(http.server.SimpleHTTPRequestHandler):
    """条件请求一律当首次请求处理，响应里再写明 no-store。"""

    def send_head(self):
        for h in ("If-Modified-Since", "If-None-Match"):
            if h in self.headers:
                del self.headers[h]
        return super().send_head()

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8791
    print("serving %s on http://127.0.0.1:%d" % (ROOT, port), flush=True)
    http.server.ThreadingHTTPServer(
        ("127.0.0.1", port),
        functools.partial(NoCache, directory=ROOT)).serve_forever()
