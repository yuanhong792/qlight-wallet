#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""把烤好的大地高度场导出成前端可以直接加载的 JS。

为什么要生成而不是手写：`desktop-wallet/data/earth-height.bin` 是唯一真身，
Rust 侧 `include_bytes!` 它，Python/Node 参考实现直接读它，只有浏览器要一份
能随 `ui/` 一起打包的副本（Tauri 的 CSP 是 `default-src 'self'`，也没有
asset: 协议权限，所以不能运行时去读磁盘）。base64 是这里面最小的编码。

跑法：python3 tools/world/gen_earth_js.py
生成：desktop-wallet/ui/earth-height.js、docs/earth-height.js
"""
import base64, json, os, sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(REPO, "desktop-wallet", "data", "earth-height.bin")
MAN = os.path.join(REPO, "desktop-wallet", "data", "earth-manifest.json")
OUTS = [os.path.join(REPO, "desktop-wallet", "ui", "earth-height.js"),
        os.path.join(REPO, "docs", "earth-height.js")]

HEAD = """/* QLight 大地高度场 · 真实地球（生成文件，勿手改）
 *
 * 生成器：tools/world/gen_earth_js.py
 * 真身：  desktop-wallet/data/earth-height.bin（256×256，1 字节/格，行主序，y=0 是最北）
 * 烤盘：  tools/world/fetch_earth_dem.py 从公开 DEM（AWS Open Data terrarium 瓦片，
 *         内容是 SRTM 30 米 / Copernicus GLO-30）按面积平均重采样 + 分段量化得来。
 *
 * 这一份字节同时被 Rust（include_bytes!）、Python 与 Node 参考实现读同一份文件；
 * 浏览器拿的是它的 base64 副本。SHA-256 写进规则串（QL-WORLD-RULES-v3），
 * 所以换一片土地 = 换一套世界规则 = 换一批指纹，谁也没法悄悄换大陆。
 */
"""


def emit(man, raw):
    b64 = base64.b64encode(raw).decode("ascii")
    lines = [HEAD, "(function (global) {\n  'use strict';\n",
             "  var QLEarth = {\n",
             "    sha256: %r,\n" % man["sha256"],
             "    fingerprint: %r,\n" % man["heights_fingerprint"],
             "    place: %r,\n" % man["place"],
             "    source: %r,\n" % man["underlying_dem"],
             "    center: [%r, %r],\n" % (man["center_lat"], man["center_lon"]),
             "    widthKm: %r,\n" % man["width_km"],
             "    metersPerCell: %r,\n" % man["meters_per_cell"],
             "    peakM: %r,\n" % man["real_elevation_m"]["max"],
             "    seaMinM: %r,\n" % man["real_elevation_m"]["min"],
             "    bbox: [%s],\n" % ", ".join(repr(v) for v in man["bbox_deg"]),
             "    demRule: %r,\n" % man["rule"],
             "    seaCells: %r,\n" % man["water_cells"],
             "    seabed: %r,\n" % man["seabed"]["note"],
             "    bytes: null,\n"]
    lines.append("    b64: ''\n")
    for i in range(0, len(b64), 1024):
        lines.append("      + %r\n" % b64[i:i + 1024])
    lines.append("""      ,
    /** 解出 65536 字节的真实高度场；长度不对当场炸，绝不静默退回噪声 */
    load: function () {
      if (this.bytes) return this.bytes;
      var bin;
      if (typeof atob === 'function') bin = atob(this.b64);
      else if (typeof Buffer === 'function') bin = Buffer.from(this.b64, 'base64').toString('binary');
      else throw new Error('QLEarth: 本运行时没有 base64 解码');
      var out = new Uint8Array(bin.length);
      for (var i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
      if (out.length !== 65536) throw new Error('QLEarth: 高度场字节数 ' + out.length + ' 不对');
      this.bytes = out;
      return out;
    }
  };
  global.QLEarth = QLEarth;
  /* Node（对拍脚本）和浏览器（钱包/官网）跑同一份文件：挂法与 world.js 一致 */
  if (typeof module !== 'undefined' && module.exports && typeof process !== 'undefined') {
    module.exports = global.QLEarth;
  }
})(typeof window !== 'undefined' ? window : globalThis);
""")
    return "".join(lines)


def main():
    raw = open(BIN, "rb").read()
    man = json.load(open(MAN, encoding="utf-8"))
    if len(raw) != 65536:
        raise SystemExit(f"高度场字节数不对：{len(raw)}")
    src = emit(man, raw)
    for p in OUTS:
        old = open(p).read() if os.path.exists(p) else ""
        open(p, "w", encoding="utf-8").write(src)
        print(("更新 " if old == src else "写入 ") + os.path.relpath(p, REPO) + f"（{len(src)} B）")


if __name__ == "__main__":
    main()
