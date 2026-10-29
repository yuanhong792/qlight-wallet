#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""把真实地球的高程数据烤成世界的大地高度场。

为什么需要它：§9.7 的铁律是"链只给坐标，像素全部本机算出"，任何第三方拿同一份
创世哈希 + 块 VRF 必须渲染出逐位相同的大陆。以前"大陆"来自整数 fbm；现在它来自
**真实地表**——公开、可复现下载的高程瓦片（AWS Open Data 的 terrarium 编码，
陆地部分是 SRTM 30 米 / Copernicus GLO-30 的全球合成DEM）。

可复现性因此换了一个锚，而且更强：
  旧：大地 = f(创世哈希)                    → 说不清这片大陆"是什么"
  新：大地 = g(公开 DEM, 边界框, 量化规则)   → 谁都能重新下载、重算、逐字节对上，
      并且拿谷歌地球去核对这段海岸线是不是真的

植被与矿脉**不用**真实数据：湿度场仍由创世哈希 ⊕ 全部块 VRF 算出，每出一个块
重画一次——"矿脉不可预挖"靠的就是这一条。真实数据只负责大地的形状。

一处必须说清的取舍：源 DEM 只测陆地，海面像元恒等于 0 米。所以
  - 海岸线是**真的**（按像元级陆/海判定，不靠高度阈值猜）；
  - 海底是**模型的**：离岸每一格下切 SHELF_M_PER_CELL 米、封顶 SHELF_MAX_M 米。
    黄海近岸本就浅（离岸 4 公里才 20 米量级），这条浅架和真值同阶；
    参数写进 manifest，任何人重算都对得上，但别把它当成实测水深。

输出都是确定性的（同输入跑两次逐字节相同）：
  desktop-wallet/data/earth-height.bin     65536 B = 256×256 高度场（1..247）
  desktop-wallet/data/earth-manifest.json  来源、边界框、量化参数、SHA-256、指纹
"""
import argparse, hashlib, json, math, os, sys, urllib.request
import numpy as np
from PIL import Image

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CACHE = os.path.join(REPO, "tools", "world", "dem_cache")
OUT_BIN = os.path.join(REPO, "desktop-wallet", "data", "earth-height.bin")
OUT_JSON = os.path.join(REPO, "desktop-wallet", "data", "earth-manifest.json")

N = 256          # 世界尺寸，与 terrain::W/H 同口径
SEA_FIELD = 86   # 规则常量 SEA：真实 0 米落在这里，海岸线因此是真的
SAND_FIELD = 98  # 规则常量 SAND：0..BEACH_M 米的低地被压进 86..98 这一段
TOP_FIELD = 247  # 高度场上限（247 >> 3 = 30 层）
BEACH_M = 4.0    # 沙带的真实厚度：真实海岸沙丘就 2~4 米高；再宽一半地图都是土黄
SHELF_M_PER_CELL = 0.5   # 海底浅架：离岸每格下切这么多米
SHELF_MAX_M = 40.0       # 封顶（黄海近岸量级），同时也是高度场深度 ramps 的分母
LAND_MIN_M = 0.5         # 源像元高于此值才算陆地（海面恒等于 0，不是实测海平面）


def ll2xy(lat, lon, z):
    n = 2.0 ** z
    x = (lon + 180.0) / 360.0 * n
    r = math.radians(lat)
    y = (1.0 - math.log(math.tan(r) + 1.0 / math.cos(r)) / math.pi) / 2.0 * n
    return x, y


def lat_of(yn):
    return math.degrees(math.atan(math.sinh(math.pi * (1.0 - 2.0 * yn))))


def lon_of(xn):
    return xn * 360.0 - 180.0


def tile_path(z, x, y):
    return os.path.join(CACHE, f"{z}_{x}_{y}.png")


def decode_tile(p):
    """解出 256×256 的 terrarium 高程：elev_m = (R*256 + G + B/256) − 32768（Mapzen/AWS 公开口径）"""
    a = np.asarray(Image.open(p).convert("RGB"), dtype=np.float64)
    if a.shape != (256, 256, 3):
        raise IOError(f"瓦片尺寸不对：{a.shape}")
    return a[:, :, 0] * 256.0 + a[:, :, 1] + a[:, :, 2] / 256.0 - 32768.0


def fetch_tile(z, x, y, allow_net=True):
    p = tile_path(z, x, y)
    if not os.path.exists(p):
        if not allow_net:
            raise FileNotFoundError(f"瓦片不在缓存：{p}")
        url = f"https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png"
        for attempt in range(5):
            try:
                with urllib.request.urlopen(url, timeout=60) as r:
                    data = r.read()
                # 验收标准是"解得开"，不是"够大"：一整片纯色公海的 PNG 就只有 757 字节，
                # 拿字节数当门槛会把合法的海面瓦片当成坏响应反复重试。
                tmp = p + ".part"
                with open(tmp, "wb") as fh:
                    fh.write(data)
                decode_tile(tmp)
                os.replace(tmp, p)
                break
            except Exception as ex:
                try:
                    os.remove(p + ".part")
                except OSError:
                    pass
                if attempt == 4:
                    raise
                print(f"  重试 {z}/{x}/{y}：{ex}", file=sys.stderr)
                import time
                time.sleep(1 + attempt)
    return decode_tile(p)


def tile_ids(lat_n, lon_w, lat_s, lon_e, z):
    xa, ya = ll2xy(lat_n, lon_w, z)
    xb, yb = ll2xy(lat_s, lon_e, z)
    tx0, tx1 = int(math.floor(xa)), int(math.floor(xb - 1e-9))
    ty0, ty1 = int(math.floor(ya)), int(math.floor(yb - 1e-9))
    return z, tx0, tx1, ty0, ty1


def mosaic(lat_n, lon_w, lat_s, lon_e, z, allow_net=True):
    """bbox 的瓦片拼成一整张高程栅格（行 0 = 最北）。返回 (栅格, 纬度边界, 经度边界)。"""
    z, tx0, tx1, ty0, ty1 = tile_ids(lat_n, lon_w, lat_s, lon_e, z)
    e = np.vstack([np.hstack([fetch_tile(z, tx, ty, allow_net) for tx in range(tx0, tx1 + 1)])
                   for ty in range(ty0, ty1 + 1)])
    # 边界数组多留一个：第 j 行的真实跨度是 [row_b[j+1], row_b[j]]。
    # 用 np.gradient 估边界不行——中心差分在端点偏半格，权重被夹成 0 之后
    # 整张高度场会静默归零（实测踩过：自检报"最高点 0 米"）。
    row_b = np.array([lat_of((ty0 + j / 256.0) / 2 ** z) for j in range(e.shape[0] + 1)])
    col_b = np.array([lon_of((tx0 + i / 256.0) / 2 ** z) for i in range(e.shape[1] + 1)])
    return e, row_b, col_b


def resample(e, row_b, col_b, lat_c, lon_c, half_km, out=N):
    """面积平均重采样，但**只对陆地像元**求均值，并单独交回每格的陆地覆盖率。

    两件事都要做对：
    1) 体素世界要的是"这一格里地面的平均高度"，不是"中心那一点"。点采样会把
       30 米的 DEM 变成锯齿——山脊忽高忽低、坡面一地麻点。
    2) 源数据是陆地 DEM：海面像元恒等于 0 米。海陆混进同一次面积平均，真实的
       海岸线会被抹成一条几米到几十米的糊边（实测沙带吃掉 44% 的地图），
       而且近岸格子的"地面高度"会变成海陆平均值——那不是地面。
    所以：h = 陆地像元的面积加权均值；frac = 陆地面积占比（0 = 这格是海）。
    """
    dlat = half_km / 111.32
    dlon = half_km / (111.32 * math.cos(math.radians(lat_c)))
    step_r, step_c = 2 * dlat / out, 2 * dlon / out
    north, south = row_b[:-1], row_b[1:]          # 纬度递减：行 0 在最北
    west, east = col_b[:-1], col_b[1:]            # 经度递增
    wr = np.zeros((e.shape[0], out))
    for k in range(out):
        hi, lo = lat_c + dlat - k * step_r, lat_c + dlat - (k + 1) * step_r
        wr[:, k] = np.clip(np.minimum(north, hi) - np.maximum(south, lo), 0, None)
    wc = np.zeros((e.shape[1], out))
    for k in range(out):
        a, b = lon_c - dlon + k * step_c, lon_c - dlon + (k + 1) * step_c
        wc[:, k] = np.clip(np.minimum(east, b) - np.maximum(west, a), 0, None)
    den_all = np.outer(wr.sum(axis=0), wc.sum(axis=0))
    # 零重叠必须当场炸：静默补 0 会把整张高度场烤成一片海，而"一片海"看上去并不荒谬
    if (den_all <= 0).any():
        raise SystemExit(f"自检失败：输出格 {int(np.argmax(den_all <= 0))} 与源栅格零重叠——边界框或瓦片范围错了")
    m = (e > LAND_MIN_M).astype(np.float64)
    land_area = wr.T @ m @ wc
    h = np.where(land_area > 0, (wr.T @ (e * m) @ wc) / np.maximum(land_area, 1e-12), 0.0)
    return h, land_area / den_all


def offshore_cells(frac):
    """离岸距离（格）：多源 BFS，从所有陆地格同时往外扩。纯整数，与浮点/顺序无关。"""
    land = frac > 0
    if not land.any():
        raise SystemExit("自检失败：窗口里一格陆地都没有")
    inf = 1 << 30
    d = np.where(land, 0, inf).astype(np.int32)
    h, w = d.shape
    for _ in range(h + w):                      # 收敛上界：最远也不会超过对角
        p = np.pad(d, 1, constant_values=inf)   # 外圈补 inf：越界不等于绕回地图另一头
        nd = d
        for dr, dc in ((0, 1), (0, -1), (1, 0), (-1, 0)):
            nd = np.minimum(nd, p[1 + dr:1 + dr + h, 1 + dc:1 + dc + w] + 1)
        if np.array_equal(nd, d):
            break
        d = nd
    return np.where(land, 0, d)


def quantize(h_m, beach_m=BEACH_M):
    """真实高程（米）→ 高度场 1..247。

    三段线性：0..beach_m 米压进 86..98（沙带只留真实沙丘那几米）；beach_m..窗口
    最高真实海拔摊到 98..247；海面以下按浅架模型给出的深度往下走，规则只给了
    85..1 这一段可表达，所以下切到 SHELF_MAX_M 就贴底——反正水不是方块，
    挖不动也不结算。
    """
    hi = float(h_m.max())
    f = np.zeros_like(h_m)
    ocean = h_m < 0
    if ocean.any():
        depth = np.clip(h_m[ocean] / -SHELF_MAX_M, 0, 1)            # 0=海平面，1=贴底
        f[ocean] = SEA_FIELD - 1 - depth * (SEA_FIELD - 2)
    beach = (~ocean) & (h_m < beach_m)
    f[beach] = SEA_FIELD + (h_m[beach] / beach_m) * (SAND_FIELD - SEA_FIELD)
    up = (~ocean) & (~beach)
    if up.any():
        f[up] = SAND_FIELD + (h_m[up] - beach_m) / max(1.0, hi - beach_m) * (TOP_FIELD - SAND_FIELD)
    return np.clip(np.rint(f), 1, TOP_FIELD).astype(np.uint8)


def bake(e, row_b, col_b, la, lo, km, beach_m=BEACH_M):
    """窗口 → (真实陆地高程, 含海底的有效高程, 高度场, 陆地覆盖率)。"""
    h_land, frac = resample(e, row_b, col_b, la, lo, km / 2.0)
    d = offshore_cells(frac)
    shelf = -SHELF_M_PER_CELL * d
    h_eff = np.where(frac > 0, h_land, np.maximum(shelf, -SHELF_MAX_M))
    return h_land, h_eff, quantize(h_eff, beach_m), frac


def fingerprint(buf):
    out = ""
    for basis, prime in ((0x811C9DC5, 16777619), (0x01000193, 3295579737)):
        d = basis
        for b in buf:
            d = (d ^ int(b)) * prime & 0xFFFFFFFF
        out += "%08x" % d
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--center", nargs=2, type=float, default=[36.130, 120.626], metavar=("LAT", "LON"))
    ap.add_argument("--width-km", type=float, default=15.36)
    ap.add_argument("--zoom", type=int, default=13)
    ap.add_argument("--name", default="崂山 · 青岛 · 黄海之滨")
    ap.add_argument("--expect-peak-m", type=float, default=1133.0, help="自检：真实主峰海拔，解码错了当场抓住")
    ap.add_argument("--beach-m", type=float, default=BEACH_M)
    ap.add_argument("--offline", action="store_true")
    a = ap.parse_args()

    la, lo = a.center
    half = a.width_km / 2.0
    dl = half / (111.32 * math.cos(math.radians(la)))
    e, row_b, col_b = mosaic(la + half / 111.32 + 0.01, lo - dl - 0.01,
                             la - half / 111.32 - 0.01, lo + dl + 0.01, a.zoom,
                             allow_net=not a.offline)
    h_land, h_eff, field, frac = bake(e, row_b, col_b, la, lo, a.width_km, a.beach_m)
    peak = float(h_land.max())
    if abs(peak - a.expect_peak_m) > max(120.0, 0.25 * a.expect_peak_m):
        raise SystemExit(f"自检失败：区域最高点 {peak:.0f} m 与预期 {a.expect_peak_m:.0f} m 差太远——解码或边界框错了")

    z, tx0, tx1, ty0, ty1 = tile_ids(la + half / 111.32 + 0.01, lo - dl - 0.01,
                                     la - half / 111.32 - 0.01, lo + dl + 0.01, a.zoom)
    tiles = [[z, tx, ty] for ty in range(ty0, ty1 + 1) for tx in range(tx0, tx1 + 1)]
    thash = hashlib.sha256(json.dumps(tiles).encode()).hexdigest()[:16]

    os.makedirs(os.path.dirname(OUT_BIN), exist_ok=True)
    open(OUT_BIN, "wb").write(field.tobytes())
    sha = hashlib.sha256(field.tobytes()).hexdigest()
    fp = fingerprint(field.tobytes())

    cell_m = a.width_km * 1000.0 / N
    water = int((frac == 0).sum())
    layers = int(field.max() >> 3) - (SEA_FIELD >> 3)
    man = {
        "rule": "QL-EARTH-DEM-v1",
        "place": a.name,
        "center_lat": la, "center_lon": lo,
        "width_km": a.width_km, "meters_per_cell": round(cell_m, 3),
        "zoom": a.zoom,
        "tile_source": "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png",
        "tile_decode": "elev_m = (R*256 + G + B/256) - 32768",
        "underlying_dem": "SRTM30m / Copernicus GLO-30 全球合成DEM（只测陆地）",
        "tiles": {"count": len(tiles), "ids": tiles, "ids_sha256_16": thash},
        "bbox_deg": [round(lo - dl, 6), round(la - half / 111.32, 6), round(lo + dl, 6), round(la + half / 111.32, 6)],
        "real_elevation_m": {"min": round(float(h_land[frac > 0].min()), 1), "max": round(peak, 1)},
        "seabed": {"kind": "model", "note": "源 DEM 不含海底：离岸每格下切 "
                   f"{SHELF_M_PER_CELL} 米、封顶 {SHELF_MAX_M} 米（黄海近岸量级）",
                   "m_per_cell": SHELF_M_PER_CELL, "max_m": SHELF_MAX_M},
        "quantize": {"sea_field": SEA_FIELD, "sand_field": SAND_FIELD, "top_field": TOP_FIELD,
                     "beach_m": a.beach_m, "land_min_m": LAND_MIN_M,
                     "resample": "area-mean over land pixels only", "bytes_per_cell": 1,
                     "row_order": "行主序，y=0 是最北（与 terrain::height_at(x,y) 的 y 同向）"},
        "grid": [N, N], "bytes": int(field.size),
        "sha256": sha, "heights_fingerprint": fp,
        "water_cells": water,
        "land_layers": layers,
    }
    os.makedirs(os.path.dirname(OUT_JSON), exist_ok=True)
    open(OUT_JSON, "w").write(json.dumps(man, ensure_ascii=False, indent=2) + "\n")

    print(f"地点   {a.name}  中心 {la},{lo}  边长 {a.width_km} km → 一格 = {cell_m:.1f} 米真实地表")
    print(f"真实陆地高程 {h_land[frac > 0].min():.0f} .. {peak:.0f} 米   海面 {water} 格（{water / field.size * 100:.1f}%）")
    print(f"量化   海面=第 {SEA_FIELD >> 3} 层，陆地最高第 {int(field.max()) >> 3} 层（{layers} 层起伏）")
    print(f"SHA-256 {sha}")
    print(f"高度场指纹 {fp}")
    print(f"写入 {os.path.relpath(OUT_BIN, REPO)} / {os.path.relpath(OUT_JSON, REPO)}")


if __name__ == "__main__":
    main()
