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
 *   G3 出块显形：铸块砖 + QLOR 数字雨只写自己的缓冲，绝不重铺 64×64，到期自动散场
 *   H 挖地：只挖露出的顶面、层号随账本下降、水面/旧层点了必须有反馈
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
/* 2D 画布替身：sprites.js / terrain-tex.js 只在启动时画图集，Node 里只要别抛错。
 * 图集内容不参与任何断言——像素对不对由浏览器点测看，这里管的是接线与几何。 */
const ctx2d = {
  imageSmoothingEnabled: false, fillStyle: '#000',
  fillRect() {}, drawImage() {}, putImageData() {},
  createImageData: (w, h) => ({ width: w, height: h, data: new Uint8ClampedArray(w * h * 4) }),
  getImageData: (x, y, w, h) => ({ width: w, height: h, data: new Uint8ClampedArray(w * h * 4) }),
};
const fakeCanvas = () => ({ width: 0, height: 0, getContext: () => ctx2d });
global.document = {
  pointerLockElement: null,
  createElement: (tag) => (tag === 'canvas' ? fakeCanvas() : { style: {} }),
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
require(path.join(UI, 'sprites.js'));
require(path.join(UI, 'qlight-hd-sprites.js'));
require(path.join(UI, 'terrain-tex.js'));
require(path.join(UI, 'earth-height.js'));
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
const stat = { blocked: 0, empty: 0, dig: [], digBlocked: 0, errors: [] };
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
  // 挖地：只记录"要挖哪一层"，账本由 src/world.rs 落；这里不自己改地形，
  // 挖掉之后的世界由 setDug（命令层快照）再推进来——与 app.js 同一路径。
  onDig: (x, z, layer) => { stat.dig.push({ x, z, layer }); },
  onDigBlocked: () => { stat.digBlocked++; },
  onSlot: (i) => { slot = i; w3.setSlot(i); },
  onFrame: () => {}, onLock: () => {}, onClose: () => {},
});

const frame = () => { t += 16; const cbs = q; q = []; cbs.forEach((cb) => cb(t)); };
const press = (b) => { sendCv('mousedown', { button: b, clientX: 640, clientY: 400, preventDefault() {} }); sendWin('mouseup', { button: b, clientX: 640, clientY: 400 }); };
const look = (my) => sendWin('mousemove', { clientX: 640, clientY: 400, movementX: 0, movementY: my });
let feed = [];
/* 没落地的那一次必须留下声音：引擎的拒绝走 onBlocked/onEmpty，计数器会跳。
 * 闸用它区分"键盘坏了"和"准星对的地方已经有块"——后者是对的，前者是事故。 */
const click5 = () => {
  feed = [];
  const a = [];
  for (let i = 0; i < 5; i++) {
    frame();
    const n = ledger.length, f0 = stat.blocked + stat.empty;
    press(2);
    feed.push(stat.blocked + stat.empty - f0);
    a.push(ledger.length > n ? [ledger[ledger.length - 1].x, ledger[ledger.length - 1].y, ledger[ledger.length - 1].layer] : null);
  }
  return a;
};

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

/* 视线钉死：
 *   鼠标灵敏度是 0.0022 弧度/像素，pitch 限幅 ±1.5。分几次 look(120) 只转 0.26 弧度/次，
 *   结果取决于上一步朝向——E 把视线拉到顶之后，H 的五次下压只到 -0.6，
 *   准星落在隔壁那列而不是脚下，测试就会报"挖错格"。
 *   一次给足 2000 像素 = 4.4 弧度，必然撞到限幅，与上一步朝向无关。 */
const lookDown = () => { look(2000); frame(); };
const lookUp = () => { look(-2000); frame(); };

const results = [];
const check = (name, ok, got) => results.push({ name, ok: !!ok, got });

/* A 浅俯角：贴地铺开
 * 连点到自己刚放下那块的前方就停：射线打的是那块朝我的一面，而那一面的邻格已经有块，
 * 引擎拒绝并报了 onBlocked——这是正确行为，不是坏键。所以这里断言的不是"5 次全落"，
 * 而是"落下的至少一半 + 每一次没落都吭声 + 落下的铺开在好几列上"。 */
w3.jumpTo(sx - 3, sz - 3, null);
frame();
const A = click5();
const Acells = new Set(A.filter(Boolean).map((p) => p[0] + '_' + p[1]));
check('A 浅俯角连点：落下的≥3 次，没落下的每次都给了反馈',
  A.filter(Boolean).length >= 3 && A.every((p, i) => !!p || feed[i] > 0), { A, feed });
check('A 浅俯角是铺开而不是叠高（>1 列）', Acells.size > 1, [...Acells]);

/* B 低头 = 原地起柱 */
ledger = []; w3.setEntities(ledger, ADDR);
w3.jumpTo(sx - 3, sz - 3, null);
lookDown();
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
lookDown();
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
lookUp();                                                  // 抬头看天
frame();
const e0 = stat.empty;
press(2);
check('E 准星确实落空', w3.state().target === null, w3.state().target);
check('E 落空必须给出反馈（不许无声失败）', stat.empty - e0 === 1, stat.empty - e0);

/* F 大地不随链长漂移（规则 v3：大地干脆不看种子，它是烘焙进仓库的真实高程表） */
const hg = (l) => W.fingerprint(W.render(W.seedWords(W.GENESIS_HASH, l)).heights);
check('F 创世哈希 = 仓库锚定值', W.GENESIS_HASH === '5dc2205a8f0153a05f84e26ab94086009796bb68fdfddff439f97ebfd2aa3104', W.GENESIS_HASH.slice(0, 12));
check('F 高度场指纹 = 88d303cd75dcc913（与 test_rules_v3.py 同一常量）', hg([]) === '88d303cd75dcc913', hg([]));
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

/* ============ G3 出块显形：铸块砖 + QLOR 数字雨（§9.16 v3 的体验层） ============
 *   纪律只有一条：这一层只许写自己的缓冲。出块动画如果顺手置了 st.dirty，
 *   玩家每 2 秒就被一次 64×64 重铺卡一下——"体验感"当场变成"体验差"。 */
const groundLayer = heights[sz * W.W + sx] >> 3;
const idle0 = up();                                  // 常态帧的上传次数（天空/星/粒子/线框）
const mesh0 = w3.state().mesh;                       // 64×64 重铺了几次——这条才是硬指标
check('G3 没有出块动画时不许画幽灵砖', w3.state().fx === 0 && w3.state().fxVerts === 0, w3.state());
check('G3 空动画不铺地形', w3.state().mesh === mesh0, { mesh0, got: w3.state().mesh });

const hash8 = 'a1b2c3d4', vrf8 = '0f1e2d3c';
w3.mintBlock({ hash: hash8 + 'eeff', vrf: vrf8 + '99', reward: 2500000000, height: 77, cell: { cx: sx, cz: sz, layer: groundLayer } });
const fxFrame = up();
check('G3 一笔发行铸出一块砖', w3.state().fx === 1, w3.state().fx);
check('G3 砖与三行刻字真的写进了缓冲', w3.state().fxVerts > 0, w3.state());
// 实体网格本来就有 ~90ms 一次的"呼吸"重算，所以上传数只设上界；重铺次数必须一分不多
check('G3 动画只多一次上传，绝不重铺区域网格', fxFrame <= idle0 + 2 && w3.state().mesh === mesh0, { idle0, fxFrame, mesh: w3.state().mesh });

const rainText = `QLOR|${sx}|${sz}|${groundLayer}`;
check('G3 矿格坐标写成符文不许缺笔', W3.missingGlyphs(rainText).length === 0, W3.missingGlyphs(rainText));
check('G3 反向对照：字模表外的字符必须报出来（不许悄悄吞字）', W3.missingGlyphs('QL©').indexOf('©') >= 0, W3.missingGlyphs('QL©'));
w3.rainAt(sx, sz, groundLayer, rainText);
const twoFx = up();
check('G3 砖与数字雨共用同一条缓冲（不是两条）', w3.state().fx === 2 && twoFx <= idle0 + 2 && w3.state().mesh === mesh0, { idle0, twoFx, fx: w3.state().fx, verts: w3.state().fxVerts });

/* 落不完也得散场：条目自己到期，缓冲与帧成本回到常态，动画不许永久占着画面 */
for (let i = 0; i < 400; i++) frame();
check('G3 动画到期自动清空', w3.state().fx === 0 && w3.state().fxVerts === 0, w3.state());
check('G3 全程没为动画铺过一次地形', w3.state().mesh === mesh0, { mesh0, got: w3.state().mesh });

/* 洪水测试：脚本狂出块也不许把一帧拖成重铺 */
for (let i = 0; i < 40; i++) {
  w3.mintBlock({ hash: hash8, vrf: vrf8, reward: 100000000, height: i, cell: { cx: sx + (i % 5), cz: sz, layer: groundLayer } });
  w3.rainAt(sx + (i % 5), sz, groundLayer, rainText);
}
check('G3 同屏动画有条上限', w3.state().fx > 0 && w3.state().fx <= W3.FX_CAP, w3.state().fx);
check('G3 洪水之后一帧仍只多一次上传', up() <= idle0 + 2 && w3.state().mesh === mesh0, { idle0, got: up(), mesh: w3.state().mesh });
check('G3 缺参数的出块事件不许炸', (() => { try { w3.mintBlock(null); w3.mintBlock({}); return true; } catch (e) { return false; } })(), null);

/* app.js 接线（与 G2 同一个道理：渲染器对了，调用点漏了玩家照样看不见） */
const blockBody = (appJs.match(/listen\("chain_block",\s*\(ev\)\s*=>\s*\{[\s\S]*?\n\}\);/) || [''])[0];
check('G3 app.js 收到新块必须铸砖', /mintBlock\s*\(/.test(blockBody), blockBody.slice(-200));
check('G3 砖上的发行量取 total_minted 的差，不是含手续费的 e.reward',
  /total_minted/.test(blockBody) && !/reward:\s*e\.reward/.test(blockBody), blockBody.slice(-260));
/* 锚点跟着 0.4.3 的改版换过一次：挖矿不再是 `btn-mine-ore` 那一下点击，
   而是程序一跳一跳的 `mineTick()`。断言一个字没动——符文仍只许在矿签真落账之后起。 */
const mineBody = (appJs.match(/async function mineTick\(\)\s*\{[\s\S]*?\n\}/) || [''])[0];
check('G3 挖到矿必须在矿格上起一串符文', /noteMinedCell\(/.test(mineBody), mineBody.slice(0, 200));
check('G3 符文只在矿签真的落账之后起（不许在签名前预告）',
  mineBody.indexOf('world_mine_once') >= 0 && mineBody.indexOf('world_mine_once') < mineBody.indexOf('noteMinedCell('), mineBody.slice(0, 240));

/* ============ H 挖地（v5 账本）：只挖露出来的那一层，水面与旧层都挖不动 ============
 *   账本侧由 src/world.rs 的 op=dig 判定；这里守的是渲染器/输入这一侧：
 *   报给命令层的层号必须是这一列**现在**露出来的顶面，而不是准星碰到的任意一格。 */
let dgx = -1, dgz = -1;
for (let z = 40; z < 200 && dgx < 0; z++) {
  for (let x = 20; x < 200; x++) {
    if (Math.abs(x - sx) + Math.abs(z - sz) < 4) continue;
    const b = blocks[z * W.W + x];
    if ((b & 7) !== 0 && !(b & 24) && heights[z * W.W + x] >> 3 > 12) { dgx = x; dgz = z; break; }
  }
}
const dsurf = heights[dgz * W.W + dgx] >> 3;
w3.setDug([{ x: dgx, y: dgz, top: dsurf - 1, removed: 1 }]);
w3.jumpTo(dgx, dgz, null);
burn();
const H1 = w3.state();
check('H 挖掉一层：顶面比原始地表低一层', H1.top === H1.surface - 1 && H1.dug === 1, H1);
check('H 挖掉一层：脚跟着落到新顶面上', H1.layer === H1.top + 1, { feet: H1.layer, top: H1.top });

lookDown();
const ht = w3.state().target;
check('H 准星对着挖过的列顶面，且标记为可挖', ht && ht.layer === H1.top && ht.dig === true, ht && [ht.cx, ht.layer, ht.cz, ht.dig]);
const d0 = stat.dig.length;
press(0);
check('H 左键挖掉一层 = 只发一次 dig（不是拆方块）', stat.dig.length - d0 === 1, stat.dig.slice(-2));
check('H dig 报的是露出来的那一层', stat.dig[stat.dig.length - 1] &&
  stat.dig[stat.dig.length - 1].layer === H1.top, stat.dig[stat.dig.length - 1]);

// 再挖一层：同一套规则继续生效（顶面继续降，不是只认第一铲）
w3.setDug([{ x: dgx, y: dgz, top: H1.top - 1, removed: 2 }]);
burn();
const H2 = w3.state();
check('H 再挖一层：顶面继续下降', H2.top === H1.top - 1 && H2.dug === 2, H2);

/* 坑壁是几何，不是贴图：深坑必须比平地多写出侧壁面。曾经按"每格 16 面"的固定预算，
 * 十层深的坑角把预算用完后 tquad 就停笔，画面是缺了半面墙的坑——玩家眼里的"洞里"。 */
w3.setDug([]);
burn();
const vFlat = w3.state().verts;
w3.setDug([{ x: dgx, y: dgz, top: dsurf - 10, removed: 10 }]);
burn();
const Hd = w3.state();
check('H 深坑的坑壁真的进了地形缓冲（比平地多出侧壁面）', Hd.verts > vFlat, { vFlat, verts: Hd.verts });
check('H 地形缓冲没被写满（写满 = 坑壁缺块）', Hd.verts < Hd.vcap, { verts: Hd.verts, vcap: Hd.vcap });

// 水面挖不动：水面不是"露出来的可挖层"，点了必须给反馈
let wx = -1, wz = -1;
for (let z = 40; z < 200 && wx < 0; z++) {
  for (let x = 20; x < 200; x++) { if ((blocks[z * W.W + x] & 7) === 0) { wx = x; wz = z; break; } }
}
w3.setDug([]);
w3.jumpTo(wx, wz, null);
burn();
lookDown();
const wt = w3.state().target;
check('H 水面不是可挖的顶面', wt && wt.dig === false, wt && [wt.cx, wt.layer, wt.cz, wt.dig]);
const b0 = stat.digBlocked;
press(0);
check('H 对着水面左键必须给出反馈，不许悄悄进账本', stat.digBlocked - b0 === 1, stat.digBlocked - b0);
check('H 收掉挖地账本：地面回到原始顶面', (() => { w3.jumpTo(dgx, dgz, null); burn(); const s = w3.state(); return s.dug === 0 && s.top === s.surface; })(), w3.state());

let fail = 0;
for (const r0 of results) {
  console.log((r0.ok ? '  ok   ' : '  FAIL ') + r0.name + (r0.ok ? '' : '   → ' + JSON.stringify(r0.got)));
  if (!r0.ok) fail++;
}
if (stat.errors.length) { console.log('  FAIL 账本判据被违反：' + stat.errors.join(' / ')); fail++; }
else console.log('  ok   全程没有重叠、没有越界、没有拆到地形或别人的块');
console.log(fail ? '\nFIRST-PERSON BUILD LOOP: ' + fail + ' FAILED' : '\nFIRST-PERSON BUILD LOOP: ALL PASS (' + results.length + ' 项)');
process.exit(fail ? 1 : 0);
