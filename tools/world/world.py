#!/usr/bin/env python3
"""QLight 确定性世界渲染器 · 参考实现（Python 侧）

规格见 DESIGN.md §9.0 / §9.5：world = render(种子, 规则, 历史)

铁律（违反即视为实现不合格）：
  - 全程整数，无一处浮点；缩放一律 2 的幂右移，不用除法
  - 无平台数学库、无内置 PRNG
  - 大地 = 真实地球高程（earth-height.bin，SHA-256 钉进规则串）；
    地表 = 创世哈希 ⊕ 各块 VRF 的 XOR 折叠——植被与矿脉因此仍然不可预挖
  - 同一 (seed, rules_version, history) 必须与 world.js 产出逐位一致的像素

用法：
  python3 world.py [--blocks blocks.jsonl] [--out world.png]
"""
import argparse, hashlib, json, os, struct, sys, zlib

W = H = 256
RULES_VERSION = "QL-WORLD-RULES-v3"
SEA, SAND, SNOW = 86, 98, 190
M_FOREST, M_DESERT = 120, 60
DENS_FOREST, DENS_DESERT, DENS_GRASS = 225, 240, 232
# 种子分道（v2 的全部改动就在这一行）：前 4 个字只由创世哈希决定 → 大地；
# 后 4 个字才混入链历史 → 地表。高度场一旦随块漂移，(x, z, layer) 就没有意义，
# 签了名的塔会被埋进地里或悬在半空——世界必须先是稳定的舞台，才谈得上在上面创造。
SEED_SPLIT = 4

# 创世哈希：与 simchain.rs 的 GENESIS_HASH_HEX 同一个锚定值（公开常量）
GENESIS_HASH = "5dc2205a8f0153a05f84e26ab94086009796bb68fdfddff439f97ebfd2aa3104"

# 大地 = 真实地球（tools/world/fetch_earth_dem.py 从公开 DEM 烤出来的一份字节）。
# 它的 SHA-256 直接进规则串：换一片土地 = 换一套世界规则 = 换一批指纹，
# 谁也没法在"规则版本号没变"的幌子下悄悄把大陆搬走。
EARTH_BIN = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                          "..", "..", "desktop-wallet", "data", "earth-height.bin"))
EARTH_SHA256 = "805868b90a88e3df2d43658cb8579dee9cebd1fe566794d52afabf05184248f8"
EARTH = None


def earth():
    """读那 65536 字节，并核对哈希：不对就停，绝不拿一片别的大陆去对拍。"""
    global EARTH
    if EARTH is None:
        raw = open(EARTH_BIN, "rb").read()
        if len(raw) != W * H:
            raise SystemExit("earth-height.bin 长度 %d 应为 %d" % (len(raw), W * H))
        got = hashlib.sha256(raw).hexdigest()
        if got != EARTH_SHA256:
            raise SystemExit("earth-height.bin 指纹分叉：%s ≠ %s" % (got[:16], EARTH_SHA256[:16]))
        EARTH = raw
    return EARTH


# 规则参数表：全部写进版本哈希——动任何一个阈值都会换出另一个世界（DESIGN.md §9.6）
PARAMS = ",".join(map(str, [W, H, SEA, SAND, SNOW, M_FOREST, M_DESERT,
                            DENS_FOREST, DENS_DESERT, DENS_GRASS, SEED_SPLIT]))
RULES_CANON = RULES_VERSION + "|" + PARAMS + "|" + EARTH_SHA256
RULES_SEED = int.from_bytes(hashlib.shake_256(RULES_CANON.encode()).digest(4), "big")


def u32(x):
    return x & 0xFFFFFFFF


def hash2(ix, iy, seed):
    """32 位整数混合（替代一切随机源）"""
    h = u32(ix * 374761393 + iy * 668265263 + seed + RULES_SEED)
    h = u32(h ^ (h >> 13))
    h = u32(h * 1274126177)
    h = u32(h ^ (h >> 16))
    return h


def smooth(t):
    """t ∈ [0,256] → t²(3−2t)，整数，结果 ∈ [0,256]"""
    return (t * t * (768 - 2 * t)) >> 16


def vnoise(x, y, cell, clog, seed):
    """格点值噪声 + smoothstep 双线性插值，全程整数"""
    xi, yi = x >> clog, y >> clog
    fx = (x & (cell - 1)) << (8 - clog)
    fy = (y & (cell - 1)) << (8 - clog)
    sx, sy = smooth(fx), smooth(fy)
    n00 = (hash2(xi, yi, seed) >> 16) & 255
    n10 = (hash2(xi + 1, yi, seed) >> 16) & 255
    n01 = (hash2(xi, yi + 1, seed) >> 16) & 255
    n11 = (hash2(xi + 1, yi + 1, seed) >> 16) & 255
    a = n00 + (((n10 - n00) * sx) >> 8)
    b = n01 + (((n11 - n01) * sx) >> 8)
    return a + (((b - a) * sy) >> 8)


AMPS = (32, 16, 8, 4, 2)
CELLS = (64, 32, 16, 8, 4)


def fbm(x, y, words):
    """5 个倍频程，权重 32/16/8/4/2 → 结果 ∈ [0,247]，无需归一化（避免除法）"""
    acc = 0
    for k in range(5):
        clog = CELLS[k].bit_length() - 1
        acc += vnoise(x, y, CELLS[k], clog, words[k & 3]) * AMPS[k]
    return acc >> 6


def seed_words(genesis_hex, vrfs):
    """种子 = 创世哈希（大地）⊕ 链历史（地表）

    前 4 个字 = 创世哈希前 16 字节，**不混历史**：高度场与天然实体因此永久稳定，
    一格一层的坐标 (x, z, layer) 才是可以被签名、被第三方复算的承诺。
    后 4 个字 = 创世哈希后 16 字节 ⊕ 全部块 VRF 的 XOR 折叠：湿度场随链生长，
    同一台机器、同一条链，历史长一格地表就重画一次。
    """
    raw = bytes.fromhex(genesis_hex)
    folded = bytearray(raw)
    for v in vrfs:
        b = bytes.fromhex(v)
        folded = bytearray(a ^ c for a, c in zip(folded, b))
    stage = raw[:16] + folded[16:]
    return [int.from_bytes(stage[i * 4:i * 4 + 4], "big") for i in range(8)]


def render(words):
    """→ (rgb bytes, era1_count, entity_count, heights, blocks)

    blocks 编码（与 JS 侧同一套，第一人称体素视图靠它）：
    0 水 1 沙 2 雪 3 森林 4 荒漠 5 草原；+8 天然树，+16 创世纪元格。
    像素与方块在**同一趟循环**里分类，杜绝两套判定分叉。"""
    px = bytearray(W * H * 3)
    hg = bytearray(W * H)
    bk = bytearray(W * H)
    era1 = ents = 0
    for y in range(H):
        for x in range(W):
            h = earth()[y * W + x]          # 大地：真实地球高程，与种子无关（v3）
            m = fbm(x, y, words[4:])
            e = hash2(x + 7919, y + 104729, words[0])
            era = (e >> 24) & 0xFF                  # 0 = 创世世代（约 1/256）
            if era == 0:
                era1 += 1

            if h < SEA:                              # 水域：深度决定蓝
                d = (SEA - h) >> 2
                r, g, b, blk = 10, 34 + ((60 - d) >> 1), 74 + (120 - d), 0
            elif h < SAND:                           # 滩涂
                r, g, b, blk = 196, 176, 122, 1
            elif h > SNOW:                           # 雪线
                r, g, b, blk = 226, 232, 240, 2
            elif m > M_FOREST:                       # 森林
                r, g, b, blk = 44, 96 - ((h - SAND) >> 2), 52, 3
            elif m < M_DESERT:                       # 荒漠
                r, g, b, blk = 178, 152, 96, 4
            else:                                    # 草原
                r, g, b, blk = 104, 148 - ((h - SAND) >> 2), 74, 5

            # 实体：树 / 植物 / 走兽，由同一整数哈希确定性落位
            if SEA <= h <= SNOW:
                t = (e >> 8) & 0xFF
                dens = 225 if m > M_FOREST else (240 if m < M_DESERT else 232)
                if t > dens:
                    ents += 1
                    blk += 8
                    k = (e >> 4) & 3
                    if k == 0:
                        r, g, b = 26, 62, 30          # 乔木
                    elif k == 1:
                        r, g, b = 150, 190, 90        # 草木
                    elif k == 2:
                        r, g, b = 118, 84, 54         # 走兽
                    else:
                        r, g, b = 176, 146, 60        # 灵矿
            if era == 0:                             # 创世世代地标：实心描金
                r, g, b = 248, 216, 104
                blk += 16

            i = y * W + x
            o = i * 3
            px[o], px[o + 1], px[o + 2] = r, g, b
            hg[i], bk[i] = h, blk
    return bytes(px), era1, ents, bytes(hg), bytes(bk)


# ---------------------------------------------------------------- 矿脉谓词 QL-ORE-v1
# §9.13「世界即矿场」的第二份实现（第一份 desktop-wallet/src/world/ore.rs，第三份 world.js）。
# 三份必须对同一个 (创世哈希, VRF 序列, 块高) 吐出**同一个有序列表**——
# 基准由 tools/world/test_ore_v1.py 现场对拍，Rust 侧再拿 ore_golden.json 复核。
# 三条自洽要求：不可预挖（矿签混历史）/ 越来越难（配额减半 + 占用递减）/ 必须贴地。
#
# 命中用 24 位均匀配额，不用湿度绝对阈值：上一版拿「湿度 ≥ 240」当门槛，
# 结果整片世界扫不出一个矿——湿度分布谁也说不准，可预算的配额才谈得上难度。
ORE_SEED_SPACE = 1 << 24
ORE_QUOTA_BASE = ORE_SEED_SPACE // 8
ORE_QUOTA_MIN = ORE_SEED_SPACE // 128
ORE_DECAY_INTERVAL = 50
SCAN_CAP = 4096
MAX_LAYER = 64


def ore_quota(h):
    halvings = min(8, h // ORE_DECAY_INTERVAL)
    return max(ORE_QUOTA_MIN, ORE_QUOTA_BASE >> halvings)


def ore_seed(x, y, words):
    """矿签：由地表种子（含全部历史 VRF）派生的 24 位均匀值——下一块的 VRF 还不存在，算不出未来"""
    return (hash2(x * 3 + 17, y * 5 + 31, words[4]) >> 8) & (ORE_SEED_SPACE - 1)


def is_ore(x, y, words, h):
    """矿 = 创世纪元格（大地给的稀土地基）∩ 矿签中配额（历史决定，不可预挖）"""
    era = (hash2(x + 7919, y + 104729, words[0]) >> 24) & 0xFF
    return era == 0 and ore_seed(x, y, words) < ore_quota(h)


def ore_layer(x, y):
    """贴地层：挖矿必须把方块放到地上，不许悬空刷满 64 层。
    v3 起只读真实高程表，连种子都不看——链历史长多少格，矿都在同一层。"""
    return min(MAX_LAYER - 1, earth()[y * W + x] >> 3)


def ore_cells(words, h, limit, taken=()):
    """湿度降序（富矿先挖），并列按 (x, y) 字典序——三边同序才能对成同一个列表"""
    hits = []
    for y in range(H):
        for x in range(W):
            if not is_ore(x, y, words, h):
                continue
            cell = (x, y, ore_layer(x, y))
            if cell in taken:
                continue
            hits.append((fbm(x, y, words[4:]), x, y, cell))
            if len(hits) >= SCAN_CAP:
                break
        if len(hits) >= SCAN_CAP:
            break
    hits.sort(key=lambda r: (-r[0], r[1], r[2]))
    return [r[3] for r in hits[:limit]]


def fingerprint(buf):
    """双 FNV-1a 32 位指纹 → 64 个十六进制位，证明逐位一致"""
    out = ""
    for basis, prime in ((0x811C9DC5, 16777619), (0x01000193, 3295579737)):
        d = basis
        for byte in buf:
            d = u32((d ^ byte) * prime)
        out += "%08x" % d
    return out


def write_png(path, rgb, w, h):
    raw = b"".join(b"\x00" + rgb[i * w * 3:(i + 1) * w * 3] for i in range(h))
    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)
    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(raw, 9))
           + chunk(b"IEND", b""))
    open(path, "wb").write(png)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--blocks", default="")
    ap.add_argument("--out", default="")
    ap.add_argument("--genesis", default="")
    a = ap.parse_args()

    here = os.path.dirname(os.path.abspath(__file__))
    genesis = a.genesis or GENESIS_HASH
    vrfs = []
    if a.blocks and os.path.exists(a.blocks):
        for line in open(a.blocks, encoding="utf-8"):
            if not line.strip():
                continue
            try:
                v = json.loads(line)["vrf_hex"]
            except Exception:
                continue  # 容忍非块行（注释/截断），与官网渲染器同一套宽容解析
            if isinstance(v, str) and len(v) == 64:
                vrfs.append(v.lower())

    words = seed_words(genesis, vrfs)
    rgb, era1, ents, hg, bk = render(words)
    print("blocks      :", len(vrfs))
    print("seed words  :", " ".join("%08x" % w for w in words[:4]))
    print("era1 tiles  :", era1, " entities:", ents)
    print("fingerprint :", fingerprint(rgb))
    print("terrain fp  :", fingerprint(hg + bk))
    if a.out:
        write_png(a.out, rgb, W, H)
        print("png         :", a.out)


if __name__ == "__main__":
    main()
