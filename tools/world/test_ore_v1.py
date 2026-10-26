#!/usr/bin/env python3
"""矿脉谓词 QL-ORE-v1 三语对拍（§9.13 的可行性证明）。

Python（`world.py`）与 Node（`world.js`）现场各算一遍，逐位比对；再把结果写成
`ore_golden.json`，交给 Rust 侧 `ore::tests::ore_matches_cross_language_golden` 复核。
三份实现算出同一片矿脉，"自动化挖矿"才谈得上可验证：矿不需要被信任，只需要被复算。

    python3 tools/world/test_ore_v1.py

一条挂了就意味着：钱包（Rust）里挖中的格子，别人用网页渲染器算不出来——
签进账本的坐标从此不可复算，整个「币即世界」的主张当场失效。
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import world  # noqa: E402

# node 不一定在 PATH 上：先看 NODE_BIN，再找 PATH，最后给出明确提示
NODE = os.environ.get("NODE_BIN") or shutil.which("node")

# 合成 VRF 的口径必须三边一致（与 ore.rs 的测试同一公式）：
# 第 i 条 = SHAKE-256("QL-WORLD-PARITY" ‖ i.to_be_bytes(8))，32 字节。
# 不能用 "%064x" % (i+1)：这类"历史"的异或和在 1..3、1..7 上都等于 0，
# 于是"换历史"其实没换世界，矿脉搬家测试会假通过或假失败。
VRF_DOMAIN = b"QL-WORLD-PARITY"


def synth(n):
    return [
        hashlib.shake_256(VRF_DOMAIN + i.to_bytes(8, "big")).hexdigest(32)
        for i in range(n)
    ]


# 高度扫描用同一条 400 条 VRF 的历史（块高 ≤ 历史长度），历史扫描固定 h=400
HEIGHTS = [0, 3, 7, 50, 150, 240, 400]
HIST_N = [400, 401, 512]
H_FIX = 400
LIMIT = 64

JS = r"""
const QL = require(process.argv[2]);
const fs = require('fs');
const spec = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
const out = [];
for (const c of spec.cases) {
  const words = QL.seedWords(spec.genesis, c.vrfs);
  out.push({
    height: c.height, n: c.n,
    quota: QL.oreQuota(c.height),
    space: QL.ORE_SEED_SPACE,
    cells: QL.oreCells(words, c.height, spec.limit, null).map(p => p.join(',')),
  });
}
process.stdout.write(JSON.stringify(out));
"""


def py_case(h, vrfs):
    words = world.seed_words(world.GENESIS_HASH, vrfs)
    cells = world.ore_cells(words, h, LIMIT)
    return {
        "height": h,
        "n": len(vrfs),
        "vrfs": vrfs,
        "quota": world.ore_quota(h),
        "space": world.ORE_SEED_SPACE,
        "cells": ["%d,%d,%d" % c for c in cells],
    }


def cases():
    out = [py_case(h, synth(400)) for h in HEIGHTS]
    out += [py_case(H_FIX, synth(n)) for n in HIST_N]
    return out


def main():
    fails = 0

    def check(name, got, want):
        nonlocal fails
        ok = got == want
        if not ok:
            fails += 1
        print("%-6s %-34s %s" % ("PASS" if ok else "FAIL", name, got if ok else "%r != %r" % (got, want)))

    def mark(name, ok, detail):
        nonlocal fails
        if not ok:
            fails += 1
        print("%-6s %-34s %s" % ("PASS" if ok else "FAIL", name, detail))

    # 1) 配额表：与减半同构的 2 的幂衰减，且有下限（矿可以稀有，不能绝迹）
    check("quota(0)==quota(49)", (world.ore_quota(0), world.ore_quota(49)),
          (world.ORE_QUOTA_BASE, world.ORE_QUOTA_BASE))
    check("quota(50/100/150) 逐级减半",
          (world.ore_quota(50), world.ore_quota(100), world.ore_quota(150)),
          (world.ORE_QUOTA_BASE >> 1, world.ORE_QUOTA_BASE >> 2, world.ORE_QUOTA_BASE >> 3))
    check("配额永不归零", world.ore_quota(10 ** 9), world.ORE_QUOTA_MIN)

    py = cases()
    for c in py:
        print("  h=%-4s vrfs=%-4s 配额=%-8s 候选=%-3s 前 3 格=%s" % (
            c["height"], c["n"], c["quota"], len(c["cells"]), c["cells"][:3]))

    # 2) 两条独立性质：
    #    - 同一历史、块高变大 → 配额收窄，后期候选必须是前期的子集（越来越难）
    #    - 同一块高、历史不同 → 矿脉必须搬家（不可预挖；换块高不算对照，
    #      50 块以内配额没变，矿格本来就该一样）
    hv = [c for c in py if c["n"] == 400]
    counts = [len(c["cells"]) for c in hv]
    mark("矿脉数量随块高单调不增",
         all(counts[i] >= counts[i + 1] for i in range(len(counts) - 1)), counts)
    mark("每个块高都挖得到矿", all(counts), counts)
    mark("后期矿脉是前期的子集",
         set(hv[-1]["cells"]) <= set(hv[0]["cells"]),
         "h=%s ⊄ h=%s" % (hv[-1]["height"], hv[0]["height"]))

    hist = {c["n"]: c["cells"] for c in py if c["height"] == H_FIX}
    moved = hist[HIST_N[0]] != hist[HIST_N[1]]
    mark("矿脉随链历史搬家（不可预挖）", moved,
         "%s ≠ %s" % (hist[HIST_N[0]][:2], hist[HIST_N[1]][:2]))

    # 3) 逐格复核：候选必须是创世纪元格、配额命中、且贴地（挖矿不许悬空刷层）
    for c in py:
        words = world.seed_words(world.GENESIS_HASH, c["vrfs"])
        bad = []
        for s in c["cells"]:
            x, y, layer = (int(v) for v in s.split(","))
            if not world.is_ore(x, y, words, c["height"]):
                bad.append((x, y, "不中矿"))
            elif layer != world.ore_layer(x, y, words):
                bad.append((x, y, "层不对 %d≠%d" % (layer, world.ore_layer(x, y, words))))
        mark("h=%s n=%s 每一格都验得过" % (c["height"], c["n"]),
             not bad, bad or "%d 格全通过" % len(c["cells"]))

    # 4) Node 侧现场对拍
    js = None
    if not NODE:
        mark("node 侧跑起来", False, "找不到 node：请装 node 或设 NODE_BIN=/path/to/node")
    else:
        spec_path = os.path.join(HERE, "_ore_spec.json")
        with open(spec_path, "w", encoding="utf-8") as f:
            json.dump({"genesis": world.GENESIS_HASH, "limit": LIMIT,
                       "cases": [{"height": c["height"], "n": c["n"], "vrfs": c["vrfs"]} for c in py]}, f)
        js_path = os.path.join(HERE, "_ore_probe.js")
        with open(js_path, "w", encoding="utf-8") as f:
            f.write(JS)
        try:
            r = subprocess.run([NODE, js_path, os.path.join(HERE, "world.js"), spec_path],
                               capture_output=True, text=True)
            if r.returncode != 0:
                mark("node 侧跑起来", False, r.stderr.strip()[:200])
            else:
                js = json.loads(r.stdout)
        finally:
            for p in (spec_path, js_path):
                if os.path.exists(p):
                    os.remove(p)

    if js is not None:
        for i, (a, b) in enumerate(zip(py, js)):
            same = a["cells"] == b["cells"] and a["quota"] == b["quota"]
            mark("python == node (#%d h=%s n=%s)" % (i, a["height"], a["n"]), same,
                 "%d 格逐位一致" % len(a["cells"]) if same else
                 "py %d/%s vs js %d/%s" % (len(a["cells"]), a["cells"][:2], len(b["cells"]), b["cells"][:2]))

    # 5) 交给 Rust 的黄金基准（ore::tests::ore_matches_cross_language_golden 复核）
    golden_path = os.path.join(HERE, "ore_golden.json")
    with open(golden_path, "w", encoding="utf-8") as f:
        json.dump({"rule": "QL-ORE-v1", "genesis": world.GENESIS_HASH,
                   "vrf_formula": "shake256(b'QL-WORLD-PARITY' || i.to_be_bytes(8), 32)[i in 0..n)",
                   "limit": LIMIT,
                   "seed_space": world.ORE_SEED_SPACE, "cases": py}, f, indent=1)
    print("       黄金基准 → %s" % golden_path)

    print()
    if fails:
        print("ORE CROSS-CHECK: %d FAILED" % fails)
        return 1
    print("ORE CROSS-CHECK: ALL PASS（%d 个用例，python/node 逐位一致，黄金基准已写盘）" % len(py))
    return 0


if __name__ == "__main__":
    sys.exit(main())
