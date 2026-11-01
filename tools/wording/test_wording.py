#!/usr/bin/env python3
"""去"演示化"口径闸（0.5.0，DESIGN §9.31）。

三件事，任何一件不成立就翻红：
  A. ui/ 里念得出的地方，不再出现「模拟链 / regtest / 演示」这类自我否定的词；
  B. 8 种语言的键集必须一模一样（少一门语言 = 那门语言在运行时念英文回退，等于没翻译）；
  C. 本轮改名的 7 个键，每一种语言的措辞都被钉住——改回旧说法会被拦下。

反向对照：把 i18n.js 里 nav.badge 改回「模拟链 regtest」再跑，本闸必须翻红（见 --selftest）。
"""
import io
import os
import re
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
UI = os.path.join(ROOT, "desktop-wallet", "ui")
I18N = os.path.join(UI, "i18n.js")

LANGS = ["zh-CN", "en", "ja", "ko", "ru", "es", "fr", "de"]

# A：禁词。按语言块整段扫，命中就报出键名，方便定位。
FORBIDDEN = ["模拟链", "演示", "regtest", "demo chain", "Demo chain", "Test chain",
             "test chain", "demo network", "Demonet", "Demonetz", "simulated", "simulierte",
             "simulée", "simulada", "симулированная", "模擬", "모의", "デモチェーン"]
# 允许出现禁词的文件位置（vendor 第三方、以及本闸自己的源码）
SCAN = ["i18n.js", "index.html", "app.js", "world.js", "world3d.js", "planet-view.js",
        "earth-map.js", "earth-grid.js", "earth-local.js", "earth-roam.js", "terrain-tex.js"]

# C：本轮钉死的措辞（语言顺序同 LANGS）
PINNED = {
    "nav.badge": ["本地链 · 未接公网", "Local chain · no public net", "ローカルチェーン（公網未接続）",
                  "로컬 체인 · 공용망 미연결", "Локальная цепь · без публичной сети",
                  "Cadena local · sin red pública", "Chaîne locale · sans réseau public",
                  "Lokale Kette · kein öffentliches Netz"],
    "net.chain": ["QLight 链", "QLight chain", "QLight チェーン", "QLight 체인", "Цепь QLight",
                  "Cadena QLight", "Chaîne QLight", "QLight-Kette"],
    "hero.unit": ["QLX 权益", "QLX entitlement", "QLX 権利", "QLX 권리", "Права QLX",
                  "Derechos QLX", "Droits QLX", "QLX-Anspruch"],
    "net.solo": ["◦ 单机运行", "◦ Solo node", "◦ 単機稼働", "◦ 단독 노드", "◦ Одиночный узел",
                 "◦ Nodo único", "◦ Nœud seul", "◦ Einzelknoten"],
}
# 这些键必须仍然承认"公网没接"这件事——去掉假话不等于去掉真话
HONEST_MUST = {
    "nav.badge": ["公网", "public", "公網", "공용망", "публичн", "pública", "public", "öffentlich"],
    "about.safe3": ["门票", "producer", "生成者", "생성자", "создатель", "productor", "producteur", "Producer"],
    "about.net": ["中继节点", "relay", "中継", "중계", "ретранслятор", "retransmisor", "relais", "Relais"],
}

fails = []


def check(cond, msg):
    if not cond:
        fails.append(msg)


def parse_dicts(src):
    """{dict_name: {lang: {key: value}}}，只收 `lang: { "key": "value" }` 两层结构。"""
    out = {}
    d = re.compile(r"^const ([A-Z0-9_]+) = \{$", re.M)
    marks = [(m.group(1), m.start()) for m in d.finditer(src)]
    for idx, (name, pos) in enumerate(marks):
        end = marks[idx + 1][1] if idx + 1 < len(marks) else len(src)
        body = src[pos:end]
        langs = {}
        lm = re.compile(r'^  "?([A-Za-z-]+)"?: \{$', re.M)
        lmarks = [(m.group(1), m.start()) for m in lm.finditer(body)]
        for j, (lang, lpos) in enumerate(lmarks):
            lend = lmarks[j + 1][1] if j + 1 < len(lmarks) else len(body)
            kv = dict(re.findall(r'^\s{4}"([^"]+)": "((?:[^"\\]|\\.)*)",?$', body[lpos:lend], re.M))
            if len(kv) >= 3:
                langs[lang] = {k: v.replace('\\"', '"') for k, v in kv.items()}
        if langs:
            out[name] = langs
    return out


def main():
    src = io.open(I18N, encoding="utf-8").read()
    dicts = parse_dicts(src)

    # B：每个字典内部，8 种语言的键集必须相同
    for name, langs in dicts.items():
        present = [l for l in LANGS if l in langs]
        check(len(present) == len(langs), "%s：语言块 %d 个，但 LANGS 只认得 %d 个（块名拼错？）" % (name, len(langs), len(present)))
        base = set(langs[LANGS[0]])
        for l in present[1:]:
            missing = base - set(langs[l])
            extra = set(langs[l]) - base
            check(not missing, "%s / %s：缺 %d 个键 %s" % (name, l, len(missing), sorted(missing)[:5]))
            check(not extra, "%s / %s：多 %d 个键 %s" % (name, l, len(extra), sorted(extra)[:5]))

    # A：禁词
    hits = []
    for fname in SCAN:
        p = os.path.join(UI, fname)
        if not os.path.exists(p):
            continue
        for n, line in enumerate(io.open(p, encoding="utf-8").read().splitlines(), 1):
            for w in FORBIDDEN:
                if w in line:
                    hits.append("%s:%d 「%s」 %s" % (fname, n, w, line.strip()[:60]))
    check(not hits, "ui/ 里还有自我否定的词：\n  " + "\n  ".join(hits))

    # C：钉死的措辞 + 真话仍在
    main_dict = dicts["I18N"]
    for key, want in PINNED.items():
        for lang, w in zip(LANGS, want):
            got = main_dict[lang].get(key)
            check(got == w, "%s / %s：%r ≠ 期望 %r" % (key, lang, got, w))
    for key, marks in HONEST_MUST.items():
        for lang, m in zip(LANGS, marks):
            got = main_dict[lang].get(key, "")
            check(m in got, "%s / %s：措辞里找不到「%s」——诚实声明不许被淡化" % (key, lang, m))

    # D：index.html 的内联中文必须与 zh 字典一致（不同就是"切语言闪一下换字"）
    page = io.open(os.path.join(UI, "index.html"), encoding="utf-8").read()
    for key in list(PINNED) + ["about.ver", "about.safe3"]:
        zh = main_dict["zh-CN"].get(key)
        if not zh or "{" in zh:
            continue
        m = re.search(r'data-i18n="%s">([^<]*)<' % re.escape(key), page)
        if m:
            check(m.group(1) in zh or zh in m.group(1) or m.group(1) in zh,
                  "index.html 内联 %s=%r 与 zh 字典 %r 对不上" % (key, m.group(1), zh))

    if "--selftest" in sys.argv:
        # 反向对照：把 nav.badge 的中文改回旧说法，闸必须翻红
        mutated = src.replace('"nav.badge": "本地链 · 未接公网"', '"nav.badge": "模拟链 regtest"', 1)
        d2 = parse_dicts(mutated)
        bad = any(w in d2["I18N"]["zh-CN"]["nav.badge"] for w in FORBIDDEN)
        check(bad, "反向对照失效：把措辞改回「模拟链 regtest」居然没被抓到")
        print("· 反向对照：旧措辞回插 → 已抓到" if bad else "· 反向对照：翻红")

    if fails:
        print("翻红 %d 项：" % len(fails))
        for f in fails:
            print("  ✗", f)
        sys.exit(1)
    n = sum(len(v) for v in dicts.values())
    print("✓ 口径闸通过：%d 个字典 × 8 语言共 %d 个语言块（键集互为同构），禁词 0 命中，%d 键措辞钉死" %
          (len(dicts), n, len(PINNED)))


if __name__ == "__main__":
    main()
