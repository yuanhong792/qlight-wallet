/* QLight 世界 · 16 种物件的像素画（16×16，纯代码绘制，零素材依赖）
 *
 * 为什么是像素画：世界本来就由一格一格的体素算出来，物品栏再画成"彩色圆点"
 * 等于把世界和手里的东西割裂成两套语言。这里每个 kind 都对应一张能认出来的
 * 16×16 图——石头、树、花、木屋、桥、灯塔、篝火、篱笆、鹿、鸟、鱼、蝴蝶、
 * 兔、绵羊、旅人、狼——物品栏、2D 地图、第一人称贴图共用同一张图。
 *
 * 图集布局：8 列 × 8 行，每格 16px（128×128）。
 *   tile 0..15 = 物品栏 16 种；
 *   tile 16..21 = 组合体（树干、树叶、木板、屋顶、火焰、灯）复用的材质。
 * 采样一律 NEAREST，放大多少倍都是硬边像素。
 */
(function (global) {
  'use strict';

  var TILE = 16, COLS = 8, ROWS = 8;

  // —— 16 种物件 ——
  var ART = {
    0: { // 石头
      pal: { D: '#4b5563', s: '#9ca3af', S: '#d1d5db' },
      rows: [
        '................',
        '.DDDDDDDDDDDDDD.',
        '.DssssssssssssD.',
        '.DsSSsssssssssD.',
        '.DsssssDDsssssD.',
        '.DsssDDsssssssD.',
        '.DsssssssSSsssD.',
        '.DsssssssssssSD.',
        '.DsDDsssssssssD.',
        '.DssssssDDssssD.',
        '.DssssssssssssD.',
        '.DsSSsssssssssD.',
        '.DsssssssDDsssD.',
        '.DssssssssssssD.',
        '.DDDDDDDDDDDDDD.',
        '................',
      ],
    },
    1: { // 橡树
      pal: { G: '#2f7d32', g: '#4caf50', t: '#6d4c2f' },
      rows: [
        '................',
        '.....GGGGGG.....',
        '...GGGGGGGGGG...',
        '..GGGgGGGGgGGG..',
        '..GGGGGGGGGGGG..',
        '.GGGgGGGGGGgGGG.',
        '.GGGGGGGGGGGGGG.',
        '..GGGGttGGGGGG..',
        '...GGGttGGGGG...',
        '......tt........',
        '......tt........',
        '......tt........',
        '......tt........',
        '.....tttt.......',
        '....tttttt......',
        '................',
      ],
    },
    2: { // 花丛
      pal: { r: '#e53935', y: '#fdd835', w: '#f5f5f5', s: '#2e7d32', g: '#7cb342', G: '#558b2f' },
      rows: [
        '................',
        '................',
        '................',
        '..r........y....',
        '.rrr......yyy...',
        '..r....w...y....',
        '..s...www..s....',
        '..s....w...s....',
        '...s.......s....',
        '...s......s.....',
        '....s.....s.....',
        '....s....s......',
        '.....s...s......',
        '..gggggggggggg..',
        '.gGgGgGgGgGgGgG.',
        'gggggggggggggggg',
      ],
    },
    3: { // 木屋
      pal: { R: '#8d5524', r: '#a86b32', W: '#e8d5a8', w: '#7ec8e3', d: '#5d4037', D: '#3e2723' },
      rows: [
        '................',
        '......RRRR......',
        '.....RRRRRR.....',
        '....RRrRRrRR....',
        '...RRRRRRRRRR...',
        '..RRRRRRRRRRRR..',
        '.RRRRRRRRRRRRRR.',
        '..WWWWWWWWWWWW..',
        '..WwwWWWWWWwwW..',
        '..WwwWWddWWwwW..',
        '..WWWWWddWWWWW..',
        '..WwwWWddWWwwW..',
        '..WwwWWddWWwwW..',
        '..WWWWWWWWWWWW..',
        '..DDDDDDDDDDDD..',
        '................',
      ],
    },
    4: { // 木桥
      pal: { p: '#5d4037', P: '#a1887f', L: '#c5a46d' },
      rows: [
        '................',
        '................',
        '..p..........p..',
        '..p..........p..',
        '..pPPPPPPPPPPp..',
        '..p..........p..',
        '..pPPPPPPPPPPp..',
        '..p..........p..',
        '..p..........p..',
        '..pPPPPPPPPPPp..',
        '..p..........p..',
        '..p..........p..',
        '.pp..........pp.',
        '................',
        '................',
        '................',
      ],
    },
    5: { // 灯塔
      pal: { L: '#455a64', Y: '#ffe082', R: '#d32f2f', W: '#f5f5f5', g: '#558b2f', G: '#7cb342' },
      rows: [
        '................',
        '.......LL.......',
        '......LLLL......',
        '......LYYL......',
        '.....RRRRRR.....',
        '.....WWWWWW.....',
        '.....RRRRRR.....',
        '.....WWWWWW.....',
        '.....RRRRRR.....',
        '.....WWWWWW.....',
        '.....RRRRRR.....',
        '.....WWWWWW.....',
        '....RRRRRRRR....',
        '...gGGGGGGGGg...',
        '..gggggggggggg..',
        '................',
      ],
    },
    6: { // 篝火
      pal: { f: '#ff7043', F: '#ffca28', Y: '#fff59d', r: '#bf360c', L: '#6d4c2f', l: '#8d6e63' },
      rows: [
        '................',
        '................',
        '.......f........',
        '......fFf.......',
        '.....fFFFf......',
        '.....fFFYf......',
        '......fYf.......',
        '....r..f..r.....',
        '...rrr...rrr....',
        '..rrrr...rrrr...',
        '.rrrrrrrrrrrrr..',
        '..LLLLLLLLLLL...',
        '.LLLLLLLLLLLLL..',
        '.lllllllllllll..',
        '................',
        '................',
      ],
    },
    7: { // 篱笆
      pal: { p: '#5d4037', P: '#8d6e63' },
      rows: [
        '................',
        '................',
        '..p.........p...',
        '..p.........p...',
        '..pPPPPPPPPPP...',
        '..p.........p...',
        '..pPPPPPPPPPP...',
        '..p.........p...',
        '..p.........p...',
        '..pPPPPPPPPPP...',
        '..p.........p...',
        '..pPPPPPPPPPP...',
        '..p.........p...',
        '..p.........p...',
        '..p.........p...',
        '................',
      ],
    },
    8: { // 鹿
      pal: { A: '#c8a165', B: '#8d6e63', b: '#a1887f', W: '#efebe9', e: '#212121' },
      rows: [
        '................',
        '..A.........A...',
        '...A.......A....',
        '....A.A.A.A.....',
        '.....AAAAA......',
        '....BBBBBB......',
        '...BBBBBBBB.....',
        '...BBBBBBBBW....',
        '...BBBBBBBBB....',
        '....BBBBBB......',
        '....b....b......',
        '....b....b......',
        '....b....b......',
        '...bb....bb.....',
        '................',
        '................',
      ],
    },
    9: { // 鸟
      pal: { B: '#42a5f5', W: '#bbdefb', O: '#ffb300', e: '#212121' },
      rows: [
        '................',
        '................',
        '................',
        '................',
        '........BBBB....',
        '......BBBBBBB...',
        '.....BBBBBBBBBB.',
        '....BBBBBBBBBBB.',
        '...OBBBeBBBBBB..',
        '.....BBBBBBBB...',
        '......BB.BB.....',
        '......B...B.....',
        '................',
        '................',
        '................',
        '................',
      ],
    },
    10: { // 鱼
      pal: { O: '#fb8c00', o: '#ffb74d', e: '#212121', W: '#ffe0b2' },
      rows: [
        '................',
        '................',
        '................',
        '................',
        '.........OOO....',
        '.......OOOOOOO..',
        '..O...OOOOOOOOO.',
        '.OOO.OOeOOOOOOO.',
        'OOOOOOOOOOOOOO..',
        '.OOO..OOOOOOOO..',
        '..O....OOOOOO...',
        '.........ooo....',
        '................',
        '................',
        '................',
        '................',
      ],
    },
    11: { // 蝴蝶
      pal: { P: '#ba68c8', p: '#7b1fa2', B: '#4a148c' },
      rows: [
        '................',
        '................',
        '....PP....PP....',
        '...PPPP..PPPP...',
        '..PPPPPP.PPPPPP.',
        '..PPpPPP.PPPpPP.',
        '...PPPPP.PPPPP..',
        '.....PP..PP.....',
        '......P..P......',
        '.....PP..PP.....',
        '...PPPPP.PPPPP..',
        '..PPpPPP.PPPpPP.',
        '..PPPPPP.PPPPPP.',
        '...PPPP..PPPP...',
        '....PP....PP....',
        '................',
      ],
    },
    12: { // 兔
      pal: { W: '#eceff1', w: '#cfd8dc', p: '#f8bbd0', e: '#212121' },
      rows: [
        '................',
        '.....W..W.......',
        '....Wp..pW......',
        '....Wp..pW......',
        '....WWWWWW......',
        '...WWWWWWWW.....',
        '...WWeWWWWW.....',
        '...WWWWWWWW.....',
        '...WWWWWWWW.....',
        '....WWWWWW......',
        '....WWWWWW......',
        '....W....W......',
        '....W....W......',
        '...WW....WW.....',
        '................',
        '................',
      ],
    },
    13: { // 绵羊
      pal: { W: '#fafafa', w: '#e0e0e0', F: '#bcaaa4', f: '#8d6e63', L: '#6d4c2f' },
      rows: [
        '................',
        '................',
        '....WWWWWW......',
        '..WWWWWWWWWW....',
        '.WWWWWWWWWWWW...',
        '.WWWWWWWWWWWWF..',
        '.wWWWWWWWWWWFF..',
        '.wWWWWWWWWWWFFF.',
        '..WWWWWWWWWWF...',
        '...WWWWWWWW.....',
        '....L....L......',
        '....L....L......',
        '....L....L......',
        '...LL....LL.....',
        '................',
        '................',
      ],
    },
    14: { // 旅人
      pal: { H: '#795548', S: '#ffccbc', B: '#3949ab', P: '#8d6e63', L: '#37474f', e: '#212121' },
      rows: [
        '................',
        '.....HHHHH......',
        '....HHHHHHH.....',
        '....HSSSSSH.....',
        '.....SeSeS......',
        '....BBBBBB......',
        '...BBBBBBBB.....',
        '...BBPPPPBB.....',
        '...BBPPPPBB.....',
        '....BBBBBB......',
        '....BB..BB......',
        '....BB..BB......',
        '....BB..BB......',
        '...LL....LL.....',
        '................',
        '................',
      ],
    },
    15: { // 狼
      pal: { G: '#78909c', g: '#90a4ae', W: '#eceff1', e: '#212121', n: '#37474f' },
      rows: [
        '................',
        '................',
        '................',
        '..GG........G...',
        '..GGG......GGG..',
        '..GGGGGGGGGGG...',
        '.GGGGGGGGGGGGG..',
        '.GGeGGGGGGGGGG..',
        '.GGGGGGGGGGGGGG.',
        '.GGGGGGGGGGGnnn.',
        '..GGGGGGGGGGGG..',
        '..GG.GG..GG.GG..',
        '..GG.GG..GG.GG..',
        '..GG.GG..GG.GG..',
        '................',
        '................',
      ],
    },

    // —— 组合体材质（不在物品栏里显示）——
    // 2026-10-31 配色整改：树干与树叶**只换了调色板，一个像素的位置都没动**。
    // 原来的叶是 H122~124（比草皮的 H88 冷了 34°）、饱和最高到 0.55、明度带宽 0.40，
    // 于是树冠和草地是"两种绿"，而且树冠自己在屏幕上闪；干是三档互相不对齐的
    // 红褐（H11~16，比泥土的 H32 更偏红）；原来三档明度带 0.24~0.47。现在两者都并到
    // 大地那条暖类比色轴上：叶 H102 / S0.28 / L0.30~0.54，干 H28 / S0.24 / L0.26~0.41
    // （与泥土同一族）。
    16: { // 树干
      pal: { a: '#524132', b: '#685340', c: '#82674f' },
      rows: [
        'bcbaabcbacbbacba',
        'abcbbacabacbbaca',
        'bbacbbacbabcabcb',
        'cbabcabcbacbabac',
        'bacbbacabacbbacb',
        'abacbbacbabcabca',
        'bcabacbbacabacbb',
        'acbbacabacbbacba',
        'bbacbabcabcbacab',
        'cabacbbacabbacbb',
        'bacbbacabacbbaca',
        'abcabacbbacabacb',
        'bbacbbacabacbbac',
        'cbabcabcbacbabac',
        'bacbbacabacbbacb',
        'abacbbacbabcabca',
      ],
    },
    17: { // 树叶
      pal: { G: '#567c46', g: '#689654', d: '#446237', l: '#7dab69' },
      rows: [
        'GGgGGGgGGGGgGGGG',
        'GgGGGGdGGgGGGGdG',
        'GGGGgGGGGGGgGGGG',
        'GdGGGGGgGGGGGGgG',
        'GGGGgGGGGGdGGGGG',
        'GgGGGGGgGGGGgGGG',
        'GGGGdGGGGGGGGGGd',
        'GGgGGGGGgGGGgGGG',
        'GdGGGgGGGGGGGGgG',
        'GGGGGGGGdGGgGGGG',
        'GgGGGdGGGGGGGGGG',
        'GGGGGGGGGgGGGdGG',
        'GgGGgGGGGGGgGGGG',
        'GGGGGGGgGGGGGGgG',
        'GdGGGGGGGGdGGGGG',
        'GGgGGGgGGGGGgGGG',
      ],
    },
    18: { // 木板
      pal: { a: '#a1887f', b: '#8d6e63', c: '#795548', d: '#6d4c2f' },
      rows: [
        'bbbbbbbbbbbbbbbb',
        'baaaaaaabaaaaaaa',
        'baaaaaaabaaaaaaa',
        'bbbbbbbbbbbbbbbb',
        'dddddddddddddddd',
        'bcccccccbccccccc',
        'bcccccccbccccccc',
        'dddddddddddddddd',
        'bbbbbbbbbbbbbbbb',
        'baaaaaaabaaaaaaa',
        'baaaaaaabaaaaaaa',
        'bbbbbbbbbbbbbbbb',
        'dddddddddddddddd',
        'bcccccccbccccccc',
        'bcccccccbccccccc',
        'dddddddddddddddd',
      ],
    },
    19: { // 屋顶瓦
      pal: { a: '#b71c1c', b: '#8d5524', c: '#5d4037', d: '#d84315' },
      rows: [
        'aaaaaaaaaaaaaaaa',
        'abababababababab',
        'abbbbbbbbbbbbbba',
        'dddddddddddddddd',
        'aabababababababa',
        'abbbbbbbbbbbbbba',
        'cccccccccccccccc',
        'aabababababababa',
        'abbbbbbbbbbbbbba',
        'cccccccccccccccc',
        'aabababababababa',
        'abbbbbbbbbbbbbba',
        'dddddddddddddddd',
        'aabababababababa',
        'abbbbbbbbbbbbbba',
        'cccccccccccccccc',
      ],
    },
    20: { // 火焰（透明底）
      pal: { f: '#ff7043', F: '#ffca28', Y: '#fff59d' },
      rows: [
        '................',
        '................',
        '................',
        '.......f........',
        '......fFf.......',
        '.....fFFFf......',
        '.....fFFYf......',
        '....fFFYYFf.....',
        '....fFYYYYf.....',
        '...fFFYYYFFf....',
        '...fFFFYFFFFf...',
        '..fFFFFFFFFFFf..',
        '..fFFFFFFFFFFf..',
        '...ffffffffff...',
        '................',
        '................',
      ],
    },
    21: { // 灯
      pal: { L: '#455a64', Y: '#ffe082', W: '#fffde7' },
      rows: [
        'LLLLLLLLLLLLLLLL',
        'LYYYYYYYYYYYYYYL',
        'LYWYYYYYYYYYWYYL',
        'LYYYYWYYYYYYYYYL',
        'LYYYYYYYYYYWYYYL',
        'LYYYYYYYYYYYYYYL',
        'LYWYYYYYYYYYWYYL',
        'LYYYYWYYYYYYYYYL',
        'LYYYYYYYYYYWYYYL',
        'LYYYYYYYYYYYYYYL',
        'LYWYYYYYYYYYWYYL',
        'LYYYYWYYYYYYYYYL',
        'LYYYYYYYYYYWYYYL',
        'LYYYYYYYYYYYYYYL',
        'LYYYYYYYYYYYYYYL',
        'LLLLLLLLLLLLLLLL',
      ],
    },
  };

  var atlasCache = null;

  function drawArt(ctx, art, ox, oy) {
    for (var y = 0; y < TILE; y++) {
      var row = art.rows[y] || '';
      for (var x = 0; x < TILE; x++) {
        var ch = row[x];
        if (!ch || ch === '.' || ch === ' ') continue;
        var col = art.pal[ch];
        if (!col) continue;
        ctx.fillStyle = col;
        ctx.fillRect(ox + x, oy + y, 1, 1);
      }
    }
  }

  function atlas() {
    if (atlasCache) return atlasCache;
    var c = document.createElement('canvas');
    c.width = COLS * TILE;
    c.height = ROWS * TILE;
    var ctx = c.getContext('2d');
    ctx.imageSmoothingEnabled = false;
    for (var k in ART) {
      var idx = Number(k);
      drawArt(ctx, ART[k], (idx % COLS) * TILE, Math.floor(idx / COLS) * TILE);
    }
    atlasCache = c;
    return c;
  }

  /** 任意 tile → size×size 的硬边图标（物品栏 / 选中框 / 第一人称手持） */
  function icon(idx, size) {
    size = size || 32;
    var a = atlas();
    var c = document.createElement('canvas');
    c.width = c.height = size;
    var ctx = c.getContext('2d');
    ctx.imageSmoothingEnabled = false;
    var sx = (idx % COLS) * TILE, sy = Math.floor(idx / COLS) * TILE;
    ctx.drawImage(a, sx, sy, TILE, TILE, 0, 0, size, size);
    return c;
  }

  function dataUrl(idx, size) {
    return icon(idx, size).toDataURL();
  }

  /** 图标的平均色（粒子爆散用；透明像素不计入） */
  function avgColor(idx) {
    var c = icon(idx, TILE), ctx = c.getContext('2d');
    var d = ctx.getImageData(0, 0, TILE, TILE).data;
    var r = 0, g = 0, b = 0, n = 0;
    for (var i = 0; i < d.length; i += 4) {
      if (d[i + 3] < 128) continue;
      r += d[i]; g += d[i + 1]; b += d[i + 2]; n++;
    }
    if (!n) return [200, 200, 200];
    return [Math.round(r / n), Math.round(g / n), Math.round(b / n)];
  }

  global.QLSprites = {
    TILE: TILE, COLS: COLS, ROWS: ROWS, COUNT: 16,
    TRUNK: 16, LEAVES: 17, PLANKS: 18, ROOF: 19, FIRE: 20, LAMP: 21,
    atlas: atlas, icon: icon, dataUrl: dataUrl, avgColor: avgColor,
  };
})(window);
