#!/usr/bin/env python3
"""烤出**整颗地球**：全球真实高程网格 + 全球真实卫星影像，随桌面程序分发。

为什么要有这一层：之前只烤了崂山边 8.96 公里的一小块，于是"世界"成了一塊补丁——
玩家被随机分到地球别处时无脚可站。这一层把口径从"窗口"改成"星球"。

**1 米格是什么单位**：一格 = 1 米是地球上最小的*劳动单位*和*地址单位*，不是存储单位。
全球按 1 米切是 5.1×10^14 格，1 字节一格要 510 TB，谁也存不下（谷歌地球也不存，它是
按层级现拉）。所以本脚本产出的是三档 LOD 里最粗的那一档：
  星球层（本文件，随包）  →  区域层（30 米 DEM，到了再拉并缓存）  →  工作体（1 米体素）
一格在全球网格里跨多少米由 hside 决定；它只回答"这是哪块地、大概多高"。

源与合法性（硬约束，别自己骗自己）：
- 高程 = Mapzen/AWS terrain tiles（terrarium 编码，SRTM/Copernicus 衍生的开放数据）。
- 影像 = NASA Blue Marble Next Generation（2012 合成）via NASA GIBS。**公有领域**，
  所以能烤进安装包。ESRI World Imagery 那种更清晰的图只能运行时由客户端自己拉瓦片、
  当缓存，不随包分发——那是许可条款，不是技术问题。

网格是等经纬（equirectangular），不是墨卡托：
- `cell(lat, lon)` 变成一次乘加，且每格的**地表面积**在极地不会虚胖十倍；
- 席位按真实面积算，用墨卡托等于承认格陵兰比刚果大十二倍。
墨卡托瓦片投到等经纬时按纬度做面积平均——墨卡托的 y 本身是纬度的非线性函数，
直接按行数平均会把高纬的地面压扁。
"""
import argparse
import hashlib
import json
import math
import os
import urllib.request
from concurrent.futures import ThreadPoolExecutor

import numpy as np
from PIL import Image

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DATA = os.path.join(REPO, "desktop-wallet", "data")
CACHE = os.path.join(REPO, "tools", "world", "planet_cache")

DEM_URL = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png"
# BMNG 在 GIBS 的 3857 矩阵集最高到 level 8，z5/z6 都取得到
IMG_URL = ("https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/"
           "BlueMarble_NextGeneration/default/2012-01-01/"
           "GoogleMapsCompatible_Level8/{z}/{y}/{x}.jpg")
UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15"

MAX_LAT = math.radians(85.05112878)  # 墨卡托世界的南北边界，超出即钳位
SEA_CODES = 128                       # 字节 < 此值 = 海，>= = 陆（陆地第一档从它开始）

# 1 字节高程桶，码位随高度单调递增：0 = 无数据，1..127 = 海（48 米一档，越浅越贴 127），
# 128..255 = 陆。陆地表是 (起点, 档宽, 档数)，档数合计正好 128，边界全是 2 的幂、
# 档宽逐段翻倍——低海拔密（玩家住这一段）、高海拔稀，且 Rust 与 JS 各抄一次也不容易抄歪。
# 上限 8192 米：珠峰的 8848 会钳到最后一档，这是明知故犯的取舍（游戏不要测绘）。
LAND_BUCKETS = [(0, 4, 32), (128, 8, 16), (256, 16, 16), (512, 32, 16),
                (1024, 64, 16), (2048, 128, 16), (4096, 256, 16)]
OCEAN_STEP = 48.0
LAND_CEIL = 8192.0


def _land_edges():
    e, base = [0.0], 0
    for (a, step, n) in LAND_BUCKETS:
        assert abs(a - e[-1]) < 1e-9, f"陆地桶不连续：{a} != {e[-1]}"
        base += n
        e.append(a + step * n)
    assert base == SEA_CODES, f"陆地码位 {base} 个，但只有 {SEA_CODES} 个可用"
    return e


EDGES = _land_edges()


def _quantize_land(lvl):
    """米 → 128..255。逐段定位，越界钳到端点。"""
    code = np.full(lvl.shape, 2 * SEA_CODES - 1, dtype=np.int64)
    base = SEA_CODES
    for bi, (a, step, n) in enumerate(LAND_BUCKETS):
        hi = EDGES[bi + 1]
        m = (lvl >= a) & (lvl < hi)
        code[m] = base + np.clip(((lvl[m] - a) / step).astype(np.int64), 0, n - 1)
        base += n
    return code


def quantize_height(h):
    """米 → 1 字节。码位必须**随高度单调递增**（海与陆同向），否则海岸两侧会打架：
    上一版海面是"越深码越大"，接到陆地的 128 起步就断了，自检当场抓住。
      0 = 无数据（墨卡托世界以外的极冠）；1..127 = 海，127 最浅；128..255 = 陆。
    """
    h = np.asarray(h, dtype=np.float64)
    out = np.empty(h.shape, dtype=np.uint8)
    sea = ~(h > 0)
    depth = np.clip(-h[sea], 0.0, OCEAN_STEP * (SEA_CODES - 2))
    out[sea] = (SEA_CODES - 1 - np.clip((depth / OCEAN_STEP).astype(np.int64), 0, SEA_CODES - 2)).astype(np.uint8)
    out[~sea] = _quantize_land(np.minimum(h[~sea], LAND_CEIL - 1e-6)).astype(np.uint8)
    return out


def unquantize_height(b):
    """字节 → 桶中心米数。前端与自检共用这一个反量化口径。"""
    b = np.asarray(b)
    flat = b.ravel().astype(np.int64)
    ocean = (flat >= 1) & (flat < SEA_CODES)
    acc = np.full(flat.shape, np.nan)
    # 0 = 无数据：留 NaN 让它炸，而不是补 0 米——补 0 会把缺数据烤成一片海，
    # 而"一片海"看上去并不荒谬，没人会发现。
    acc[flat == 0] = float("nan")
    depth = (SEA_CODES - 1 - flat[ocean] + 0.5) * OCEAN_STEP
    acc[ocean] = -depth
    base = SEA_CODES
    for (a, step, n) in LAND_BUCKETS:
        m = (flat >= base) & (flat < base + n)
        acc[m] = a + (flat[m] - base + 0.5) * step
        base += n
    acc[(flat >= base) & (flat >= SEA_CODES)] = LAND_CEIL
    return acc.reshape(b.shape)


def get(url, path):
    if not os.path.exists(path):
        req = urllib.request.Request(url, headers={"User-Agent": UA})
        for attempt in range(3):
            try:
                with urllib.request.urlopen(req, timeout=60) as r:
                    blob = r.read()
                break
            except Exception:
                if attempt == 2:
                    raise
        with open(path, "wb") as f:
            f.write(blob)
    return path


def merc01(lat_rad):
    """纬度弧度 → 墨卡托归一化 y（0 = 最北）。超界钳到墨卡托世界边。"""
    lat = min(max(lat_rad, -MAX_LAT), MAX_LAT)
    return 0.5 - math.asinh(math.tan(lat)) / (2 * math.pi)


def selftest_quantize():
    """桶表自证：单调、无断档、可表示区内误差不超过半档、越界饱和而非绕回。

    这张表错了不会炸，只会让喜马拉雅变成丘陵、或者把海烤成一圈墙——所以每次烤
    之前先自己对一遍。上一版就是靠这一段抓出"陆地码位要 782 个但只有 128 个"。
    """
    sweep = np.linspace(-6000, 8100, 28001)   # 可表示区间内，两端各留一档避开钳位边
    codes = quantize_height(sweep)
    assert (codes[1:] >= codes[:-1]).all(), "量化不单调：高度表烤出来会打乱山序"
    assert (np.diff(codes) <= 1).all(), "码位跳跃超过 1：桶表中间断档了"
    back = unquantize_height(codes)
    step_at = np.full(sweep.shape, OCEAN_STEP)
    for (a, step, n) in LAND_BUCKETS:
        step_at[(sweep >= a) & (sweep < a + step * n)] = step
    bad = np.abs(back - sweep) > step_at / 2 + 1e-6
    assert not bad.any(), f"{int(bad.sum())} 个采样点反量化超出半档，例如 {sweep[bad][0]} m"
    assert quantize_height(np.array([-9000.0]))[0] == quantize_height(np.array([-7000.0]))[0], "深渊必须饱和"
    assert quantize_height(np.array([99000.0]))[0] == 255, "超高山必须钳到最后一档"
    assert np.isnan(unquantize_height(np.array([0]))[0]), "0 必须是'无数据'，不能是 0 米的海"


def col_weights(px, out_w):
    """等经纬的列（经度等分）→ 墨卡托像素列的面积权重。一次算好，逐行复用。"""
    step = px / out_w
    edge = np.arange(out_w + 1) * step
    k = int(math.ceil(edge[1] - edge[0])) + 1
    j0 = edge[:-1].astype(np.int64)
    idx = np.minimum(j0[:, None] + np.arange(k)[None, :], px - 1)
    w = np.clip(np.minimum(edge[1:, None], idx + 1) - np.maximum(edge[:-1, None], idx), 0, None)
    if (w.sum(axis=1) <= 0).any():
        raise SystemExit("自检失败：某输出列与源像素列零重叠")
    return idx, w / w.sum(axis=1, keepdims=True)


def col_reduce(ci, cw, v):
    """逐墨卡托源列的值 → 等经纬输出列（面积加权，邻列 gather）。

    这里必须逐行点积（einsum 'ij,ij->i'）而不是 tensordot：v[ci] 和 cw 同为
    (输出列, 邻列)，tensordot 消掉邻列轴会得到 (输出列, 输出列) 那张 1600 万的方阵。
    """
    return np.einsum("ij,ij->i", v[ci], cw)


def row_band(j, out_h, px):
    """等经纬输出第 j 行的纬度带 → 墨卡托源行索引与面积权重。

    墨卡托世界只到 ±85.05°，再往北(南)没有像素。那里**贴边取最近一行**，
    不静默补 0：补 0 会把两极烤成一片海，而"一片海"看上去并不荒谬，没人会发现。
    """
    lat_hi = math.radians(90.0 - 180.0 * j / out_h)
    lat_lo = math.radians(90.0 - 180.0 * (j + 1) / out_h)
    r_hi, r_lo = merc01(lat_hi) * px, merc01(lat_lo) * px
    if r_lo - r_hi <= 1e-9:
        r = int(min(max(round(r_hi), 0), px - 1))
        return np.array([r]), np.array([1.0])
    i0 = min(max(int(math.floor(r_hi)), 0), px - 1)
    i1 = min(max(int(math.ceil(r_lo)), i0 + 1), px)
    idx = np.arange(i0, i1)
    w = np.clip(np.minimum(idx + 1.0, r_lo) - np.maximum(idx, r_hi), 0, None)
    if w.sum() <= 0:
        raise SystemExit(f"自检失败：输出行 {j} 与源栅格零重叠")
    return idx, w


def decode_dem(z, n):
    """全球 terrarium → (px, px) int16 米。逐瓦片填进预分配数组：绝不做 vstack，
    上一版就是在拼大数组时被系统静默 SIGKILL（连报错都没有，很骗人）。"""
    px = n * 256
    H = np.empty((px, px), dtype=np.int16)
    for y in range(n):
        for x in range(n):
            p = os.path.join(CACHE, f"dem_{z}_{x}_{y}.png")
            t = np.asarray(Image.open(p).convert("RGB"), dtype=np.int32)
            H[y * 256:(y + 1) * 256, x * 256:(x + 1) * 256] = (t[:, :, 0] * 256 + t[:, :, 1] - 32768).astype(np.int16)
        if (y + 1) % 8 == 0:
            print(f"  dem 行 {y + 1}/{n}", flush=True)
    return H


def bake_height(H, out_w):
    px = H.shape[0]
    out_h = out_w // 2
    ci, cw = col_weights(px, out_w)
    rows = np.empty((out_h, out_w), dtype=np.uint8)
    for j in range(out_h):
        idx, w = row_band(j, out_h, px)
        band = H[idx].astype(np.float64)                 # (k, px) 米
        land = (band > 0).astype(np.float64)
        wn = w / w.sum()
        # 先把这一纬度带压成"每条墨卡托源列"的陆地均值与陆地占比：海陆混进同一次
        # 平均会抹平岸线，与 fetch_earth_dem.resample 同一个理由。
        lf = (wn[:, None] * land).sum(axis=0)
        lh = (wn[:, None] * np.where(land > 0, band, 0.0)).sum(axis=0) / np.maximum(lf, 1e-9)
        sf = (wn[:, None] * (1.0 - land)).sum(axis=0)
        sh = (wn[:, None] * np.where(land > 0, 0.0, -band)).sum(axis=0) / np.maximum(sf, 1e-9)
        # 再跨列做面积平均。cw 是 (输出列, k)，所以必须按 ci gather 出邻列再点积，
        # 直接 cw @ 逐源列向量是把 8192 当成 3 来乘（踩过，报错很直白）。
        frac = col_reduce(ci, cw, lf)
        lmean = col_reduce(ci, cw, lh * lf) / np.maximum(frac, 1e-9)
        smean = col_reduce(ci, cw, sh)
        vals = np.where(frac >= 0.5, lmean, -smean)
        rows[j] = quantize_height(vals)
        if (j + 1) % 256 == 0:
            print(f"  height 行 {j + 1}/{out_h}", flush=True)
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--zoom", type=int, default=6, help="源瓦片层级（全球 2^z × 2^z 张）")
    ap.add_argument("--hside", type=int, default=8192, help="全球高程网格宽，高为一半")
    ap.add_argument("--isz", type=int, default=6, help="影像源瓦片层级")
    ap.add_argument("--iside", type=int, default=8192, help="全球影像贴图宽，高为一半")
    ap.add_argument("--step", type=int, default=0, help="只下载每 N 张之一（先试跑用）")
    ap.add_argument("--skip-dem", action="store_true")
    ap.add_argument("--skip-img", action="store_true")
    ap.add_argument("--offline", action="store_true")
    a = ap.parse_args()

    os.makedirs(CACHE, exist_ok=True)
    os.makedirs(DATA, exist_ok=True)
    info = {}
    selftest_quantize()

    if not a.skip_dem:
        z, n = a.zoom, 2 ** a.zoom
        jobs = [(x, y) for y in range(n) for x in range(n)]
        if a.step:
            jobs = jobs[::a.step]
        print(f"高程：z{z} 全球 {n}x{n} 张，取 {len(jobs)} 张")
        if not a.offline:
            with ThreadPoolExecutor(max_workers=10) as pool:
                list(pool.map(lambda j: get(DEM_URL.format(z=z, x=j[0], y=j[1]),
                                            os.path.join(CACHE, f"dem_{z}_{j[0]}_{j[1]}.png")), jobs))
        H = decode_dem(z, n) if not a.step else decode_partial(z, n, jobs)
        print(f"高程栅格 {H.shape[0]}x{H.shape[1]} · 最高 {int(H.max())} m / 最低 {int(H.min())} m")
        rows = bake_height(H, a.hside)
        del H
        # 自检：反量化回来 + 陆地占比。烤糊了要当场看见，不能等到玩家在画面里
        # 发现喜马拉雅是平的、或者整个北冰洋变成一块陆地。
        back = unquantize_height(rows)
        land = float((rows >= SEA_CODES).mean())
        print(f"全球高程 {rows.shape[1]}x{rows.shape[0]} · 反量化最高 {back.max():.0f} m · 陆地占比 {land:.1%}")
        # 地球陆地占 29%。烤出来的占比跑到 20%~40% 之外，说明海陆判据或面积权重错了，
        # 而不是"这次数据有点不一样"——这一条拦的是把西伯利亚烤成海那类静默事故。
        assert 0.20 < land < 0.40, f"陆地占比 {land:.1%} 偏离地球真实值 29% 太远，海陆判据错了"
        assert back.max() >= 6000, f"反量化最高只有 {back.max():.0f} m：山被烤平了（桶表或量化钳位有问题）"
        hb = rows.tobytes()
        with open(os.path.join(DATA, "earth-global-h.bin"), "wb") as f:
            f.write(hb)
        info["height"] = {"file": "earth-global-h.bin", "zoom_src": z, "width": a.hside,
                          "height": a.hside // 2, "bytes": len(hb),
                          "sha256": hashlib.sha256(hb).hexdigest(),
                          "cell_m_equator": round(40075.0167 / a.hside, 3),
                          "quantize": "LAND_BUCKETS/OCEAN_STEP in fetch_earth_planet.py"}
        print(f"  {info['height']['bytes']/1048576:.2f} MiB · sha256 {info['height']['sha256'][:16]}")

    if not a.skip_img:
        z, n = a.isz, 2 ** a.isz
        jobs = [(x, y) for y in range(n) for x in range(n)]
        if a.step:
            jobs = jobs[::a.step]
        print(f"影像：z{z} 全球 {n}x{n} 张，取 {len(jobs)} 张")
        if not a.offline:
            def dl(j):
                return get(IMG_URL.format(z=z, x=j[0], y=j[1]),
                           os.path.join(CACHE, f"img_{z}_{j[0]}_{j[1]}.jpg"))
            with ThreadPoolExecutor(max_workers=10) as pool:
                list(pool.map(dl, jobs))
        img = bake_merc_image(z, n, a.iside)
        im = Image.fromarray(img, "RGB")
        jp = os.path.join(DATA, "earth-global.jpg")
        im.save(jp, quality=82, optimize=True, subsampling=1)
        raw = open(jp, "rb").read()
        info["imagery"] = {"file": "earth-global.jpg", "source": IMG_URL,
                           "attribution": "NASA Blue Marble Next Generation (2012) via NASA GIBS — public domain",
                           "zoom_src": z, "width": a.iside, "height": a.iside // 2,
                           "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}
        print(f"  {len(raw)/1048576:.2f} MiB · sha256 {info['imagery']['sha256'][:16]}")

    mp = os.path.join(DATA, "earth-manifest.json")
    man = json.load(open(mp)) if os.path.exists(mp) else {}
    # 只 --skip-dem / --skip-img 半跑时保留另一半的记录：否则一次半跑会把已经烤好的
    # 那半边的 sha 从 manifest 里抹掉，前端就再也对不上指纹了
    man["planet"] = {**man.get("planet", {}),
                     "rule": "QL-EARTH-PLANET-v1", "projection": "equirectangular",
                     "grid_unit_m": 1, "lod": ["planet", "region-30m", "workvolume-1m"], **info}
    with open(mp, "w") as f:
        json.dump(man, f, ensure_ascii=False, indent=2)
        f.write("\n")
    print("manifest.planet 已写入")


def decode_partial(z, n, jobs):
    """试跑：只解码抓到的那些瓦片，其余留 0（自检只看形状，不看数值）。"""
    px = n * 256
    H = np.zeros((px, px), dtype=np.int16)
    have = set(jobs)
    for y in range(n):
        for x in range(n):
            if (x, y) not in have:
                continue
            t = np.asarray(Image.open(os.path.join(CACHE, f"dem_{z}_{x}_{y}.png")).convert("RGB"), dtype=np.int32)
            H[y * 256:(y + 1) * 256, x * 256:(x + 1) * 256] = (t[:, :, 0] * 256 + t[:, :, 1] - 32768).astype(np.int16)
    return H


def bake_merc_image(z, n, out_w):
    """墨卡托瓦片拼成的全球影像 → 等经纬 RGB（面积平均，逐行流式，不留 201 MB 中间量）。"""
    px = n * 256
    out_h = out_w // 2
    ci, cw = col_weights(px, out_w)
    # 瓦片行缓存：一次只装 2 行瓦片高度（512 px × px × 3 ≈ 12 MB @ z5）
    img = np.empty((out_h, out_w, 3), dtype=np.uint8)
    tile_row_cache = {}

    def src_rows(r0, r1):
        """取源像素行 [r0, r1)（通道在最后）。按需抓取/保留瓦片行。"""
        tr0, tr1 = r0 // 256, (r1 + 255) // 256
        for tr in range(tr0, tr1):
            if tr not in tile_row_cache:
                tiles = []
                for tx in range(n):
                    p = os.path.join(CACHE, f"img_{z}_{tx}_{tr}.jpg")
                    tiles.append(np.asarray(Image.open(p).convert("RGB"), dtype=np.uint8))
                tile_row_cache[tr] = np.hstack(tiles)
        keep = set(range(tr0, tr1))
        for tr in [t for t in tile_row_cache if t not in keep]:
            del tile_row_cache[tr]
        band = np.concatenate([tile_row_cache[tr] for tr in range(tr0, tr1)], axis=0)
        base = tr0 * 256
        return band[r0 - base:r1 - base]

    for j in range(out_h):
        idx, w = row_band(j, out_h, px)
        band = src_rows(int(idx[0]), int(idx[-1]) + 1).astype(np.float32)
        wn = (w / w.sum()).astype(np.float32)
        row = np.tensordot(wn, band, axes=([0], [0]))          # (px, 3) 墨卡托一行压成一条色带
        gathered = row[ci]                                       # (out_w, k, 3)
        # einsum 逐输出行点积；tensordot 会把"消掉邻列轴"变成"再附一个 out_w 轴"，
        # 得到 (out_w, 3, out_w) 那张方阵（高程那一段踩过同一脚坑）
        img[j] = np.einsum("ijl,ij->il", gathered, cw.astype(np.float32)).clip(0, 255).astype(np.uint8)
        if (j + 1) % 256 == 0:
            print(f"  image 行 {j + 1}/{out_h}", flush=True)
    return img


if __name__ == "__main__":
    main()
