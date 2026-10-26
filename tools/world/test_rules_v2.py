#!/usr/bin/env python3
"""规则 v2 的公开基准对拍（DESIGN.md §9.5 表里那几个指纹就是这里断言的）。

跑法：python3 tools/world/test_rules_v2.py
任何一条不成立，说明渲染器被改成了"另一次大爆炸"——四份副本（本文件、
tools/world/world.js、docs/world.js、desktop-wallet/ui/world.js）必须同时改，
并且要换 rules_version，否则历史世界无法复现。
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

# 公开基准（跨语言对拍：CPython 与 Node 三份 JS 副本逐位一致）
BASE_PIXELS = "bb95cc46d3bc8f90"          # 只有创世哈希
HISTORY_PIXELS = "90d19d515c01835f"       # 创世 + 上面那 240 个 VRF
TAMPER_PIXELS = "f4f3ee18cd05b9d2"        # 规则常量 +1
BASE_TERRAIN = "6292fab918db142b"         # heights ‖ blocks
HEIGHTS_FP = "7b2863f076bce5fa"           # 只算高度场：必须与历史无关
RULES_SEED = 0xD120FE8F


def check(name, got, want):
    ok = got == want
    print("%-34s %s  %s" % (name, got, "✓" if ok else "✗ 期望 %s" % want))
    return ok


def main():
    alive = True
    alive &= check("rules_seed", "%08x" % world.RULES_SEED, "%08x" % RULES_SEED)

    def render(vrfs):
        rgb, _era1, _ents, hg, bk = world.render(world.seed_words(GENESIS, vrfs))
        return world.fingerprint(rgb), world.fingerprint(hg + bk), world.fingerprint(hg)

    p0, t0, h0 = render([])
    alive &= check("pixels  只有创世", p0, BASE_PIXELS)
    alive &= check("terrain 只有创世", t0, BASE_TERRAIN)
    alive &= check("heights 只有创世", h0, HEIGHTS_FP)

    p1, _t1, h1 = render(FIXTURE)
    alive &= check("pixels  创世 + 240 历史", p1, HISTORY_PIXELS)
    # v2 的全部赌注在这一行：地表随链生长，大地一动不动。
    # 高度场若随历史改变，(x, z, layer) 就不再是可验证的承诺，签了名的塔会被埋进地里。
    alive &= check("heights 与历史无关", "PASS" if h1 == h0 else "FAIL", "PASS")

    saved = world.RULES_SEED
    world.RULES_SEED = (saved + 1) & 0xFFFFFFFF
    pt, _tt, _ht = render([])
    world.RULES_SEED = saved
    alive &= check("pixels  规则常量 +1", pt, TAMPER_PIXELS)

    print("ALL PASS" if alive else "FAILED")
    return 0 if alive else 1


if __name__ == "__main__":
    sys.exit(main())
