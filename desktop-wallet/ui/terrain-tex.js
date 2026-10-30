/* QLight 世界 · 地形贴图（图集槽位 22..37，16 格；物品栏 0..21 一格不碰）
 *
 * 为什么单独一个文件：地形不是物件。物件是"你手里能造的东西"（16 种，编号 0..15），
 * 地形是"世界本身"（草皮/泥土/石/矿/水/雪/沙/元金）。两套画混在一个表里，
 * 换贴图包时就会把世界和物品栏一起换掉——所以地形占图集后半段，单独一份契约。
 *
 * 槽位契约（任何贴图包都能整组替换，见 window.QLTerrainPack）：
 *   22 草顶   23 草侧   24 泥土   25 石     26 圆石   27 苔石   28 沙
 *   29 雪顶   30 雪侧   31 水     32 深石   33 金矿   34 铁矿   35 晶矿   36 元金   37 岩床
 * 逻辑分辨率固定 16×16（《我的世界》原版就是 16×16），再整数放大到图集格宽
 * （HD 包是 32px 一格，于是每个逻辑像素 2×2），放大用 NEAREST，边缘保持硬像素。
 *
 * 16 种物件由 sprites.js / qlight-hd-sprites.js 负责，这里只**复制**它们的结果，
 * 不覆盖 0..21 的任何一格。
 */
(function (global) {
  'use strict';
  var base = global.QLSprites;
  if (!base || typeof base.atlas !== 'function') return;

  var COLS = 8, ROWS = 8, LOG = 16;
  var TILE = base.TILE || 16;          // HD 包会把 TILE 提到 32

  var S = {
    GRASS_TOP: 22, GRASS_SIDE: 23, DIRT: 24, STONE: 25,
    COBBLE: 26, MOSSY: 27, SAND: 28, SNOW_TOP: 29,
    SNOW_SIDE: 30, WATER: 31, DEEP: 32,
    ORE_GOLD: 33, ORE_IRON: 34, ORE_CRYSTAL: 35,
    ERA_GOLD: 36, BEDROCK: 37,
  };

  // —— 调色板：2026-10-30 用 /tmp/mc-tex/measure.py 逐像素量原版《我的世界》1.20.4 的
  //    block/*.png 得到的数（覆盖率、色数、色相、饱和、中间明度全在 DESIGN §9.24 记着）。
  //    三条硬结论直接改了我们原来的画法：
  //      1) 实心块 100% 覆盖，四边一个透明像素都不留（顶/底空行 0、左/右空列 0）；
  //      2) 一格只用 3~7 个色（stone 4 色、dirt 7 色、cobblestone 6 色、snow 3 色），
  //         而且是 2~4 像素的**连片色斑**，不是逐像素雪花——雪花就是"地面发花"的来源；
  //      3) 每种材质有固定的色相与明度档：泥土 25.7°/54%/V121（橙褐，不是灰），
  //         凝灰岩 60°/12%/V111，深层岩 V81，沙 48°/V218，雪 V254。
  var GRASS = [[106, 158, 72], [127, 178, 88], [86, 131, 57], [69, 110, 46]];
  var DIRT = [[121, 85, 58], [150, 108, 74], [185, 133, 92], [89, 61, 41]];
  var STONE = [[133, 131, 123], [157, 155, 146], [110, 108, 99], [93, 91, 82]];
  var DEEP = [[81, 81, 86], [101, 101, 106], [61, 61, 67], [47, 47, 55]];
  var SAND = [[218, 207, 163], [227, 219, 176], [205, 190, 146], [185, 168, 124]];
  var SNOW = [[247, 250, 253], [255, 255, 255], [228, 238, 247], [206, 219, 232]];
  var WATER = [[41, 96, 148], [58, 118, 172], [31, 78, 124], [22, 62, 102]];
  var GOLD = [[246, 207, 91], [255, 233, 138], [209, 166, 46], [171, 132, 31]];
  var ORE_GOLD = [[245, 197, 66], [255, 231, 122], [198, 148, 26]];
  var ORE_IRON = [[216, 179, 150], [238, 205, 178], [168, 131, 104]];
  var ORE_CRYSTAL = [[111, 224, 207], [163, 245, 232], [63, 184, 168]];

  // —— 确定性像素噪声：同一台机器、同一版本画出来逐位相同（世界不能有两套花纹）——
  function rnd(x, y, s) {
    var h = Math.imul(x + 1, 0x27d4eb2d) ^ Math.imul(y + 1, 0x165667b1) ^ Math.imul(s + 1, 0x9e3779b1);
    h = Math.imul(h ^ (h >>> 15), 0x85ebca6b);
    h = Math.imul(h ^ (h >>> 13), 0xc2b2ae35);
    return ((h ^ (h >>> 16)) >>> 0) / 4294967296;
  }
  function blank() { return new Uint8ClampedArray(LOG * LOG * 4); }
  function fill(d, c) {
    for (var i = 0; i < d.length; i += 4) { d[i] = c[0]; d[i + 1] = c[1]; d[i + 2] = c[2]; d[i + 3] = 255; }
    return d;
  }
  function set(d, x, y, c, a) {
    if (x < 0 || y < 0 || x >= LOG || y >= LOG) return;
    var i = (y * LOG + x) * 4;
    d[i] = c[0]; d[i + 1] = c[1]; d[i + 2] = c[2]; d[i + 3] = a == null ? 255 : a;
  }
  /** 底色铺满 + 盖几片连片的斑：spec = [[占全格的百分比, 调色板序号, 宽, 高], …]。
   *  先铺底再盖斑，所以覆盖永远是 100%——原版的实心块四边不留一个透明像素。
   *  为什么按**目标占比**铺而不是按片数撒：按片数撒时后一档会盖掉前一档，
   *  四档撒完实测底色还剩 63%，而原版 dirt 是 42/27/15/13——底色占七成就是"一片脏"，
   *  不是石头。所以每一档只往还没被占过的像素上落，落够配额就停，
   *  档位配比于是可以直接照抄原版量出来的那张表。
   *  落点仍然走**网格 + 确定性洗牌**：纯随机撒会成团，网格摊得开又逐位可重现。 */
  function blot(pal, s, spec) {
    var d = fill(blank(), pal[0]);
    var used = new Uint8Array(LOG * LOG);
    for (var k = 0; k < spec.length; k++) {
      var target = Math.round(LOG * LOG * spec[k][0] / 100);
      var c = pal[spec[k][1]], pw = spec[k][2], ph = spec[k][3];
      if (target <= 0) continue;
      var gx = Math.ceil(LOG / pw), gy = Math.ceil(LOG / ph), slots = [];
      for (var q = 0; q < gx * gy; q++) slots.push(q);
      for (var i = slots.length - 1; i > 0; i--) {          // 确定性洗牌：同种子同图
        var j = (rnd(i, k * 977 + s, 5) * (i + 1)) | 0, t = slots[i]; slots[i] = slots[j]; slots[j] = t;
      }
      var got = 0;
      for (var n = 0; n < slots.length && got < target; n++) {
        var ci = slots[n], x = (ci % gx) * pw, y = ((ci / gx) | 0) * ph;
        x += (rnd(ci, s, k + 1) * 2) | 0; y += (rnd(ci + 5, s, k + 1) * 2) | 0;   // 落点抖一格，别露出网格
        for (var yy = 0; yy < ph && got < target; yy++) for (var xx = 0; xx < pw; xx++) {
          // 斑缺一两个角：整块矩形是"贴上去的花纸"，缺角才是"石头上的色斑"
          if (pw > 1 && rnd(xx, yy, s + k * 31) < 0.10) continue;
          var px = ((x + xx) % LOG + LOG) % LOG, py = ((y + yy) % LOG + LOG) % LOG;
          if (used[py * LOG + px]) continue;                 // 已被前一档占过：让位，不盖
          used[py * LOG + px] = 1; got++;
          set(d, px, py, c);
        }
      }
    }
    return d;
  }

  function grassTop() {
    // 原版草顶是灰阶 + 生物群系乘色；我们没有乘色图，直接把乘完的结果画进格子里。
    // 档位照抄原版 grass_block_top 的实测分布（35/28/18/12），明度档压在 V112~195：
    // 对比再大就成了"发花的地板"，不是草地。
    return blot(GRASS, 1, [[30, 1, 3, 2], [22, 2, 2, 2], [13, 3, 3, 1]]);
  }
  function dirtTile() {
    // 原版 dirt 实测 42/27/15/13，就按这张表铺：底色占七成不是泥，是"一块脏"
    return blot(DIRT, 2, [[27, 1, 3, 2], [15, 2, 4, 3], [13, 3, 2, 2]]);
  }
  /** 草方块侧面：草皮只在最上 3~5 像素，下缘参差并往下滴一两格；再往下全是土。
   *  量原版 grass_block_side：草绿色只占 2%，其余 98% 是那块橙褐土。 */
  function grassSide() {
    var d = dirtTile();
    for (var x = 0; x < LOG; x++) {
      var dep = 3 + ((rnd(x, 21, 7) * 3) | 0);
      for (var y = 0; y < dep; y++) {
        var c = (y === dep - 1) ? GRASS[2] : (rnd(x, y, 22) < 0.3 ? GRASS[1] : GRASS[0]);
        set(d, x, y, c);
      }
      if (rnd(x, 23, 7) > 0.62) set(d, x, dep, GRASS[2]);      // 草须往下滴一格
      if (rnd(x, 24, 7) > 0.88) set(d, x, dep + 1, GRASS[3]);
    }
    return d;
  }
  function snowTop() {
    // 原版 snow.png 三档 46/30/24，明度全挤在 247~255：雪面靠的是极小的明度差，不是花斑
    return blot(SNOW, 8, [[30, 1, 4, 3], [24, 2, 3, 2]]);
  }
  function snowSide() {
    var d = dirtTile();
    for (var x = 0; x < LOG; x++) {
      var dep = 3 + ((rnd(x, 41, 13) * 2) | 0);
      for (var y = 0; y < dep; y++) set(d, x, y, y === 0 ? SNOW[1] : SNOW[0]);
      set(d, x, dep - 1, SNOW[2]);
      if (rnd(x, 42, 13) > 0.75) set(d, x, dep, SNOW[2]);
    }
    return d;
  }
  function sandTile() {
    // 原版 sand 实测 42/26/23/4（四档），沙的"颗粒感"来自档数而不是对比
    return blot(SAND, 9, [[23, 1, 3, 2], [26, 2, 2, 2], [5, 3, 2, 1]]);
  }
  /** 石：4 个色、连片斑、几道裂纹。原版的 stone 整张只有 4 档，实测 46/28/20/7；
   *  我们以前用纯灰 #7f7f7f 那组加逐像素噪声，山坡上就成了一片水泥味的灰。 */
  function stoneTile() {
    var d = blot(STONE, 10, [[28, 2, 3, 2], [20, 1, 2, 2], [7, 3, 2, 1]]);
    for (var k = 0; k < 3; k++) {                     // 裂纹：一条两到四格的暗色折线
      var x = 1 + ((rnd(k, 61, 19) * (LOG - 4)) | 0), y = 1 + ((rnd(k, 62, 19) * (LOG - 5)) | 0);
      for (var i = 0; i < 4; i++) {
        set(d, x, y, STONE[3]);
        if (rnd(k, i, 63) > 0.5) set(d, x + 1, y, STONE[2]);
        y += 1; if (rnd(k, i, 64) > 0.6) x += 1;
      }
    }
    return d;
  }
  function deepTile() {
    // 原版 deepslate 实测 30/27/23/10/9：底色是最亮的那档，暗档反而铺开——深岩是"压暗的花岗"，不是黑石头
    return blot(DEEP, 11, [[27, 2, 3, 2], [32, 1, 2, 2], [10, 3, 2, 1]]);
  }
  function waterTile() {
    // 原版 water_still 四档 40/38/14：以前只撒了几条横波纹，量出来 88% 是一整块死蓝
    // 6×1 的横条斑：既保住"水面是横着流动"的方向感，又把明暗摊到原版的配比
    return blot(WATER, 12, [[38, 1, 6, 1], [14, 2, 3, 2]]);
  }
  /** 矿石 = 石头底 + 几簇矿粒。矿粒只有外观意义：共识只认"这一带是矿"。 */
  function oreTile(ore, s, count) {
    var d = stoneTile();
    for (var k = 0; k < count; k++) {
      var x = 1 + ((rnd(k, 91, s) * 12) | 0), y = 1 + ((rnd(k, 92, s) * 12) | 0);
      set(d, x, y, ore[1]);
      set(d, x + 1, y, ore[0]);
      set(d, x, y + 1, ore[0]);
      set(d, x + 1, y + 1, ore[2]);
      if (rnd(k, 93, s) > 0.45) set(d, x + 2, y, ore[0]);
      if (rnd(k, 94, s) > 0.6) set(d, x, y + 2, ore[2]);
    }
    return d;
  }
  /** 圆石：4×4 的卵石 + 缝。量原版 cobblestone：6 个色，缝 [82,82,82]、面 [136,135,136]。
   *  我们保留它的低饱和（石头家族饱和 ≤3%），但整块压暖一档，和 STONE 同一个岩性。 */
  function cobbleTile(mossy) {
    var d = fill(blank(), [104, 102, 95]);
    for (var ry = 0; ry < 4; ry++) {
      for (var rx = 0; rx < 4; rx++) {
        var ox = rx * 4 + ((ry & 1) ? 2 : 0), oy = ry * 4;
        var v = 118 + ((rnd(rx, ry, 101) * 34) | 0);
        for (var y = 0; y < 4; y++) {
          for (var x = 0; x < 4; x++) {
            if ((x === 0 || x === 3) && (y === 0 || y === 3)) continue;
            set(d, (ox + x) & 15, oy + y,
              y === 0 ? [v + 20, v + 19, v + 16] : y === 3 ? [v - 22, v - 22, v - 25] : [v, v, v - 3]);
          }
        }
      }
    }
    if (mossy) {
      for (var k = 0; k < 14; k++) {
        var mx = (rnd(k, 111, 103) * LOG) | 0, my = (rnd(k, 112, 103) * LOG) | 0;
        set(d, mx, my, GRASS[1]);
        if (rnd(k, 113, 103) > 0.5) set(d, mx + 1, my, GRASS[0]);
        if (rnd(k, 114, 103) > 0.6) set(d, mx, my + 1, GRASS[2]);
      }
    }
    return d;
  }
  function eraGoldTile() {
    var d = blank();
    for (var y = 0; y < LOG; y++) {
      for (var x = 0; x < LOG; x++) {
        var t = (x + y) / 30;                        // 斜向光：左上亮、右下暗
        set(d, x, y, t < 0.34 ? GOLD[1] : t < 0.7 ? GOLD[0] : GOLD[2]);
      }
    }
    for (var i = 0; i < LOG; i++) {
      set(d, i, 0, GOLD[1]); set(d, 0, i, GOLD[1]);
      set(d, i, LOG - 1, GOLD[3]); set(d, LOG - 1, i, GOLD[3]);
    }
    var sp = [[3, 3], [12, 4], [5, 12], [11, 11]];
    for (var k = 0; k < sp.length; k++) {
      set(d, sp[k][0], sp[k][1], [255, 246, 201]);
      set(d, sp[k][0] + 1, sp[k][1] - 1, GOLD[1]);
    }
    return d;
  }
  function bedrockTile() {
    var d = blank();
    for (var ry = 0; ry < 4; ry++) {
      for (var rx = 0; rx < 4; rx++) {
        var v = 40 + ((rnd(rx, ry, 121) * 52) | 0);
        for (var y = 0; y < 4; y++) {
          for (var x = 0; x < 4; x++) {
            if (x === 0 || y === 0) { set(d, rx * 4 + x, ry * 4 + y, [24, 24, 26]); continue; }
            var hi = rnd(rx * 4 + x, ry * 4 + y, 122) < 0.18;
            set(d, rx * 4 + x, ry * 4 + y, hi ? [v + 26, v + 26, v + 26] : [v, v, v]);
          }
        }
      }
    }
    return d;
  }

  var ART = {};
  ART[S.GRASS_TOP] = grassTop();
  ART[S.GRASS_SIDE] = grassSide();
  ART[S.DIRT] = dirtTile();
  ART[S.STONE] = stoneTile();
  ART[S.COBBLE] = cobbleTile(false);
  ART[S.MOSSY] = cobbleTile(true);
  ART[S.SAND] = sandTile();
  ART[S.SNOW_TOP] = snowTop();
  ART[S.SNOW_SIDE] = snowSide();
  ART[S.WATER] = waterTile();
  ART[S.DEEP] = deepTile();
  ART[S.ORE_GOLD] = oreTile(ORE_GOLD, 131, 5);
  ART[S.ORE_IRON] = oreTile(ORE_IRON, 137, 5);
  ART[S.ORE_CRYSTAL] = oreTile(ORE_CRYSTAL, 139, 4);
  ART[S.ERA_GOLD] = eraGoldTile();
  ART[S.BEDROCK] = bedrockTile();

  // —— 拼图集：先原样复制 0..21（HD 包/基础包已经画好的物件），再把 22..37 落上去 ——
  var canvas = document.createElement('canvas');
  canvas.width = TILE * COLS;
  canvas.height = TILE * ROWS;
  var ctx = canvas.getContext('2d');
  ctx.imageSmoothingEnabled = false;
  ctx.drawImage(base.atlas(), 0, 0);

  var tmp = document.createElement('canvas');
  tmp.width = tmp.height = LOG;
  var tctx = tmp.getContext('2d');
  var img = tctx.createImageData(LOG, LOG);
  // 用户的地形贴图包：QLTerrainPack[槽位] = <canvas|Image>（顺序或对象都行）。
  // 有就整格替换，没有就用程序化那一份——换贴图不需要改任何代码。
  var pack = global.QLTerrainPack;
  for (var key in ART) {
    var id = Number(key);
    var custom = pack && (pack[id] || (pack[id - S.GRASS_TOP] && pack.length ? pack[id - S.GRASS_TOP] : null));
    var dx = (id % COLS) * TILE, dy = ((id / COLS) | 0) * TILE;
    if (custom && custom.width) {
      ctx.drawImage(custom, 0, 0, custom.width, custom.height, dx, dy, TILE, TILE);
      continue;
    }
    img.data.set(ART[id]);
    tctx.putImageData(img, 0, 0);
    ctx.drawImage(tmp, 0, 0, LOG, LOG, dx, dy, TILE, TILE);
  }

  function icon(idx, size) {
    size = size || 32;
    var c = document.createElement('canvas');
    c.width = c.height = size;
    var g = c.getContext('2d');
    g.imageSmoothingEnabled = false;
    g.drawImage(canvas, (idx % COLS) * TILE, ((idx / COLS) | 0) * TILE, TILE, TILE, 0, 0, size, size);
    return c;
  }
  function avgColor(idx) {
    var d = icon(idx, TILE).getContext('2d').getImageData(0, 0, TILE, TILE).data;
    var r = 0, g = 0, b = 0, n = 0;
    for (var i = 0; i < d.length; i += 4) {
      if (d[i + 3] < 128) continue;
      r += d[i]; g += d[i + 1]; b += d[i + 2]; n++;
    }
    return n ? [Math.round(r / n), Math.round(g / n), Math.round(b / n)] : [200, 200, 200];
  }

  base.TILE = TILE;
  base.atlas = function () { return canvas; };
  base.icon = icon;
  base.dataUrl = function (idx, size) { return icon(idx, size).toDataURL(); };
  base.avgColor = avgColor;
  base.TERRAIN = S;
})(window);
