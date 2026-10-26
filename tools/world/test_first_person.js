#!/usr/bin/env node
/* QLight 确定性世界 · 第一人称建造闭环回归测试（零依赖，纯 Node）
 *
 * 为什么值得常驻：第一人称编辑器的全部"可玩性"都压在两份数据的字段对齐上——
 * 账本 `Entity{x, y, layer}` 里的 `y` 就是俯视图的行号，也就是三维里的 z。
 * 渲染器曾经按 `e.z` 去读，拿到 undefined → 坐标 NaN → 签过名的方块既不显形也不占格，
 * 玩家看到的就是"世界到处是洞、点了没反应"。这类错误没有编译器管，只有跑一遍才看得见。
 *
 * 覆盖（与 DESIGN.md §9.10 / §9.7 的口径一一对应）：
 *   A 浅俯角连点 = 贴地铺开          B 低头对准脚下连点 = 同一列原地起柱
 *   C 自己垒的塔踩得住（support 含活方块）  D 左键拆 = 释放归属，块同时从三维与占格索引里消失
 *   E 准星落空必须有反馈（不许无声失败）    F 大地（高度场）钉死在创世哈希上，不随链长漂移
 *   G 写完的方块当场显形（几何变了才重铺，轮询不卡）    G2 app.js 的快照刷新必须推给 3D 场景
 *
 * 用法：node tools/world/test_first_person.js
 */
'use strict';
const path = require('path');
const UI = path.join(__dirname, '..', '..', 'desktop-wallet', 'ui');

/* —— 最小 DOM：world3d.js 只用到这些 —— */
let t = 0, q = [];
global.performance = { now: () => t };
global.requestAnimationFrame = (cb) => { q.push(cb); return q.length; };
global.cancelAnimationFrame = () => {};
global.setTimeout = () => 0;
const docL = {};
global.document = {
  pointerLockElement: null,
  addEventListener: (n, f) => { (docL[n] = docL[n] || []).push(f); },
  removeEventListener: () => {},
  exitPointerLock: () => { global.document.pointerLockElement = null; fire('pointerlockchange'); },
};
const fire = (n, e) => (docL[n] || []).forEach((f) => f(e || {}));
const winL = {};
global.addEventListener = (n, f) => { (winL[n] = winL[n] || []).push(f); };
global.removeEventListener = () => {};
const sendWin = (n, e) => (winL[n] || []).forEach((f) => f(e));
const cvL = {};
const canvas = {
  width: 0, height: 0, clientWidth: 1280, clientHeight: 720, style: {},
  addEventListener: (n, f) => { (cvL[n] = cvL[n] || []).push(f); },
  removeEventListener: () => {},
  requestPointerLock: () => { global.document.pointerLockElement = canvas; fire('pointerlockchange'); },
  getContext: () => gl,
};
const sendCv = (n, e) => (cvL[n] || []).forEach((f) => f(e));

/* —— 最小 WebGL：只记录调用，不出图 —— */
const GL = { ARRAY_BUFFER: 1, DYNAMIC_DRAW: 2, FLOAT: 3, TRIANGLES: 4, LINES: 5, DEPTH_TEST: 6, CULL_FACE: 7, COLOR_BUFFER_BIT: 8, DEPTH_BUFFER_BIT: 9, VERTEX_SHADER: 10, FRAGMENT_SHADER: 11, COMPILE_STATUS: 12, LINK_STATUS: 13 };
const glCalls = {};
const gl = new Proxy({}, { get(o, k) {
  if (k in GL) return GL[k];
  if (['createShader', 'createProgram', 'createBuffer'].includes(k)) return () => ({});
  if (['getShaderParameter', 'getProgramParameter'].includes(k)) return () => true;
  if (['getShaderInfoLog', 'getProgramInfoLog'].includes(k)) return () => '';
  if (k === 'getAttribLocation') return (p, n) => (n === 'a_c' ? 1 : 0);
  if (k === 'getUniformLocation') return (p, n) => ({ n });
  return () => { glCalls[k] = (glCalls[k] || 0) + 1; };
}, set() { return true; } });

global.window = global;
require(path.join(UI, 'world.js'));
require(path.join(UI, 'world3d.js'));
const W = global.QLWorld, W3 = global.QLWorld3D;

/* —— 链历史：任意一串 64 hex 即可，F 段要证明的正是"它不影响大地" —— */
let r = 0x243f6a88;
const hex64 = () => { let o = ''; for (let i = 0; i < 8; i++) { r ^= r << 13; r ^= r >>> 17; r ^= r << 5; r >>>= 0; o += ('00000000' + r.toString(16)).slice(-8); } return o; };
const vrfs = [];
for (let i = 0; i < 240; i++) vrfs.push(hex64());

const out = W.render(W.seedWords(W.GENESIS_HASH, vrfs));
const heights = out.heights, blocks = out.blocks;

/* —— 实体账本替身：字段与 src/world.rs 的 Entity 完全同形（行号叫 y，没有 z）—— */
const ADDR = 'ql1test';
let ledger = [], seq = 0;
const live = () => ledger.filter((e) => !e.released);
const stat = { blocked: 0, empty: 0, errors: [] };
let slot = 0;
const w3 = W3.create(canvas, {
  onPlace: (x, z, layer) => {
    if (layer >= W3.MAX_LAYER) { stat.errors.push('layer 越界 ' + layer); return; }
    if (live().some((e) => e.x === x && e.y === z && e.layer === layer)) { stat.errors.push('一格两块 ' + x + ',' + z + ',' + layer); return; }
    ledger.push({ entity_id_hex: 'e' + (++seq), name: 'B' + seq, x, y: z, layer, kind: slot, owner: ADDR, released: false });
    w3.setEntities(ledger, ADDR);            // == app.js 里 await loadWorld() 之后 syncWorld3D()
  },
  onBreak: (x, z, layer, ent) => {
    if (!ent) { stat.errors.push('拆到了地形'); return; }
    if (ent.owner !== ADDR) { stat.errors.push('拆了别人的块'); return; }
    ent.released = true;                     // 断租不删除：记录永久留，体素重新可造
    w3.setEntities(ledger, ADDR);
  },
  onBlocked: () => { stat.blocked++; },
  onEmpty: () => { stat.empty++; },
  onSlot: (i) => { slot = i; w3.setSlot(i); },
  onFrame: () => {}, onLock: () => {}, onClose: () => {},
});

const frame = () => { t += 16; const cbs = q; q = []; cbs.forEach((cb) => cb(t)); };
const press = (b) => { sendCv('mousedown', { button: b, clientX: 640, clientY: 400, preventDefault() {} }); sendWin('mouseup', { button: b, clientX: 640, clientY: 400 }); };
const look = (my) => sendWin('mousemove', { clientX: 640, clientY: 400, movementX: 0, movementY: my });
const click5 = () => { const a = []; for (let i = 0; i < 5; i++) { frame(); const n = ledger.length; press(2); a.push(ledger.length > n ? [ledger[ledger.length - 1].x, ledger[ledger.length - 1].y, ledger[ledger.length - 1].layer] : null); } return a; };

w3.setWorld(heights, blocks);
w3.setMaxLayer(W3.MAX_LAYER);
w3.setColors([[120, 120, 120], [90, 90, 90], [80, 80, 80], [70, 70, 70], [60, 60, 60], [50, 50, 50], [40, 40, 40], [30, 30, 30]]);
w3.setEntities(ledger, ADDR);
w3.open();
frame();
press(2);                        // 第一次点击：既要申请指针锁定，也必须真的落一块
ledger = []; w3.setEntities(ledger, ADDR);

/* 找一片开阔平地：非水、无树、非创世描金，且地表够高 */
let sx = -1, sz = -1;
for (let z = 40; z < 220 && sx < 0; z++) {
  for (let x = 20; x < 220; x++) {
    const b = blocks[z * W.W + x];
    if ((b & 7) !== 0 && !(b & 24) && heights[z * W.W + x] >> 3 > 12) { sx = x; sz = z; break; }
  }
}

const results = [];
const check = (name, ok, got) => results.push({ name, ok: !!ok, got });

/* A 浅俯角：贴地铺开 */
w3.jumpTo(sx - 3, sz - 3, null);
frame();
const A = click5();
const Acells = new Set(A.filter(Boolean).map((p) => p[0] + '_' + p[1]));
check('A 浅俯角连点 5 次全部落地', A.every(Boolean), A);
check('A 浅俯角是铺开而不是叠高（>1 列）', Acells.size > 1, [...Acells]);

/* B 低头 = 原地起柱 */
ledger = []; w3.setEntities(ledger, ADDR);
w3.jumpTo(sx - 3, sz - 3, null);
for (let i = 0; i < 5; i++) { look(120); frame(); }        // pitch 触底 -1.5，正对脚下
const B = click5();
const Bcell = new Set(B.filter(Boolean).map((p) => p[0] + '_' + p[1]));
const Blayer = B.filter(Boolean).map((p) => p[2]);
check('B 低头连点 5 次全部落地', B.every(Boolean), B);
check('B 五块全在同一列', Bcell.size === 1, [...Bcell]);
check('B 层号严格 +1 递增（原地起柱）', Blayer.every((v, i, a) => i === 0 || v === a[i - 1] + 1), Blayer);

/* C 自己垒的塔踩得住 */
const top = Math.max(...live().map((e) => e.layer));
w3.jumpTo(B[0][0], B[0][1], null);
frame();
check('C 站到柱顶上（脚下 = 最高层 + 1）', w3.state().layer === top + 1, { feet: w3.state().layer, top });

/* D 左键拆自己那块：三维与占格索引同时消失 */
for (let i = 0; i < 5; i++) { look(120); frame(); }
const aim = w3.state().target;
const n0 = live().length;
press(0);
const n1 = live().length;
frame();
check('D 准星正对着自己签的块', aim && aim.entity && aim.entity.owner === ADDR, aim && [aim.cx, aim.layer, aim.cz]);
check('D 一次左键正好少一块', n1 === n0 - 1, { n0, n1 });
const re = w3.state().target;
check('D 拆完的块不再被当成实心（可重新造）', !re || !re.entity || !re.entity.released, re && [re.cx, re.layer, re.cz, !!re.entity]);

/* E 准星落空必须有反馈 */
w3.jumpTo(sx + 6, sz + 6, null);
for (let i = 0; i < 16; i++) { look(-70); frame(); }       // 抬头看天
frame();
const e0 = stat.empty;
press(2);
check('E 准星确实落空', w3.state().target === null, w3.state().target);
check('E 落空必须给出反馈（不许无声失败）', stat.empty - e0 === 1, stat.empty - e0);

/* F 大地不随链长漂移（规则 v2 的种子分道） */
const hg = (l) => W.fingerprint(W.render(W.seedWords(W.GENESIS_HASH, l)).heights);
check('F 创世哈希 = 仓库锚定值', W.GENESIS_HASH === '5dc2205a8f0153a05f84e26ab94086009796bb68fdfddff439f97ebfd2aa3104', W.GENESIS_HASH.slice(0, 12));
check('F 高度场指纹 = 7b2863f076bce5fa（与 test_rules_v2.py 同一常量）', hg([]) === '7b2863f076bce5fa', hg([]));
check('F 0 / 240 / 250 个块的历史渲染出同一片大地', hg(vrfs) === hg([]) && hg(vrfs.concat([hex64()])) === hg([]), hg(vrfs));

/* G 快照 → 3D 场景的推送（app.js 的 loadWorld 每 2 秒做一次）
 *   玩家报的「造了东西只弹一个坐标，币面上什么都没有」就是这一步从未发生：
 *   实体表只在 openWorld3D 推一次，写完必须关了重开才显形。
 *   反过来，轮询每次都重铺 64×64 区域网格会让站着不动也卡一下，
 *   所以 setEntities 只对"几何真的变了"回应，签名漏改才是回归。 */
const up = () => { const b = glCalls.bufferData || 0; frame(); return (glCalls.bufferData || 0) - b; };
const feet = () => { w3.jumpTo(sx, sz, null); return w3.state().layer; };
const burn = () => frame();          // jumpTo 也会置 dirty：不计帧把它消掉，否则下一步计数不干净

w3.jumpTo(sx, sz, null);
const S0 = w3.state().surface;
burn();
const gEnt = (layer) => [{ entity_id_hex: 'g1', name: 'G1', x: sx, y: sz, layer, kind: 0, owner: ADDR, released: false }];

const gNew = (() => { w3.setEntities(gEnt(S0 + 1), ADDR); return up(); })();
const feetNew = feet();
burn();
/* 条数不变、只有层号变：签名如果只数条数，这一步就会被吞掉，块永远停在旧层 */
const gMove = (() => { w3.setEntities(gEnt(S0 + 2), ADDR); return up(); })();
const feetMove = feet();
burn();
/* 内容完全一致的第二次推送（新数组、新对象）：不许重铺 */
const gPoll = (() => { w3.setEntities(gEnt(S0 + 2).map((e) => ({ ...e })), ADDR); return up(); })();

check('G 新块推进场景必须重铺网格', gNew > 0, gNew);
check('G 新块踩得住（脚在块顶上一层）', feetNew === S0 + 2, { S0, feetNew });
check('G 同格上移一层（条数不变）也必须重铺', gMove > 0 && gMove === gNew, { gNew, gMove });
check('G 上移后脚下高度跟着变（签名没吞掉几何变化）', feetMove === S0 + 3, { S0, feetMove });
check('G 内容未变的轮询不重铺', gPoll < gMove, { gMove, gPoll });

/* G2 接线：上面全部走的是 harness 自己仿写的 onPlace→setEntities。
 *   真正出问题的是 app.js 那一侧——实体表只在 openWorld3D 推过一次，
 *   loadWorld 只刷了 2D 面板。这一行丢了，渲染器再对也照样"只弹坐标不见实物"。 */
const appJs = require('fs').readFileSync(path.join(UI, 'app.js'), 'utf8');
const loadWorldBody = (appJs.match(/async function loadWorld\(\)\s*\{[\s\S]*?\n\}/) || [''])[0];
check('G2 app.js 的 loadWorld 把实体表推给开着的 3D 场景', /setEntities\s*\(/.test(loadWorldBody), loadWorldBody.slice(0, 150));

let fail = 0;
for (const r0 of results) {
  console.log((r0.ok ? '  ok   ' : '  FAIL ') + r0.name + (r0.ok ? '' : '   → ' + JSON.stringify(r0.got)));
  if (!r0.ok) fail++;
}
if (stat.errors.length) { console.log('  FAIL 账本判据被违反：' + stat.errors.join(' / ')); fail++; }
else console.log('  ok   全程没有重叠、没有越界、没有拆到地形或别人的块');
console.log(fail ? '\nFIRST-PERSON BUILD LOOP: ' + fail + ' FAILED' : '\nFIRST-PERSON BUILD LOOP: ALL PASS (' + results.length + ' 项)');
process.exit(fail ? 1 : 0);
