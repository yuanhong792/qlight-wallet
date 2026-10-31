#!/usr/bin/env python3
"""对拍用的静态服务器：每条响应都不许缓存。

为什么不用 `python3 -m http.server`：`make-preview.py` 只能给 index.html 里写死的
`src`/`href` 打 `?v=<mtime>`，而 app.js 是**动态 import** 那些模块的
（earth-map.js → earth-local.js）。动态 import 的 URL 没有版本号，浏览器一发
条件请求拿回 304，页面就跑在半新半旧的模块图上：`earth-map.js` 要 `BEACH_M`，
缓存里的 `earth-local.js` 还没导出它——报"模块没这个导出"，看着像代码改坏了，
其实是缓存。对拍的证据必须可复现，所以这里不留缓存这一档。

官网那一页更要走这里：`docs/earth-map.js` 是钱包模块的逐字节副本，改完梯度口径刷新却拿回
304，画出来的还是**上一版那颗地球**——旧地球并不报错，看着一切正常，这种"没生效"最难查。

    python3 desktop-wallet/dev/serve.py 8791                      # 根落在 desktop-wallet/
    python3 desktop-wallet/dev/serve.py 8790 --root docs          # 起官网那页
    python3 desktop-wallet/dev/serve.py 8791 --sink /tmp/shots    # 外加 8812：收 harness 抓的帧
"""
import base64
import functools
import http.server
import os
import sys
import threading

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


class Sink(http.server.BaseHTTPRequestHandler):
    """收 `__cap('名字.png')` POST 来的 data: URL，落成 PNG。

    为什么固化在这里：`__fp.html` 与 `roam-harness.html` 的抓帧都是把 data URL
    POST 到 127.0.0.1:8812，而那个接收端此前只活在某一次命令行里。文档里每一张"实测帧"
    都要能被别人重新拍出来，否则那句话就退化成了断言。

    只收 `image/png` 的 data URL、文件名取调用方给的名字但**只留 basename**、
    写之前拒绝越出 sink 目录——这台机器上跑的是钱包，不想让一个页面把 PNG 写到 ~/.ssh。
    """

    def _sink_dir(self):
        return self.server.sink_dir

    def do_POST(self):
        name = os.path.basename(self.path.strip("/").split("?")[0]) or "shot.png"
        if not name.endswith(".png"):
            name += ".png"
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        head, _, b64 = body.partition(b",")
        if not head.startswith(b"data:image/png;base64"):
            return self._reply(415, "only data:image/png;base64")
        try:
            raw = base64.b64decode(b64, validate=True)
        except Exception as e:
            return self._reply(400, "bad base64: %s" % e)
        path = os.path.realpath(os.path.join(self._sink_dir(), name))
        if not path.startswith(os.path.realpath(self._sink_dir()) + os.sep):
            return self._reply(400, "name escapes sink dir")
        with open(path, "wb") as f:
            f.write(raw)
        print("wrote %s (%d B)" % (path, len(raw)), flush=True)
        self._reply(200, path)

    def _reply(self, code, msg):
        data = msg.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    argv = sys.argv[1:]
    sink = os.path.abspath(argv[argv.index("--sink") + 1]) if "--sink" in argv else None
    if sink:
        del argv[argv.index("--sink") + 1]
        argv.remove("--sink")
    # 官网那页也要同一套不缓存：`docs/earth-map.js` 是钱包模块的逐字节副本，
    # 用 `python3 -m http.server` 起它，改完刷新拿回 304，看到的还是旧地球——
    # 而旧地球看起来完全合法，这种"没生效"最难查。
    root = os.path.abspath(argv[argv.index("--root") + 1]) if "--root" in argv else ROOT
    if "--root" in argv:
        del argv[argv.index("--root") + 1]
        argv.remove("--root")
    port = int(argv[0]) if argv else 8791
    print("serving %s on http://127.0.0.1:%d" % (root, port), flush=True)
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", port),
                                          functools.partial(NoCache, directory=root))
    if sink:
        os.makedirs(sink, exist_ok=True)
        rec = http.server.ThreadingHTTPServer(("127.0.0.1", 8812), Sink)
        rec.sink_dir = sink
        threading.Thread(target=rec.serve_forever, daemon=True).start()
        print("frame sink on http://127.0.0.1:8812 -> %s" % sink, flush=True)
    srv.serve_forever()

