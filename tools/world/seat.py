#!/usr/bin/env python3
"""席位：一把地址 → 地球上唯一的一格地。

产品口径（业主 2026-10-30）：世界地图不该是"一块地的地图"，而是**整个地球**；
地球被分成若干份，每份地形按币上的链条生成，但每一份都在地球上真实存在的位置。

所以"你的地在哪儿"必须是**纯函数**，不能是任何人（包括我）填的表：

    canon = "QL-SEAT-v1|" + 地址(小写) + "|" + 创世哈希(小写 hex)
    h     = SHA-256(canon)                  → 第 r 轮：h = SHA-256(h_{r-1})
    i     = 大端 u32(h[0..4]) % 2048        （列：西经 180° 起）
    j     = 大端 u32(h[4..8]) % 1024        （行：北纬 90° 起）
    命中 H[j*GW+i] ≥ 128（陆地码）即取；否则换下一轮哈希再试

三条纪律：
- **大端**取字节：JS 手写、Rust `from_be_bytes`、Python `int.from_bytes(...,'big')` 三方
  只有一个解释，Little-Endian 会在字节序上静默分叉。
- **0 码不是海**：0 = "这一格没有数据"（南极部分冰下基岩图缺格）。当海跳过还行，
  当陆地接受就会造出一个查无此地的席位。
- **4096 轮试不中直接炸**：宁可报错也不"随便找块地"——落点被偷偷改一次，
  之前所有签过名的 (x, z) 承诺就全废了。

地球被分成 **2048 × 1024 = 2,097,152 份**（1 格 ≈ 19.6 公里见方，等经纬，不是墨卡托），
其中陆地格 ≈ 60 万份。这就是"若干份"的真身：不是美术切图，是随包那 2 MiB 高程网格的格。

一条必须说清楚的边界（别对客户吹）：**哈希只保证不可重掷，不保证全球唯一**。
60 万个陆地格，第 ~1000 个钱包起撞号的概率就过半（生日碰撞）。唯一性只能由链给：
候选格由本函数算，占位以先出块者得，后来者按格序确定性顺延——那是链上记名，
是另一轮共识改动，不在这一份文件里。
"""
import hashlib
import json
import sys

import numpy as np

from gen_planet_local import (GW, GH, SEA_CODES, N, CELL_M, RULE, LAND_BYTES,
                              load_planet, load_measured, gen, fingerprint, to_png)

SEAT_RULE = "QL-SEAT-v1"
TRIES_MAX = 4096
# 共识规则种子（QL-WORLD-RULES-v3 的 32 位摘要）：微地形的种子必须是它，
# 换种子 = 换一片地貌 = 换世界，所以这里不留 CLI 旋钮，只从真身取。
RULES_SEED = 0x50D296E0


def seat_of(address, genesis_hex, H):
    """地址 + 创世哈希 → 席位。返回 None 表示 4096 轮没命中陆地（调用方必须炸，不许兜底）。"""
    canon = f"{SEAT_RULE}|{address.strip().lower()}|{genesis_hex.strip().lower()}"
    h = hashlib.sha256(canon.encode("utf-8")).digest()
    for tries in range(TRIES_MAX):
        if tries:
            h = hashlib.sha256(h).digest()
        i = int.from_bytes(h[0:4], "big") % GW
        j = int.from_bytes(h[4:8], "big") % GH
        code = int(H[j, i])       # H 是 (GH, GW) 二维视图；JS/Rust 那份是平铺的 [j*GW+i]
        if code >= SEA_CODES:                     # 0 码（无数据）也走这里被拒
            lon = -180.0 + (i + 0.5) / GW * 360.0
            lat = 90.0 - (j + 0.5) / GH * 180.0
            return {"i": i, "j": j, "cell": j * GW + i, "code": code,
                    "lat": lat, "lon": lon, "tries": tries, "canon": canon}
    return None


def describe(seat, H):
    """席位 → 给人看的一行：位置、海拔、这块地是什么性格（平原/丘陵/山区由 σ 说话）。"""
    from gen_planet_local import regional_relief, _land_meters
    sigma = regional_relief(H, seat["lat"], seat["lon"])
    m = _land_meters(seat["code"])
    kind = "山区" if sigma >= 800 else ("丘陵" if sigma >= 200 else "平原")
    return {"lat": round(seat["lat"], 4), "lon": round(seat["lon"], 4),
            "cell": seat["cell"], "elev_m": round(m, 1),
            "relief_sigma_m": round(sigma, 1), "terrain": kind, "tries": seat["tries"]}


def selftest():
    H = load_planet()
    land = int((H >= SEA_CODES).sum())
    print(f"地球分成      {GW}×{GH} = {GW * GH:,} 份（1 格 ≈ 19.6 km），"
          f"陆地格 {land:,} 份（{land / (GW * GH):.1%}）")

    a = "ql1qqqspfa0h8u73wp0lapt9pgy329w8yd3zup4mxedswqke6220nlqn4wgcl"
    g = "5dc2205a8f0153a05f84e26ab94086009796bb68fdfddff439f97ebfd2aa3104"
    s1 = seat_of(a, g, H)
    s2 = seat_of(a, g, H)
    assert s1 == s2, "同地址同创世算出两个席位——不可重掷这条没了"
    assert s1["code"] >= SEA_CODES, "席位落在非陆地格"
    d = describe(s1, H)
    print(f"本机地址席位  {d['lat']:+.4f}°, {d['lon']:+.4f}° · 第 {d['cell']:,} 号 · "
          f"{d['elev_m']} m · {d['terrain']}（σ={d['relief_sigma_m']} m）· 试 {d['tries']} 轮")

    # 大小写与首尾空白不改席位：地址在链上是小写 bech32，HUD 可能显示成大写
    assert seat_of(a.upper(), g.upper(), H) == s1, "地址大小写换出了另一个席位"

    # 逐位定长输入必须逐位换席位（至少不能大面积撞在同一个格）
    rng = np.random.default_rng(11)
    cells, tries_hist = set(), []
    land_total = int((H >= SEA_CODES).sum())
    for _ in range(4000):
        addr = "ql1" + hashlib.sha256(rng.bytes(8)).hexdigest()[:54]
        s = seat_of(addr, g, H)
        assert s is not None, "4096 轮没命中陆地——陆地占比 ~29%，这不可能随机发生"
        cells.add(s["cell"])
        tries_hist.append(s["tries"])
    assert len(cells) >= 3900, f"4000 个地址只有 {len(cells)} 个不同席位——哈希没铺开"
    worst = max(tries_hist)
    print(f"哈希分布      4000 个地址 → {len(cells)} 个不同格（陆地 {land_total:,} 格，"
          f"生日期望碰撞 ~{4000 * 4000 / (2 * land_total):.0f} 次），最多试 {worst} 轮")
    assert worst < 40, f"最多试 {worst} 轮——拒绝采样效率差到不可用"

    # 换创世 = 换席位：世界跟着币走，而不是跟着地址走
    s3 = seat_of(a, hashlib.sha256(b"other genesis").hexdigest(), H)
    assert s3 != s1, "换了创世哈希席位没动——地形就没有跟着链条变"

    # 席位必须能烤出一块地（这才是"能在地球上找到"的意思）
    m = load_measured()
    f, info = gen(H, s1["lat"], s1["lon"], RULES_SEED, measured=m)
    assert f.shape == (N, N) and f.dtype == np.uint8
    assert info["land_cells"] > 0, "席位格是陆地，窗口里却没有一格陆地——中心点算歪了"
    print(f"烤出一块地    {RULE} {N}×{N} @ {CELL_M:.0f} m = {N * CELL_M / 1000:.2f} km · "
          f"陆地 {info['land_cells']:,}/65536 · 来源 {info['source']} · 指纹 {fingerprint(f.tobytes())}")
    json.dump({"rule": SEAT_RULE, "seat": d, "block": info,
               "heights_fingerprint": fingerprint(f.tobytes())},
              open("/tmp/ql-seat-selftest.json", "w"), ensure_ascii=False, indent=1)
    print("→ /tmp/ql-seat-selftest.json")


def main():
    argv = sys.argv[1:]
    if "--selftest" in argv:
        selftest()
        return
    addr = argv[argv.index("--address") + 1] if "--address" in argv else \
        "ql1qqqspfa0h8u73wp0lapt9pgy329w8yd3zup4mxedswqke6220nlqn4wgcl"
    genesis = argv[argv.index("--genesis") + 1] if "--genesis" in argv else \
        "5dc2205a8f0153a05f84e26ab94086009796bb68fdfddff439f97ebfd2aa3104"
    H = load_planet()
    s = seat_of(addr, genesis, H)
    if s is None:
        raise SystemExit(f"{SEAT_RULE}: {TRIES_MAX} 轮没命中陆地，拒绝给这块币派地")
    d = describe(s, H)
    print(json.dumps(d, ensure_ascii=False, indent=1))
    f, info = gen(H, s["lat"], s["lon"], RULES_SEED, measured=load_measured())
    print(f"{info['source']:9s} 陆地 {info['land_cells']:6,}/65536 基面 {info.get('base_m', '—'):>8} m "
          f"起伏 {info.get('relief_m', '—'):>7} m 档距 {info.get('gain_m_per_byte', '—')} m/B "
          f"夸大 {info.get('exag', '—')}× 指纹 {fingerprint(f.tobytes())}")
    if "--png" in argv:
        to_png(f, argv[argv.index("--png") + 1])
        print(f"→ {argv[argv.index('--png') + 1]}")


if __name__ == "__main__":
    main()
