#!/usr/bin/env python3
"""规则 v3 的公开基准对拍（DESIGN.md §9.5 表里那几个指纹就是这里断言的）。

跑法：python3 tools/world/test_rules_v3.py

v3 与 v2 的分界线只有一句话：**大地不再是算出来的**。
高度场来自 `tools/world/fetch_earth_dem.py` 烘焙的真实地球高程表
（崂山东麓 8.96 km 窗口，SRTM30m / Copernicus GLO-30），它的 SHA-256 写进
`RULES_CANON`。所以任何一条断言挂掉，有两种截然不同的原因：
  1) `data/earth-height.bin` 被换过而常量没跟着换 → 那是换了一片土地，
     湿度/群系/矿签全跟着 RULES_SEED 一起漂，属于共识分叉；
  2) 渲染管线被改 → 那是把"同一条链"改成了"另一次大爆炸"。

两种都要么四份副本（本文件、tools/world/world.js、docs/world.js、
desktop-wallet/ui/world.js）+ Rust 共识侧同时改，要么就别改。
"""
import hashlib
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import world  # noqa: E402

GENESIS = world.GENESIS_HASH
# 240 个"任意"VRF 的确定性定义：任何人重跑都得到同一批字节，不依赖任何链数据
FIXTURE = [hashlib.shake_256(("QL-WORLD-PARITY-%d" % i).encode()).digest(32).hex()
           for i in range(240)]

# 公开基准（跨语言对拍：CPython、三份 JS 副本、Rust 共识侧逐位一致）
BASE_PIXELS = "5e6a108d9dfe62bb"          # 只有创世哈希
HISTORY_PIXELS = "55349f5fa2465335"       # 创世 + 上面那 240 个 VRF
TAMPER_PIXELS = "60280b6d8d158dc3"        # 规则常量 +1
BASE_TERRAIN = "58f2236502f1f1c5"         # heights ‖ blocks
HEIGHTS_FP = "88d303cd75dcc913"           # 只算高度场：必须与历史无关，且等于 earth-height.bin
RULES_SEED = 0x50D296E0
# 大地本体：`(x, y, layer)` 能被签进账本，靠的就是这 65536 字节一动不动
EARTH_SHA256 = "805868b90a88e3df2d43658cb8579dee9cebd1fe566794d52afabf05184248f8"
BASE_ERA1 = 274
BASE_ENTS = 3635
HIST_ENTS = 3775


def check(name, got, want):
    ok = got == want
    print("%-34s %s  %s" % (name, got, "✓" if ok else "✗ 期望 %s" % want))
    return ok


def main():
    alive = True
    alive &= check("rules_seed", "%08x" % world.RULES_SEED, "%08x" % RULES_SEED)

    def render(vrfs):
        rgb, era1, ents, hg, bk = world.render(world.seed_words(GENESIS, vrfs))
        return (world.fingerprint(rgb), world.fingerprint(hg + bk),
                world.fingerprint(hg), era1, ents)

    p0, t0, h0, e0, n0 = render([])
    alive &= check("pixels  只有创世", p0, BASE_PIXELS)
    alive &= check("terrain 只有创世", t0, BASE_TERRAIN)
    alive &= check("heights 只有创世", h0, HEIGHTS_FP)
    alive &= check("创世格数 只有创世", e0, BASE_ERA1)
    alive &= check("实体数 只有创世", n0, BASE_ENTS)

    # 大地是嵌进仓库的一张表，不是一个算式：字节级对拍，跑不了假
    raw = open(world.EARTH_BIN, "rb").read()
    alive &= check("earth-height.bin SHA", hashlib.sha256(raw).hexdigest(), EARTH_SHA256)
    alive &= check("earth-height.bin 长度", len(raw), 256 * 256)

    p1, _t1, h1, _e1, n1 = render(FIXTURE)
    alive &= check("pixels  创世 + 240 历史", p1, HISTORY_PIXELS)
    alive &= check("实体数 创世 + 240 历史", n1, HIST_ENTS)
    # v3 的全部赌注在这两行：地表随链生长，大地一动不动——而且大地根本不是链的函数。
    # 高度场若随历史改变，(x, z, layer) 就不再是可验证的承诺，签了名的塔会被埋进地里。
    alive &= check("heights 与历史无关", "PASS" if h1 == h0 == HEIGHTS_FP else "FAIL", "PASS")

    saved = world.RULES_SEED
    world.RULES_SEED = (saved + 1) & 0xFFFFFFFF
    pt, _tt, ht, _et, _nt = render([])
    world.RULES_SEED = saved
    alive &= check("pixels  规则常量 +1", pt, TAMPER_PIXELS)
    # 换一片土地（改 RULES_SEED 的那条路）不能挪动山：只有地表跟着变
    alive &= check("heights 规则常量 +1", "PASS" if ht == HEIGHTS_FP else "FAIL", "PASS")

    print("ALL PASS" if alive else "FAILED")
    return 0 if alive else 1


if __name__ == "__main__":
    sys.exit(main())
