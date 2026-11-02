#!/usr/bin/env python3
"""UI ↔ Rust 命令接线门：官网/对拍页盖不住的那一半。

桌面钱包的每个按钮最后都是一句 `invoke('命令名', {键: …})`。名字或键名写错，
在真 GUI 里就是"点了没反应 / invalid args"，而 dev/ 里那份 stub 对拍页对任意
命令名都回一份假数据——**它测不出接线错，只测得出渲染错**。所以这一遍静态核对：

  ① UI 调到的每个命令名，`src/` 里真有这个 `#[tauri::command]`；
  ② 传出去的每个键名，对得上该命令的某个形参（Tauri 2 默认 camelCase ↔ snake_case）；
  ③ 该命令每个**必填**形参（非 `Option`），调用点真的给了；
  ④ 反向对照：故意造一个错命令名 + 错键名，这道门必须当场红——
     否则"零告警"可能只是解析器什么都没看见。
"""
import re
import sys
import pathlib

ROOT = pathlib.Path(__file__).resolve().parents[2] / 'desktop-wallet'
# 由 Tauri 注入、JS 侧不该也不会出现在 invoke 参数里的形参类型
INJECTED = re.compile(r'^(?:&|tauri::)?(?:State|AppHandle|Window|WebviewWindow|Manager|AppCtx)')


def split_top(s):
    parts, depth, cur = [], 0, ''
    for c in s:
        if c in '<([{':
            depth += 1
        elif c in '>)]}':
            depth -= 1
        if c == ',' and depth == 0:
            parts.append(cur)
            cur = ''
        else:
            cur += c
    if cur.strip():
        parts.append(cur)
    return parts


def obj_body(text):
    """text 以 '{' 开头，返回花括号配平后的内部文本（不认字符串里的括号，够用）"""
    depth = 0
    for j, c in enumerate(text):
        if c == '{':
            depth += 1
        elif c == '}':
            depth -= 1
            if depth == 0:
                return text[1:j]
    return ''


def top_keys(body):
    """对象字面量本层的键：`k:` 与 ES6 简写 `{ x, y }` 都算，嵌套层里的键不算"""
    keys, depth, j = [], 0, 0
    while j < len(body):
        c = body[j]
        if c in '{[(':
            depth += 1
        elif c in '}])':
            depth -= 1
        elif depth == 0:
            prev = body[:j].rstrip()
            if not prev or prev[-1] in '{,':
                hit = re.match(r'([A-Za-z_$][\w$]*)\s*(?::(?!:)|[,}]|\s*$)', body[j:])
                if hit:
                    keys.append(hit.group(1))
                    j += hit.end()
                    continue
        j += 1
    return keys


def camel_to_snake(k):
    return re.sub(r'(?<!^)(?=[A-Z])', '_', k).lower()


def rust_sigs():
    out = {}
    for f in sorted((ROOT / 'src').glob('*.rs')):
        if f.name.startswith('._'):
            continue
        src = f.read_text(encoding='utf-8', errors='replace')
        for m in re.finditer(r'#\[tauri::command[^\]]*\]\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)\s*\(', src):
            name, i = m.group(1), m.end()
            depth, start = 1, i
            while i < len(src) and depth:
                if src[i] == '(':
                    depth += 1
                elif src[i] == ')':
                    depth -= 1
                i += 1
            params = []
            for p in split_top(src[start:i - 1]):
                pm = re.match(r'\s*(?:mut\s+)?([A-Za-z_]\w*)\s*:\s*(.+)', p, re.S)
                if pm and not INJECTED.match(pm.group(2).strip()):
                    params.append((pm.group(1), pm.group(2).strip()))
            out[name] = params
    return out


def check(sigs, text, origin):
    """扫一段前端源码里的 invoke 调用点，返回告警列表"""
    bad = []
    n = 0
    for m in re.finditer(r"invoke\(\s*['\"](\w+)['\"]\s*(?:,\s*)?", text):
        cmd = m.group(1)
        line = text[:m.start()].count('\n') + 1
        rest = text[m.end():]
        keys = top_keys(obj_body(rest)) if rest.lstrip().startswith('{') else []
        n += 1
        if cmd not in sigs:
            bad.append(f'{origin}:{line} invoke `{cmd}` —— src/ 里没有这个 #[tauri::command]')
            continue
        names = [p[0] for p in sigs[cmd]]
        allowed = set(names) | {camel_to_snake(k) for k in names} | {k.replace('_', '') for k in names}
        for k in keys:
            if k not in allowed and camel_to_snake(k) not in allowed:
                bad.append(f'{origin}:{line} invoke `{cmd}` 传了形参里没有的键 `{k}`（形参 {names}）')
        if keys:  # 只给"确实传了对象字面量"的调用点查缺参，传变量的看不见就不误报
            got = set(keys) | {camel_to_snake(k) for k in keys}
            for pn, pt in sigs[cmd]:
                if 'Option' in pt or pn in got or pn.replace('_', '') in got:
                    continue
                bad.append(f'{origin}:{line} invoke `{cmd}` 少了必填形参 `{pn}`: {pt[:44]}')
    return bad, n


def main():
    sigs = rust_sigs()
    files = [f for f in list((ROOT / 'ui').glob('*.js')) + list((ROOT / 'ui').glob('*.html'))
             if not f.name.startswith('._')]
    bad, calls = [], 0
    for f in sorted(files):
        b, n = check(sigs, f.read_text(encoding='utf-8', errors='replace'), f.name)
        bad += b
        calls += n

    # ④ 反向对照：解析器若瞎了，这两条会让本门当场红
    ctl, cn = check(sigs, "invoke('no_such_cmd', {a: 1});\n"
                          f"invoke('{next(iter(sigs))}', {{definitelyNotAParam: 1}});", 'negative-control')
    if cn != 2 or len(ctl) < 2:
        print(f'反向对照失效：2 个坏调用点只报出 {len(ctl)} 条 —— 解析器看不见东西，这道门是假的')
        return 1

    print(f'Rust 命令 {len(sigs)} 个 · 前端文件 {len(files)} 个 · invoke 调用点 {calls} 处')
    print(f'反向对照：2 个故意写错的调用点，报出 {len(ctl)} 条 ✓')
    if bad:
        print('--- 接线对不上（真 GUI 里就是死按钮）---')
        print('\n'.join(bad))
        return 1
    print('✓ 每个调用点的命令名、键名、必填形参都对得上')
    return 0


if __name__ == '__main__':
    sys.exit(main())
