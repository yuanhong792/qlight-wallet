/* QLight 世界 · 第一人称体素编辑器（Minecraft 式创造，纯 WebGL1，零依赖）
 *
 * 分工与 2D 地图页同一条纪律（DESIGN.md §9.8 / §9.11）：**地形与方块全部在本地算**，
 * 链只给实体坐标（x, z, layer）、槽位与归属。本文件不签名、不写账本——放置/拆除
 * 通过回调交给 app.js，由 src/world.rs 落一条 ML-DSA-65 记录。
 *
 * 与账本严格对齐的三条规则（都在 src/world.rs 里判定，这里只镜像显示）：
 *   1. 一格一维 = 一个体素：贴地 layer = 高度场 >> 3，往上叠塔就是更大的 layer
 *   2. 同一 (x, z, layer) 只能有一个活着的方块 → 所以本文件也只在被算成空的体素上高亮放置框
 *   3. 已释放归属的实体不显形、不占格（断租不删除：记录永久可查，但空间重新可造）
 *
 * 地形可以挖，但**挖这件事本身要签名**：每铲掉一层落一条 `op=dig` 记录（v5 账本），
 * 坐标、层号、材料都由链上重放判定。像素仍然在本地算：链只记"这一列被挖掉了几层"。
 */
(function (global) {
  'use strict';

  var W = 256, H = 256;
  var LEVEL_SHIFT = 3;          // 高度场 0..255 → 层（8 分之一格一层），与 world.rs MAX_LAYER 同口径
  var MAX_LAYER = 64;           // 可建造的高度层上限（链侧常量，双端必须一致）
  var REACH = 7;                // 可编辑距离（格）
  var REGION = 64;              // 每次重建网格的边长（格）
  var EYE = 1.62;               // 视高（格）
  // —— 真实世界的物理常数：一格就是一米，人就是一个 1.75 米高、0.6 米宽的盒子。
  //    这套数字不是手感调参，是米每秒和米每二次方秒，任何人都能拿卷尺量。
  var BODY_H = 1.75;            // 身高（米）
  var BODY_R = 0.3;             // 半身宽（米）：身体占 0.6 米，比一格窄，才会卡在门缝里
  var GRAV = 9.81;              // 重力加速度
  var JUMP_V = 5.0;             // 起跳初速 → 跳 1.27 米高，正好上得去一格台阶
  var WALK = 1.4;               // 人走路实测 1.4 米/秒
  var RUN = 4.2;                // 全力跑 4.2 米/秒（旧版是 7 米/秒 = 每秒跨七米，不是人）
  var STEP_UP = 0.62;           // 不用跳就能跨上去的高度（真人约半步）
  var WATER_TOP = (((global.QLWorld && global.QLWorld.RULES.SEA) || 86) >> 3) + 1 - 0.36;
  // 海面 y：海平面那一格的顶面再低 36 厘米——水线在沙滩顶面之下，岸才有厚度。
  // 这个数和 world3d-three.js 的 WATER_Y 必须是同一个，否则眼睛里的水和画出来的水不是一层。
  // 雾：区域只有 REGION 格，边界外是真空。用距离雾在边界之前把地形融进天空，
  // 否则走两步就看见"世界在 32 格外被整齐切断"——确定性世界的边缘不该是穿帮。
  var SKY = [0.55, 0.72, 0.92];
  var FOG_NEAR = 12, FOG_FAR = REGION / 2 + 1;

  // 《我的世界》最值钱的东西不是方块，是"世界活着"：太阳会落、天会黑、雾会变。
  // 这里用一个块 = 2 秒的真实节奏做昼夜：600 块 = 20 分钟一天（与 MC 同量级）。
  var DAY_BLOCKS = 600;
  var SKY_DAY = [0.55, 0.72, 0.92], SKY_DUSK = [0.96, 0.53, 0.32], SKY_NIGHT = [0.03, 0.05, 0.14];
  var TINT_DAY = [1, 1, 1], TINT_DUSK = [1.0, 0.82, 0.68], TINT_NIGHT = [0.46, 0.56, 0.82];


  // 与 world.js 的像素分类一一对应（blk 低 3 位）——俯视图的像素与走进去的方块不可能对不上
  var PAL = [
    [18, 54, 118],   // 0 水
    [196, 176, 122], // 1 沙
    [226, 232, 240], // 2 雪
    [44, 96, 52],    // 3 森林
    [178, 152, 96],  // 4 荒漠
    [104, 148, 74],  // 5 草原
  ];
  var TREE_TRUNK = [118, 84, 54];
  var TREE_LEAF = [26, 62, 30];
  var ERA_GOLD = [248, 216, 104];
  var BLOCK_SHADE = 0.94;       // 签出来的方块：与地形同色相但略微收边，看得出是"人造的"

  // —— 挖地（实体账本 v5 = QL-ENTREC-v5）：全部与 src/world.rs 同一口径 ——
  //    地表层 = 原始高度场 >> 3；挖掉一层，这一列露出的顶面就降一层；挖到 DUG_FLOOR 见底。
  //    材料只按"离原始地表多深"分带：0–3 层是土、4–9 层是石、10 层往下是矿。
  //    这三个数一旦与 Rust 侧不同，客户端显示的"下一铲会出什么"就会撒谎。
  var DUG_FLOOR = 1, DUG_SOIL_DEPTH = 3, DUG_STONE_DEPTH = 9;
  // 地形贴图槽位（图集 22..37，物品栏永远不占）。terrain-tex.js 负责画进去；
  // 用户的地形贴图包可以整组替换这些槽位，本文件只认槽位号。
  var TIL = (global.QLSprites && global.QLSprites.TERRAIN) || null;

  // 面的朝向（外法线）常量——quad() 靠它决定顶点序，见下方 quad 的注释
  var N_UP = [0, 1, 0], N_DOWN = [0, -1, 0], N_N = [0, 0, -1], N_S = [0, 0, 1], N_E = [1, 0, 0], N_W = [-1, 0, 0];

  var VS = 'attribute vec3 a_p;attribute vec3 a_c;uniform mat4 u_vp;uniform vec3 u_eye;' +
    'varying vec3 v_c;varying vec3 v_w;varying float v_d;' +
    'void main(){v_c=a_c;v_w=a_p;v_d=distance(a_p,u_eye);gl_PointSize=3.0;gl_Position=u_vp*vec4(a_p,1.0);}';
  // 大地也吃一层 8×8 的程序化颗粒：没有贴图，但每个方块表面不再是纯色塑料片。
  var FS = 'precision mediump float;varying vec3 v_c;varying vec3 v_w;varying float v_d;' +
    'uniform vec4 u_tint;uniform vec3 u_fog;uniform vec2 u_fog_r;' +
    'void main(){float f=clamp((v_d-u_fog_r.x)/(u_fog_r.y-u_fog_r.x),0.0,1.0);f*=f;' +
    'float h=fract(sin(dot(floor(v_w*8.0),vec3(12.9898,78.233,37.719)))*43758.5453);' +
    'vec3 base=v_c*(0.85+0.16*h);' +
    'gl_FragColor=vec4(mix(base*u_tint.rgb,u_fog,f),1.0);}';

  // 贴图管线：像素图集 + alpha 裁剪（树叶/花/活物不需要混合排序，直接 discard）
  var VS2 = 'attribute vec3 a_p;attribute vec2 a_uv;attribute float a_s;uniform mat4 u_vp;uniform vec3 u_eye;' +
    'varying vec2 v_uv;varying float v_s;varying float v_d;' +
    'void main(){v_uv=a_uv;v_s=a_s;v_d=distance(a_p,u_eye);gl_Position=u_vp*vec4(a_p,1.0);}';
  var FS2 = 'precision mediump float;uniform sampler2D u_tex;uniform vec4 u_tint;uniform vec3 u_fog;uniform vec2 u_fog_r;' +
    'varying vec2 v_uv;varying float v_s;varying float v_d;' +
    'void main(){vec4 t=texture2D(u_tex,v_uv);if(t.a<0.5)discard;' +
    'float f=clamp((v_d-u_fog_r.x)/(u_fog_r.y-u_fog_r.x),0.0,1.0);f*=f;' +
    'gl_FragColor=vec4(mix(t.rgb*v_s*u_tint.rgb,u_fog,f),1.0);}';


  function perspective(fovy, aspect, near, far) {
    var f = 1 / Math.tan(fovy / 2), nf = 1 / (near - far);
    return new Float32Array([f / aspect, 0, 0, 0, 0, f, 0, 0, 0, 0, (far + near) * nf, -1, 0, 0, 2 * far * near * nf, 0]);
  }
  function lookAt(eye, center, up) {
    var zx = eye[0] - center[0], zy = eye[1] - center[1], zz = eye[2] - center[2];
    var l = Math.hypot(zx, zy, zz) || 1; zx /= l; zy /= l; zz /= l;
    var xx = up[1] * zz - up[2] * zy, xy = up[2] * zx - up[0] * zz, xz = up[0] * zy - up[1] * zx;
    l = Math.hypot(xx, xy, xz) || 1; xx /= l; xy /= l; xz /= l;
    var yx = zy * xz - zz * xy, yy = zz * xx - zx * xz, yz = zx * xy - zy * xx;
    return new Float32Array([
      xx, yx, zx, 0, xy, yy, zy, 0, xz, yz, zz, 0,
      -(xx * eye[0] + xy * eye[1] + xz * eye[2]),
      -(yx * eye[0] + yy * eye[1] + yz * eye[2]),
      -(zx * eye[0] + zy * eye[1] + zz * eye[2]), 1,
    ]);
  }
  // WebGL 列主序：out[c*4+r] = sum_k a[k*4+r] * b[c*4+k]
  function mul(a, b) {
    var o = new Float32Array(16);
    for (var c = 0; c < 4; c++) for (var r = 0; r < 4; r++) {
      o[c * 4 + r] = a[r] * b[c * 4] + a[4 + r] * b[c * 4 + 1] + a[8 + r] * b[c * 4 + 2] + a[12 + r] * b[c * 4 + 3];
    }
    return o;
  }

  function hex2rgb(h) {
    return [parseInt(h.slice(1, 3), 16), parseInt(h.slice(3, 5), 16), parseInt(h.slice(5, 7), 16)];
  }

  // 3×5 像素字模：每行一个八进制位（左=4 中=2 右=1），自上而下拼成 5 位字符串。
  // 没有贴图字体也能把块哈希刻进砖里——世界里的字必须是像素，不是 DOM。
  var FONT35 = {
    '0': '25652', '1': '26227', '2': '71247', '3': '71317', '4': '55711',
    '5': '74717', '6': '74757', '7': '71244', '8': '55755', '9': '75717',
    'A': '25575', 'B': '65656', 'C': '34443', 'D': '65556', 'E': '74747', 'F': '74744',
    'Q': '25523', 'L': '44447', 'O': '25552', 'R': '65655', 'X': '55255',
    '|': '22222', '.': '00002', '+': '02720', '-': '00700', '#': '57575', ' ': '00000',
  };
  /** 文本里没有的字模要显式报出来，不许悄悄吞字（数字雨少一位就是假象） */
  function missingGlyphs(text) {
    var out = [];
    for (var i = 0; i < text.length; i++) {
      var ch = text[i].toUpperCase();
      if (FONT35[ch] === undefined && out.indexOf(ch) < 0) out.push(ch);
    }
    return out;
  }

  // 出块显形层的节奏：砖 2.4 秒落进矿格，符文雨 5.2 秒流完；同屏最多 8 条，写满就挤掉最老的
  var FX_MINT_MS = 2400, FX_RAIN_MS = 5200, FX_CAP = 8, FX_PX = 0.075;
  // 在场邻居最多画这么多个人：铸块砖与数字雨共用同一条缓冲，人不能把动画挤出去
  var POS_DRAW_MAX = 16;

  function World3D(canvas, hooks) {
    var gl = canvas.getContext('webgl', { antialias: false, alpha: false });
    if (!gl) { hooks.onUnsupported && hooks.onUnsupported(); return null; }
    // 地形已经不是纯色块了：没有地形图集就等于没有世界。宁可当场报错，也不要
    // 让人对着一个空天空以为链坏了。
    if (!TIL) throw new Error('world3d: 地形图集未加载（terrain-tex.js 必须排在 world3d.js 之前）');

    var prog = gl.createProgram(), progT = gl.createProgram();
    function shader(prog, type, src) {
      var s = gl.createShader(type);
      gl.shaderSource(s, src); gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
      gl.attachShader(prog, s); return s;
    }
    function link(prog, vs, fs) {
      shader(prog, gl.VERTEX_SHADER, vs); shader(prog, gl.FRAGMENT_SHADER, fs);
      gl.linkProgram(prog);
      if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog));
    }
    link(prog, VS, FS); link(progT, VS2, FS2);
    gl.useProgram(prog);
    var A_P = gl.getAttribLocation(prog, 'a_p'), A_C = gl.getAttribLocation(prog, 'a_c');
    var AT_P = gl.getAttribLocation(progT, 'a_p'), AT_UV = gl.getAttribLocation(progT, 'a_uv'), AT_S = gl.getAttribLocation(progT, 'a_s');
    var U = {
      vp: gl.getUniformLocation(prog, 'u_vp'), tint: gl.getUniformLocation(prog, 'u_tint'),
      eye: gl.getUniformLocation(prog, 'u_eye'), fog: gl.getUniformLocation(prog, 'u_fog'),
      fogr: gl.getUniformLocation(prog, 'u_fog_r'),
    };
    var UT = {
      vp: gl.getUniformLocation(progT, 'u_vp'), tint: gl.getUniformLocation(progT, 'u_tint'),
      eye: gl.getUniformLocation(progT, 'u_eye'), fog: gl.getUniformLocation(progT, 'u_fog'),
      fogr: gl.getUniformLocation(progT, 'u_fog_r'), tex: gl.getUniformLocation(progT, 'u_tex'),
    };
    gl.enable(gl.DEPTH_TEST); gl.enable(gl.CULL_FACE);
    gl.clearColor(SKY[0], SKY[1], SKY[2], 1);
    gl.uniform3f(U.fog, SKY[0], SKY[1], SKY[2]);
    gl.uniform2f(U.fogr, FOG_NEAR, FOG_FAR);

    // 像素图集：NEAREST + CLAMP，放大到多大都是硬边方块画
    var atlasTex = gl.createTexture();
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, atlasTex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    if (global.QLSprites) {
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, global.QLSprites.atlas());
    } else {
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 1, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array([255, 255, 255, 255]));
    }

    var st = {
      heights: null, blocks: null, entities: [], byCell: {}, address: '', entSig: '',
      peers: [], wantShot: null,
      slot: 0, slots: 16, colors: [], maxLayer: MAX_LAYER,
      x: 128.5, z: 128.5, y: 30, yaw: 0.6, pitch: -0.45,
      keys: {}, dirty: true, regionKey: '', target: null, locked: false, drag: false,
      height: 0, heightAt: performance.now(), day: null, lastYaw: 1e9, lastAnim: 0, mesh: 0,
      bob: 0, bobPhase: 0, stepMark: 0, parts: [], entAt: 0, speed: 0, punch: 0,
      // 真实物理要的是速度状态，不是"按住键就位移"：vy 是竖直速度（米/秒）
      vx: 0, vz: 0, vy: 0, grounded: false, inWater: false,
      // 资源层（QL-RES-v1）：长在地上的树/石/花，采伐记录才进光尘账本
      res: null, resMap: null, resSig: '',
      // 挖地账本（v5）：cellKey → 已挖掉的层数。挖出来的坑是"少了一层"的同一片地形，
      // 不是另画一份几何——所以 level()/solid()/support() 读它，坑壁由地形自己长出来。
      dugMap: {}, dugSig: '',
      bufG: gl.createBuffer(), nG: 0,
      bufE: gl.createBuffer(), nE: 0, bufH: gl.createBuffer(), nH: 0,      bufP: gl.createBuffer(), nP: 0, bufS: gl.createBuffer(), nS: 0, bufStar: gl.createBuffer(), nStar: 0,
      // 资源层单独一条缓冲：采伐后只重铺这一层，不动 16 种签名的实体网格
      bufR: gl.createBuffer(), nR: 0,
      // 铸块砖 + QLOR 数字雨：只有这一层自己的缓冲。出块的动画不该重铺 64×64 地形。
      bufF: gl.createBuffer(), nF: 0, fx: [],
      // 地形贴图缓冲：容量由这一帧的真实面数决定（见 countQuads）。固定"每格 16 面"
      // 的预算在十层深的坑角会被用完，tquad 停笔的后果是坑壁缺一块——正是玩家说的"洞里"。
      // 这里按需分配、只增不减，平地仍只有每格 1~5 面，深坑才付它该付的内存。
      arrG: new Float32Array(REGION * REGION * 4 * 36),
      arrE: new Float32Array(2048 * 36 * 6),
      // 资源格按 64×64 区域全铺满留余量：树 432 浮点/格，写满即停笔（与 quad 同纪律）
      arrR: new Float32Array(REGION * REGION * 160),
      arrH: new Float32Array(6 * 24 * 6),
      arrP: new Float32Array(512 * 6),
      arrS: new Float32Array(2 * 6 * 6),
      arrStar: new Float32Array(140 * 6),
      // 一个 FX 条目最多 12 行 × 24 字 × 15 像素，每像素 6 顶点 × 6 浮点
      arrF: new Float32Array(9000 * 36),
      fps: 0, frames: 0, fpsMark: 0,
    };

    // —— 只读的地形/体素查询（全部来自 QLWorld.render() 那一趟循环的产物）
    /** 原始地表层：高度场 >> 3。它只由创世种子决定，挖多少层都不会变——挖坑的深度基准。 */
    function surf(cx, cz) {
      if (cx < 0 || cz < 0 || cx >= W || cz >= H) return -1;
      return st.heights[cz * W + cx] >> LEVEL_SHIFT;
    }
    function removedAt(cx, cz) {
      var r = st.dugMap[cellKey(cx, cz)];
      return r || 0;
    }
    /** 这一列**现在**露出来的顶面层：原始地表被挖掉几层就降几层，硬底见 DUG_FLOOR */
    function level(cx, cz) {
      var s = surf(cx, cz);
      if (s < 0) return -1;
      var r = removedAt(cx, cz);
      if (!r) return s;
      var top = s - r;
      return top < DUG_FLOOR ? (s < DUG_FLOOR ? s : DUG_FLOOR) : top;
    }
    function blk(cx, cz) { return st.blocks[cz * W + cx]; }
    function cellKey(cx, cz) { return cx * H + cz; }
    function inColumn(cx, cz) { return st.byCell[cellKey(cx, cz)] || []; }
    function blockAt(cx, cz, layer) {
      var list = inColumn(cx, cz);
      for (var i = 0; i < list.length; i++) if (list[i].layer === layer) return list[i];
      return null;
    }
    /** 该格该层长的资源（树干/石头/花都算地表那一层）：被采走的不再命中。
     *  被挖过的地表上不再有地表资源——树根/花畦跟着那一层土一起没了。 */
    function resAt(cx, cy, cz) {
      if (!st.resMap) return null;
      var n = st.resMap[cellKey(cx, cz)];
      if (!n || n.taken) return null;
      if (removedAt(cx, cz)) return null;
      return cy === level(cx, cz) ? n : null;
    }
    /** 该列最高实心顶面：地形顶面与所有活方块取最大——自己叠的塔，自己踩得住 */
    function support(cx, cz) {
      var top = level(cx, cz) + 1;
      var list = inColumn(cx, cz);
      for (var i = 0; i < list.length; i++) {
        var t = list[i].layer + 1;
        if (t > top) top = t;
      }
      return top;
    }
    /** 体素是否实心：地形（含低于顶面的一切）或该格该层的活方块 */
    function solid(cx, cy, cz) {
      if (cy < 0 || cx < 0 || cz < 0 || cx >= W || cz >= H) return true;
      if (cy <= level(cx, cz)) return true;
      return !!blockAt(cx, cz, cy);
    }

    // —— 地形贴图选择：地表按生态，地下按"离原始地表多深"的同一个三带划分 ——
    // 挖坑之后露出来的顶面/坑壁因此自动换成泥土、石、矿：你挖多深，世界就变多深，
    // 而不是在草皮下面贴一张"土色的皮"。深度用原始地表（surf）算，不是用当前顶面。
    function deepTile(cx, cz, depth) {
      if (depth <= DUG_SOIL_DEPTH) return TIL.DIRT;
      if (depth <= DUG_STONE_DEPTH) return TIL.STONE;
      // 矿带：石头里按格子哈希撒矿脉（共识只认"这一带是矿"，具体哪种是外观）
      var h = (Math.imul(cx + 17, 73856093) ^ Math.imul(cz + 31, 19349663) ^ Math.imul(depth + 7, 83492791)) >>> 0;
      var r = h % 12;
      return r < 2 ? TIL.ORE_GOLD : r < 4 ? TIL.ORE_IRON : r < 5 ? TIL.ORE_CRYSTAL : TIL.DEEP;
    }
    function topTile(cx, cz, kind, depth) {
      if (kind === 0) return TIL.WATER;                       // 水柱：挖不动，永远是水面
      if (depth === 0) {
        return kind === 2 ? TIL.SNOW_TOP
          : (kind === 1 || kind === 4) ? TIL.SAND
            : TIL.GRASS_TOP;
      }
      return deepTile(cx, cz, depth);
    }
    function sideTile(cx, cz, kind, depth) {
      if (kind === 0) return TIL.WATER;
      if (depth === 0) {
        if (kind === 2) return TIL.SNOW_SIDE;
        if (kind === 1 || kind === 4) return TIL.SAND;
        return TIL.GRASS_SIDE;
      }
      return deepTile(cx, cz, depth);
    }

    // —— 顶点写入（pos3 + color3 交错）；越界立即停笔，绝不越写缓冲区
    var vp = 0, arr = null;
    function vert(x, y, z, c, shade) {
      if (vp + 6 > arr.length) return;
      arr[vp++] = x; arr[vp++] = y; arr[vp++] = z;
      arr[vp++] = c[0] / 255 * shade; arr[vp++] = c[1] / 255 * shade; arr[vp++] = c[2] / 255 * shade;
    }
    /**
     * 一个面 = 4 点 + 它的**朝向外法线**。
     * 手写顶点顺序去凑 CCW 是这类渲染最常见的穿帮来源（本项目曾经地面全被背面剔除，
     * 于是"世界看起来是散在天空里的石壁"）。这里改成：算出几何法线，与给定的朝向
     * 点乘，符号不对就自动翻转顶点序——写代码的人只需要说清"这面朝哪边"。
     */
    function quad(p, c, shade, n) {
      if (vp + 36 > arr.length) return;
      var ax = p[3] - p[0], ay = p[4] - p[1], az = p[5] - p[2];
      var bx = p[6] - p[0], by = p[7] - p[1], bz = p[8] - p[2];
      var dot = (ay * bz - az * by) * n[0] + (az * bx - ax * bz) * n[1] + (ax * by - ay * bx) * n[2];
      var i1 = dot < 0 ? 9 : 3, i3 = dot < 0 ? 3 : 9;
      vert(p[0], p[1], p[2], c, shade); vert(p[i1], p[i1 + 1], p[i1 + 2], c, shade); vert(p[6], p[7], p[8], c, shade);
      vert(p[0], p[1], p[2], c, shade); vert(p[6], p[7], p[8], c, shade); vert(p[i3], p[i3 + 1], p[i3 + 2], c, shade);
    }
    function cube(x0, y0, z0, x1, y1, z1, c) {
      quad([x0, y1, z0, x1, y1, z0, x1, y1, z1, x0, y1, z1], c, 1.0, N_UP);      // 顶
      quad([x0, y0, z1, x1, y0, z1, x1, y0, z0, x0, y0, z0], c, 0.5, N_DOWN);    // 底
      quad([x0, y0, z0, x1, y0, z0, x1, y1, z0, x0, y1, z0], c, 0.8, N_N);       // 北 -z
      quad([x1, y0, z1, x0, y0, z1, x0, y1, z1, x1, y1, z1], c, 0.8, N_S);       // 南 +z
      quad([x1, y0, z0, x1, y0, z1, x1, y1, z1, x1, y1, z0], c, 0.65, N_E);      // 东 +x
      quad([x0, y0, z1, x0, y0, z0, x0, y1, z0, x0, y1, z1], c, 0.65, N_W);      // 西 -x
    }

    /** 一格某一面的四个角：k = 0 北 / 1 南 / 2 东 / 3 西，从 ya 到 yb。地形侧壁按"草皮 + 土壁"分两段画。 */
    function sface(cx, cz, k, ya, yb) {
      return k === 0 ? [cx, ya, cz, cx + 1, ya, cz, cx + 1, yb, cz, cx, yb, cz]
        : k === 1 ? [cx + 1, ya, cz + 1, cx, ya, cz + 1, cx, yb, cz + 1, cx + 1, yb, cz + 1]
          : k === 2 ? [cx + 1, ya, cz + 1, cx + 1, ya, cz, cx + 1, yb, cz, cx + 1, yb, cz + 1]
            : [cx, ya, cz, cx, ya, cz + 1, cx, yb, cz + 1, cx, yb, cz];
    }
    // —— 贴图顶点写入：pos3 + uv2 + shade1（stride 24，和颜色管线同宽）
    var COLS = 8, ROWS = 8, UV_E = 0.03;
    var tarr = null, tvp = 0;
    function tvert(x, y, z, u, v, s) {
      if (tvp + 6 > tarr.length) return;
      tarr[tvp++] = x; tarr[tvp++] = y; tarr[tvp++] = z;
      tarr[tvp++] = u; tarr[tvp++] = v; tarr[tvp++] = s;
    }
    function uvOf(tile, u, v) {
      var col = tile % COLS, row = (tile / COLS) | 0;
      return [(col + UV_E + u * (1 - 2 * UV_E)) / COLS, (row + UV_E + v * (1 - 2 * UV_E)) / ROWS];
    }
    /** 贴图四边形：p 四点顺序与 uv 一一对应，绕向按几何法线自动翻（同 quad） */
    function tquad(p, tile, shade, n) {
      if (tvp + 36 > tarr.length) return;
      var ax = p[3] - p[0], ay = p[4] - p[1], az = p[5] - p[2];
      var bx = p[6] - p[0], by = p[7] - p[1], bz = p[8] - p[2];
      var dot = (ay * bz - az * by) * n[0] + (az * bx - ax * bz) * n[1] + (ax * by - ay * bx) * n[2];
      var flip = dot < 0;
      var uv = [[0, 1], [1, 1], [1, 0], [0, 0]];
      function put(i) {
        var q = uvOf(tile, uv[i][0], uv[i][1]);
        tvert(p[i * 3], p[i * 3 + 1], p[i * 3 + 2], q[0], q[1], shade);
      }
      var i1 = flip ? 3 : 1, i3 = flip ? 1 : 3;
      put(0); put(i1); put(2); put(0); put(2); put(i3);
    }
    function tcube(x0, y0, z0, x1, y1, z1, tile) {
      tquad([x0, y1, z0, x1, y1, z0, x1, y1, z1, x0, y1, z1], tile, 1.0, N_UP);
      tquad([x0, y0, z1, x1, y0, z1, x1, y0, z0, x0, y0, z0], tile, 0.5, N_DOWN);
      tquad([x0, y0, z0, x1, y0, z0, x1, y1, z0, x0, y1, z0], tile, 0.8, N_N);
      tquad([x1, y0, z1, x0, y0, z1, x0, y1, z1, x1, y1, z1], tile, 0.8, N_S);
      tquad([x1, y0, z0, x1, y0, z1, x1, y1, z1, x1, y1, z0], tile, 0.65, N_E);
      tquad([x0, y0, z1, x0, y0, z0, x0, y1, z0, x0, y1, z1], tile, 0.65, N_W);
    }
    /** 十字交叉面（花、活物）：两个互相垂直的竖面，任何角度都看得到 */
    function cross(cx, y0, cz, w, h, tile) {
      var a = cx + 0.5 - w / 2, b = cz + 0.5 - w / 2, c = cx + 0.5 + w / 2, d = cz + 0.5 + w / 2;
      tquad([a, y0, b, c, y0, d, c, y0 + h, d, a, y0 + h, b], tile, 1.0, N_S);
      tquad([a, y0, d, c, y0, b, c, y0 + h, b, a, y0 + h, d], tile, 0.9, N_E);
    }

    // 天然树的材质：与实体树共用图集里的树干/树叶两格（同一张画，不另起一套）
    var TRUNK_TILE = 16, LEAF_TILE = 17;

    /** 这一片区域要写多少个面：顶面 1 + 每个露出侧壁的层 + 天然树的 3 个盒子。
     *  先数后写，缓冲永远刚好装得下；深坑不会因为"预算用完"而缺半面墙。 */
    function countQuads(x0, z0) {
      var n = 0;
      for (var dz = 0; dz < REGION; dz++) {
        for (var dx = 0; dx < REGION; dx++) {
          var cx = x0 + dx, cz = z0 + dz;
          if (cx >= W || cz >= H) continue;
          var lv = level(cx, cz), b = blk(cx, cz);
          n += 1;
          for (var k = 0; k < 4; k++) {
            var nx = cx + (k === 2 ? 1 : k === 3 ? -1 : 0);
            var nz = cz + (k === 0 ? -1 : k === 1 ? 1 : 0);
            var nl = level(nx, nz);
            if (nl < lv) n += lv - nl;
          }
          if ((b & 8) && !(b & 16) && lv === surf(cx, cz)) n += 3;
        }
      }
      return n;
    }

    function buildTerrain(x0, z0) {
      st.mesh++;                       // 铺一次 64×64 记一次：出块动画若碰了这个数，回归闸当场就能看见
      var need = countQuads(x0, z0) * 36;
      if (st.arrG.length < need) st.arrG = new Float32Array(need + REGION * 36);
      tarr = st.arrG; tvp = 0;
      for (var dz = 0; dz < REGION; dz++) {
        for (var dx = 0; dx < REGION; dx++) {
          var cx = x0 + dx, cz = z0 + dz;
          if (cx >= W || cz >= H) continue;
          var sv = surf(cx, cz), lv = level(cx, cz), b = blk(cx, cz);
          var gold = (b & 16) !== 0, kind = b & 7;
          var removed = sv - lv;                 // 这一列被挖掉几层 = 现在顶面的"地下深度"
          var y0 = lv + 1;
          // 顶面：草地/沙/雪/水面；挖过的顶面直接换成泥土/石/矿，一眼看出铲到了哪一带
          tquad([cx, y0, cz, cx + 1, y0, cz, cx + 1, y0, cz + 1, cx, y0, cz + 1],
            gold ? TIL.ERA_GOLD : topTile(cx, cz, kind, removed), 1.0, N_UP);
          // 侧壁：邻格顶面更低才补，**逐层一个面**——草皮、泥土、石、矿各自贴自己的图，
          // 悬崖与挖坑的坑壁因此自动长成一层一层的岩性剖面。只从邻格顶面拉起到本格顶面，
          // 一直画到 y=0 会让区域边界看起来像一排插在天空里的石柱。
          for (var k = 0; k < 4; k++) {
            var nx = cx + (k === 2 ? 1 : k === 3 ? -1 : 0);
            var nz = cz + (k === 0 ? -1 : k === 1 ? 1 : 0);
            var nl = level(nx, nz);
            if (nl >= lv) continue;
            var sh = k < 2 ? 0.8 : 0.65;
            var nrm = k === 0 ? N_N : k === 1 ? N_S : k === 2 ? N_E : N_W;
            for (var L = nl + 1; L <= lv; L++) {
              tquad(sface(cx, cz, k, L, L + 1),
                gold ? TIL.ERA_GOLD : sideTile(cx, cz, kind, sv - L), sh, nrm);
            }
          }
          // 天然树：与 2D 地图上同一个深色像点（同一趟分类，绝不两套判定）
          // 树干必须是落到地面的实心柱——只画一片 0.24 见方的顶面会让树冠悬在半空，
          // 看上去就是"世界里到处是洞"。挖过的地表不再长树：树根跟着那一层土一起没了。
          if ((b & 8) && !gold && !removed) {
            tcube(cx + 0.34, y0, cz + 0.34, cx + 0.66, y0 + 2.35, cz + 0.66, TRUNK_TILE);
            tcube(cx + 0.06, y0 + 2.2, cz + 0.06, cx + 0.94, y0 + 3.35, cz + 0.94, LEAF_TILE);
          }
        }
      }
      st.nG = tvp / 6;
      gl.bindBuffer(gl.ARRAY_BUFFER, st.bufG);
      gl.bufferData(gl.ARRAY_BUFFER, st.arrG.subarray(0, tvp), gl.DYNAMIC_DRAW);
    }

    /** 签过名的方块：按 layer 画像素贴图实体（叠塔因此成立），只画当前区域内的。
     *
     *  16 种物件不是 16 个换色立方体：
     *  - 石头/木屋/灯塔/桥/篱笆/篝火 = 贴图立方体拼出来的"建筑"；
     *  - 树 = 树干 + 树叶；花 = 十字面；鹿/鸟/鱼/蝴蝶/兔/羊/旅人/狼 = 十字面活物。
     *  全部共用 sprites.js 的 16×16 图集，2D 地图、物品栏、世界里看到的是同一张画。
     */
    function buildEntities(x0, z0) {
      tarr = st.arrE; tvp = 0;
      var now = performance.now();
      for (var i = 0; i < st.entities.length; i++) {
        var e = st.entities[i];
        if (e.released) continue; // 断租不删除，但也不占格、不显形
        if (e.x < x0 || e.x >= x0 + REGION || e.y < z0 || e.y >= z0 + REGION) continue;
        // 活物轻微呼吸；蝴蝶/鸟再高一点，像悬在草尖上
        var bob = e.kind >= 8 ? Math.sin(now * 0.0016 + (e.x * 7 + e.y * 13) % 63) * 0.045 : 0;
        var y = e.layer + 0.002 + bob;
        var x = e.x, z = e.y;
        switch (e.kind) {
          case 0: // 石头
            tcube(x + 0.03, y, z + 0.03, x + 0.97, e.layer + 1, z + 0.97, 0);
            break;
          case 1: // 橡树：树干 + 树冠（略高于一格，像长出来的树而不是插在地上的块）
            tcube(x + 0.37, y, z + 0.37, x + 0.63, y + 1.15, z + 0.63, 16);
            tcube(x + 0.08, y + 0.95, z + 0.08, x + 0.92, y + 1.62, z + 0.92, 17);
            break;
          case 2: // 花丛
            cross(x, y, z, 0.92, 0.95, 2);
            break;
          case 3: // 木屋：木板墙 + 瓦屋顶 + 烟囱
            tcube(x + 0.04, y, z + 0.04, x + 0.96, y + 0.62, z + 0.96, 18);
            tcube(x + 0.0, y + 0.62, z + 0.0, x + 1.0, y + 0.8, z + 1.0, 19);
            tcube(x + 0.72, y + 0.6, z + 0.18, x + 0.9, y + 0.98, z + 0.36, 0);
            break;
          case 4: // 木桥：桥面 + 四根桩 + 两道栏杆
            tcube(x + 0.02, y + 0.14, z + 0.1, x + 0.98, y + 0.26, z + 0.9, 18);
            var px = [0.08, 0.84], pz = [0.16, 0.76];
            for (var a = 0; a < 2; a++) for (var b = 0; b < 2; b++)
              tcube(x + px[a], y, z + pz[b], x + px[a] + 0.08, y + 0.62, z + pz[b] + 0.08, 16);
            tcube(x + 0.02, y + 0.52, z + 0.14, x + 0.98, y + 0.58, z + 0.2, 16);
            tcube(x + 0.02, y + 0.52, z + 0.8, x + 0.98, y + 0.58, z + 0.86, 16);
            break;
          case 5: // 灯塔：条纹塔身 + 灯室 + 顶
            tcube(x + 0.28, y, z + 0.28, x + 0.72, y + 1.38, z + 0.72, 5);
            tcube(x + 0.24, y + 1.38, z + 0.24, x + 0.76, y + 1.58, z + 0.76, 21);
            tcube(x + 0.26, y + 1.58, z + 0.26, x + 0.74, y + 1.72, z + 0.74, 19);
            break;
          case 6: // 篝火：交叉木柴 + 火苗
            tcube(x + 0.12, y + 0.02, z + 0.4, x + 0.88, y + 0.16, z + 0.6, 16);
            tcube(x + 0.4, y + 0.04, z + 0.12, x + 0.6, y + 0.18, z + 0.88, 16);
            cross(x, y + 0.08, z, 0.78, 0.82, 20);
            break;
          case 7: // 篱笆：三根柱子 + 两道横杆
            for (var f = 0; f < 3; f++)
              tcube(x + 0.12 + f * 0.32, y, z + 0.44, x + 0.24 + f * 0.32, y + 0.85, z + 0.56, 16);
            tcube(x + 0.06, y + 0.28, z + 0.44, x + 0.94, y + 0.36, z + 0.56, 16);
            tcube(x + 0.06, y + 0.6, z + 0.44, x + 0.94, y + 0.68, z + 0.56, 16);
            break;
          default: // 8..15 活物：鹿、鸟、鱼、蝴蝶、兔、绵羊、旅人、狼
            cross(x, y, z, 0.9, 0.95, e.kind);
            break;
        }
      }
      st.nE = tvp / 6;
      gl.bindBuffer(gl.ARRAY_BUFFER, st.bufE);
      gl.bufferData(gl.ARRAY_BUFFER, tarr.subarray(0, tvp), gl.DYNAMIC_DRAW);
    }

    /** 资源层（QL-RES-v1）：长在地表、采一次就消失的树/石/花。
     *
     *  可采树的抽样与装饰林互斥（见 resource.rs 的不变量测试），所以这里可以
     *  直接落完整几何，不会和 buildTerrain 的天然树叠在一起。
     */
    function buildResources(x0, z0) {
      if (!st.resMap) { st.nR = 0; return; }
      tarr = st.arrR; tvp = 0;
      for (var dz = 0; dz < REGION; dz++) {
        for (var dx = 0; dx < REGION; dx++) {
          var cx = x0 + dx, cz = z0 + dz;
          if (cx >= W || cz >= H) continue;
          var n = st.resMap[cellKey(cx, cz)];
          if (!n || n.taken) continue;
          if (removedAt(cx, cz)) continue;   // 被挖过的地表：资源跟着表层一起没了
          var y0 = level(cx, cz) + 1;
          switch (n.kind) {
            case 0: // 石头：半嵌在地里的岩块
              tcube(cx + 0.12, y0 - 0.45, cz + 0.12, cx + 0.88, y0 + 0.3, cz + 0.88, 0);
              break;
            case 1: // 橡树：树干 + 树冠
              tcube(cx + 0.37, y0, cz + 0.37, cx + 0.63, y0 + 1.15, cz + 0.63, 16);
              tcube(cx + 0.08, y0 + 0.95, cz + 0.08, cx + 0.92, y0 + 1.62, cz + 0.92, 17);
              break;
            default: // 花丛
              cross(cx, y0, cz, 0.92, 0.85, 2);
              break;
          }
        }
      }
      st.nR = tvp / 6;
      gl.bindBuffer(gl.ARRAY_BUFFER, st.bufR);
      gl.bufferData(gl.ARRAY_BUFFER, tarr.subarray(0, tvp), gl.DYNAMIC_DRAW);
    }


    /** 线框：白 = 正在看的实心体素，金 = 松手就落下去的空体素 */
    function wire(x0, y0, z0, x1, y1, z1, col) {
      var L = [[x0, y0, z0, x1, y0, z0], [x1, y0, z0, x1, y0, z1], [x1, y0, z1, x0, y0, z1], [x0, y0, z1, x0, y0, z0],
               [x0, y1, z0, x1, y1, z0], [x1, y1, z0, x1, y1, z1], [x1, y1, z1, x0, y1, z1], [x0, y1, z1, x0, y1, z0],
               [x0, y0, z0, x0, y1, z0], [x1, y0, z0, x1, y1, z0], [x1, y0, z1, x1, y1, z1], [x0, y0, z1, x0, y1, z1]];
      for (var i = 0; i < L.length; i++) {
        if (vp + 12 > arr.length) break; // 线框也只写满不写穿（与 vert/quad 同一纪律）
        var p = L[i];
        arr[vp++] = p[0]; arr[vp++] = p[1]; arr[vp++] = p[2];
        arr[vp++] = col[0]; arr[vp++] = col[1]; arr[vp++] = col[2];
        arr[vp++] = p[3]; arr[vp++] = p[4]; arr[vp++] = p[5];
        arr[vp++] = col[0]; arr[vp++] = col[1]; arr[vp++] = col[2];
      }
    }
    function buildHighlight() {
      arr = st.arrH; vp = 0;
      var t = st.target;
      if (t) {
        var e = 0.02;
        wire(t.cx - e, t.layer - e, t.cz - e, t.cx + 1 + e, t.layer + 1 + e, t.cz + 1 + e, [0.06, 0.07, 0.09]);
        if (t.place) wire(t.place.cx - e, t.place.layer - e, t.place.cz - e,
          t.place.cx + 1 + e, t.place.layer + 1 + e, t.place.cz + 1 + e, [0.98, 0.85, 0.32]);
      }
      // 刚被这块砖铸出来的矿格：金框亮一下再退场
      var e2 = 0.06, nowMs = performance.now();
      for (var i = st.fx.length - 1; i >= 0; i--) {
        var f = st.fx[i];
        if (f.type !== 'mint') continue;
        var g = Math.max(0, 1 - (nowMs - f.t0) / f.life);
        if (g <= 0.02) continue;
        wire(f.cx - e2, f.layer - e2, f.cz - e2, f.cx + 1 + e2, f.layer + 1 + e2, f.cz + 1 + e2,
          [0.98 * g, 0.85 * g, 0.32 * g]);
        break;
      }
      st.nH = vp / 6;
      gl.bindBuffer(gl.ARRAY_BUFFER, st.bufH);
      gl.bufferData(gl.ARRAY_BUFFER, st.arrH.subarray(0, vp), gl.DYNAMIC_DRAW);
    }

    function dir() {
      var cp = Math.cos(st.pitch);
      return [Math.sin(st.yaw) * cp, Math.sin(st.pitch), Math.cos(st.yaw) * cp];
    }
    function eye() { return [st.x, st.y + st.bob, st.z]; }

    /** 准星 → 逐体素步进：命中哪个体素、应当在哪个空体素落下方块 */
    function pick() {
      if (!st.heights) return null;
      var o = eye(), d = dir();
      var px = Math.floor(o[0]), py = Math.floor(o[1]), pz = Math.floor(o[2]);
      var t = 0;
      while (t < REACH) {
        t += 0.1;
        var x = o[0] + d[0] * t, y = o[1] + d[1] * t, z = o[2] + d[2] * t;
        var cx = Math.floor(x), cy = Math.floor(y), cz = Math.floor(z);
        if (cx < 0 || cz < 0 || cx >= W || cz >= H || cy < 0) return null;
        if (cx === px && cy === py && cz === pz) continue;
        if (solid(cx, cy, cz)) {
          // 地表那一层若长着资源、且没有玩家方块占格，准星优先落在资源上：
          // 左键 = 采它，而不是去拆脚下的地。
          var nd = resAt(cx, cy, cz);
          if (nd && !blockAt(cx, cz, cy)) {
            return { cx: cx, cz: cz, layer: cy, entity: null, node: nd, place: null };
          }
          // 只有"头所在的那一格"算被身体占用。脚下那格允许落块——块一落地，move()
          // 立刻把人抬到新块顶上，于是"低头、连点"就是 Minecraft 的原地起柱：
          // 一次点击一块，不卡跳跃帧。曾经连脚下也一起禁，结果平视/俯视的点击
          // 全都静默落空，整个编辑器看起来是死的。
          var insideMe = px === Math.floor(st.x) && pz === Math.floor(st.z) && py === Math.floor(st.y);
          var okPlace = !insideMe && px >= 0 && pz >= 0 && px < W && pz < H && py >= 0 && py < st.maxLayer &&
            !blockAt(px, pz, py);
          // 能不能挖：只有这一列露出来的顶面能挖（与 world.rs 的判据逐字一致）。
          // 侧壁上的旧层、水面、硬底都不行——点了也不该悄悄落进账本。
          var top = level(cx, cz);
          var diggable = !blockAt(cx, cz, cy) && cy === top && top >= DUG_FLOOR &&
            (blk(cx, cz) & 7) !== 0;
          return {
            cx: cx, cz: cz, layer: cy,
            entity: blockAt(cx, cz, cy),
            place: okPlace ? { cx: px, cz: pz, layer: py } : null,
            dig: diggable,
          };
        }
        px = cx; py = cy; pz = cz;
      }
      return null;
    }

    // —— 昼夜与天空（只用块高推时间，不读墙钟：同一高度在任何机器上都是同一个天色）——
    function clamp01(v) { return v < 0 ? 0 : v > 1 ? 1 : v; }
    function smooth(a, b, x) { var t = clamp01((x - a) / (b - a)); return t * t * (3 - 2 * t); }
    function mix3(a, b, t) { return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]; }
    function dayState(now) {
      var frac = Math.max(0, (now - st.heightAt) / 2000); // 2 秒一个块
      var phase = ((((st.height + frac) % DAY_BLOCKS) + DAY_BLOCKS) % DAY_BLOCKS) / DAY_BLOCKS;
      var ang = phase * Math.PI * 2;
      var sh = Math.sin(ang);
      var day = smooth(-0.1, 0.26, sh);
      var dusk = Math.exp(-Math.pow(sh / 0.17, 2));
      var sky = mix3(SKY_NIGHT, SKY_DAY, day);
      sky = mix3(sky, SKY_DUSK, dusk * 0.62);
      var tint = mix3(TINT_NIGHT, TINT_DAY, day);
      tint = mix3(tint, TINT_DUSK, dusk * 0.44);
      return {
        day: day, dusk: dusk, sky: sky, tint: tint,
        sun: [Math.cos(ang), Math.sin(ang), 0.28], // 归一化在 draw 里做
      };
    }

    // 星空：固定 140 个方向（简单 LCG，实现间可复现）
    var STARS = (function () {
      var out = [], s = 20261026;
      for (var i = 0; i < 140; i++) {
        s = (s * 1664525 + 1013904223) >>> 0;
        var u = s / 4294967296;
        s = (s * 1664525 + 1013904223) >>> 0;
        var v = s / 4294967296;
        s = (s * 1664525 + 1013904223) >>> 0;
        var w = s / 4294967296;
        var th = u * Math.PI * 2, y = v * 1.6 - 0.2, r = Math.sqrt(Math.max(0, 1 - y * y));
        out.push([Math.cos(th) * r, y, Math.sin(th) * r, 0.45 + w * 0.55]);
      }
      return out;
    })();

    function skyBodies(ds, e0, d) {
      var rx = d[2], rz = -d[0], rl = Math.hypot(rx, rz) || 1; rx /= rl; rz /= rl;
      var ux = d[1] * rz, uy = d[2] * rx - d[0] * rz, uz = -d[1] * rx;
      var ul = Math.hypot(ux, uy, uz) || 1; ux /= ul; uy /= ul; uz /= ul;
      var sl = Math.hypot(ds.sun[0], ds.sun[1], ds.sun[2]) || 1;
      var sx = ds.sun[0] / sl, sy = ds.sun[1] / sl, sz = ds.sun[2] / sl;

      // 直接写顶点，省掉中间数组
      arr = st.arrS; vp = 0;
      function tri(ax, ay, az, bx, by, bz, cx2, cy2, cz2, col) {
        vert(ax, ay, az, col, 1); vert(bx, by, bz, col, 1); vert(cx2, cy2, cz2, col, 1);
      }
      function quadS(cx, cy, cz, size, col) {
        var x0 = cx - rx * size - ux * size, y0 = cy - uy * size, z0 = cz - rz * size - uz * size;
        var x1 = cx + rx * size - ux * size, y1 = cy - uy * size, z1 = cz + rz * size - uz * size;
        var x2 = cx + rx * size + ux * size, y2 = cy + uy * size, z2 = cz + rz * size + uz * size;
        var x3 = cx - rx * size + ux * size, y3 = cy + uy * size, z3 = cz - rz * size + uz * size;
        tri(x0, y0, z0, x1, y1, z1, x2, y2, z2, col);
        tri(x0, y0, z0, x2, y2, z2, x3, y3, z3, col);
      }
      // 太阳在白天可见，月亮在夜里可见；都带一点"像素方块"的硬边
      if (ds.day > 0.08) quadS(e0[0] + sx * 180, e0[1] + sy * 180, e0[2] + sz * 180, 9, [255, 243, 186]);
      if (ds.day < 0.92) quadS(e0[0] - sx * 178, e0[1] - sy * 178, e0[2] - sz * 178, 6, [226, 233, 255]);
      var nS = vp / 6;
      gl.bindBuffer(gl.ARRAY_BUFFER, st.bufS);
      gl.bufferData(gl.ARRAY_BUFFER, st.arrS.subarray(0, vp), gl.DYNAMIC_DRAW);
      return nS;
    }

    // 星空点：只在夜里出现，和太阳月亮同一套颜色管线
    function starPoints(e0, ds) {
      if (ds.day > 0.72) return 0;
      var arr2 = st.arrStar, n = 0, bright = 1 - ds.day;
      for (var i = 0; i < STARS.length; i++) {
        var s = STARS[i];
        arr2[n++] = e0[0] + s[0] * 176; arr2[n++] = e0[1] + s[1] * 176; arr2[n++] = e0[2] + s[2] * 176;
        var c = s[3] * bright * 0.9;
        arr2[n++] = c; arr2[n++] = c; arr2[n++] = c * 1.05;
      }
      gl.bindBuffer(gl.ARRAY_BUFFER, st.bufStar);
      gl.bufferData(gl.ARRAY_BUFFER, arr2.subarray(0, n), gl.DYNAMIC_DRAW);
      return n / 6;
    }

    // —— 粒子：放置/拆除时爆一小把同色像素点（MC 的"手感"有一半来自这个）
    function burst(cx, layer, cz, kind) {
      var c = global.QLSprites ? global.QLSprites.avgColor(kind) : [200, 200, 200];
      for (var i = 0; i < 16; i++) {
        var a = Math.random() * Math.PI * 2, sp = 0.6 + Math.random() * 1.8;
        st.parts.push({
          x: cx + 0.5 + (Math.random() - 0.5) * 0.4,
          y: layer + 0.35 + Math.random() * 0.3,
          z: cz + 0.5 + (Math.random() - 0.5) * 0.4,
          vx: Math.cos(a) * sp * 0.5, vy: 1.2 + Math.random() * 2.2, vz: Math.sin(a) * sp * 0.5,
          life: 0.55 + Math.random() * 0.35, rgb: c,
        });
      }
      if (st.parts.length > 400) st.parts.splice(0, st.parts.length - 400);
    }
    function stepParts(dt) {
      if (!st.parts.length) { st.nP = 0; return; }
      for (var i = st.parts.length - 1; i >= 0; i--) {
        var p = st.parts[i];
        p.life -= dt;
        if (p.life <= 0) { st.parts.splice(i, 1); continue; }
        p.vy -= 9.5 * dt;
        p.x += p.vx * dt; p.y += p.vy * dt; p.z += p.vz * dt;
        if (p.y < (st.heights ? level(p.x | 0, p.z | 0) + 1 : 0)) { p.y = level(p.x | 0, p.z | 0) + 1.01; p.vy = -p.vy * 0.3; }
      }
      var a = st.arrP, n = 0;
      for (var j = 0; j < st.parts.length; j++) {
        var q = st.parts[j];
        a[n++] = q.x; a[n++] = q.y; a[n++] = q.z;
        a[n++] = q.rgb[0] / 255; a[n++] = q.rgb[1] / 255; a[n++] = q.rgb[2] / 255;
      }
      gl.bindBuffer(gl.ARRAY_BUFFER, st.bufP);
      gl.bufferData(gl.ARRAY_BUFFER, a.subarray(0, n), gl.DYNAMIC_DRAW);
      st.nP = n / 6;
    }

    // —— §9.16 出块显形：铸块砖（刻块哈希 / VRF / 滚动发行量）与矿格上方的 QLOR 数字雨
    // 只写 st.arrF 这一条缓冲：动画绝不该把 64×64 的区域网格重新铺一遍。

    function fxPush(e) {
      st.fx.push(e);
      if (st.fx.length > FX_CAP) st.fx.splice(0, st.fx.length - FX_CAP);
    }
    function fxPixel(cx, cy, cz, rx, ry, rz, ux, uy, uz, nv, c, k) {
      var hp = FX_PX * 0.5;
      function pt(sx, sy) {
        return [cx + rx * sx + ux * sy, cy + ry * sx + uy * sy, cz + rz * sx + uz * sy];
      }
      var a = pt(-hp, -hp), b = pt(hp, -hp), g = pt(hp, hp), i2 = pt(-hp, hp);
      quad([a[0], a[1], a[2], b[0], b[1], b[2], g[0], g[1], g[2], i2[0], i2[1], i2[2]], c, k, nv);
    }
    /** 一行像素字：以 (x,y,z) 为中心，沿相机的 right/up 铺开（永远正对眼睛） */
    function fxText(x, y, z, rx, ry, rz, ux, uy, uz, nv, text, c, k) {
      var s = String(text).toUpperCase();
      var ox = -(((s.length * 4 - 1) - 1) / 2) * FX_PX, oy = ((5 - 1) / 2) * FX_PX;
      for (var ci = 0; ci < s.length; ci++) {
        var rows = FONT35[s[ci]];
        if (!rows) continue; // 缺字模就不画，绝不拿别的字顶替（G3 用 missingGlyphs 兜住）
        for (var r0 = 0; r0 < 5; r0++) {
          var bits = rows.charCodeAt(r0) - 48;
          for (var q = 0; q < 3; q++) {
            if (((bits >> (2 - q)) & 1) === 0) continue;
            var sx = ox + (ci * 4 + q) * FX_PX, sy = oy - r0 * FX_PX;
            fxPixel(x + rx * sx + ux * sy, y + ry * sx + uy * sy, z + rz * sx + uz * sy,
              rx, ry, rz, ux, uy, uz, nv, c, k);
          }
        }
      }
    }

    function drawMint(f, age, rx, ry, rz, ux, uy, uz, nv) {
      var k = age / f.life;
      var drop = 1 - smooth(0, 1, Math.min(1, k / 0.62));   // 前 62% 从半空压下来
      var y = f.layer + 1.5 + drop * 6.5;
      var fade = k < 0.7 ? 1 : Math.max(0, 1 - (k - 0.7) / 0.3);
      var c = f.reward > 0 ? ERA_GOLD : [150, 158, 170];
      cube(f.cx + 0.16, y - 0.34, f.cz + 0.16, f.cx + 0.84, y + 0.34, f.cz + 0.84, c);
      var roll = Math.min(1, k / 0.5);                      // 发行量从 0 滚到真值
      var amt = ((f.reward / 1e9) * roll).toFixed(3);
      var cx = f.cx + 0.5, cz = f.cz + 0.5;
      fxText(cx, y + 1.35, cz, rx, ry, rz, ux, uy, uz, nv, '#' + f.height + ' ' + f.hash, c, fade);
      fxText(cx, y + 0.9, cz, rx, ry, rz, ux, uy, uz, nv, 'VRF ' + f.vrf, [214, 226, 240], fade);
      fxText(cx, y + 0.45, cz, rx, ry, rz, ux, uy, uz, nv, '+' + amt + ' QLX', f.reward > 0 ? [255, 246, 190] : [176, 184, 198], fade);
    }

    function drawRain(f, age, rx, ry, rz, ux, uy, uz, nv) {
      var k = age / f.life;
      var rise = (age / 1000) * 1.15;
      for (var i = 0; i < f.rows; i++) {
        var y = f.base + i * 0.62 + rise;
        var away = Math.min(1, (y - f.base) / 8);
        var kk = (1 - k) * (1 - away * 0.7);
        if (kk <= 0.03) continue;
        fxText(f.cx + 0.5, y, f.cz + 0.5, rx, ry, rz, ux, uy, uz, nv, f.text, [104, 240, 188], kk);
      }
    }

    /** 邻居在哪：在场广播画出来的人形。
     *  与铸块砖同一条例——只写 st.arrF，每帧重算，绝不为"有人在场"重铺 64×64 区域网格。
     *  坐标来自对端自报，所以它只表示"他声称自己站在这儿"，不代表任何归属。 */
    function drawPeer(p, rx, ry, rz, ux, uy, uz, nv) {
      if (Math.hypot(p.x - st.x, p.z - st.z) > FOG_FAR) return;   // 雾外的标记不如不画
      var feet = p.y, body = [150, 200, 255], head = [222, 236, 255];
      cube(p.x - 0.3, feet, p.z - 0.3, p.x + 0.3, feet + 1.15, p.z + 0.3, body);
      cube(p.x - 0.22, feet + 1.15, p.z - 0.22, p.x + 0.22, feet + 1.6, p.z + 0.22, head);
      // 朝向针：cube 的角必须按 min/max 给，反过来写三角面序会翻、被背面剔除吃掉
      var tx = p.x + Math.sin(p.yaw) * 0.55, tz = p.z + Math.cos(p.yaw) * 0.55;
      cube(Math.min(p.x, tx) - 0.04, feet + 1.66, Math.min(p.z, tz) - 0.04,
           Math.max(p.x, tx) + 0.04, feet + 1.82, Math.max(p.z, tz) + 0.04, ERA_GOLD);
      fxText(p.x, feet + 2.15, p.z, rx, ry, rz, ux, uy, uz, nv,
        String(p.addr || '').slice(-6).toUpperCase(), [190, 220, 255], 1);
    }

    /** 每帧重算 FX 缓冲：过期条目当场剔除，画多少个三角形回给 draw */
    function stepFx(now, d) {
      var rx = d[2], ry = 0, rz = -d[0], rl = Math.hypot(rx, rz) || 1; rx /= rl; rz /= rl;
      var ux = d[1] * rz, uy = d[2] * rx - d[0] * rz, uz = -d[1] * rx;
      var ul = Math.hypot(ux, uy, uz) || 1; ux /= ul; uy /= ul; uz /= ul;
      var nv = [-d[0], -d[1], -d[2]];
      arr = st.arrF; vp = 0;
      for (var i = st.fx.length - 1; i >= 0; i--) {
        var f = st.fx[i];
        f.age = now - f.t0;
        if (f.age > f.life) { st.fx.splice(i, 1); continue; }
        if (f.type === 'mint') drawMint(f, f.age, rx, ry, rz, ux, uy, uz, nv);
        else drawRain(f, f.age, rx, ry, rz, ux, uy, uz, nv);
      }
      // 在场邻居：数量有上限，因为这条缓冲是共享的，人多了要给动画留地方
      for (var pi = 0; pi < st.peers.length && pi < POS_DRAW_MAX; pi++) {
        drawPeer(st.peers[pi], rx, ry, rz, ux, uy, uz, nv);
      }
      st.nF = vp / 6;
      if (!st.nF) return 0;   // 没东西可画就连这次上传也省掉：常态帧不许为动画买单
      gl.bindBuffer(gl.ARRAY_BUFFER, st.bufF);
      gl.bufferData(gl.ARRAY_BUFFER, st.arrF.subarray(0, vp), gl.DYNAMIC_DRAW);
      return st.nF;
    }

    function draw() {
      if (!st.heights) return;
      var w = canvas.clientWidth | 0, h = canvas.clientHeight | 0;
      if (!w || !h) return;
      if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
      gl.viewport(0, 0, w, h);
      var now = performance.now();
      var ds = dayState(now);
      gl.clearColor(ds.sky[0], ds.sky[1], ds.sky[2], 1);
      gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);

      var d = dir(), e0 = eye();
      var m = mul(perspective(1.22, w / h, 0.1, 400), lookAt(e0, [e0[0] + d[0], e0[1] + d[1], e0[2] + d[2]], [0, 1, 0]));
      var fogFar = FOG_FAR * (0.58 + 0.42 * ds.day);

      function colorUse() {
        gl.useProgram(prog);
        gl.uniformMatrix4fv(U.vp, false, m);
        gl.uniform3f(U.eye, e0[0], e0[1], e0[2]);
        gl.uniform4f(U.tint, ds.tint[0], ds.tint[1], ds.tint[2], 1);
        gl.uniform3f(U.fog, ds.sky[0], ds.sky[1], ds.sky[2]);
        gl.uniform2f(U.fogr, FOG_NEAR, fogFar);
      }
      function drawBuf(buf, n, mode) {
        if (!n) return;
        gl.bindBuffer(gl.ARRAY_BUFFER, buf);
        gl.enableVertexAttribArray(A_P); gl.vertexAttribPointer(A_P, 3, gl.FLOAT, false, 24, 0);
        gl.enableVertexAttribArray(A_C); gl.vertexAttribPointer(A_C, 3, gl.FLOAT, false, 24, 12);
        gl.drawArrays(mode, 0, n);
      }

      // 天空先画：太阳/月亮/星星不吃雾（否则它们会被自己的"远处"吃掉）。
      // 地形随后覆盖它——深度测试会把挡在前面的地形留下来。
      colorUse();
      gl.disable(gl.CULL_FACE);
      gl.uniform2f(U.fogr, 100000, 100001);
      gl.uniform4f(U.tint, 1, 1, 1, 1);
      var nS = skyBodies(ds, e0, d);
      drawBuf(st.bufS, nS, gl.TRIANGLES);
      var nStar = starPoints(e0, ds);
      drawBuf(st.bufStar, nStar, gl.POINTS);
      gl.enable(gl.CULL_FACE);

      // 贴图管线：地形 → 签名的方块 → 资源（花/活物是十字面，不能剔除背面）
      gl.useProgram(progT);
      gl.uniformMatrix4fv(UT.vp, false, m);
      gl.uniform3f(UT.eye, e0[0], e0[1], e0[2]);
      gl.uniform4f(UT.tint, ds.tint[0], ds.tint[1], ds.tint[2], 1);
      gl.uniform3f(UT.fog, ds.sky[0], ds.sky[1], ds.sky[2]);
      gl.uniform2f(UT.fogr, FOG_NEAR, fogFar);
      gl.uniform1i(UT.tex, 0);
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, atlasTex);
      function drawTex(buf, n) {
        if (!n) return;
        gl.bindBuffer(gl.ARRAY_BUFFER, buf);
        gl.enableVertexAttribArray(AT_P); gl.vertexAttribPointer(AT_P, 3, gl.FLOAT, false, 24, 0);
        gl.enableVertexAttribArray(AT_UV); gl.vertexAttribPointer(AT_UV, 2, gl.FLOAT, false, 24, 12);
        gl.enableVertexAttribArray(AT_S); gl.vertexAttribPointer(AT_S, 1, gl.FLOAT, false, 24, 20);
        gl.drawArrays(gl.TRIANGLES, 0, n);
      }
      // 地形是闭合体素：背面剔除先开着（省一半片元）；花/活物/树冠贴片再关掉
      drawTex(st.bufG, st.nG);
      gl.disable(gl.CULL_FACE);
      drawTex(st.bufE, st.nE);
      drawTex(st.bufR, st.nR);
      gl.enable(gl.CULL_FACE);

      // 粒子（放置/拆除反馈）
      if (st.nP) {
        colorUse();
        gl.uniform2f(U.fogr, 100000, 100001);
        gl.uniform4f(U.tint, 1, 1, 1, 1);
        drawBuf(st.bufP, st.nP, gl.POINTS);
      }

      // 出块显形（铸块砖 + QLOR 数字雨）：正对相机、不吃雾，也不碰地形网格
      var nF = stepFx(now, d);
      if (nF) {
        colorUse();
        gl.uniform2f(U.fogr, 100000, 100001);
        gl.uniform4f(U.tint, 1, 1, 1, 1);
        drawBuf(st.bufF, nF, gl.TRIANGLES);
      }

      // 准星线框
      colorUse();
      gl.disable(gl.CULL_FACE);
      drawBuf(st.bufH, st.nH, gl.LINES);
      gl.enable(gl.CULL_FACE);
    }


    // —— 主循环
    var raf = 0, last = 0;
    function tick(now) {
      raf = global.requestAnimationFrame(tick);
      var dt = Math.min(0.1, (now - last) / 1000 || 0); last = now;
      move(dt);
      stepParts(dt);
      var x0 = Math.max(0, Math.min(W - REGION, Math.floor(st.x) - (REGION >> 1)));
      var z0 = Math.max(0, Math.min(H - REGION, Math.floor(st.z) - (REGION >> 1)));
      var key = x0 + ',' + z0;
      if (key !== st.regionKey || st.dirty) {
        st.regionKey = key; st.dirty = false;
        buildTerrain(x0, z0);
        buildEntities(x0, z0);
        buildResources(x0, z0);
        st.entAt = now;
      } else if (now - st.entAt > 90) {
        // 活物呼吸/走动：约 11Hz 重算实体网格。比每帧上传便宜，也够让画面不再"定格"。
        // 资源不长脚，只在地形/资源集变化时重铺，不跟着这一层一起上传。
        st.entAt = now;
        buildEntities(x0, z0);
      }
      st.target = pick();
      buildHighlight();
      draw();
      // 帧内截图：缓冲只在 draw() 之后、本次回调返回之前是满的
      if (st.wantShot) {
        var shot = st.wantShot; st.wantShot = null;
        try { shot(canvas.toDataURL('image/png')); } catch (e) { shot(null); }
      }
      st.frames++;
      if (now - st.fpsMark > 1000) { st.fps = st.frames; st.frames = 0; st.fpsMark = now; }
      hooks.onFrame && hooks.onFrame(state());
    }
    function state() {
      var cx = Math.max(0, Math.min(W - 1, st.x | 0)), cz = Math.max(0, Math.min(H - 1, st.z | 0));
      var t = st.target;
      var ds = dayState(performance.now());
      return {
        x: cx, z: cz, layer: Math.floor(st.y - EYE + 0.01),
        // surface = 原始地表（高度场）；top = 这一列现在露出来的顶面；dug = 挖掉了几层。
        // HUD 三个都显示：只看 top 会分不清"地势本来就低"和"是我挖下去的"。
        surface: surf(cx, cz), top: level(cx, cz), dug: surf(cx, cz) - level(cx, cz),
        block: blk(cx, cz), target: t, fps: st.fps, locked: st.locked,
        day: ds.day, dusk: ds.dusk, height: st.height, slots: st.slots,
        fx: st.fx.length, fxVerts: st.nF, mesh: st.mesh,
        // 地形顶点数与缓冲容量：两者相等就说明这一帧被截断了（坑壁缺块 / 悬崖缺面）
        verts: st.nG, vcap: (st.arrG.length / 6) | 0,
        // 连续坐标与朝向：在场广播要的是"我这一刻站在哪"，取整的格子坐标不够用
        px: st.x, py: st.y - EYE, pz: st.z, yaw: st.yaw, pitch: st.pitch,
        // 第一人称的手：手持格、实测速度（摆臂幅度由它定）、步相、出拳计数，
        // 还有 above = 眼睛离自己脚下那一列顶面几米——离开人的高度，手就该消失。
        held: st.slot, speed: st.speed, bob: st.bobPhase, bobY: st.bob, punch: st.punch,
        // 物理状态要能被外部断言：没有这两条，"站住了吗""下水了吗"只能靠猜，
        // 而 tools/ 里的物理对拍正是靠它们判定落地、起跳和浮力有没有真的生效
        grounded: st.grounded, inWater: st.inWater,
        above: (st.y - EYE) - support(cx, cz),
        peers: st.peers.length,
      };
    }

    // —— 物理：一格一米，人有体重、有体积、会摔、要跳，水有浮力。
    //    旧的 move() 没有竖直速度、水平方向零碰撞：能穿方块、能直接走上十米断崖、
    //    走出崖边会永远悬在半空。那不叫创造模式，那叫没有物理。
    /** 身体盒子（0.6 米宽 × 1.75 米高）在 (x, feet, z) 是否卡进实心格 */
    function hits(x, feet, z) {
      var x0 = Math.floor(x - BODY_R), x1 = Math.floor(x + BODY_R);
      var z0 = Math.floor(z - BODY_R), z1 = Math.floor(z + BODY_R);
      var y0 = Math.floor(feet + 0.002), y1 = Math.floor(feet + BODY_H - 0.002);
      for (var cy = y0; cy <= y1; cy++)
        for (var cx = x0; cx <= x1; cx++)
          for (var cz = z0; cz <= z1; cz++)
            if (solid(cx, cy, cz)) return true;
      return false;
    }

    /** 身体占的那几列里、不高于 notAbove 的最高可站顶面：跨台阶和落地都读它 */
    function standTop(x, z, notAbove) {
      var best = -Infinity;
      var x0 = Math.floor(x - BODY_R), x1 = Math.floor(x + BODY_R);
      var z0 = Math.floor(z - BODY_R), z1 = Math.floor(z + BODY_R);
      for (var cx = x0; cx <= x1; cx++) for (var cz = z0; cz <= z1; cz++) {
        if (cx < 0 || cz < 0 || cx >= W || cz >= H) continue;
        var t = support(cx, cz);
        if (t <= notAbove + 0.001 && t > best) best = t;
      }
      return best;
    }

    function move(dt) {
      var k = st.keys;
      dt = Math.min(dt, 0.05);                    // 卡帧一步跨两米会穿墙，先把时间夹住
      var feet = st.y - EYE;
      var sup = support(st.x | 0, st.z | 0);
      var water = feet < WATER_TOP && sup < WATER_TOP - 0.01;
      st.inWater = water;

      // 自己把方块放在脚下了：先把自己抬出去，否则会卡死在墙里
      if (hits(st.x, feet, st.z)) {
        for (var up = 0.05; up <= 1.35; up += 0.05) {
          if (!hits(st.x, feet + up, st.z)) { feet += up; if (st.vy < 0) st.vy = 0; break; }
        }
      }

      // —— 水平：按键定目标速度，实际速度按加速度逼近（不是"一按就满速"的纸片手感）
      var fx = Math.sin(st.yaw), fz = Math.cos(st.yaw);
      var wx = 0, wz = 0;
      if (k['w'] || k['arrowup']) { wx += fx; wz += fz; }
      if (k['s'] || k['arrowdown']) { wx -= fx; wz -= fz; }
      if (k['a'] || k['arrowleft']) { wx += fz; wz -= fx; }
      if (k['d'] || k['arrowright']) { wx -= fz; wz += fx; }
      var wn = Math.hypot(wx, wz);
      var wish = water ? (k['shift'] ? RUN : WALK) * 0.62 : (k['shift'] && st.grounded ? RUN : WALK);
      var tvx = 0, tvz = 0;
      if (wn > 0) { tvx = wx / wn * wish; tvz = wz / wn * wish; }
      var accel = water ? 5 : (st.grounded ? 16 : 5);   // 空中只能微调方向，不能想拐就拐
      st.vx += (tvx - st.vx) * Math.min(1, accel * dt);
      st.vz += (tvz - st.vz) * Math.min(1, accel * dt);

      var ox = st.x, oz = st.z;
      // X 轴：撞墙就贴墙站住；脚下有 STEP_UP 以内的台阶就顺势跨上去
      var nx = Math.max(0.5, Math.min(W - 0.5, st.x + st.vx * dt));
      if (!hits(nx, feet, st.z)) st.x = nx;
      else {
        var lift = (st.grounded || water) ? standTop(nx, st.z, feet + STEP_UP) : -Infinity;
        if (lift > feet + 0.001 && lift <= feet + STEP_UP && !hits(nx, lift, st.z)) { st.x = nx; feet = lift; }
        else st.vx = 0;
      }
      var nz = Math.max(0.5, Math.min(H - 0.5, st.z + st.vz * dt));
      if (!hits(st.x, feet, nz)) st.z = nz;
      else {
        var lift2 = (st.grounded || water) ? standTop(st.x, nz, feet + STEP_UP) : -Infinity;
        if (lift2 > feet + 0.001 && lift2 <= feet + STEP_UP && !hits(st.x, lift2, nz)) { st.z = nz; feet = lift2; }
        else st.vz = 0;
      }

      // —— 竖直：重力、浮力、跳跃，全部走速度，不再"按住键就升"
      if (water) {
        // 浮力按浸进水的身体比例给：头一出水净上浮就归零，人自然漂在水面上
        var sub = Math.max(0, Math.min(1, (WATER_TOP - feet) / BODY_H));
        st.vy += (sub * 13.5 - GRAV) * dt;
        st.vy *= Math.max(0, 1 - 4.2 * dt);                 // 水阻
        if (k[' ']) st.vy += 7.5 * dt;                       // 往上划
        if (k['c']) st.vy -= 6.5 * dt;                       // 往下潜
        st.grounded = false;
      } else {
        if (k[' '] && st.grounded) { st.vy = JUMP_V; st.grounded = false; }
        st.vy -= GRAV * dt;
        if (st.vy < -55) st.vy = -55;                        // 收尾速度
      }
      var ny = feet + st.vy * dt;
      if (st.vy > 0) {
        if (hits(st.x, ny + 0.02, st.z)) st.vy = 0;          // 顶头
        else { feet = ny; if (!water) st.grounded = false; }
      } else {
        var top = standTop(st.x, st.z, feet + 0.001);
        if (top > -Infinity && ny <= top) { feet = top; st.vy = 0; st.grounded = true; }
        else { feet = ny; st.grounded = false; }
      }
      var cap = st.maxLayer + 8 - EYE;
      if (feet > cap) { feet = cap; if (st.vy > 0) st.vy = 0; }
      st.y = feet + EYE;

      // 走路时视线轻微起伏 + 脚步音：MC 的"在场感"一大半来自这两件小事
      var moved = Math.hypot(st.x - ox, st.z - oz);
      st.speed = moved / dt;             // 米/秒：摆臂幅度读它，不能按"有没有按键"甩手
      if (moved > 1e-4 && st.grounded && !water) {
        st.bobPhase += moved * 5.2;                          // 步频跟着真实步幅走
        st.bob = Math.sin(st.bobPhase) * 0.045;
        if (st.bobPhase - st.stepMark > Math.PI) {
          st.stepMark = st.bobPhase;
          if (global.QLSfx) global.QLSfx.step();
        }
      } else {
        st.bob *= Math.max(0, 1 - dt * 10);
        if (Math.abs(st.bob) < 0.001) st.bob = 0;
      }
    }

    // —— 输入：指针锁定可用就是真 Minecraft 手感；不可用（比如某些 WebView）自动退化成
    //    "按住左键拖拽转视角、松开不移动即拆除"，功能一条不缺。
    var look = null;
    function onKeyDown(ev) {
      if (ev.metaKey || ev.ctrlKey || ev.altKey) return;
      var key = ev.key.toLowerCase();
      if (key === 'escape') { hooks.onClose && hooks.onClose(); return; }
      if (key === 'q') { st.yaw += 0.14; ev.preventDefault(); return; }
      if (key === 'e') { st.yaw -= 0.14; ev.preventDefault(); return; }
      if (key === 'f') { ev.preventDefault(); act(2); return; }
      if (key === 'x' || key === 'delete' || key === 'backspace') { ev.preventDefault(); act(0); return; }
      if (key === 'm') {
        var on = global.QLSfx ? global.QLSfx.toggle() : false;
        hooks.onSound && hooks.onSound(on);
        ev.preventDefault();
        return;
      }
      if (key >= '0' && key <= '9' && key.length === 1) {
        var idx = key === '0' ? 9 : Number(key) - 1;
        if (idx < st.slots) { hooks.onSlot && hooks.onSlot(idx); ev.preventDefault(); return; }
      }
      st.keys[key] = true;
      if (key === ' ') ev.preventDefault();
    }
    function onKeyUp(ev) { st.keys[ev.key.toLowerCase()] = false; }
    function onMouseMove(ev) {
      if (document.pointerLockElement === canvas) {
        st.yaw += ev.movementX * 0.0022;
        st.pitch = Math.max(-1.5, Math.min(1.5, st.pitch - ev.movementY * 0.0022));
      } else if (look) {
        st.yaw += (ev.clientX - look.x) * 0.005;
        st.pitch = Math.max(-1.5, Math.min(1.5, st.pitch - (ev.clientY - look.y) * 0.005));
        if (Math.abs(ev.clientX - look.x0) + Math.abs(ev.clientY - look.y0) > 6) look.moved = true;
        look.x = ev.clientX; look.y = ev.clientY;
      }
    }
    function onWheel(ev) {
      ev.preventDefault();
      var d = ev.deltaY > 0 ? 1 : -1;
      hooks.onSlot && hooks.onSlot((st.slot + d + st.slots) % st.slots);
    }
    function onDown(ev) {
      if (ev.button === 1) return;
      ev.preventDefault();
      // 第一次点击顺手申请指针锁定，但**绝不吞掉这一次操作**：拿不到锁定的 WebView 里，
      // "点一下只是锁鼠标"会让整个编辑器看起来是死的。
      if (!st.locked) lock();
      if (ev.button === 2) { act(2); return; }
      if (ev.button === 0) {
        if (document.pointerLockElement === canvas) act(0);
        else look = { x: ev.clientX, y: ev.clientY, x0: ev.clientX, y0: ev.clientY, moved: false };
      }
    }
    function onUp(ev) {
      if (ev.button === 0 && look) {
        if (!look.moved) act(0);
        look = null;
      }
    }
    /** 一次编辑动作：0 = 挖/拆/采，2 = 造（签一条 claim 记录） */
    function act(kind) {
      st.punch++;                       // 手要先挥出去：有没有打到东西是账本的事，挥拳是手感的事
      var t = st.target;
      if (!t) { hooks.onEmpty && hooks.onEmpty(kind); return; }
      // 准星落在资源上：左键是"采"，不拆地、不拆别人的方块
      if (t.node) {
        if (kind === 0) hooks.onHarvest && hooks.onHarvest(t.cx, t.cz, t.node);
        return;
      }
      if (kind === 2) {
        if (!t.place) { hooks.onBlocked && hooks.onBlocked(t); return; }
        hooks.onPlace && hooks.onPlace(t.place.cx, t.place.cz, t.place.layer, st.slot);
      } else if (t.entity) {
        hooks.onBreak && hooks.onBreak(t.cx, t.cz, t.layer, t.entity);
      } else if (t.dig) {
        // 地形：只挖露出来的那一层。挖掉的层号由这里报给命令层，链上还要再判一次。
        hooks.onDig && hooks.onDig(t.cx, t.cz, t.layer);
      } else {
        hooks.onDigBlocked && hooks.onDigBlocked(t);
      }
    }
    function canLock() { return typeof canvas.requestPointerLock === 'function'; }
    function dragMode() { st.locked = true; st.drag = true; hooks.onLock && hooks.onLock(true); }
    function lock() {
      if (!canLock()) { dragMode(); return; }
      try {
        var r = canvas.requestPointerLock();
        if (r && typeof r.catch === 'function') r.catch(function () { if (document.pointerLockElement !== canvas) dragMode(); });
      } catch (e) { dragMode(); return; }
      // 兜底：某些内核既不报错也不进锁定态，150ms 后仍未锁定就转拖拽
      global.setTimeout(function () {
        if (document.pointerLockElement !== canvas && !st.drag) dragMode();
      }, 150);
    }
    function onLockChange() {
      var on = document.pointerLockElement === canvas;
      st.locked = on || st.drag;
      if (on) st.drag = false;
      hooks.onLock && hooks.onLock(st.locked);
    }
    function onCtx(ev) { ev.preventDefault(); }

    return {
      gl: gl,
      setWorld: function (heights, blocks) {
        var prev = st.heights;
        var oldGround = prev ? support(st.x | 0, st.z | 0) : 0;
        st.heights = heights; st.blocks = blocks; st.dirty = true;
        // 规则 v2 把高度场钉在创世哈希上：链长一格只重画地表，脚下的地面不动。
        // 真的动了（换链/换规则版本）就重新落回新地面，而不是悬在半空看风景；
        // 但飞在自己塔顶往上垒时不该被拽下来，所以只在高度确实变了的那一次才落。
        if (prev && prev !== heights) {
          var g = support(st.x | 0, st.z | 0);
          if (g !== oldGround) st.y = g + EYE;
        }
      },
      setPeers: function (list) {
        var ps = list || [], out = [];
        for (var i = 0; i < ps.length; i++) {
          var p = ps[i];
          // heading 上网时是「度 × POS_SCALE」，回到来就是度；这里再转弧度给朝向针
          out.push({
            x: Number(p.x) || 0, y: Number(p.y) || 0, z: Number(p.z) || 0,
            yaw: (Number(p.heading) || 0) * Math.PI / 180, addr: p.addr || '',
          });
        }
        st.peers = out;
      },
      /** 挖地账本（快照的 dug 数组：[{x, y, top, removed}]）。
       *  只认 removed —— top 是派生值，谁算都一样（removed 才是链上的事实）。
       *  签名只算"挖了哪几列、多深"，站着不动每 2 秒重收一份也不会重铺地形。 */
      setDug: function (list) {
        var src = list || [], map = {}, sig = 0;
        for (var i = 0; i < src.length; i++) {
          var d = src[i], rem = d.removed | 0;
          if (rem <= 0) continue;
          map[cellKey(d.x, d.y)] = rem;
          sig = (Math.imul(sig, 31) + d.x * 3 + d.y * 5 + rem * 7) | 0;
        }
        var key = sig + ':' + src.length;
        if (key === st.dugSig) return;
        st.dugSig = key;
        var wasStanding = st.heights &&
          Math.abs(st.y - (support(st.x | 0, st.z | 0) + EYE)) < 0.06;
        st.dugMap = map;
        st.dirty = true;
        // 脚下这一列刚好被挖了：站着的人跟着地面一起下去（悬空看风景不是"挖到了"）
        if (wasStanding) st.y = support(st.x | 0, st.z | 0) + EYE;
      },
      // 截图必须**在同一帧内**抓：context 创建时没开 preserveDrawingBuffer，帧尾一过
      // 缓冲就是空的——toDataURL 不报错，只给你一张全透明图。所以回调排进 tick。
      snapshot: function (cb) { st.wantShot = cb; },
      setEntities: function (list, address) {
        var ents = list || [];
        // 快照每 2 秒就送一份全新数组（自己的写入和别人的写入都走这里）。只有几何
        // 真的变了才重铺 64×64 区域网格，否则玩家站着不动也会被重建卡一下。
        // 签名只算占格/层/种类：改名不改几何，断租（released）会改变活着的条数。
        var sig = 0, live = 0;
        for (var j = 0; j < ents.length; j++) {
          var c = ents[j];
          if (c.released) continue;
          live++;
          sig = (sig * 31 + c.x * 3 + c.y * 5 + c.layer * 7 + c.kind) | 0;
        }
        var sigKey = sig + ':' + live + ':' + ents.length + ':' + (address || '');
        if (sigKey === st.entSig) return;
        st.entSig = sigKey;
        st.entities = ents; st.address = address || '';
        st.byCell = {};
        for (var i = 0; i < st.entities.length; i++) {
          var e = st.entities[i];
          if (e.released) continue; // 无主 = 不占格（与 world.rs live_at 同一判据）
          var ck = cellKey(e.x, e.y);
          if (!st.byCell[ck]) st.byCell[ck] = [];
          st.byCell[ck].push(e);
        }
        st.dirty = true;
      },
      setColors: function (c) { st.colors = c; st.dirty = true; },
      /** 资源节点列表（world_resources 的原样结构）：签名只算没采走的节点 */
      setResources: function (list) {
        var nodes = list || [];
        var sig = 0, live = 0;
        for (var i = 0; i < nodes.length; i++) {
          var n = nodes[i];
          if (n.taken) continue;
          live++;
          sig = (sig * 31 + n.x * 3 + n.y * 5 + n.kind * 7) | 0;
        }
        var key = sig + ':' + live + ':' + nodes.length;
        if (key === st.resSig) return;
        st.resSig = key;
        st.res = nodes;
        st.resMap = {};
        for (var j = 0; j < nodes.length; j++) {
          var m = nodes[j];
          // taken 的也留着：地形层要靠它把"这一轮被采走的树"从装饰林里摘掉
          st.resMap[cellKey(m.x, m.y)] = m;
        }
        st.dirty = true;
      },
      setSlot: function (i) { st.slot = i; },
      setSlots: function (n) { st.slots = Math.max(1, Math.min(16, n | 0 || 1)); if (st.slot >= st.slots) st.slot = 0; },
      setTime: function (height) { st.height = Math.max(0, height | 0); st.heightAt = performance.now(); },
      setMaxLayer: function (n) { if (n > 0) st.maxLayer = n; },
      burst: burst,
      /** 挖掉一层的碎屑：颜色取这一带地层的贴图均色（土/石/矿），与坑壁同一个色系 */
      digBurst: function (cx, cz, layer, kind) {
        var tile = kind === 0 ? TIL.DIRT : kind === 1 ? TIL.STONE
          : kind === 2 ? TIL.ORE_GOLD : TIL.DIRT;
        burst(cx, layer, cz, tile);
      },
      /** 出块显形：一块刻着块哈希/VRF/发行量的砖落进矿格。cell 缺省时落在脚下。 */
      mintBlock: function (info) {
        if (!info || !st.heights) return;
        var cell = info.cell || { cx: st.x | 0, cz: st.z | 0, layer: support(st.x | 0, st.z | 0) - 1 };
        fxPush({
          type: 'mint', t0: performance.now(), age: 0, life: FX_MINT_MS,
          cx: cell.cx | 0, cz: cell.cz | 0, layer: Math.max(0, cell.layer | 0),
          hash: String(info.hash || '').slice(0, 8), vrf: String(info.vrf || '').slice(0, 8),
          reward: Number(info.reward) || 0, height: Number(info.height) || 0,
        });
      },
      /** 矿格上方起一串向上流的 QLOR 符文（x/y/layer 就是这一格的坐标，与账本同一口径） */
      rainAt: function (cx, cz, layer, text) {
        fxPush({
          type: 'rain', t0: performance.now(), age: 0, life: FX_RAIN_MS, rows: 6,
          cx: cx | 0, cz: cz | 0, base: Math.max(0, (layer | 0) + 1),
          text: String(text || 'QLOR'),
        });
      },
      fxCount: function () { return st.fx.length; },
      spawn: function (cx, cz) {
        st.x = cx + 0.5; st.z = cz + 0.5;
        st.y = Math.max(st.y, support(cx, cz) + EYE);
        st.dirty = true;
      },
      jumpTo: function (cx, cz, layer) {
        st.x = cx + 0.5; st.z = cz + 0.5; st.y = (layer == null ? support(cx, cz) : layer + 1) + EYE;
        st.dirty = true;
      },
      setView: function (yaw, pitch) {
        if (yaw != null) st.yaw = yaw;
        if (pitch != null) st.pitch = pitch;
      },
      open: function () {
        global.addEventListener('keydown', onKeyDown);
        global.addEventListener('keyup', onKeyUp);
        global.addEventListener('mousemove', onMouseMove);
        global.addEventListener('mouseup', onUp);
        document.addEventListener('pointerlockchange', onLockChange);
        canvas.addEventListener('wheel', onWheel, { passive: false });
        canvas.addEventListener('mousedown', onDown);
        canvas.addEventListener('contextmenu', onCtx);
        if (!raf) { last = performance.now(); st.fpsMark = last; raf = global.requestAnimationFrame(tick); }
      },
      close: function () {
        global.removeEventListener('keydown', onKeyDown);
        global.removeEventListener('keyup', onKeyUp);
        global.removeEventListener('mousemove', onMouseMove);
        global.removeEventListener('mouseup', onUp);
        document.removeEventListener('pointerlockchange', onLockChange);
        canvas.removeEventListener('wheel', onWheel);
        canvas.removeEventListener('mousedown', onDown);
        canvas.removeEventListener('contextmenu', onCtx);
        look = null; st.keys = {}; st.locked = false; st.drag = false;
        if (document.pointerLockElement === canvas) document.exitPointerLock();
        if (raf) { global.cancelAnimationFrame(raf); raf = 0; }
      },
      state: state,
    };
  }

  global.QLWorld3D = {
    create: World3D, LEVEL_SHIFT: LEVEL_SHIFT, MAX_LAYER: MAX_LAYER, REACH: REACH,
    REGION: REGION, PAL: PAL, TREE_TRUNK: TREE_TRUNK, TREE_LEAF: TREE_LEAF, ERA_GOLD: ERA_GOLD,
    // 挖地与地形贴图的可测面：app.js 用 TERRAIN 给物品栏/材料上色，测试用深度常量对账
    DUG_FLOOR: DUG_FLOOR, DUG_SOIL_DEPTH: DUG_SOIL_DEPTH, DUG_STONE_DEPTH: DUG_STONE_DEPTH,
    TERRAIN: TIL,
    // 出块显形这一层的可测面：字模表与"这行字有没有缺笔"的判据（回归闸用它自己拼字符串）
    FONT35: FONT35, missingGlyphs: missingGlyphs,
    FX_MINT_MS: FX_MINT_MS, FX_RAIN_MS: FX_RAIN_MS, FX_CAP: FX_CAP,
  };
})(window);
