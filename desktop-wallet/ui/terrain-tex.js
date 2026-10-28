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

  // —— 调色板：取《我的世界》方块图鉴的配色（亮/基/暗/最暗四档）——
  var GRASS = [[110, 169, 74], [138, 196, 94], [87, 140, 59], [71, 117, 47]];
  var DIRT = [[139, 106, 70], [160, 124, 84], [116, 86, 57], [95, 68, 45]];
  var STONE = [[127, 127, 127], [150, 150, 150], [105, 105, 105], [88, 88, 88]];
  var DEEP = [[86, 88, 92], [104, 106, 110], [68, 70, 74], [54, 56, 60]];
  var SAND = [[226, 211, 166], [241, 228, 188], [205, 188, 143], [184, 166, 122]];
  var SNOW = [[244, 248, 252], [255, 255, 255], [224, 234, 243], [206, 219, 232]];
  var WATER = [[47, 111, 196], [73, 143, 222], [35, 87, 160], [26, 70, 134]];
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
  function set(d, x, y, c, a) {
    if (x < 0 || y < 0 || x >= LOG || y >= LOG) return;
    var i = (y * LOG + x) * 4;
    d[i] = c[0]; d[i + 1] = c[1]; d[i + 2] = c[2]; d[i + 3] = a == null ? 255 : a;
  }
  /** 颗粒底：两层哈希混出"大小颗粒交错"的地面，不是电视雪花 */
  function grain(pal, s) {
    var d = blank();
    for (var y = 0; y < LOG; y++) {
      for (var x = 0; x < LOG; x++) {
        var n = rnd(x, y, s) * 0.62 + rnd(x >> 1, y >> 1, s + 17) * 0.38;
        set(d, x, y, n < 0.22 ? pal[1] : n < 0.78 ? pal[0] : n < 0.93 ? pal[2] : pal[3]);
      }
    }
    return d;
  }

  function grassTop() {
    var d = grain(GRASS, 1);
    for (var k = 0; k < 10; k++) {
      var x = (rnd(k, 1, 3) * LOG) | 0, y = (rnd(k, 2, 3) * (LOG - 2)) | 0;
      set(d, x, y, GRASS[1]);
      if (rnd(k, 3, 3) > 0.4) set(d, x, y + 1, GRASS[1]);
      if (rnd(k, 4, 3) > 0.7) set(d, x + 1, y, GRASS[2]);
    }
    return d;
  }
  function dirtTile() {
    var d = grain(DIRT, 2);
    for (var k = 0; k < 5; k++) {
      var x = (rnd(k, 11, 5) * (LOG - 1)) | 0, y = (rnd(k, 12, 5) * (LOG - 1)) | 0;
      set(d, x, y, DIRT[2]); set(d, x + 1, y, DIRT[2]);
      if (rnd(k, 13, 5) > 0.5) set(d, x, y + 1, DIRT[3]);
      if (rnd(k, 14, 5) > 0.7) set(d, x + 1, y + 1, DIRT[1]);
    }
    return d;
  }
  /** 草方块侧面：上半截是草皮，下缘参差（3..5 像素），再往下才是土 */
  function grassSide() {
    var d = dirtTile();
    for (var x = 0; x < LOG; x++) {
      var dep = 3 + ((rnd(x, 21, 7) * 3) | 0);
      for (var y = 0; y < dep; y++) {
        var c = (y === dep - 1) ? GRASS[2] : (rnd(x, y, 22) < 0.28 ? GRASS[1] : GRASS[0]);
        set(d, x, y, c);
      }
      if (rnd(x, 23, 7) > 0.7) set(d, x, dep, GRASS[2]);
    }
    return d;
  }
  function snowTop() {
    var d = grain(SNOW, 8);
    for (var k = 0; k < 8; k++) {
      var x = (rnd(k, 31, 11) * LOG) | 0, y = (rnd(k, 32, 11) * LOG) | 0;
      set(d, x, y, SNOW[1]);
      if (rnd(k, 33, 11) > 0.6) set(d, x + 1, y, SNOW[2]);
    }
    return d;
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
    var d = grain(SAND, 9);
    for (var k = 0; k < 7; k++) {
      var x = (rnd(k, 51, 17) * LOG) | 0, y = (rnd(k, 52, 17) * LOG) | 0;
      set(d, x, y, SAND[2]);
      if (rnd(k, 53, 17) > 0.5) set(d, x + 1, y, SAND[3]);
    }
    return d;
  }
  function stoneTile() {
    var d = grain(STONE, 10);
    for (var k = 0; k < 3; k++) {
      var x = (rnd(k, 61, 19) * 13) | 0, y = (rnd(k, 62, 19) * 11) | 0;
      for (var i = 0; i < 4; i++) {
        set(d, x, y, STONE[2]);
        if (rnd(k, i, 63) > 0.6) set(d, x + 1, y, STONE[2]);
        y += 1;
      }
    }
    for (var k2 = 0; k2 < 4; k2++) set(d, (rnd(k2, 64, 19) * LOG) | 0, (rnd(k2, 65, 19) * LOG) | 0, STONE[1]);
    return d;
  }
  function deepTile() {
    var d = grain(DEEP, 11);
    for (var k = 0; k < 6; k++) set(d, (rnd(k, 71, 23) * LOG) | 0, (rnd(k, 72, 23) * LOG) | 0, DEEP[1]);
    return d;
  }
  function waterTile() {
    var d = grain(WATER, 12);
    for (var y = 0; y < LOG; y++) {
      if (rnd(y, 81, 29) < 0.45) {
        var x0 = (rnd(y, 82, 29) * 12) | 0, len = 3 + ((rnd(y, 83, 29) * 5) | 0);
        for (var x = x0; x < x0 + len && x < LOG; x++) {
          set(d, x, y, rnd(x, y, 84) < 0.5 ? WATER[1] : WATER[0]);
        }
      }
      if (rnd(y, 85, 29) < 0.3) set(d, (rnd(y, 86, 29) * LOG) | 0, y, WATER[2]);
    }
    return d;
  }
  /** 矿石 = 石头底 + 几簇矿脉。矿脉只有外观意义：共识只认"这一带是矿"。 */
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
  function cobbleTile(mossy) {
    var d = blank();
    for (var i = 0; i < d.length; i += 4) { d[i] = 92; d[i + 1] = 92; d[i + 2] = 92; d[i + 3] = 255; }
    for (var ry = 0; ry < 4; ry++) {
      for (var rx = 0; rx < 4; rx++) {
        var ox = rx * 4 + ((ry & 1) ? 2 : 0), oy = ry * 4;
        var v = 108 + ((rnd(rx, ry, 101) * 40) | 0);
        for (var y = 0; y < 4; y++) {
          for (var x = 0; x < 4; x++) {
            if ((x === 0 || x === 3) && (y === 0 || y === 3)) continue;
            set(d, (ox + x) & 15, oy + y, y === 0 ? [v + 22, v + 22, v + 22] : y === 3 ? [v - 26, v - 26, v - 26] : [v, v, v]);
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
