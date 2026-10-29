/* QLight 确定性世界渲染器 · 钱包内嵌版（与 tools/world/world.js 同一算法）
 *
 * 规格见 DESIGN.md §9.0 / §9.5 / §9.8：world = render(种子, 规则, 历史)。
 * **链只给坐标与归属，像素全部在这里、在本机、在每一台机器上算出来**——
 * 钱包不下载任何地图、不存任何贴图，同一份链历史在任何一台机器上都必须
 * 渲染出逐位相同的大陆（tools/world/ 的 Python 实现就是用来对拍的）。
 *
 * v3 起，「大陆」不再由哈希凭空长出来：大地 = 真实地球（tools/world/fetch_earth_dem.py
 * 从公开 DEM 烤出的 256×256 高度场，随包发布在 earth-height.js），地表（湿度 /
 * 植被 / 矿脉）仍然只由创世哈希 ⊕ 链历史算出。形状是真的，随机是链给的。
 *
 * 32 位算术纪律：
 *   - 需要回绕的乘法一律 Math.imul（普通 * 超过 2^53 会丢精度）
 *   - 取 u32 用 >>>0，取 i32 用 |0
 *   - 右移只用 2 的幂，不用除法
 */
(function (global) {
  'use strict';

  var W = 256, H = 256;
  var RULES_VERSION = 'QL-WORLD-RULES-v3';
  var SEA = 86, SAND = 98, SNOW = 190;
  var M_FOREST = 120, M_DESERT = 60;
  var DENS_FOREST = 225, DENS_DESERT = 240, DENS_GRASS = 232;
  // 种子分道（v2 定下的、v3 沿用）：前 4 个字 = 大地，后 4 个字 = 地表。
  // v3 之后大地已经不看种子了（它来自真实高程），但**湿度/矿脉仍按这条分道走**：
  // 高度场随块漂移会让 (x, z, layer) 失去意义——签了名的塔会被埋进地里或悬在
  // 半空，世界就不可能被建造。
  var SEED_SPLIT = 4;

  var GENESIS_HASH = '5dc2205a8f0153a05f84e26ab94086009796bb68fdfddff439f97ebfd2aa3104';

  /* 大地 = 真实地球。缺失即致命：静默退回噪声会让这份实现悄悄画出
   * 另一片大陆，而四份副本的对拍正是这个项目"可验证"的全部凭据。 */
  if (!global.QLEarth) throw new Error('world.js: 缺少 earth-height.js，大地无从确定');
  var EARTH = global.QLEarth.load();

  var AMPS = [32, 16, 8, 4, 2];
  var CELLS = [64, 32, 16, 8, 4];
  var CLOG = [6, 5, 4, 3, 2];

  var u32 = function (x) { return x >>> 0; };

  /* 规则指纹（DESIGN.md §9.6）：
       canon = "QL-WORLD-RULES-v3|256,256,86,98,190,120,60,225,240,232,4|<大地 SHA-256>"
       seed  = shake_256(canon)[0..4]  → 0x50d296e0
     与 world.py 同源。规则版本必须是公开常量而非每次重算，故此处直接锚定；
     改上面任何一个阈值、或换一片土地，都必须同步改这里，否则两份实现会静默分叉。 */
  var RULES_SEED = 0x50d296e0;

  function setRulesSeed(v) { RULES_SEED = v >>> 0; }

  function hash2(ix, iy, seed) {
    var h = u32(ix * 374761393 + iy * 668265263 + seed + RULES_SEED);
    h = u32(h ^ (h >>> 13));
    h = u32(Math.imul(h, 1274126177));
    h = u32(h ^ (h >>> 16));
    return h;
  }

  function smooth(t) { return (t * t * (768 - 2 * t)) >> 16; }

  function vnoise(x, y, cell, clog, seed) {
    var xi = x >> clog, yi = y >> clog;
    var fx = (x & (cell - 1)) << (8 - clog);
    var fy = (y & (cell - 1)) << (8 - clog);
    var sx = smooth(fx), sy = smooth(fy);
    var n00 = (hash2(xi, yi, seed) >> 16) & 255;
    var n10 = (hash2(xi + 1, yi, seed) >> 16) & 255;
    var n01 = (hash2(xi, yi + 1, seed) >> 16) & 255;
    var n11 = (hash2(xi + 1, yi + 1, seed) >> 16) & 255;
    var a = n00 + (((n10 - n00) * sx) >> 8);
    var b = n01 + (((n11 - n01) * sx) >> 8);
    return a + (((b - a) * sy) >> 8);
  }

  function fbm(x, y, words, off) {
    var acc = 0;
    for (var k = 0; k < 5; k++) {
      acc += vnoise(x, y, CELLS[k], CLOG[k], words[off + (k & 3)]) * AMPS[k];
    }
    return acc >> 6;
  }

  function hexToBytes(hex) {
    var out = new Uint8Array(hex.length >> 1);
    for (var i = 0; i < out.length; i++) out[i] = parseInt(hex.substr(i * 2, 2), 16);
    return out;
  }

  /* 种子 = 大地 ⊕ 地表两条道：
       words[0..3] = 创世哈希            → 天然实体与创世纪元格（永不变）
       words[4..7] = 创世哈希 ⊕ 全部块 VRF → 湿度场（链长一格，地表重画一次）
     这就是「链即世界的历史」在可建造前提下的唯一写法。
     （v3 起高度场已经在这两条道之外——它是真实地球；但实体与矿脉照旧走分道，
       所以"下一块之前谁也算不出未来哪里有矿"这条一分未动。） */
  function seedWords(genesisHex, vrfList) {
    var raw = hexToBytes(genesisHex);
    var folded = raw.slice(0);
    for (var j = 0; j < vrfList.length; j++) {
      var b = hexToBytes(vrfList[j]);
      for (var i = 0; i < 32; i++) folded[i] ^= b[i];
    }
    var words = [];
    for (var w = 0; w < 8; w++) {
      var src = w < SEED_SPLIT ? raw : folded;
      words.push(((src[w * 4] << 24) | (src[w * 4 + 1] << 16) | (src[w * 4 + 2] << 8) | src[w * 4 + 3]) >>> 0);
    }
    return words;
  }

  /* 方块类型（第一人称体素视图用；与像素同一趟分类算出，绝不允许两套判定分叉）
     0 水 1 沙 2 雪 3 森林 4 沙漠 5 草原；+8 天然树，+16 创世纪元格 */
  function render(words) {
    var px = new Uint8Array(W * H * 3);
    var hg = new Uint8Array(W * H);
    var bk = new Uint8Array(W * H);
    var era1 = 0, ents = 0;
    for (var y = 0; y < H; y++) {
      for (var x = 0; x < W; x++) {
        var hgt = EARTH[y * W + x];   // 大地：真实地球高程，与种子无关（v3）
        var mo = fbm(x, y, words, 4);
        var e = hash2(x + 7919, y + 104729, words[0]);
        var era = (e >>> 24) & 0xFF;
        if (era === 0) era1++;
        var blk;

        var r, g, b;
        if (hgt < SEA) {
          var d = (SEA - hgt) >> 2;
          r = 10; g = 34 + ((60 - d) >> 1); b = 74 + (120 - d); blk = 0;
        } else if (hgt < SAND) {
          r = 196; g = 176; b = 122; blk = 1;
        } else if (hgt > SNOW) {
          r = 226; g = 232; b = 240; blk = 2;
        } else if (mo > M_FOREST) {
          r = 44; g = 96 - ((hgt - SAND) >> 2); b = 52; blk = 3;
        } else if (mo < M_DESERT) {
          r = 178; g = 152; b = 96; blk = 4;
        } else {
          r = 104; g = 148 - ((hgt - SAND) >> 2); b = 74; blk = 5;
        }

        if (hgt >= SEA && hgt <= SNOW) {
          var t = (e >>> 8) & 0xFF;
          var dens = mo > M_FOREST ? DENS_FOREST : (mo < M_DESERT ? DENS_DESERT : DENS_GRASS);
          if (t > dens) {
            ents++;
            blk += 8;
            var k = (e >>> 4) & 3;
            if (k === 0) { r = 26; g = 62; b = 30; }
            else if (k === 1) { r = 150; g = 190; b = 90; }
            else if (k === 2) { r = 118; g = 84; b = 54; }
            else { r = 176; g = 146; b = 60; }
          }
        }
        if (era === 0) { r = 248; g = 216; b = 104; blk += 16; }

        var o = (y * W + x) * 3;
        px[o] = r; px[o + 1] = g; px[o + 2] = b;
        hg[y * W + x] = hgt; bk[y * W + x] = blk;
      }
    }
    return { rgb: px, era1: era1, entities: ents, heights: hg, blocks: bk };
  }

  /* 双 FNV-1a 32 位指纹 → 16 个十六进制位，证明逐位一致 */
  function fingerprint(buf) {
    var out = '';
    var specs = [[0x811C9DC5, 16777619], [0x01000193, 3295579737]];
    for (var s = 0; s < 2; s++) {
      var d = specs[s][0], prime = specs[s][1];
      for (var i = 0; i < buf.length; i++) d = Math.imul(d ^ buf[i], prime) >>> 0;
      out += ('0000000' + d.toString(16)).slice(-8);
    }
    return out;
  }

  /* 把 RGB 像素铺到 canvas（补 alpha 通道） */
  function paint(ctx, rgb) {
    var rgba = new Uint8ClampedArray(W * H * 4);
    for (var i = 0, o = 0; i < rgb.length; i += 3, o += 4) {
      rgba[o] = rgb[i]; rgba[o + 1] = rgb[i + 1]; rgba[o + 2] = rgb[i + 2]; rgba[o + 3] = 255;
    }
    ctx.putImageData(new ImageData(rgba, W, H), 0, 0);
  }

  global.QLWorld = {
    W: W, H: H, GENESIS_HASH: GENESIS_HASH, RULES_VERSION: RULES_VERSION,
    setRulesSeed: setRulesSeed, seedWords: seedWords, render: render,
    fingerprint: fingerprint, hash2: hash2, fbm: fbm, earth: global.QLEarth,
    RULES: { SEA: SEA, SAND: SAND, SNOW: SNOW }, paint: paint
  };
})(window);
