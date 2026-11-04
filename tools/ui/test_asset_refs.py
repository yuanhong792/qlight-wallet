#!/usr/bin/env python3
"""资源引用门：被引用的文件必须真的在包里（也必须在网站上）。

接线闸管的是"按钮那句 invoke 对得上 Rust 命令"，管不到**取数路径**。
而钱包的世界是这么起来的：`index.html` 里一串 `<script src=…>`，JS 里
`import("./planet-view.js")`、`fetch('earth-global-h.bin')`，CSS 里
`url("assets/world-gate-v1.png")`。路径写错或文件被挪走，真 GUI 里的表现不是
弹窗报错，是**一片空白的世界**——访客从官网下回去、装好、点「启动挖矿」看到
黑屏，而前面六道闸一路全绿。dev/ 的 stub 对拍页也测不出这类错（它按 URL 直接
从磁盘取，取到的就是那份文件）。所以这一遍静态核对，两个根都扫：

  ① `desktop-wallet/ui/`（发行包的前端）与 `docs/`（官网）各自的 `index.html`
     每个 `src`/`href`、顶层 `*.js` 每个字面量引用（`import("…")` / `fetch('…')` /
     `new URL('…', import.meta.url)` / 裸的 `'xxx.bin'`）、`style.css` 每个
     `url(…)`，都要能在该根下解析到一个真文件；
  ② 共识侧 `include_bytes!` 内嵌的那两份二进制在场，且星球网格与前端 fetch 的
     同一尺寸——编译期缺不了它，但"两份是不是同一张"是 §9.35 记过的真风险；
  ③ 反向对照：把 `earth-global-h.bin` 与 `planet-view.js` 各改一个名字，两条都
     必须判为缺失；再往临时目录里真注入一份 HTML（一条引用存在、一条断掉），
     扫描管线必须恰好报出断掉的那条——否则"零告警"只说明解析器什么都没看见；
  ④ 顺带报"孤儿资产"：`ui/assets/` 里随包分发却没有任何文件引用的东西。
     只报数不判红——那是业主的素材，删不删、挂哪一面由他定（§9 立的规矩）。

带 `${…}` 的动态拼串不在射程内，明确跳过并计数报出来。
写这道闸的当场抓到过一个盲区：包里最大那张图（world-gate-v1.png，2.48 MB）唯一的
引用点在 CSS 的 `url()` 里——只扫 HTML 与 JS 会绿着漏掉它最想防的那类错。
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
UI = os.path.join(ROOT, 'desktop-wallet', 'ui')
DOCS = os.path.join(ROOT, 'docs')
SRC = os.path.join(ROOT, 'desktop-wallet', 'src', 'world')

RE_SRC_HREF = re.compile(r'''\b(?:src|href)\s*=\s*["']([^"'?#]+)(?:[?#][^"']*)?["']''')
RE_IMPORT = re.compile(r'''\bimport\(\s*["']([^"']+)["']\s*\)''')
RE_FETCH = re.compile(r'''\bfetch\(\s*["']([^"']+)["']''')
RE_NEWURL = re.compile(r'''\bnew\s+URL\(\s*["']([^"']+)["']''')
RE_BARE = re.compile(r'''["']([A-Za-z0-9_./@-]+\.bin|[A-Za-z0-9_./@-]+\.(?:png|jpg|jpeg|svg|css|wasm|json))["']''')
RE_CSS_URL = re.compile(r'''\burl\(\s*["']?([^"')\s]+)["']?\s*\)''')
LOOKS_LIKE_PATH = re.compile(
    r'^[A-Za-z0-9_./@-]+\.(?:js|css|html|json|bin|png|jpg|jpeg|svg|ico|icns|wasm|mp3|wav|ttf|woff2?)$')


def is_dynamic(p):
    return any(t in p for t in ('${', '%', '+'))


def is_external(p):
    return p.startswith(('http://', 'https://', 'data:', 'blob:', 'mailto:', 'tel:', '//'))


def refs_in(text, patterns):
    out = []
    for rx in patterns:
        out.extend(rx.findall(text))
    return out


def resolve(root, name):
    """把引用名解析成该根目录下的实际路径；跑出这个根或不存在都返回 None"""
    n = name.lstrip('/').split('?', 1)[0].split('#', 1)[0]
    while n.startswith('./'):
        n = n[2:]
    cand = os.path.normpath(os.path.join(root, n))
    if cand != root and not cand.startswith(root + os.sep):
        return None
    return cand if os.path.isfile(cand) else None


def scan_root(label, root, st):
    """扫一个根：index.html + style.css + 顶层 *.js"""
    if not os.path.isdir(root):
        st['broken'].append((label, '入口目录不存在：%s' % root))
        return
    files = {}
    for f in ('index.html', 'style.css'):
        p = os.path.join(root, f)
        if os.path.isfile(p):
            files[f] = open(p, encoding='utf-8', errors='replace').read()
    for f in sorted(os.listdir(root)):
        if f.endswith('.js') and not f.startswith('._'):
            files[f] = open(os.path.join(root, f), encoding='utf-8', errors='replace').read()
    for fname, text in files.items():
        pats = ([RE_SRC_HREF, RE_CSS_URL] if fname.endswith(('.html', '.css'))
                else [RE_IMPORT, RE_FETCH, RE_NEWURL, RE_BARE])
        for r in refs_in(text, pats):
            if is_external(r) or r.startswith('#') or not LOOKS_LIKE_PATH.match(r):
                continue
            if is_dynamic(r):
                st['dynamic'] += 1
                continue
            st['checked'] += 1
            p = resolve(root, r)
            if p is None:
                st['broken'].append(('%s/%s' % (label, fname), r))
            else:
                st['hit'].add(p)
    print('  %s：%d 个入口文件' % (label, len(files)))


def main():
    st = {'checked': 0, 'broken': [], 'dynamic': 0, 'hit': set()}

    # ① 两个根都扫
    print('扫描引用：')
    scan_root('ui', UI, st)
    scan_root('docs', DOCS, st)

    # ② 共识侧内嵌的那两份二进制
    grid = os.path.join(UI, 'earth-global-h.bin')
    incl = []
    for f in ('earth_local.rs', 'terrain.rs'):
        incl += re.findall(r'include_bytes!\("([^"]+)"\)',
                           open(os.path.join(SRC, f), encoding='utf-8').read())
    for rel in incl:
        p = os.path.normpath(os.path.join(SRC, rel))
        st['checked'] += 1
        if not os.path.isfile(p):
            st['broken'].append(('include_bytes!', rel))
            continue
        if os.path.basename(p) == 'earth-global-h.bin':
            same = os.path.isfile(grid) and os.path.getsize(p) == os.path.getsize(grid)
            print('  星球网格 %s = %d B，与前端 fetch 的那份%s'
                  % (os.path.relpath(p, ROOT), os.path.getsize(p),
                     '同一尺寸 ✓' if same else '尺寸不同！'))
            if not same:
                st['broken'].append(('grid-mismatch', os.path.relpath(p, ROOT)))
    print('  include_bytes! 内嵌 %d 份资产' % len(incl))

    # ③ 反向对照：改名后的引用必须判为缺失
    ctrl = [probe for probe in ('earth-global-h.bin', 'planet-view.js')
            if resolve(UI, probe.replace('.', '.typo.')) is None]
    if len(ctrl) != 2:
        print('反向对照没生效：本该两条都缺失，实得 %s' % ctrl)
        return 1

    # ③b 管线级反向对照：临时目录里一份 HTML，一条引用真存在、一条断掉，
    #     必须正好报出断的那条——否则"0 条断裂"可能只是解析器什么都没看见
    import tempfile
    with tempfile.TemporaryDirectory() as td:
        os.mkdir(os.path.join(td, 'assets'))
        open(os.path.join(td, 'assets', 'here.png'), 'wb').write(b'\x89PNG')
        open(os.path.join(td, 'style.css'), 'w', encoding='utf-8').write('.ok{color:red}')
        open(os.path.join(td, 'index.html'), 'w', encoding='utf-8').write(
            '<img src="assets/here.png"><img src="assets/gone.png">'
            '<link rel="stylesheet" href="style.css">'
            '<style>.x{background:url(assets/gone.css)}</style>')
        probe_st = {'checked': 0, 'broken': [], 'dynamic': 0, 'hit': set()}
        scan_root('probe', td, probe_st)
        got = sorted(r for _, r in probe_st['broken'])
        if probe_st['checked'] != 4 or got != ['assets/gone.css', 'assets/gone.png']:
            print('管线级反向对照没生效：应核对 4 条、报出 gone.png 与 gone.css，'
                  '实得 checked=%d broken=%s' % (probe_st['checked'], got))
            return 1
    print('  管线级反向对照：4 条核对 / 2 条断裂全部报出 ✓')

    for srcf, r in st['broken']:
        print('✗ %s 引用 %s，该根下没有这个文件' % (srcf, r))
    if st['broken']:
        return 1

    print('引用核对 %d 条 · 动态拼串跳过 %d 条' % (st['checked'], st['dynamic']))
    print('反向对照：2 条改名引用全部判为缺失 ✓')

    # ④ 孤儿资产：随包分发却没有任何引用（只报数，不判红）
    hitreal = {os.path.realpath(x) for x in st['hit']}
    orphans, obytes = [], 0
    for dirpath, _, filenames in os.walk(os.path.join(UI, 'assets')):
        for f in filenames:
            if f.startswith('._'):
                continue
            p = os.path.join(dirpath, f)
            if os.path.realpath(p) not in hitreal:
                orphans.append(os.path.relpath(p, UI))
                obytes += os.path.getsize(p)
    if orphans:
        print('孤儿资产 %d 个 / %d B 随包分发却无人引用：%s'
              % (len(orphans), obytes, '、'.join(sorted(orphans))))

    print('✓ 每个被引用的资源都真在包里——不会出现"点了是白屏"')
    return 0


if __name__ == '__main__':
    sys.exit(main())
