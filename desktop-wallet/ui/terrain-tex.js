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

  // —— 调色板：2026-10-31 配色整改（业主：「色彩搭配一定要合理，要美观，让人在里面一直都不会累」）
  //    上一版是拿原版《我的世界》逐像素量出来的数，量得很准，但**摆在一起不好看**：
  //    草 H96/S.37 和土 H26/S.35 是一对近似互补色，格内明度带宽又宽
  //    （草 L .31~.52、土 L .25~.54），于是脚下每一格都在和邻格打架——
  //    这就是「地面泥棕发花」的真因，不是亮度不够，是大面积互补 + 格内高对比。
  //    这一版按三条纪律重定，全部用 HSL 反算成 rgb，任何人都能复核：
  //      ① **大面积材质走一条暖类比色轴**：草 H88、土 H32，同属暖侧，只差明度与饱和，
  //         不再互补对打；土是"草的阴影色"，不是另一种材料。
  //      ② **饱和按面积分配**：草/土/石/沙/雪 S ≤0.35，水 0.48（它是暗部，容得下饱和），
  //         高饱和只留给小面积点缀（矿粒、元金、火）——点缀之所以是点缀，因为大面积克制。
  //      ③ **格内明度带宽按面积收**：越占屏幕的越要平——草 0.129、土 0.141（原来 0.216 /
  //         0.288），花纹读起来是"同一种材料的起伏"，不是迷彩；石/沙/雪/水允许 0.15~0.20，
  //         它们要么面积小、要么本来就靠颗粒感成立。这三个数都是 HSL 的 L，反算可核。
  var GRASS = [[120, 152, 82], [130, 166, 89], [109, 139, 75], [96, 123, 66]];
  var DIRT = [[129, 109, 86], [144, 121, 96], [116, 98, 78], [101, 85, 67]];
  var STONE = [[120, 128, 135], [136, 143, 150], [103, 110, 116], [86, 92, 97]];
  var DEEP = [[72, 75, 81], [86, 90, 97], [55, 57, 62], [43, 45, 49]];
  var SAND = [[211, 199, 166], [220, 210, 183], [201, 186, 146], [189, 171, 122]];
  var SNOW = [[244, 245, 246], [252, 252, 253], [227, 230, 232], [210, 214, 218]];
  var WATER = [[42, 87, 121], [50, 103, 143], [34, 71, 98], [28, 57, 79]];
  var GOLD = [[211, 179, 105], [222, 197, 140], [201, 162, 69], [174, 137, 50]];
  var ORE_GOLD = [[226, 192, 80], [234, 208, 123], [211, 169, 34]];
  var ORE_IRON = [[208, 182, 159], [221, 203, 187], [188, 151, 118]];
  var ORE_CRYSTAL = [[114, 203, 185], [151, 216, 203], [69, 186, 163]];

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
    // 四档只占 58%，底色留足：档数越多、越铺满，一格在屏幕上就越像迷彩。
    // 配比按"大面积克制"重定（原来 30/22/13 全铺满 = 65% 是异色）。
    return blot(GRASS, 1, [[22, 1, 3, 2], [16, 2, 2, 2], [8, 3, 3, 1]]);
  }
  function dirtTile() {
    // 与草顶同一档数、同一带宽：土和草是"同一片地的亮面与暗面"，不是两种材料
    return blot(DIRT, 2, [[18, 1, 3, 2], [12, 2, 4, 3], [9, 3, 2, 2]]);
  }
  /** 草方块侧面：草皮 + 土壁。
   *  为什么草皮要比原版厚（原版量出来只有 2% 是绿的）：**我们看到的绝大多数地面不是顶面，
   *  是侧壁**。大地是真实高程按 1 层 = 1 米量化出来的台阶，实测每格台阶高差直方图
   *  0:74.3% / 1:20.0% / 2:5.7%，也就是四分之一的格边有一道 1~2 米的坎；
   *  平视时屏幕上坎的面积远大于顶面。照抄原版那种"98% 土 + 一条草须"，
   *  整片草原就渲染成了一片泥地——这正是「地面泥棕发花」的第二条真因。
   *  所以草皮铺到 5~8/16（约一半），并且加一行过渡色，边缘不再是整齐的一条线。 */
  function grassSide() {
    var d = dirtTile();
    var TURF = [GRASS[0], GRASS[1], GRASS[0], GRASS[2], GRASS[3]];
    for (var x = 0; x < LOG; x++) {
      var dep = 5 + ((rnd(x, 21, 7) * 4) | 0);
      for (var y = 0; y < dep; y++) {
        var c = y === dep - 1 ? GRASS[3] : (rnd(x, y, 22) < 0.34 ? TURF[(y + x) % 5] : GRASS[0]);
        set(d, x, y, c);
      }
      if (rnd(x, 23, 7) > 0.55) set(d, x, dep, GRASS[3]);           // 草须往下滴一格
      if (rnd(x, 24, 7) > 0.82) set(d, x, dep + 1, DIRT[3]);        // 再往下是浮土，不是裸土
    }
    return d;
  }
  function snowTop() {
    // 雪面靠的是极小的明度差，不是花斑：带宽 0.15，占比再压低，否则雪地在屏幕上闪
    return blot(SNOW, 8, [[18, 1, 4, 3], [12, 2, 3, 2]]);
  }
  function snowSide() {
    var d = dirtTile();
    for (var x = 0; x < LOG; x++) {
      var dep = 5 + ((rnd(x, 41, 13) * 4) | 0);
      for (var y = 0; y < dep; y++) set(d, x, y, y === 0 ? SNOW[1] : SNOW[0]);
      set(d, x, dep - 1, SNOW[2]);
      if (rnd(x, 42, 13) > 0.62) set(d, x, dep, SNOW[2]);
    }
    return d;
  }
  function sandTile() {
    // 沙的"颗粒感"来自档数而不是对比：占比压到 35%，四档明度带宽 0.18
    return blot(SAND, 9, [[16, 1, 3, 2], [14, 2, 2, 2], [5, 3, 2, 1]]);
  }
  /** 石：4 个色、连片斑、几道裂纹。石是中性冷灰（H208/S.06），
   *  和暖侧的草/土拉开的是**色温**，不是色相对打——山坡上一片灰才不刺眼。 */
  function stoneTile() {
    var d = blot(STONE, 10, [[18, 2, 3, 2], [13, 1, 2, 2], [6, 3, 2, 1]]);
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
    // 深层岩是"压暗的花岗"，带宽只有 0.18：越是大面积的地下材质越不能花
    return blot(DEEP, 11, [[16, 2, 3, 2], [18, 1, 2, 2], [8, 3, 2, 1]]);
  }
  function waterTile() {
    // 水是全场最暗的大面积材质（L .21~.38），所以它容得下 0.48 的饱和：
    // 暗 + 饱和 = 有分量；亮 + 饱和 = 刺眼。6×1 的横条斑保住"水面横着流动"的方向感。
    return blot(WATER, 12, [[30, 1, 6, 1], [12, 2, 3, 2]]);
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
  /** 圆石：4×4 的卵石 + 缝。石头家族整组偏冷（H≈210/S≤.07）——
   *  它要和暖侧的草、土分开的是**色温**，靠色相对打只会让画面更吵。 */
  function cobbleTile(mossy) {
    var d = fill(blank(), [101, 106, 111]);
    for (var ry = 0; ry < 4; ry++) {
      for (var rx = 0; rx < 4; rx++) {
        var ox = rx * 4 + ((ry & 1) ? 2 : 0), oy = ry * 4;
        var v = 114 + ((rnd(rx, ry, 101) * 30) | 0);
        for (var y = 0; y < 4; y++) {
          for (var x = 0; x < 4; x++) {
            if ((x === 0 || x === 3) && (y === 0 || y === 3)) continue;
            set(d, (ox + x) & 15, oy + y,
              y === 0 ? [v + 12, v + 15, v + 20] : y === 3 ? [v - 18, v - 20, v - 24] : [v, v + 2, v + 6]);
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
