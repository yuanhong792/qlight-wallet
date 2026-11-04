#!/usr/bin/env python3
"""文档引用闸：`§9.26` 这种小节号，必须指到 DESIGN.md 里真存在的那一节。

DESIGN 是一路往上加的编年记录，§9.x 的编号就是它的地址。地址会漂：删一节、
并一节、手写一个没存在的号（今晚真犯过：正文里写「这条坑 §9.29 记过」，而 §9.29
通篇没提这件事——**那个引用是编的**）。README、官网、`tools/*.py` 的说明也都引
§号，读者顺着跳过去发现那节讲的是别的，这一篇就不可信了。

这一遍查三件事：

  ① 每个 `§N` / `§N.M` 都能在 DESIGN.md 的小节标题里解析到；
  ② `### 9.x` 这条编号序列**连续、无重复**——出现 9.40 之后直接 9.42，说明中间
     那节被删了而引用还留着；
  ③ 反向对照：往临时副本里注入一条指向不存在小节的引用（小节号 9.99）与一条重复编号，
     闸必须当场各报一条。只报"零告警"不算数，可能只是解析器什么都没看见。

**这闸拦不住什么，说清楚**：它只保证"这一节存在"，不保证"这一节讲的就是那句话"。
今晚抓到的三处错引用里有两处是 §9.34 / §9.27——**小节真的存在，只是内容不相干**。
那种错要靠人读，闸只能管住"指到一个根本没有的小节"这一半。
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DESIGN = os.path.join(ROOT, 'DESIGN.md')

# 扫描范围：会被读者顺着 §号跳的那些文本文件
SCAN = ['README.md', 'docs/index.html']
SCAN_DIR = 'tools'

RE_HEAD = re.compile(r'^#{2,4}\s+(\d+(?:\.\d+)?)[.\s]', re.M)
RE_HEAD9 = re.compile(r'^###\s+9\.(\d+)\b', re.M)
RE_REF = re.compile(r'§(\d+(?:\.\d+)?)')


def read(path):
    try:
        with open(path, encoding='utf-8') as fh:
            return fh.read()
    except UnicodeDecodeError:
        return None                 # exFAT 的 ._ 资源叉在这里，不是源码


def targets(text):
    """返回 (小节号集合, 引用列表[(行号, 小节号)])"""
    heads = set(RE_HEAD.findall(text))
    refs = []
    for ln, line in enumerate(text.splitlines(), 1):
        for m in RE_REF.finditer(line):
            refs.append((ln, m.group(1)))
    return heads, refs


def scan_file(path, heads, label):
    text = read(path)
    if text is None:
        return ['%s 读不出 UTF-8（不是文本文件？）' % path]
    problems = []
    _, refs = targets(text)
    for ln, num in refs:
        if num not in heads:
            problems.append('%s:%d 引用 §%s，DESIGN.md 里没有这一节' % (label, ln, num))
    return problems


def numbering_gaps(text):
    nums = sorted({int(n) for n in RE_HEAD9.findall(text)})
    out = []
    if not nums:
        return ['DESIGN.md 里一个 `### 9.x` 标题都没有']
    for a, b in zip(nums, nums[1:]):
        if b - a > 1:
            out.append('§9.%d 之后直接跳到 §9.%d——中间 %d 节没了，引用它的那句得改'
                       % (a, b, b - a - 1))
    if len(RE_HEAD9.findall(text)) != len(nums):
        out.append('`### 9.x` 有重复编号')
    if nums[0] != 0:
        out.append('§9.x 从 9.%d 起，不是 9.0——编号起点变了' % nums[0])
    return out


def negative_control(heads):
    """注入一条断引用 + 一条重复编号，闸必须都报出来

    探针的小节号在这里拼出来，不写整串：这个闸也扫自己的源码，源码里出现
    完整的「§ + 号」就会被自己判为断引用，一道闸把自己的反向对照当成了缺陷。
    """
    probe_num = '9.' + '99'
    fake = read(DESIGN)
    probe = fake + '\n### 9.0 重复编号的反向对照\n\n见 ' + '§' + probe_num + '。\n'
    _, refs = targets(probe)
    dangling = [n for _, n in refs if n not in heads and n == probe_num]
    gaps = numbering_gaps(probe)
    if len(dangling) != 1:
        return '注入的假引用（小节号 %s）没被判为断引用（实得 %d 条）——正则没看见引用' % (probe_num, len(dangling))
    if not gaps:
        return '注入的重复 `### 9.0` 没被编号检查发现——编号闸是空转的'
    return None


def main():
    if not os.path.isfile(DESIGN):
        print('找不到 DESIGN.md：%s' % DESIGN)
        return 1

    design = read(DESIGN)
    if design is None:
        print('DESIGN.md 读不出 UTF-8')
        return 1
    heads, self_refs = targets(design)

    problems = []
    files = [(f, f) for f in SCAN]
    for dirpath, dirnames, filenames in os.walk(os.path.join(ROOT, SCAN_DIR)):
        dirnames[:] = [d for d in dirnames if d not in ('target', '.git', '__pycache__')]
        for fn in filenames:
            if fn.endswith(('.py', '.md', '.js', '.html')) and not fn.startswith('._'):
                rel = os.path.relpath(os.path.join(dirpath, fn), ROOT)
                files.append((rel, rel))
    files.append(('DESIGN.md', 'DESIGN.md'))

    total_refs = 0
    for rel, label in files:
        p = os.path.join(ROOT, rel)
        if not os.path.isfile(p):
            problems.append('清单里的文件不在场：%s' % rel)
            continue
        text = read(p)
        if text is None:
            continue
        total_refs += len(targets(text)[1])
        problems += scan_file(p, heads, rel)

    problems += numbering_gaps(design)

    why = negative_control(heads)
    if why:
        print('✗ 反向对照失效：%s' % why)
        return 1
    if problems:
        for x in problems:
            print('✗ %s' % x)
        print('\n共 %d 条 §引用，%d 处指空/编号断裂' % (total_refs, len(problems)))
        return 1

    print('小节 %d 个（§9.x %d 节，编号连续）· 扫 %d 个文件 · §引用 %d 条全部解析到真小节 ✓'
          % (len(heads), len({int(n) for n in RE_HEAD9.findall(design)}), len(files), total_refs))
    print('DESIGN.md 自身 %d 条，README/官网/tools 共 %d 条'
          % (len(self_refs), total_refs - len(self_refs)))
    print('反向对照：注入的假引用与重复的 `### 9.0` 都当场报出 ✓')
    print('注意：本闸只保证"这一节存在"，不保证"这一节讲的就是那句话"——')
    print('      今晚抓到的三处错引用里，§9.34 与 §9.27 都是**存在但内容不相干**，那类错要靠人读。')
    return 0


if __name__ == '__main__':
    sys.exit(main())
