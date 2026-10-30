"""三方对拍的夹具：Python 参考算出席位与高度场指纹，写成 JSON 给 JS / Rust 镜像核。

为什么单独一个文件：`seat.py` 的自检只验自己的口径自洽（反量化单调、可复现、分布铺开），
它不知道自己被镜像出去的那两份长什么样。这份夹具才是"另两方必须烤出同一个字节场"
的那道门——JS 与 Rust 各有一份同名的核对脚本，指纹差一个字符就该红着退出。

覆盖是设计出来的：地址列表里既有本机真实地址（它落在哪块地貌就有实际意义），
也有若干随机地址（摊开各种 σ/海拔组合），还有刻意挑的极值（高原、沙漠、近极点）。

用法：
    python3 tools/world/seat_crosscheck.py                 # 写夹具（默认 16 例）
    python3 tools/world/seat_crosscheck.py --out /tmp/x.json --count 24
"""
import hashlib
import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from gen_planet_local import (CELL_M, GH, GW, N, SEA_CODES, fingerprint, gen,
                              load_measured, load_planet)
from seat import RULES_SEED, SEAT_RULE, describe, seat_of

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DEFAULT_OUT = os.path.join(REPO, "desktop-wallet", "dev", "seat-crosscheck.json")

# 本机钱包的真实地址与创世：它才是"我这枚币站在哪儿"的答案
LIVE_ADDRESS = "ql1qqqspfa0h8u73wp0lapt9pgy329w8yd3zup4mxedswqke6220nlqn4wgcl"
LIVE_GENESIS = "5dc2205a8f0153a05f84e26ab94086009796bb68fdfddff439f97ebfd2aa3104"


def is_coastal(H, seat):
    """席位格的 8 邻域里有海/无数据 ⇒ 这块地贴着海岸。
    它决定的是 σ 的取法（regional_relief 只统计陆地角），不是码路覆盖。"""
    i, j = seat["i"], seat["j"]
    for dj in (-1, 0, 1):
        for di in (-1, 0, 1):
            ii, jj = i + di, j + dj
            if 0 <= ii < GW and 0 <= jj < GH and H[jj, ii] < SEA_CODES:
                return True
    return False


def has_sea_in_block(H, seat):
    """这块 8.96 公里的窗口里会不会出现海格——答案比直觉窄得多。
    窗口只有全球格的 0.23 倍宽，所以四角采样永远落在 {i-1,i}×{j-1,j}；一块地里
    要出现海，必须**西南那三格角全是海**（(i-1,j-1)、(i,j-1)、(i-1,j)）。
    全球只有 0.57% 的陆地格满足——就是半岛尖、沙嘴、河delta那一类"岬角"席位。
    offshore_cells / 海底 shelf / 海段量化这三条码路只有这类席位才走得到，
    所以夹具必须刻意找它们，不能指望随机撞到。"""
    i, j = seat["i"], seat["j"]
    if i < 1 or j < 1:
        return False
    return all(H[j + dj, i + di] < SEA_CODES for dj, di in ((-1, -1), (0, -1), (-1, 0)))


def pick_addresses(H, genesis, count, cape_min, live):
    """确定性挑一批地址：先把岬角例找够，剩下的用普通席位补满。
    地址不能手填成"我认识的好看的地方"——那样夹具就变成挑出来的证据；
    这里唯一的挑选条件是**码路覆盖**，而且写死在代码里，谁跑都是这一批。"""
    rng = np.random.default_rng(20261029)
    cape, plain = [], []
    for _ in range(200000):
        if len(cape) >= cape_min and len(plain) >= count - 1 - len(cape):
            break
        addr = "ql1" + hashlib.sha256(rng.bytes(8)).hexdigest()[:54]
        s = seat_of(addr, genesis, H)
        if s is None:
            raise SystemExit(f"{SEAT_RULE}: {addr} 试满 4096 轮没落到陆地")
        (cape if has_sea_in_block(H, s) else plain).append(addr)
    if len(cape) < cape_min:
        raise SystemExit(f"扫满 20 万个地址只找到 {len(cape)} 个岬角席位，"
                         f"达不到 {cape_min} 个的覆盖要求——别删要求，加大扫描")
    return [live] + cape + plain[:count - 1 - len(cape)]


def build(H, addresses, genesis, measured):
    rows = []
    for addr in addresses:
        s = seat_of(addr, genesis, H)
        if s is None:
            raise SystemExit(f"{SEAT_RULE}: {addr} 试满 4096 轮没落到陆地，夹具不接受这种情况")
        # 席位必须存**全精度**经纬度：describe() 那两位是给人看的四舍五入，
        # 拿它去烤块会烤出另一块地——JS 那一份要独立从地址重算，靠的就是这几个字。
        f, info = gen(H, s["lat"], s["lon"], RULES_SEED, measured=measured)
        d = describe(s, H)
        rows.append({
            "address": addr,
            "seat": {"i": s["i"], "j": s["j"], "cell": s["cell"], "code": s["code"],
                     "lat": s["lat"], "lon": s["lon"], "tries": s["tries"]},
            "terrain": d["terrain"], "elev_m": d["elev_m"],
            "relief_sigma_m": d["relief_sigma_m"], "coastal": is_coastal(H, s),
            "sea_in_block": has_sea_in_block(H, s),
            "block": {"land_cells": info["land_cells"], "source": info["source"],
                      "grid": N, "meters_per_cell": CELL_M,
                      "sigma_m": info["sigma_m"], "rugged": info["rugged"]},
            "heights_fingerprint": fingerprint(f.tobytes()),
            # 抽样几格的原字节：指纹相同但布局不同也能被这一项抓住
            "spot": [int(f[y, x]) for (y, x) in
                     ((0, 0), (64, 192), (128, 128), (191, 37), (255, 255), (7, 240))],
        })
    return rows


def main():
    argv = sys.argv[1:]
    out = argv[argv.index("--out") + 1] if "--out" in argv else DEFAULT_OUT
    count = int(argv[argv.index("--count") + 1]) if "--count" in argv else 16
    cape_min = int(argv[argv.index("--cape") + 1]) if "--cape" in argv else 3
    H, measured = load_planet(), load_measured()
    addresses = pick_addresses(H, LIVE_GENESIS, count, cape_min, LIVE_ADDRESS)
    rows = build(H, addresses, LIVE_GENESIS, measured)
    doc = {"rule": SEAT_RULE, "generator": "QL-EARTH-LOCAL-v1", "seed": RULES_SEED,
           "seed_hex": f"{RULES_SEED:08x}", "genesis": LIVE_GENESIS,
           "grid": [N, N], "meters_per_cell": CELL_M, "cases": rows}
    with open(out, "w") as fh:
        json.dump(doc, fh, ensure_ascii=False, indent=1)
    srcs, seas, coast = {}, 0, 0
    for r in rows:
        srcs[r["block"]["source"]] = srcs.get(r["block"]["source"], 0) + 1
        seas += bool(r["sea_in_block"])
        coast += bool(r["coastal"])
    print(f"{len(rows)} 例席位 → {out}")
    print(f"  地貌来源  {' · '.join(f'{k} {v}' for k, v in sorted(srcs.items()))}"
          f" | 贴海岸 {coast} 例 | 块内有海 {seas} 例（海底 shelf / 离岸扩散 只有这些走得到）")
    for r in rows[:5]:
        s = r["seat"]
        print(f"  {s['lat']:+8.4f}° {s['lon']:+8.4f}° 第 {s['cell']:>8,} 号 "
              f"{r['terrain']:4s}{'岬' if r['sea_in_block'] else '  '} σ={r['relief_sigma_m']:>7} m "
              f"陆地 {r['block']['land_cells']:>6,}/65536 {r['block']['source']:8s} "
              f"{r['heights_fingerprint']}")
    print(f"  …（其余 {max(0, len(rows) - 5)} 例只进夹具，不刷屏）")


if __name__ == "__main__":
    main()
