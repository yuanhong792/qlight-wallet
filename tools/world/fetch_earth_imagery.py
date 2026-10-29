#!/usr/bin/env python3
"""把同一片 8.96 km 窗口的**真实卫星影像**烤成一张地面贴图。

大地为什么不该由我上色：`groundColor()` 那种手写的"海拔→颜色"色带，
不管怎么调都是猜的——猜出来的绿色永远不像那片山。ESRI World Imagery 里
这张照片是崂山东麓本来的样子，拿它当地面，颜色就不是我的判断了。

对齐口径与高程表完全一致：同一套 Web Mercator 瓦片边界（tile_ids/mosaic 的
row_b、col_b）、同一个中心与窗宽、同一套面积平均。所以贴图的第 (x,y) 格
就是高程的第 (x,y) 格，岸线不会比地形差半个格。
"""
import argparse
import hashlib
import json
import math
import os
import sys
import urllib.request
from concurrent.futures import ThreadPoolExecutor

import numpy as np
from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fetch_earth_dem import ll2xy, lat_of, lon_of, tile_ids  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DATA = os.path.join(REPO, "desktop-wallet", "data")
CACHE = os.path.join(REPO, "tools", "world", "img_cache")
MANIFEST = os.path.join(DATA, "earth-manifest.json")
URL = "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}"
UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15"


def cache_path(z, x, y):
    return os.path.join(CACHE, f"{z}_{x}_{y}.jpg")


def fetch_tile(z, x, y, allow_net=True):
    p = cache_path(z, x, y)
    if not os.path.exists(p):
        if not allow_net:
            raise SystemExit(f"离线且缺瓦片 {z}/{x}/{y}：先联网跑一次，缓存留在 tools/world/img_cache")
        req = urllib.request.Request(URL.format(z=z, x=x, y=y), headers={"User-Agent": UA})
        with urllib.request.urlopen(req, timeout=30) as r:
            blob = r.read()
        with open(p, "wb") as f:
            f.write(blob)
    return np.asarray(Image.open(p).convert("RGB"), dtype=np.float32)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--zoom", type=int, default=16)
    ap.add_argument("--out", type=int, default=3072, help="成品贴图边长（像素）")
    ap.add_argument("--offline", action="store_true")
    a = ap.parse_args()

    man = json.load(open(MANIFEST))
    lat_c, lon_c = man["center_lat"], man["center_lon"]
    half = man["width_km"] / 2.0
    dlat = half / 111.32
    dlon = half / (111.32 * math.cos(math.radians(lat_c)))
    north, south, west, east = lat_c + dlat, lat_c - dlat, lon_c - dlon, lon_c + dlon

    z, tx0, tx1, ty0, ty1 = tile_ids(north, west, south, east, a.zoom)
    ids = [(tx, ty) for ty in range(ty0, ty1 + 1) for tx in range(tx0, tx1 + 1)]
    print(f"窗口 {west:.6f},{south:.6f} → {east:.6f},{north:.6f} · z{a.zoom} 瓦片 "
          f"x{tx0}..{tx1} y{ty0}..{ty1} = {len(ids)} 张")
    os.makedirs(CACHE, exist_ok=True)
    if not a.offline:
        def get(job):
            tx, ty = job
            if not os.path.exists(cache_path(a.zoom, tx, ty)):
                try:
                    fetch_tile(a.zoom, tx, ty)
                except Exception as e:  # 单张失败重试一次即可，不该拖垮整批
                    fetch_tile(a.zoom, tx, ty)
                    print("retry", tx, ty, e)
        with ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(get, ids))

    rows = []
    for ty in range(ty0, ty1 + 1):
        rows.append(np.hstack([fetch_tile(a.zoom, tx, ty) for tx in range(tx0, tx1 + 1)]))
    e = np.vstack(rows)

    # 与高程同一套面积平均口径，但**不能**用稠密权重矩阵去乘：3072 输出格 × 4864
    # 源行的稠密 matmul 三通道要 360 GFLOP，几分钟不返回（踩过）。每个输出格实际只
    # 压住 2~3 行源像元，所以先把经纬边界插成"源像元的分数行号/列号"，再 gather
    # 相邻的那几行按重叠宽度加权——同一件事，快两个数量级。
    # 边界数组多留一个端点，理由与 fetch_earth_dem 一样：行 j 的真实跨度是
    # [row_b[j+1], row_b[j]]，中心差分在端点会偏半格。
    row_b = np.array([lat_of((ty0 + j / 256.0) / 2 ** z) for j in range(e.shape[0] + 1)])
    col_b = np.array([lon_of((tx0 + i / 256.0) / 2 ** z) for i in range(e.shape[1] + 1)])
    out = a.out
    step_r, step_c = 2 * dlat / out, 2 * dlon / out
    lat_edges = np.array([north - k * step_r for k in range(out + 1)])
    lon_edges = np.array([west + k * step_c for k in range(out + 1)])
    # np.interp 要求 xp 递增：源栅格行 0 在最北（纬度递减），取负再插值
    fr = np.interp(-lat_edges, -row_b, np.arange(len(row_b)))
    fc = np.interp(lon_edges, col_b, np.arange(len(col_b)))

    def axis_weight(b, n_src):
        idx_max = int(np.ceil(np.diff(b).max())) + 1
        j0 = np.clip(np.floor(b[:-1]).astype(np.int64), 0, n_src - 1)
        idx = np.minimum(j0[:, None] + np.arange(idx_max)[None, :], n_src - 1)
        w = np.clip(np.minimum(b[1:, None], idx + 1) - np.maximum(b[:-1, None], idx), 0, None)
        # 零重叠 = 输出格落在源栅格之外，静默补 0 会烤出一条假黑边，必须当场炸
        if (w.sum(axis=1) <= 0).any():
            raise SystemExit("自检失败：输出格与源瓦片零重叠，窗口或瓦片范围错了")
        return idx, w / w.sum(axis=1, keepdims=True)

    ri, rw = axis_weight(fr, e.shape[0])
    ci, cw = axis_weight(fc, e.shape[1])

    img = np.empty((out, out, 3), dtype=np.float32)
    for k0 in range(0, out, 256):
        k1 = min(out, k0 + 256)
        band = np.tensordot(e[ri[k0:k1]], rw[k0:k1], axes=([1], [1]))
        img[k0:k1] = np.tensordot(band[:, ci, :], cw, axes=([2], [1]))
    # 源栅格行 0 在最北，输出网格 y=0 也在最北（与 terrain::height_at 同向），不用翻转
    im = Image.fromarray(np.clip(img, 0, 255).astype(np.uint8), "RGB")

    os.makedirs(DATA, exist_ok=True)
    jpg = os.path.join(DATA, "earth-satellite.jpg")
    im.save(jpg, quality=86, optimize=True, subsampling=0)
    raw = open(jpg, "rb").read()
    sha = hashlib.sha256(raw).hexdigest()
    print(f"贴图 {out}x{out} · {len(raw)/1048576:.2f} MiB · sha256 {sha[:16]}")

    man["satellite"] = {
        "rule": "QL-EARTH-IMG-v1",
        "source": "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
        "attribution": "Imagery © Esri, Maxar, Earthstar Geographics",
        "zoom": a.zoom,
        "tiles": len(ids),
        "tile_ids_sha256_16": hashlib.sha256(
            "|".join(f"{a.zoom}:{x}:{y}" for x, y in ids).encode()).hexdigest()[:16],
        "side_px": out,
        "meters_per_px": round(man["width_km"] * 1000.0 / out, 3),
        "resample": "area-mean over the same Mercator tile boundaries as the DEM",
        "sha256": sha,
        "file": "earth-satellite.jpg",
    }
    with open(MANIFEST, "w") as f:
        json.dump(man, f, ensure_ascii=False, indent=2)
        f.write("\n")
    print("manifest 已写入 satellite 段")


if __name__ == "__main__":
    main()
