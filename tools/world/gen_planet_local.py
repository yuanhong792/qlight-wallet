#!/usr/bin/env python3
"""局部块生成：给地球上任意 (纬度, 经度)，烤出一块 256×256 的高度场。

为什么要这一层：星球层（earth-global-h.bin，1 格 ≈ 19.6 公里）只回答"这是哪块地、
大概多高"，渲染器要的是脚下 8.96 公里、1 格 35 米的那张场。注册钱包后席位被随机分到
地球各处，所以**任意**一处都得能生成，而不是只有崂山那一小块。

三件来源，按可信度排：
1. **实测块**：席位落在已烤过的 30 米 DEM 矩形里 → 直接用 earth-height.bin，一个字都不编。
2. **星球层**：其余地方的"骨架"——对全球网格做陆地-only 双线性，得到区域倾斜与海岸线。
3. **哈希微地形**：全球网格 19.6 公里一格，分辨不出 9 公里内的山脊，所以细节由 fbm 补；
   幅度由星球层该处的**真实起伏**（周围 3×3 格陆地极差）决定——山区生成山区的性格，
   平原生成平缓的地面，而不是全场一个振幅。种子 = 共识 RULES_SEED，同一坐标谁生成都一样。

字节口径（与 fetch_earth_dem.py quantize 同一条，别自创一套）：
  海 85..1（离岸每格下切 0.5 米、封顶 40 米，黄海近岸量级）
  沙 86..98（真实 0..4 米，**绝对**高程）
  陆 98..247（基准 = max(4 米, 块内陆地最低点)，往上按块内真实起伏摊开：
  gain = 起伏/149，但不低于 GAIN_MIN——平原不许被放大成悬崖）
渲染器 1 字节 = 35/8 = 4.375 米垂直，所以显示的垂直夸大倍数 = 4.375/gain。
GAIN_MIN = 4.375/2.5 = 1.75 米/字节 ⇒ 夸大封顶 2.5 倍；起伏再大就照真实比例压
（实测崂山块 1088 米 / 149 档 = 7.30 米/字节 = 0.6 倍，正是这条规则的一个特例）。

已知取舍（别对客户吹）：
- 星球层 1 格 ≈ 19.6 公里，而一块窗口只有 8.96 公里——**比一格还小**。骨架在块内几乎
  不变（实测：崂山窗口四角高程只差 0.1 米），所以 30 米以下的形状是哈希推出来的，不是
  测出来的。真实信息只有两条：**绝对海拔**（这是哪带地形）和**区域起伏 σ**（这是平原、
  丘陵还是高山）。本模块的诚实性全在于让这两条决定画面，而不是把平原烤成高山。
- 想在块内看见真山脊，得把星球层加密到 ~5 公里（8192×4096 = 32 MiB，正好是免费全球 DEM
  金字塔 z5 的原生分辨率）。那是打包体积的问题，不是算法问题。
- 高度场只有 149 档陆地，1 字节在渲染器里 = 4.375 米垂直：一块地装得下 652 米起伏还是
  1:1，再多就得压（喜马拉雅一块 835 米 → 0.78 倍）。宁可压扁，也不削顶成桌面。
- 块与块相邻时归一化基准各自独立（陆地最低点逐块取），接缝会有几字节的台阶——现在一个
  席位只有一块地，等区域层串流时要改成全局基准。
- 高纬：经度每度的实际距离趋 0，dlon 用 cos(纬度) 下限兜住，不然窗口在极点会炸成无限宽。
"""
import argparse
import hashlib
import json
import math
import os

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DATA = os.path.join(REPO, "desktop-wallet", "data")
UI = os.path.join(REPO, "desktop-wallet", "ui")

# —— 星球层（与 fetch_earth_planet.py / ui/planet-view.js 同口径）——
GW, GH = 2048, 1024
SEA_CODES = 128
OCEAN_STEP = 48.0
LAND_BUCKETS = [(0, 4, 32), (128, 8, 16), (256, 16, 16), (512, 32, 16),
                (1024, 64, 16), (2048, 128, 16), (4096, 256, 16)]
CEILING_M = 8192.0

# —— 局部块（与 consensus terrain.rs / fetch_earth_dem.py 同口径）——
N = 256
CELL_M = 35.0
WIDTH_KM = N * CELL_M / 1000.0            # 8.96 公里
SEA_FIELD, SAND_FIELD, TOP_FIELD = 86, 98, 247
BEACH_M = 4.0
SHELF_M_PER_CELL = 0.5
SHELF_MAX_M = 40.0
M_PER_DEG = 111.32e3                      # fetch_earth_dem 用的那一套
LAND_BYTES = TOP_FIELD - SAND_FIELD       # 149 档可表达陆地起伏
# 实测崂山块的比例：1088.3 米 / 149 档。它不是设计输入，是"起伏恰好 1088 米时
# gain = 起伏/149 自然得到的数"——留着当自检的锚，别拿它去钉别的块。
MEASURED_GAIN = (1092.3 - BEACH_M) / LAND_BYTES
MAX_EXAG = 2.5                            # 显示垂直夸大封顶：再平的地面也不许烤成悬崖
GAIN_MIN = (CELL_M / 8.0) / MAX_EXAG      # 1.75 米/字节
NOISE_L = 280.0                           # 基频格子 280 米 ≈ 8 格
NOISE_GAIN = (1.0, 0.5, 0.25)             # 三个倍频
RUGGED_SIGMA_M = 800.0                    # 多少起伏算"山区"
AMP_FLOOR_M = 4.0                         # 平原也要有一点点性格，不然是一面镜子
# σ 是在星球层一格（≈39 公里，3×3 邻域）的量级上量的，块只有 8.96 公里。
# 地形是自仿射的：可见起伏 ≈ σ·(L/Lσ)^H。取 H=0.65 → 一块地摊到 0.38σ。
# 直接拿 σ 当块内起伏会高估一倍多，后果是低海拔块被自己的"山谷"挖到海平面以下。
RELIEF_H = 0.65
RELIEF_FOLD = (WIDTH_KM / (2 * 19.6)) ** RELIEF_H      # ≈ 0.383
SEA_DETAIL = 0.35                         # 海底细节只做减法：水不是方块，不许冒出海面
MIN_COS_LAT = math.cos(math.radians(89.0))

RULE = "QL-EARTH-LOCAL-v1"
MEASURED = {
    "path": os.path.join(DATA, "earth-height.bin"),
    "manifest": os.path.join(DATA, "earth-manifest.json"),
    "center": (36.1527, 120.6652),
    "bbox_deg": (120.615359, 36.112456, 120.715041, 36.192944),   # west, south, east, north
    "sha256": "805868b90a88e3df2d43658cb8579dee9cebd1fe566794d52afabf05184248f8",
}


# ---------------------------------------------------------------- 星球层反量化

def land_meters(code):
    """陆地码 128..255 → 米（档中值），向量化。与 ui/planet-view.js 里那份逐字同义。

    逐档推进：进不了本档的把档宽扣掉再看下一档，落进本档的当场定值。"""
    c = np.asarray(code, dtype=np.int64) - SEA_CODES
    acc = np.zeros(c.shape, dtype=np.float64)
    for start, step, count in LAND_BUCKETS:
        inb = (c >= 0) & (c < count)
        acc = np.where(inb, start + (c + 0.5) * step, acc)
        c = np.where(inb, -1, c - count)
    top_start, top_step, top_count = LAND_BUCKETS[-1]
    return np.where(c >= 0, top_start + (top_count - 0.5) * top_step, acc)


def _land_meters(code):
    """标量版，给 regional_relief 那种一次一格的调用。"""
    return float(land_meters(np.array([code]))[0])


def unquantize(b):
    """星球层 1 字节 → 米。0 = 无数据 → NaN，绝不静默当 0 米（补 0 会把缺数据烤成一片海）。"""
    b = np.asarray(b, dtype=np.uint8)
    out = np.full(b.shape, np.nan, dtype=np.float64)
    sea = (b > 0) & (b < SEA_CODES)
    out[sea] = -((SEA_CODES - 1 - b[sea].astype(np.int64)) + 0.5) * OCEAN_STEP
    land = b >= SEA_CODES
    if land.any():
        out[land] = land_meters(b[land])
    return out


# ---------------------------------------------------------------- 输入

def load_planet():
    p = os.path.join(UI, "earth-global-h.bin")
    buf = open(p, "rb").read()
    assert len(buf) == GW * GH, f"星球层尺寸 {len(buf)} != {GW * GH}"
    return np.frombuffer(buf, dtype=np.uint8).reshape(GH, GW)


def load_measured():
    """实测块 + 它的清单；文件缺失或 sha256 不配对时返回 None，宁可不覆盖也不能悄悄用错数据。"""
    p = MEASURED["path"]
    if not os.path.exists(p):
        return None
    buf = open(p, "rb").read()
    if hashlib.sha256(buf).hexdigest() != MEASURED["sha256"]:
        raise SystemExit(f"实测块 sha256 不配对：{p} 已被改动，拒绝当真实数据用")
    return np.frombuffer(buf, dtype=np.uint8).reshape(N, N)


# ---------------------------------------------------------------- 骨架

def grid_coords(lat_c, lon_c):
    """窗口内每格中心（行 0 在最北，列 0 在最西），与 terrain::height_at(x,y) 同向。

    注意单位：窗口宽是**公里**，M_PER_DEG 是**米/度**，差 1000 倍。之前直接相除，
    烤出来的"块"只有 9 米宽——星球层四角几乎同值、噪声格点几乎不动，于是每一块地
    都是平的，怎么调量化都没用。"""
    half_m = WIDTH_KM * 1000.0 / 2.0
    dlat = half_m / M_PER_DEG
    dlon = half_m / (M_PER_DEG * max(math.cos(math.radians(lat_c)), MIN_COS_LAT))
    lats = lat_c + dlat - (np.arange(N) + 0.5) * (2 * dlat / N)
    lons = lon_c - dlon + (np.arange(N) + 0.5) * (2 * dlon / N)
    return lats, lons


def offshore_cells(land):
    """离岸距离（格）：四邻扩散，边界外补 inf（越界不等于绕回地图另一头）。
    与 fetch_earth_dem.offshore_cells 同算法同结果——纯整数，与浮点/顺序无关。"""
    inf = 1 << 30
    d = np.where(land, 0, inf).astype(np.int32)
    hh, ww = d.shape
    for _ in range(hh + ww):
        p = np.pad(d, 1, constant_values=inf)
        nd = d
        for dr, dc in ((0, 1), (0, -1), (1, 0), (-1, 0)):
            nd = np.minimum(nd, p[1 + dr:1 + dr + hh, 1 + dc:1 + dc + ww] + 1)
        if np.array_equal(nd, d):
            break
        d = nd
    return np.where(land, 0, d)


def regional_relief(H, lat_c, lon_c, r=1):
    """周围 (2r+1)² 格**陆地**的极差（米）——微地形幅度的唯一依据。海格不参与：
    把 -6000 米的海底混进极差，海岸线上每一块平原都会被烤成高山。"""
    i = int(np.clip((lon_c + 180) / 360 * GW, 0, GW - 1))
    j = int(np.clip((90 - lat_c) / 180 * GH, 0, GH - 1))
    vals = [int(H[jj, ii]) for jj in range(max(j - r, 0), min(j + r + 1, GH))
            for ii in range(max(i - r, 0), min(i + r + 1, GW))]
    vals = [_land_meters(c) for c in vals if c >= SEA_CODES]
    return (max(vals) - min(vals)) if vals else 0.0


def base_field(H, lats, lons):
    """窗口骨架：陆地-only 加权双线性 + 离岸下切的海底模型。

    为什么不能直接双线性：跨海岸的四角混合会把 -6000 米的海搅进地面高度，海岸线两侧
    各 19 公里都是假的深坑。所以权重只算陆地角，权重和为 0 的格判成海。"""
    fx = (lons + 180.0) / 360.0 * GW - 0.5     # 列 → 星球层 x
    fy = (90.0 - lats) / 180.0 * GH - 0.5      # 行 → 星球层 y
    i0, tx = np.floor(fx).astype(np.int64), (fx - np.floor(fx))[None, :]
    j0, ty = np.floor(fy).astype(np.int64), (fy - np.floor(fy))[:, None]
    num = np.zeros((N, N))
    den = np.zeros((N, N))
    land = np.zeros((N, N), dtype=bool)
    for dj, wy in ((0, 1 - ty), (1, ty)):
        for di, wx in ((0, 1 - tx), (1, tx)):
            jj = np.clip(j0 + dj, 0, GH - 1)[:, None]
            ii = np.clip(i0 + di, 0, GW - 1)[None, :]
            code = H[jj, ii]
            l = code >= SEA_CODES
            w = wx * wy
            num += np.where(l, unquantize(code), 0.0) * (w * l)
            den += w * l
            land |= l & (w > 0.25)
    h_land = np.where(den > 0, num / np.maximum(den, 1e-12), 0.0)
    shelf = np.maximum(-SHELF_M_PER_CELL * offshore_cells(land), -SHELF_MAX_M)
    return np.where(land, h_land, shelf), land


# ---------------------------------------------------------------- 微地形

def h32(x, y, seed):
    """32 位整数哈希（Murmur3 的 finalizer）。JS 用 Math.imul、Rust 用 wrapping_mul，
    三方结果逐位相同——这是"不可作假"能成立的前提。标量版，只给自检用。"""
    m = 0xFFFFFFFF
    h = (int(seed) ^ (int(x) * 0x27D4EB2D) ^ (int(y) * 0x165667B1)) & m
    h = (h ^ (h >> 16)) & m
    h = (h * 0x85EBCA6B) & m
    h = (h ^ (h >> 13)) & m
    h = (h * 0xC2B2AE35) & m
    return (h ^ (h >> 16)) & m


_U32 = np.uint64(0xFFFFFFFF)


def h32_grid(gx, gy, seed):
    """h32 的向量化版：gx/gy 是 int64 数组，逐 uint64 运算后取低 32 位——
    与 JS 的 Math.imul、Rust 的 wrapping_mul 同余。"""
    h = (np.uint64(seed & 0xFFFFFFFF)
         ^ (gx.astype(np.uint64) * np.uint64(0x27D4EB2D) & _U32)
         ^ (gy.astype(np.uint64) * np.uint64(0x165667B1) & _U32)) & _U32
    h = (h ^ (h >> 16)) & _U32
    h = (h * np.uint64(0x85EBCA6B)) & _U32
    h = (h ^ (h >> 13)) & _U32
    h = (h * np.uint64(0xC2B2AE35)) & _U32
    return (h ^ (h >> 16)) & _U32


def value_noise(lats, lons, seed, step_m):
    """格值噪声：平滑（smoothstep）双线性。点阵坐标用等经纬"平面米"，所以相邻块落在
    同一套格点上，不会因为各自取整而错开。"""
    xm = (lons + 180.0) * M_PER_DEG / step_m
    ym = (90.0 - lats) * M_PER_DEG / step_m
    gx0, fx = np.floor(xm).astype(np.int64), xm - np.floor(xm)
    gy0, fy = np.floor(ym).astype(np.int64), ym - np.floor(ym)
    sx = (fx * fx * (3 - 2 * fx))[None, :]
    sy = (fy * fy * (3 - 2 * fy))[:, None]
    v = np.zeros((N, N))
    for dy, wy in ((0, 1 - sy), (1, sy)):
        for dx, wx in ((0, 1 - sx), (1, sx)):
            v += h32_grid(gx0[None, :] + dx, gy0[:, None] + dy, seed) * (wx * wy)
    return v / 4294967296.0                       # [0,1]


def fbm(lats, lons, seed, octaves=3):
    acc = np.zeros((N, N))
    den = 0.0
    for o in range(octaves):
        a = NOISE_GAIN[o]
        acc += a * value_noise(lats, lons, (seed + o * 0x9E3779B9) & 0xFFFFFFFF, NOISE_L / (1 << o))
        den += a
    return acc / den


def detail_field(sigma, seed, lats, lons, base_min_m=None):
    """微地形（米）+ (幅度, 山区度)。

    幅度跟着**真实**区域起伏走：峰到谷铺开 σ 折算到块尺度后的自仿射值（amp = 0.5·σ·fold）。
    之前那个 0.30σ 封顶 260 米是把喜马拉雅烤成一块地毯的元凶；不折算直接取 σ 又会把
    55 米高的开罗挖出海平面。平原走平滑起伏，山区走脊状（ridge = 1-|2n-1|）：
    山脊是尖的、谷是圆的，这是"像山"和"像麻点"的分别。"""
    r = min(1.0, sigma / RUGGED_SIGMA_M)
    amp = max(AMP_FLOOR_M, 0.5 * sigma * RELIEF_FOLD)
    if base_min_m is not None and amp > base_min_m > 0:
        amp = base_min_m            # 谷底不许低于海平面：海岸块本就该是平的
    n = fbm(lats, lons, seed)
    shape = (1.0 - r) * (n - 0.5) + r * ((1.0 - np.abs(2.0 * n - 1.0)) - 0.45)
    return amp * 2.0 * shape, amp, r


# ---------------------------------------------------------------- 量化

def land_basis(hl):
    """陆地段的 (基准米, 米/字节)：从 max(4 米, 最低点) 起算，gain 摊满 149 档，
    但不低于 GAIN_MIN。量化和体检报告共用这一条，免得两边算出两个数。"""
    ref = max(BEACH_M, float(hl.min()))
    return ref, max((float(hl.max()) - ref) / LAND_BYTES, GAIN_MIN)


def quantize_local(h_m, is_land):
    """有效高程（米）→ 高度场字节 1..247。

    海与沙用**绝对**高程：字节 86 必须是真海平面，这是共识常量，逐块归一化会把它冲掉。
    陆从 land_basis 的基准起算、摊满量程；平原起伏小 → gain 贴 GAIN_MIN，只占几档。"""
    f = np.zeros(h_m.shape, dtype=np.float64)
    sea = ~is_land
    if sea.any():
        depth = np.clip(h_m[sea] / -SHELF_MAX_M, 0.0, 1.0)          # 0=海平面，1=贴底
        f[sea] = SEA_FIELD - 1 - depth * (SEA_FIELD - 2)
    if not is_land.any():
        return np.clip(np.rint(f), 1, TOP_FIELD).astype(np.uint8)
    hl = h_m[is_land]
    ref, gain = land_basis(hl)
    beach = hl < BEACH_M                      # 只有基准贴近海平面时才有这一段
    f_l = np.where(beach, SEA_FIELD + (np.clip(hl, 0.0, BEACH_M) / BEACH_M) * (SAND_FIELD - SEA_FIELD),
                   SAND_FIELD + (hl - ref) / gain)
    f[is_land] = f_l
    return np.clip(np.rint(f), 1, TOP_FIELD).astype(np.uint8)


def gen(H, lat_c, lon_c, seed, measured=None):
    """返回 (256×256 字节场, 说明 dict)。"""
    lats, lons = grid_coords(lat_c, lon_c)
    sigma = regional_relief(H, lat_c, lon_c)
    h_eff, is_land = base_field(H, lats, lons)
    det, amp, rugged = detail_field(sigma, seed, lats, lons,
                                    float(h_eff[is_land].min()) if is_land.any() else None)
    h = h_eff + np.where(is_land, det, -np.abs(det) * SEA_DETAIL)
    f = quantize_local(h, is_land)
    src = "derived"
    if measured is not None and in_measured_rect(lat_c, lon_c):
        f, src = measured, "measured"
    land = h[is_land]
    info = {"lat": lat_c, "lon": lon_c, "sigma_m": round(sigma, 1), "rugged": round(rugged, 3),
            "land_cells": int(is_land.sum()), "source": src}
    if is_land.any():
        _ref, gain = land_basis(land)
        # base_m 在 JS 与 Rust 那份镜像里都是"字节 SAND_FIELD 那一档的真实海拔"（基面），
        # 不是均值。同名不同义是最坏的一种分叉：字段还在、对拍也过，读的人却各算各的。
        # 均值另存 mean_m，报表要哪个说哪个。
        info.update(amp_m=round(amp, 1), base_m=round(_ref, 1),
                    mean_m=round(float(np.nanmean(h_eff[is_land])), 1),
                    relief_m=round(float(land.max() - land.min()), 1),
                    gain_m_per_byte=round(gain, 2),
                    exag=round((CELL_M / 8.0) / gain, 2))   # 显示垂直夸大倍数
    used = f[f >= SAND_FIELD]
    info["land_bytes_used"] = int(used.max()) - SAND_FIELD if used.size else 0
    return f, info


def in_measured_rect(lat_c, lon_c):
    w, s, e, n = MEASURED["bbox_deg"]
    return w <= lon_c <= e and s <= lat_c <= n


def fingerprint(buf):
    """与 fetch_earth_dem.fingerprint 同一条：两路独立折叠拼成 16 位十六进制。
    改任何一格都会变——JS 镜像实现就靠它对拍。"""
    out = ""
    for basis, prime in ((0x811C9DC5, 16777619), (0x01000193, 3295579737)):
        d = basis
        for b in buf:
            d = (d ^ int(b)) * prime & 0xFFFFFFFF
        out += "%08x" % d
    return out


# ---------------------------------------------------------------- 自检 / 入口

def selftest():
    """口径自检：反量化单调且封顶、哈希三方同余、量化段界没跑偏、同坐标可复现。"""
    b = np.arange(256, dtype=np.uint8)
    u = unquantize(b)
    assert np.isnan(u[0]), "0 码必须是 NaN"
    assert np.all(np.diff(u[1:SEA_CODES]) > 0), "海码不单调"
    assert u[SEA_CODES - 1] == -0.5 * OCEAN_STEP, "最浅海码应是半个档"
    assert u[1] == -((SEA_CODES - 2) + 0.5) * OCEAN_STEP, "最深海码 -6072 米"
    land = u[SEA_CODES:]
    assert np.all(np.diff(land) >= 0), "陆码不单调"
    assert abs(land[0] - 2.0) < 1e-9, f"陆地第一档中值应为 2 米，实为 {land[0]}"
    assert land.max() <= CEILING_M and _land_meters(255) == CEILING_M - LAND_BUCKETS[-1][1] / 2, \
        "陆码上限不对"
    print(f"星球层反量化  海 1..127 单调、陆 128..255 档中值、{CEILING_M:.0f} 米封顶，0 = NaN ✓")

    # 哈希：向量化必须和标量逐位相同（JS/Rust 镜像照抄的是标量那几行）
    rng = np.random.default_rng(7)
    xs, ys = rng.integers(0, 1 << 20, 512), rng.integers(0, 1 << 20, 512)
    got = h32_grid(xs, ys, 0x12345678)
    assert all(int(g) == h32(int(x), int(y), 0x12345678) for x, y, g in zip(xs, ys, got)), \
        "h32 向量化与标量不一致——三方对拍会当场崩"
    assert h32(0, 0, 0) == 0, "Murmur3 finalizer 的 0 不动点没保住"
    print("格点哈希    向量化 == 标量（Math.imul / wrapping_mul 同余）✓")

    # 量化：三段边界各验一次（海=绝对深度，沙=0..4 米绝对高程，陆=块内基准起算）
    h = np.array([-40.0, -2.0, 0.0, 2.0, 4.0, 100.0, 1092.3])
    q = quantize_local(h, h >= 0)
    assert q[0] == 1 and q[1] == 81, f"海底段跑偏：{q[:2]}"
    assert q[2] == SEA_FIELD and q[3] == 92 and q[4] == SAND_FIELD, f"沙带跑偏：{q[2:5]}"
    assert q[5] == 111 and q[6] == TOP_FIELD, f"陆地档跑偏：{q[5:]}"
    assert np.all(np.diff(q) >= 0), "高程越高字节必须不减"
    # 内陆高原：整块都在 800 米以上，字节 86 不该出现，最低一格落在 98
    pq = quantize_local(np.array([800.0, 1000.0]), np.ones(2, dtype=bool))
    assert pq[0] == SAND_FIELD and pq[1] > pq[0], f"内陆基准跑偏：{pq}"
    # 摊满量程 + 夸大封顶：山区铺到顶，平原只占几档，谁也不许把 20 米放大成悬崖
    assert land_basis(np.array([4.0, 1092.3]))[1] == MEASURED_GAIN, "实测崂山的档距锚变了"
    mtn = quantize_local(np.linspace(1000.0, 4000.0, 64), np.ones(64, dtype=bool))
    assert mtn[0] == SAND_FIELD and mtn[-1] == TOP_FIELD, f"3000 米山区没铺满量程：{mtn[-1]}"
    plain = quantize_local(np.linspace(200.0, 220.0, 64), np.ones(64, dtype=bool))
    assert plain[-1] - plain[0] == int(20.0 / GAIN_MIN), f"20 米平原被放大了：{plain[-1] - plain[0]} 档"
    assert (CELL_M / 8.0) / land_basis(np.linspace(200.0, 220.0, 64))[1] <= MAX_EXAG + 1e-9
    print(f"局部量化    海 85..1 / 沙 {SEA_FIELD}..{SAND_FIELD}（0..{BEACH_M:.0f} 米绝对）/ "
          f"陆 {SAND_FIELD}..{TOP_FIELD} 摊满量程，gain ≥ {GAIN_MIN:.2f} 米/字节"
          f"（夸大封顶 {MAX_EXAG}×；实测崂山 {MEASURED_GAIN:.2f} = 0.6×）✓")

    # 可复现：同坐标同种子两次必须逐字节相同
    H = load_planet()
    la, lo = grid_coords(0.0, 0.0)
    for axis, got in (("南北", (la[0] - la[1]) * M_PER_DEG),
                      ("东西", (lo[1] - lo[0]) * M_PER_DEG)):
        assert abs(got - CELL_M) < 0.02 * CELL_M, \
            f"格子{axis}边长 {got:.1f} 米，应为 {CELL_M} 米——窗口尺寸算错了，块会烤成一面平的"
    a, ia = gen(H, 46.5, 10.2, 42)
    same, _ = gen(H, 46.5, 10.2, 42)
    assert np.array_equal(a, same), "同坐标同种子不可复现——链上无法重放验证"
    d, _ = gen(H, 46.5, 10.2, 43)
    assert not np.array_equal(a, d), "换种子没换出东西——哈希没进地形"
    assert ia["land_cells"] > 0, "自检点应落在陆地"
    assert ia["relief_m"] > 100.0, f"东阿尔卑斯一块地只有 {ia['relief_m']} 米起伏——微地形没进高程场"
    assert int(a.max()) - int(a.min()) > 40, f"阿尔卑斯块只用了 {int(a.max()) - int(a.min())} 档字节"
    # 低海拔不许被自己的"山谷"挖到海平面以下（开罗 55 米：整块陆地档 ≥ 98）
    _, ic = gen(H, 30.05, 31.24, 42)
    assert ic["relief_m"] > 50.0 and ic["land_bytes_used"] > 30, f"开罗块平了：{ic}"
    fc, _ = gen(H, 30.05, 31.24, 42)
    assert fc[fc >= SAND_FIELD].min() == SAND_FIELD and (fc >= SEA_FIELD).all(), \
        "陆地格掉进了海底段——海岸块会把城市烤成一摊平地"
    print(f"确定性      同坐标同种子逐字节相同、换种子即变；格子 {CELL_M:.0f} 米见方、"
          f"东阿尔卑斯块起伏 {ia['relief_m']:.0f} 米（{RULE}）✓")

    m = load_measured()
    if m is not None:
        man = json.load(open(MEASURED["manifest"]))
        assert fingerprint(m.tobytes()) == man["heights_fingerprint"], "实测块指纹与清单不符"
        f, info = gen(H, *MEASURED["center"], 42, measured=m)
        assert info["source"] == "measured" and np.array_equal(f, m), "席位在实测矩形内却没用到实测数据"
        print(f"实测优先    崂山窗口取到 earth-height.bin（指纹 {man['heights_fingerprint']}）✓")


def to_png(f, path):
    from PIL import Image
    img = (f.astype(np.float64) - f.min()) / max(1.0, float(f.max() - f.min()))
    Image.fromarray((img * 255).astype(np.uint8), "L").resize((512, 512), Image.NEAREST).save(path)


def emit_js(f, path, info):
    """把这块地写成一份 QLEarth（world.js 只认 `QLEarth.load()` 这一个口）。

    为什么要这个出口：想拿真实渲染器看一块**不是崂山**的地，唯一不改共识的办法就是
    换掉喂给它的那 65536 字节。base64 而不是 fetch，是因为 Tauri 的 CSP 是
    `default-src 'self'`，预览页与打包页共用同一条加载路径。"""
    import base64
    b64 = base64.b64encode(f.tobytes()).decode("ascii")
    src = ["/* 生成文件，勿手改：tools/world/gen_planet_local.py --emit-js\n"
           " * 用途只有两个——用真实渲染器预览一块派生席位的地，以及与 JS 实现对拍指纹。 */\n",
           "(function (global) {\n  'use strict';\n  global.QLEarth = {\n",
           f"    center: [{info['lat']!r}, {info['lon']!r}],\n",
           f"    rule: {RULE!r},\n    fingerprint: {fingerprint(f.tobytes())!r},\n",
           f"    sha256: {hashlib.sha256(f.tobytes()).hexdigest()!r},\n",
           "    bytes: null,\n    b64: ''\n"]
    src += [f"      + {b64[i:i + 1024]!r}\n" for i in range(0, len(b64), 1024)]
    src.append("""      ,
    load: function () {
      if (this.bytes) return this.bytes;
      var bin = typeof atob === 'function' ? atob(this.b64) : Buffer.from(this.b64, 'base64').toString('binary');
      var out = new Uint8Array(bin.length);
      for (var i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
      if (out.length !== 65536) throw new Error('QLEarth: 高度场字节数 ' + out.length + ' 不对');
      this.bytes = out;
      return out;
    }
  };
  if (typeof module !== 'undefined' && module.exports && typeof process !== 'undefined') module.exports = global.QLEarth;
})(typeof window !== 'undefined' ? window : globalThis);
""")
    open(path, "w", encoding="utf-8").write("".join(src))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--center", nargs=2, type=float, metavar=("LAT", "LON"),
                    default=[36.13, 120.626])
    ap.add_argument("--seed", type=lambda v: int(v, 0), default=0)
    ap.add_argument("--many", default="", metavar="LAT,LON[;...]",
                    help="批量体检：分号分隔的坐标（负纬度以 - 开头会被 argparse 当成选项，所以收成一个串）")
    ap.add_argument("--out", help="把高度场写到 .bin（与 JS 实现对拍）")
    ap.add_argument("--png", help="同时写一张灰度俯视图，肉眼先看形状对不对")
    ap.add_argument("--emit-js", help="写成一份 QLEarth（换掉渲染器喂的那 65536 字节，用真画面看一块派生席位）")
    ap.add_argument("--manifest", help="把本次生成参数与指纹写成 JSON")
    ap.add_argument("--use-measured", action="store_true", help="允许席位落在实测矩形时直接用 30 米 DEM")
    ap.add_argument("--selftest-only", action="store_true")
    a = ap.parse_args()

    selftest()
    if a.selftest_only:
        return

    H = load_planet()
    meas = load_measured() if a.use_measured else None
    spots = [tuple(map(float, s.split(","))) for s in a.many.split(";") if s.strip()]
    if not spots:
        spots = [tuple(a.center)]
    for lat, lon in spots:
        if not (-90 <= lat <= 90 and -180 <= lon <= 180):
            raise SystemExit(f"坐标越界：{lat},{lon}")
        f, info = gen(H, lat, lon, a.seed, meas)
        g = lambda k, d="—": info.get(k, d)
        print(f"{lat:+8.3f},{lon:+9.3f}  {info['source']:8s} 陆地 {info['land_cells']:5d}/65536 "
              f"基面 {g('base_m'):>8} m 均值 {g('mean_m'):>8} m 起伏 {g('relief_m'):>8} m σ={info['sigma_m']:7.1f}m "
              f"r={info['rugged']:.2f} 档距 {g('gain_m_per_byte'):>6} m/B 夸大 {g('exag'):>5}× "
              f"字节 {f.min():3d}..{f.max():3d} 用档 {info['land_bytes_used']:3d} "
              f"峰层 {int(f.max()) >> 3:2d}  指纹 {fingerprint(f.tobytes())}")
    if a.out and len(spots) == 1:
        open(a.out, "wb").write(f.tobytes())
        print(f"→ {a.out}  sha256 {hashlib.sha256(f.tobytes()).hexdigest()[:16]}")
    if a.png and len(spots) == 1:
        to_png(f, a.png)
        print(f"→ {a.png}")
    if a.emit_js and len(spots) == 1:
        emit_js(f, a.emit_js, info)
        print(f"→ {a.emit_js}  指纹 {fingerprint(f.tobytes())}")
    if a.manifest and len(spots) == 1:
        man = {"rule": RULE, "seed": a.seed, "grid": [N, N], "meters_per_cell": CELL_M,
               "width_km": WIDTH_KM, "bytes": int(f.nbytes),
               "planet_layer": {"grid": [GW, GH], "file": "earth-global-h.bin"},
               "sources": ["measured 30m DEM inside bbox" if meas is not None else None,
                           "planet layer 19.6km bilinear (land-only)",
                           f"fbm hash detail L={NOISE_L:.0f}m oct=3"],
               "quantize": {"sea_field": SEA_FIELD, "sand_field": SAND_FIELD,
                            "top_field": TOP_FIELD, "beach_m": BEACH_M,
                            "land_basis": "max(beach_m, block land min)",
                            "gain_m_per_byte": "relief/149, floored",
                            "gain_min_m_per_byte": round(GAIN_MIN, 4),
                            "max_vertical_exaggeration": MAX_EXAG},
               "detail": {"amplitude_m": "0.5 * regional relief sigma",
                          "rugged_sigma_m": RUGGED_SIGMA_M, "amp_floor_m": AMP_FLOOR_M},
               **info, "heights_fingerprint": fingerprint(f.tobytes()),
               "sha256": hashlib.sha256(f.tobytes()).hexdigest()}
        man["sources"] = [s for s in man["sources"] if s]
        open(a.manifest, "w").write(json.dumps(man, ensure_ascii=False, indent=1))
        print(f"→ {a.manifest}")


if __name__ == "__main__":
    main()
