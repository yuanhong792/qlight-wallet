#!/usr/bin/env node
/* QLight 确定性世界 · 可去之处（QL-ROAM-v1）回归闸（零依赖，纯 Node）
 *
 * 为什么值得常驻：这一层的全部风险都是"说假话"的风险，不是算错的风险。
 * 它把一块**不属于账本窗口**的地画到屏幕上，同时HUD 还在报经纬度、指纹、在场人数。
 * 任何一处忘了切，用户看见的就是一张自洽的假图——比崩溃难发现得多。
 * 这些断言不测"好看"，只测"画面上说的每句话都还经得起复算"。
 *
 * 覆盖：
 *   A 解锁曲线（k²）的边界与单调：曲线只由累计矿格数决定，不看币数
 *   B 清单与席位同源：path[0] 必须等于 Rust 算的那一格，对不上就 ok=false 并明说
 *   C 漫游那块地的地表分类 ≡ 家里的 render()（去掉创世元金格）：档位只有一组
 *   D 写字的入口全部先过只读闸门：world_place / world_dig / world_release / 采集
 *   E 只读画面不得把家里的东西搬过来：物件、挖过的坑、资源、在场、分块根、指纹
 *   F 挖矿不被漫游挡住：看见别处不影响自己那块地照常落格（只挡特效）
 *   G 这一层没碰共识：src/ 里搜不到 roam，判据与账本格式一字未改
 *   H i18n：ROAM 键在 8 语言齐全、占位符一致、用到的键都有定义
 *
 * 用法：node tools/world/test_roam_v1.js
 */
'use strict';
const fs = require('fs');
const path = require('path');
const ROOT = path.join(__dirname, '..', '..');
const UI = path.join(ROOT, 'desktop-wallet', 'ui');

global.window = global;
require(path.join(UI, 'earth-height.js'));
require(path.join(UI, 'world.js'));
require(path.join(UI, 'earth-roam.js'));
const W = global.QLWorld, ROAM = global.QLEarthRoam;

let fail = 0, checks = 0;
const ok = (cond, msg) => {
  checks++;
  if (cond) return true;
  fail++;
  console.log('  ✗ ' + msg);
  return false;
};
const eq = (a, b, msg) => ok(a === b, `${msg}（期望 ${JSON.stringify(b)}，得到 ${JSON.stringify(a)}）`);
const section = (name) => console.log(`\n—— ${name}`);

/* ————————————————————————————— A 解锁曲线 */
section('A 曲线：第 k 处要 k² 格（只看累计矿格数，不看币）');
eq(ROAM.ROAM_MAX, 64, '清单上限 64 处（界面决定，可改不分叉）');
eq(ROAM.needCells(1), 1, '第 1 处 = 1 格');
eq(ROAM.needCells(2), 4, '第 2 处 = 4 格');
eq(ROAM.needCells(ROAM.ROAM_MAX), ROAM.ROAM_MAX * ROAM.ROAM_MAX, '最后一处 = 4096 格');
let prev = -1;
for (let k = 1; k <= ROAM.ROAM_MAX; k++) {
  const n = ROAM.needCells(k);
  if (n <= prev) { ok(false, `曲线在第 ${k} 处不单调：${n} <= ${prev}`); break; }
  prev = n;
}
eq(ROAM.roamUnlocked(0), 0, '0 格 = 只有家');
eq(ROAM.roamUnlocked(1), 1, '1 格 = 解锁第 1 处');
eq(ROAM.roamUnlocked(3), 1, '3 格还在第 1 处（差 1 格不给）');
eq(ROAM.roamUnlocked(4), 2, '4 格 = 2 处');
eq(ROAM.roamUnlocked(4095), 63, '4095 格 = 63 处');
eq(ROAM.roamUnlocked(4096), 64, '4096 格 = 封顶 64 处');
eq(ROAM.roamUnlocked(999999), 64, '超了也不越界');
eq(ROAM.roamUnlocked(-5), 0, '负数不产生"解锁 -1 处"');
eq(ROAM.roamUnlocked(NaN), 0, 'NaN 当 0 处理，不炸');
eq(ROAM.nextNeed(0), 1, '还 0 格时下一处要 1 格');
eq(ROAM.nextNeed(4096), null, '到顶之后没有"下一处"');

/* ————————————————————————————— B 清单与席位同源 */
section('B 清单：同一条席位链往后取，第 0 项必须就是家');
const fake = (n) => Array.from({ length: n }, (_, k) => ({
  i: (k * 37) % 2048, j: (k * 61) % 1024, cell: k * 1000 + 7, code: 200,
  lat: 40 - k * 0.5, lon: -30 + k * 1.5, tries: k,
}));
const P = fake(1 + ROAM.ROAM_MAX);
const r25 = ROAM.plan(P, P[0].cell, 25);
eq(r25.rule, 'QL-ROAM-v1', '清单带规则名');
eq(r25.ok, true, 'path[0] 与 Rust 的席位格一致 → ok');
eq(r25.open, 5, '25 格 = 5 处');
eq(r25.places, 6, '可去之处含家 = 1 + 5');
eq(r25.items.length, ROAM.ROAM_MAX, '清单条目 = ROAM_MAX（锁着的也在，看得见才叫进度）');
eq(r25.items[0].n, 1, '第 1 项是"第 1 处"，家不占号');
eq(r25.items.filter((it) => it.unlocked).length, 5, '解锁 5 项');
eq(r25.items[5].need, 36, '第 6 处要 36 格');
eq(r25.items[5].unlocked, false, '第 6 处还锁着');
eq(r25.next, 36, 'nextNeed 与清单里第一个锁着的行同一个数');
const bad = ROAM.plan(P, 999999, 25);
eq(bad.ok, false, '对不上就 ok=false——不许画一张"看起来对得上"的地图');
eq(bad.places, 1, '对不上时不把解锁数报出去，只算家一处');
eq(ROAM.plan([], 7, 100).ok, false, '空清单不能算通过');
eq(ROAM.plan(P, P[0].cell, 0).open, 0, '0 格时一个入口都不给');

/* ————————————————————————————— C 地表档位只有一组 */
section('C visit() 的分类 ≡ 家里 render() 的分类（创世元金格除外）');
const earth = global.QLEarth.load();
eq(earth.length, W.W * W.H, '大地高度场 65536 字节');
const words = W.seedWords(W.GENESIS_HASH, []);
const rendered = W.render(words);
const v = ROAM.visit(earth, W, words);
eq(v.blocks.length, rendered.blocks.length, '两组数组同长');
let diff = 0, gold = 0, firstBad = -1;
for (let i = 0; i < v.blocks.length; i++) {
  const home = rendered.blocks[i] & 0x0F;         // 去掉 +16 元金格位
  if (rendered.blocks[i] & 16) gold++;
  if (home !== v.blocks[i]) { diff++; if (firstBad < 0) firstBad = i; }
}
eq(diff, 0, `逐格分类必须一致（首个不同：${firstBad < 0 ? '—' : `${firstBad % W.W},${(firstBad / W.W) | 0}`} `
  + `家里 ${firstBad < 0 ? '—' : rendered.blocks[firstBad]}，漫游 ${firstBad < 0 ? '—' : v.blocks[firstBad]}）`);
ok(gold > 0, '这块场里确实有创世元金格（否则上面那次 & 0x0F 是空过）');
eq(v.stats.land + v.stats.water, earth.length, '陆地 + 水 = 全图，没有第三类');
let eHi = -1, eLo = 256;
for (let i = 0; i < earth.length; i++) { if (earth[i] > eHi) eHi = earth[i]; if (earth[i] < eLo) eLo = earth[i]; }
eq(v.stats.hi, eHi, 'stats.hi 是真最高档');
eq(v.stats.lo, eLo, 'stats.lo 是真最低档');
// 漫游窗口不许带元金格：那是"账本里那一格"的事实，别处没有账本
ok(v.blocks.every((b) => (b & 16) === 0), '只读窗口里不许出现创世元金格');
// 种子换不得：地貌种子必须是共识 rules_id，这里只验"传进去就真的用了"。
// 必须用非零 VRF——全零异或等于没换种子，那条断言就会空过。
const v2 = ROAM.visit(earth, W, W.seedWords(W.GENESIS_HASH, ['a3'.repeat(32)]));
ok(v2.blocks.some((b, i) => b !== v.blocks[i]), '换一串链条 → 树的位置要变（种子确实进了分类）');

/* ————————————————————————————— D 写字的入口先过闸门 */
section('D 只读期间不许产生任何一条账本记录');
const app = fs.readFileSync(path.join(UI, 'app.js'), 'utf8');
const bodyOf = (name) => {
  const at = app.search(new RegExp(`(async )?function ${name}\\s*\\(`));
  if (at < 0) return null;
  const start = app.indexOf('{', at);
  let depth = 0;
  for (let i = start; i < app.length; i++) {
    if (app[i] === '{') depth++;
    else if (app[i] === '}') { depth--; if (!depth) return app.slice(start, i + 1); }
  }
  return null;
};
for (const fn of ['w3Place', 'w3Dig', 'w3Break']) {
  const b = bodyOf(fn);
  ok(b && /invoke\("world_(place|dig|release)"/.test(b), `${fn} 里确有落账调用`);
  ok(b && /roamBlocked\(\)/.test(b), `${fn} 先过只读闸门（roamBlocked）`);
  if (b) {
    const callAt = b.search(/invoke\("world_/);
    const guardAt = b.search(/roamBlocked\(\)/);
    ok(guardAt < callAt, `${fn}：闸门在落账之前，不是之后`);
  }
}
const harvest = app.match(/onHarvest:\s*\(([^)]*)\)\s*=>\s*\{[\s\S]*?\n      \}/);
ok(harvest && /roamBlocked\(\)/.test(harvest[0]), '3D 左键采集（onHarvest）也先过闸门');

/* ————————————————————————————— E 不把家里的东西搬到别处 */
section('E 只读画面里不许出现账本世界的物件与数字');
const sync = bodyOf('syncWorld3D');
ok(sync, '找到 syncWorld3D');
const roamBranch = sync && sync.slice(sync.indexOf('if (roamAt)'), sync.indexOf('} else {'));
ok(roamBranch && /setEntities\(\[\]/.test(roamBranch), '漫游分支清空物件（账本里的塔不属于这块地）');
ok(roamBranch && /setDug\(\[\]/.test(roamBranch), '漫游分支清空挖过的坑');
ok(roamBranch && /setResources\(\[\]/.test(roamBranch), '漫游分支清空资源节点');
ok(roamBranch && /setPeers\(\[\]/.test(roamBranch), '漫游分支清空别人的人');
ok(roamBranch && /stations = \[\]/.test(roamBranch), '漫游分支收起柜台（柜台在自己那块地上）');
const hud = bodyOf('renderW3Hud');
ok(hud && /roamAt \? roamAt\.fp : terrain\.fp/.test(hud), 'HUD 的地形指纹：漫游时报这一块自己的，不是家里的');
ok(hud && !/f3\.chunk/.test((hud.match(/if \(roamAt\) \{[\s\S]*?\n  \} else/) || [''])[0]),
  '漫游时不报分块根（那属于账本窗口）');
ok(/world3d\.f3\.roam\.count/.test(hud), 'HUD 里有「已解锁 N 处 / 累计 M 格」这一行');
const shot = app.match(/\$\("w3d-shot"\)\.addEventListener[\s\S]*?\n\}\);/);
ok(shot && /shareGeo\(s\)/.test(shot[0]), '截图的坐标在按快门那一帧就定稿（之后换地方不改这张图的出处）');
ok(/function shareGeo/.test(app) && /world\.roam\.share/.test(app), '漫游截图改报经纬度，不报账本 (x, z)');
const keydown = app.match(/document\.addEventListener\("keydown"[\s\S]*?\n\}, true\);/);
ok(keydown && /k === "h"/.test(keydown[0]) && /k === "n"/.test(keydown[0]), '只读时 H 回家、N 下一处（捕获阶段拦下，引擎不再当移动键）');

/* ————————————————————————————— F 漫游不挡挖矿 */
section('F 看见别处 ≠ 停止在自己那块地上落格');
const mine = bodyOf('mineTick');
ok(mine && /invoke\("world_mine_once"/.test(mine), 'mineTick 仍在提交发行凭据');
ok(mine && !/roamBlocked\(\)/.test(mine), 'mineTick 不被只读闸门挡住——挡它就是让"看风景"变成掉收益');
ok(mine && /if \(w3 && !roamAt\)/.test(mine), '只挡住特效：砖与数字雨不许落在别的地块上');
const note = bodyOf('noteMinedCell') || (app.match(/function noteMinedCell[\s\S]*?\n}/) || [null])[0];
ok(!note || /!roamAt/.test(note), '落格后的 QLOR 数字雨也认这一块地');

/* ————————————————————————————— G 这一层没碰共识 */
section('G 只读漫游不进判据：共识侧一字未改');
const srcDir = path.join(ROOT, 'desktop-wallet', 'src');
const walk = (dir) => fs.readdirSync(dir, { withFileTypes: true }).flatMap((d) => {
  const p = path.join(dir, d.name);
  if (d.isDirectory()) return walk(p);
  // exFAT 卷上的 ._xxx 是 AppleDouble 元数据，不是源码
  return d.name.startsWith('._') || !p.endsWith('.rs') ? [] : [p];
});
const hits = [];
for (const p of walk(srcDir)) {
  const t = fs.readFileSync(p, 'utf8');
  const re = /roam/gi;
  let m;
  while ((m = re.exec(t))) hits.push(`${path.basename(p)}:${m.index}`);
}
eq(hits.length, 0, `Rust 侧不该出现 roam（找到 ${hits.length} 处：${hits.slice(0, 4).join(' ')}）`);
const py = fs.readFileSync(path.join(__dirname, 'world.py'), 'utf8');
ok(!/roam/i.test(py), 'world.py 的判据不含漫游');

/* ————————————————————————————— H i18n 八语言齐 */
section('H ROAM 键在 8 语言齐全、占位符一致、用到的键都有定义');
const i18n = fs.readFileSync(path.join(UI, 'i18n.js'), 'utf8');
const at = i18n.indexOf('const ROAM_I18N = {');
ok(at >= 0, 'i18n.js 里有 ROAM_I18N 词条块');
const end = i18n.indexOf('};', i18n.indexOf('Erreichbare', at));
const block = i18n.slice(at, end);
const langs = ['zh-CN', 'en', 'ja', 'ko', 'ru', 'es', 'fr', 'de'];
const dicts = {};
langs.forEach((l, idx) => {
  const head = l === 'zh-CN' ? `"${l}": {` : `${l}: {`;
  const s = block.indexOf(head);
  ok(s >= 0, `ROAM 块里有 ${l}`);
  if (s < 0) return;
  const nxt = idx + 1 < langs.length
    ? block.indexOf(langs[idx + 1] === 'zh-CN' ? `"${langs[idx + 1]}": {` : `${langs[idx + 1]}: {`, s + head.length)
    : block.length;
  const seg = block.slice(s, nxt < 0 ? block.length : nxt);
  const pairs = [...seg.matchAll(/"([a-zA-Z0-9._]+)": "((?:[^"\\]|\\.)*)"/g)];
  // 取的是捕获组，不是整个匹配：直接用 matchAll 的结果会让"键"变成整条 `"a": "b"`，
  // 于是每个语言都报缺键，红的其实是闸自己。
  dicts[l] = Object.fromEntries(pairs.map((m) => [m[1], m[2]]));
});
const ref = Object.keys(dicts['zh-CN']);
ok(ref.length >= 20, `ROAM 键数量 ${ref.length}`);
for (const l of langs) {
  const k = Object.keys(dicts[l] || {});
  const miss = ref.filter((x) => !k.includes(x));
  const extra = k.filter((x) => !ref.includes(x));
  ok(miss.length === 0 && extra.length === 0, `${l} 与 zh-CN 键集不同：缺 ${miss.join(',')} 多 ${extra.join(',')}`);
  for (const key of ref) {
    const ph = (s) => [...String(s || '').matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort().join(',');
    ok(ph(dicts[l][key]) === ph(dicts['zh-CN'][key]), `${l}.${key} 的占位符与 zh-CN 不一致`);
  }
}
const used = new Set();
for (const m of app.matchAll(/\bt\("((?:world|world3d)\.[a-zA-Z0-9._]*roam[a-zA-Z0-9._]*)"/g)) used.add(m[1]);
const html = fs.readFileSync(path.join(UI, 'index.html'), 'utf8');
for (const m of html.matchAll(/data-i18n="((?:world|world3d)\.[a-zA-Z0-9._]*roam[a-zA-Z0-9._]*)"/g)) used.add(m[1]);
for (const key of used) ok(ref.includes(key), `用到却没定义：${key}`);
const cjk = /[㐀-䶿一-鿿]/;
for (const l of ['en', 'ru', 'es', 'fr', 'de']) {
  for (const key of ref) {
    ok(!cjk.test(dicts[l][key] || ''), `${l}.${key} 里混进了汉字：${(dicts[l][key] || '').slice(0, 40)}`);
  }
}

/* ————————————————————————————— 结论 */
console.log(`\n${fail ? '✗' : '✓'} QL-ROAM-v1 回归闸：${checks} 项断言，${fail} 项失败`);
if (fail) process.exit(1);
