/* 可去之处（QL-ROAM-v1）：把「这个世界里落过多少格矿」换成「地球上还有几块地走得进去」。
 *
 * 业主口径（2026-10-31）：「初始地不能永远都是中国，要在全球的各个地方才行」+「解锁可去之处」。
 *
 * 这一层是**只读**的，边界写死在这里：
 *   · 不产出一条账本记录、不参与任何判据、不碰发行与兑换——所以这条曲线以后可以随时改，
 *     改它不会造成分叉。反过来，一旦让"漫游的那块地"进 DIG 合法性或 entity_root，
 *     它立刻变成共识，那就必须走一次版本变更（#46 + parcel 进坐标，另拍）。
 *   · 席位链（QL-SEAT-v1）只写一遍：漫游清单就是**同一条链往后多取几项**，
 *     第 0 项必须等于 `snapshot.seat`。对不上就是 bug，所以这条断言在回归闸 B 段。
 *   · 漫游那一格的地貌来自 QL-EARTH-LOCAL-v1（与 Rust / Python 逐位同构的那份），
 *     地表分类来自与家里**同一组档位**（QLWorld.RULES），不新增第二种地貌语言。
 *
 * 为什么是"累计矿格数"而不是"币数"：币会被兑换池与转账改，矿格数只有一层含义——
 * 你在这个世界里真的落下过几格可复算的矿。它同时也是发行节奏那把梯子（50→10）的自变量，
 * 所以"走得更远"和"挖得更熟"用的是同一个事实，不是两个各刷各的进度条。
 *
 * 零 import：所有生成器由调用方（app.js）把数组与 QLWorld 递进来。为的是这份纯函数
 * 能在 Node 里直接 require 并被回归闸跑（tools/world/test_roam_v1.js）。
 */
(function (global) {
  'use strict';

  var ROAM_RULE = 'QL-ROAM-v1';
  // 清单上限是一个**界面**决定，不是规则：列表要数得过来。只读层所以它可以随时上调而不分叉。
  var ROAM_MAX = 64;

  /** 解锁第 k 处要累计 k² 格矿：1、4、9、16…4096。
   *  为什么是 k²：每一步都要比上一步更明显地"再挖一片"才换得到新的一格视野，
   *  线性会一眼望到头，指数会在第 10 处就把人劝退。64 处 = 4096 格，与发行梯子
   *  （10 块一格）合起来是"长期回同一张世界"的量级，不是一个下午。 */
  function needCells(k) { return k * k; }

  /** 已经解锁了几处（不含家）：只由累计矿格数决定，无状态、可复算、客户端说了不算也无所谓——
   *  它解锁的是一个只读视野，不是任何权益。 */
  function roamUnlocked(cells) {
    var c = Math.max(0, Math.floor(Number(cells) || 0));
    return Math.min(ROAM_MAX, Math.floor(Math.sqrt(c)));
  }

  /** 下一处还要几格（到顶返回 null） */
  function nextNeed(cells) {
    var k = roamUnlocked(cells) + 1;
    return k > ROAM_MAX ? null : needCells(k);
  }

  /** 目的地清单。`path` = seatPath() 取回的前 1+ROAM_MAX 个陆地格（第 0 项是家）。
   *  席位与清单必须同源：`seatCell` 是 Rust 算的那一格，path[0] 是本机链算出来的，
   *  两者不一致时这里直接返回 ok=false，由上层明说——不许画一张"看起来对得上"的地图。 */
  function plan(path, seatCell, cells) {
    var list = path || [], open = roamUnlocked(cells);
    var home = list.length ? list[0] : null;
    var ok = !!(home && seatCell != null && home.cell === seatCell);
    var items = [];
    for (var k = 1; k < list.length; k++) {
      var n = k;                       // 第 n 处（家不算）
      items.push({ n: n, need: needCells(n), unlocked: n <= open, cell: list[k].cell,
                   i: list[k].i, j: list[k].j, lat: list[k].lat, lon: list[k].lon,
                   tries: list[k].tries });
    }
    return { rule: ROAM_RULE, ok: ok, max: ROAM_MAX, open: open,
             places: 1 + (ok ? open : 0), items: items, home: home, next: nextNeed(cells) };
  }

  /** 一块地（QL-EARTH-LOCAL-v1 的 256×256 字节场）→ 第一人称要的两组数组。
   *  分类逐条照 world.js render() 的同一组档位，连树密度的算法都一样，
   *  只去掉「创世元金格」(+16)：元金格是账本世界里的事实，漫游窗口里没有账本，
   *  把它跟着带到每块地上等于宣称"元金格在地球各处都有"——那是假话。
   *  回归闸 C 段拿家里的 EARTH 高度场喂这个函数，要求与 QLWorld.render() 的
   *  blocks 逐格一致，所以哪天改了一档，会当场红，不会静默分叉。 */
  function visit(field, world, words) {
    var R = world.RULES;
    var n = Math.round(Math.sqrt(field.length));
    var blocks = new Uint8Array(field.length);
    var land = 0, water = 0, trees = 0, hi = -1, lo = 256;
    for (var y = 0; y < n; y++) {
      for (var x = 0; x < n; x++) {
        var hgt = field[y * n + x];
        if (hgt > hi) hi = hgt;
        if (hgt < lo) lo = hgt;
        var mo = world.fbm(x, y, words, 4);
        var e = world.hash2(x + 7919, y + 104729, words[0]);
        var blk;
        if (hgt < R.SEA) blk = 0;
        else if (hgt < R.SAND) blk = 1;
        else if (hgt > R.SNOW) blk = 2;
        else if (mo > R.M_FOREST) blk = 3;
        else if (mo < R.M_DESERT) blk = 4;
        else blk = 5;
        if (hgt >= R.SEA && hgt <= R.SNOW) {
          var t = (e >>> 8) & 0xFF;
          var dens = mo > R.M_FOREST ? R.DENS_FOREST : (mo < R.M_DESERT ? R.DENS_DESERT : R.DENS_GRASS);
          if (t > dens) blk += 8;
        }
        if (blk === 0) water++; else land++;
        if (blk & 8) trees++;
        blocks[y * n + x] = blk;
      }
    }
    // 世界是 256×256，地块也是 256×256：同一套数组，引擎不需要知道这是别处
    return { heights: field, blocks: blocks,
             stats: { land: land, water: water, trees: trees, hi: hi, lo: lo } };
  }

  global.QLEarthRoam = {
    ROAM_RULE: ROAM_RULE, ROAM_MAX: ROAM_MAX,
    needCells: needCells, roamUnlocked: roamUnlocked, nextNeed: nextNeed,
    plan: plan, visit: visit,
  };
})(typeof window !== 'undefined' ? window : globalThis);
