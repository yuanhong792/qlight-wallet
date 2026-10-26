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
 * 地形不能被挖：它不是数据，是 `render(种子, 规则, 历史)` 算出来的Nature。
 */
(function (global) {
  'use strict';

  var W = 256, H = 256;
  var LEVEL_SHIFT = 3;          // 高度场 0..255 → 层（8 分之一格一层），与 world.rs MAX_LAYER 同口径
  var MAX_LAYER = 64;           // 可建造的高度层上限（链侧常量，双端必须一致）
  var REACH = 7;                // 可编辑距离（格）
  var REGION = 64;              // 每次重建网格的边长（格）
  var EYE = 1.62;               // 视高（格）
  // 雾：区域只有 REGION 格，边界外是真空。用距离雾在边界之前把地形融进天空，
  // 否则走两步就看见"世界在 32 格外被整齐切断"——确定性世界的边缘不该是穿帮。
  var SKY = [0.55, 0.72, 0.92];
  var FOG_NEAR = 12, FOG_FAR = REGION / 2 + 1;

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

  // 面的朝向（外法线）常量——quad() 靠它决定顶点序，见下方 quad 的注释
  var N_UP = [0, 1, 0], N_DOWN = [0, -1, 0], N_N = [0, 0, -1], N_S = [0, 0, 1], N_E = [1, 0, 0], N_W = [-1, 0, 0];

  var VS = 'attribute vec3 a_p;attribute vec3 a_c;uniform mat4 u_vp;uniform vec3 u_eye;' +
    'varying vec3 v_c;varying float v_d;' +
    'void main(){v_c=a_c;v_d=distance(a_p,u_eye);gl_Position=u_vp*vec4(a_p,1.0);}';
  var FS = 'precision mediump float;varying vec3 v_c;varying float v_d;' +
    'uniform vec4 u_tint;uniform vec3 u_fog;uniform vec2 u_fog_r;' +
    'void main(){float f=clamp((v_d-u_fog_r.x)/(u_fog_r.y-u_fog_r.x),0.0,1.0);f*=f;' +
    'gl_FragColor=vec4(mix(v_c*u_tint.rgb,u_fog,f),1.0);}';

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

  function World3D(canvas, hooks) {
    var gl = canvas.getContext('webgl', { antialias: false, alpha: false });
    if (!gl) { hooks.onUnsupported && hooks.onUnsupported(); return null; }

    var prog = gl.createProgram();
    function shader(type, src) {
      var s = gl.createShader(type);
      gl.shaderSource(s, src); gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
      gl.attachShader(prog, s); return s;
    }
    shader(gl.VERTEX_SHADER, VS); shader(gl.FRAGMENT_SHADER, FS);
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog));
    gl.useProgram(prog);
    var A_P = gl.getAttribLocation(prog, 'a_p'), A_C = gl.getAttribLocation(prog, 'a_c');
    var U_VP = gl.getUniformLocation(prog, 'u_vp'), U_TINT = gl.getUniformLocation(prog, 'u_tint');
    var U_EYE = gl.getUniformLocation(prog, 'u_eye'), U_FOG = gl.getUniformLocation(prog, 'u_fog');
    var U_FOGR = gl.getUniformLocation(prog, 'u_fog_r');
    gl.enable(gl.DEPTH_TEST); gl.enable(gl.CULL_FACE);
    gl.clearColor(SKY[0], SKY[1], SKY[2], 1);
    gl.uniform3f(U_FOG, SKY[0], SKY[1], SKY[2]);
    gl.uniform2f(U_FOGR, FOG_NEAR, FOG_FAR);

    var st = {
      heights: null, blocks: null, entities: [], byCell: {}, address: '',
      slot: 0, colors: [], maxLayer: MAX_LAYER,
      x: 128.5, z: 128.5, y: 30, yaw: 0.6, pitch: -0.45,
      keys: {}, dirty: true, regionKey: '', target: null, locked: false, drag: false,
      bufT: gl.createBuffer(), nT: 0, bufE: gl.createBuffer(), nE: 0, bufH: gl.createBuffer(), nH: 0,
      // 缓冲区按 12 面/格预算（平均只有约 2.4 面：树只占林地的 ~12%）。
      // 真写满了 quad() 会直接停笔——最坏结果是区域最后几行（雾里那几格）缺面，
      // 不会越界写坏内存，也不会影响任何一条链上判定。
      arrT: new Float32Array(REGION * REGION * 12 * 6 * 6),
      arrE: new Float32Array(2048 * 36 * 6),
      arrH: new Float32Array(2 * 24 * 6),
      fps: 0, frames: 0, fpsMark: 0,
    };

    // —— 只读的地形/体素查询（全部来自 QLWorld.render() 那一趟循环的产物）
    function level(cx, cz) {
      if (cx < 0 || cz < 0 || cx >= W || cz >= H) return -1;
      return st.heights[cz * W + cx] >> LEVEL_SHIFT;
    }
    function blk(cx, cz) { return st.blocks[cz * W + cx]; }
    function baseColor(b) { return (b & 16) ? ERA_GOLD : (PAL[b & 7] || PAL[5]); }
    function cellKey(cx, cz) { return cx * H + cz; }
    function inColumn(cx, cz) { return st.byCell[cellKey(cx, cz)] || []; }
    function blockAt(cx, cz, layer) {
      var list = inColumn(cx, cz);
      for (var i = 0; i < list.length; i++) if (list[i].layer === layer) return list[i];
      return null;
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
    function upload(buf, count) {
      gl.bindBuffer(gl.ARRAY_BUFFER, buf);
      gl.bufferData(gl.ARRAY_BUFFER, arr.subarray(0, vp), gl.DYNAMIC_DRAW);
      return count / 6;
    }

    function buildTerrain(x0, z0) {
      arr = st.arrT; vp = 0;
      for (var dz = 0; dz < REGION; dz++) {
        for (var dx = 0; dx < REGION; dx++) {
          var cx = x0 + dx, cz = z0 + dz;
          if (cx >= W || cz >= H) continue;
          var lv = level(cx, cz), b = blk(cx, cz), c = baseColor(b);
          var y0 = lv + 1;
          quad([cx, y0, cz, cx + 1, y0, cz, cx + 1, y0, cz + 1, cx, y0, cz + 1], c, 1.0, N_UP);
          // 四个方向的侧壁：邻格更低才补，且只从邻格顶面拉起到本格顶面——
          // 一直画到 y=0 会让区域边界看起来像一排插在天空里的石柱
          var nb = [[cx, cz - 1, 0.8, N_N], [cx, cz + 1, 0.8, N_S], [cx + 1, cz, 0.65, N_E], [cx - 1, cz, 0.65, N_W]];
          for (var k = 0; k < 4; k++) {
            var nl = level(nb[k][0], nb[k][1]);
            if (nl >= lv) continue;
            var yb = nl + 1;
            var q = k === 0 ? [cx, y0, cz, cx + 1, y0, cz, cx + 1, yb, cz, cx, yb, cz]
              : k === 1 ? [cx + 1, y0, cz + 1, cx, y0, cz + 1, cx, yb, cz + 1, cx + 1, yb, cz + 1]
              : k === 2 ? [cx + 1, y0, cz + 1, cx + 1, y0, cz, cx + 1, yb, cz, cx + 1, yb, cz + 1]
              : [cx, y0, cz, cx, y0, cz + 1, cx, yb, cz + 1, cx, yb, cz];
            quad(q, c, nb[k][2], nb[k][3]);
          }
          // 天然树：与 2D 地图上同一个深色像点（同一趟分类，绝不两套判定）
          // 树干必须是落到地面的实心柱——只画一片 0.24 见方的顶面会让树冠悬在半空，
          // 看上去就是"世界里到处是洞"。
          if ((b & 8) && !(b & 16)) {
            // 树干 0.32 宽、树冠 0.88 宽：再细就成了俯视时的一根黑针，比悬空还难看
            cube(cx + 0.34, y0, cz + 0.34, cx + 0.66, y0 + 2.35, cz + 0.66, TREE_TRUNK);
            cube(cx + 0.06, y0 + 2.2, cz + 0.06, cx + 0.94, y0 + 3.35, cz + 0.94, TREE_LEAF);
          }
        }
      }
      st.nT = upload(st.bufT, vp);
    }

    /** 签过名的方块：按 layer 画完整立方体（叠塔因此成立），只画当前区域内的。
     *  账本把行号叫 `y`（world.rs 的 Entity{x, y, layer}），三维里它就是 z——曾经按 `e.z`
     *  去读，拿到 undefined → 坐标 NaN：签过的方块既不显形也不占格，看上去就是
     *  "世界到处是洞、点了没反应"。 */
    function buildEntities(x0, z0) {
      arr = st.arrE; vp = 0;
      for (var i = 0; i < st.entities.length; i++) {
        var e = st.entities[i];
        if (e.released) continue; // 断租不删除，但也不占格、不显形
        if (e.x < x0 || e.x >= x0 + REGION || e.y < z0 || e.y >= z0 + REGION) continue;
        var c = st.colors[e.kind] || st.colors[0];
        if (typeof c === 'string') c = hex2rgb(c);
        // 贴地那层刻意抬高 0.002：否则方块底面与地形顶面严格共面，深度测试会闪噪点
        var yb = e.layer + 0.002;
        cube(e.x + 0.04, yb, e.y + 0.04, e.x + 0.96, e.layer + 1, e.y + 0.96, c);
        // 顶面再压一层自己的颜色，让塔看起来是一块一块垒起来的
        quad([e.x + 0.04, e.layer + 1.001, e.y + 0.04, e.x + 0.96, e.layer + 1.001, e.y + 0.04,
              e.x + 0.96, e.layer + 1.001, e.y + 0.96, e.x + 0.04, e.layer + 1.001, e.y + 0.96], c, BLOCK_SHADE, N_UP);
      }
      st.nE = upload(st.bufE, vp);
    }

    /** 线框：白 = 正在看的实心体素，金 = 松手就落下去的空体素 */
    function wire(x0, y0, z0, x1, y1, z1, col) {
      var L = [[x0, y0, z0, x1, y0, z0], [x1, y0, z0, x1, y0, z1], [x1, y0, z1, x0, y0, z1], [x0, y0, z1, x0, y0, z0],
               [x0, y1, z0, x1, y1, z0], [x1, y1, z0, x1, y1, z1], [x1, y1, z1, x0, y1, z1], [x0, y1, z1, x0, y1, z0],
               [x0, y0, z0, x0, y1, z0], [x1, y0, z0, x1, y1, z0], [x1, y0, z1, x1, y1, z1], [x0, y0, z1, x0, y1, z1]];
      for (var i = 0; i < L.length; i++) {
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
      st.nH = vp / 6;
      gl.bindBuffer(gl.ARRAY_BUFFER, st.bufH);
      gl.bufferData(gl.ARRAY_BUFFER, st.arrH.subarray(0, vp), gl.DYNAMIC_DRAW);
    }

    function dir() {
      var cp = Math.cos(st.pitch);
      return [Math.sin(st.yaw) * cp, Math.sin(st.pitch), Math.cos(st.yaw) * cp];
    }
    function eye() { return [st.x, st.y, st.z]; }

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
          // 只有"头所在的那一格"算被身体占用。脚下那格允许落块——块一落地，move()
          // 立刻把人抬到新块顶上，于是"低头、连点"就是 Minecraft 的原地起柱：
          // 一次点击一块，不卡跳跃帧。曾经连脚下也一起禁，结果平视/俯视的点击
          // 全都静默落空，整个编辑器看起来是死的。
          var insideMe = px === Math.floor(st.x) && pz === Math.floor(st.z) && py === Math.floor(st.y);
          var okPlace = !insideMe && px >= 0 && pz >= 0 && px < W && pz < H && py >= 0 && py < st.maxLayer &&
            !blockAt(px, pz, py);
          return {
            cx: cx, cz: cz, layer: cy,
            entity: blockAt(cx, cz, cy),
            place: okPlace ? { cx: px, cz: pz, layer: py } : null,
          };
        }
        px = cx; py = cy; pz = cz;
      }
      return null;
    }

    function draw() {
      if (!st.heights) return;
      var w = canvas.clientWidth | 0, h = canvas.clientHeight | 0;
      if (!w || !h) return;
      if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
      gl.viewport(0, 0, w, h);
      gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
      var d = dir(), e0 = eye();
      var m = mul(perspective(1.22, w / h, 0.1, 400), lookAt(e0, [e0[0] + d[0], e0[1] + d[1], e0[2] + d[2]], [0, 1, 0]));
      gl.uniformMatrix4fv(U_VP, false, m);
      gl.uniform3f(U_EYE, e0[0], e0[1], e0[2]);
      gl.uniform4f(U_TINT, 1, 1, 1, 1);

      function drawBuf(buf, n, mode) {
        if (!n) return;
        gl.bindBuffer(gl.ARRAY_BUFFER, buf);
        gl.enableVertexAttribArray(A_P); gl.vertexAttribPointer(A_P, 3, gl.FLOAT, false, 24, 0);
        gl.enableVertexAttribArray(A_C); gl.vertexAttribPointer(A_C, 3, gl.FLOAT, false, 24, 12);
        gl.drawArrays(mode, 0, n);
      }
      drawBuf(st.bufT, st.nT, gl.TRIANGLES);
      drawBuf(st.bufE, st.nE, gl.TRIANGLES);
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
      var x0 = Math.max(0, Math.min(W - REGION, Math.floor(st.x) - (REGION >> 1)));
      var z0 = Math.max(0, Math.min(H - REGION, Math.floor(st.z) - (REGION >> 1)));
      var key = x0 + ',' + z0;
      if (key !== st.regionKey || st.dirty) {
        st.regionKey = key; st.dirty = false;
        buildTerrain(x0, z0);
        buildEntities(x0, z0);
      }
      st.target = pick();
      buildHighlight();
      draw();
      st.frames++;
      if (now - st.fpsMark > 1000) { st.fps = st.frames; st.frames = 0; st.fpsMark = now; }
      hooks.onFrame && hooks.onFrame(state());
    }
    function state() {
      var cx = Math.max(0, Math.min(W - 1, st.x | 0)), cz = Math.max(0, Math.min(H - 1, st.z | 0));
      var t = st.target;
      return {
        x: cx, z: cz, layer: Math.floor(st.y - EYE + 0.01), surface: level(cx, cz),
        block: blk(cx, cz), target: t, fps: st.fps, locked: st.locked,
      };
    }

    // —— 移动：站在算出来的地形或自己垒起来的方块顶上（创造模式可飞）
    function move(dt) {
      var k = st.keys, sp = (k['shift'] ? 15 : 7) * dt;
      var fx = Math.sin(st.yaw), fz = Math.cos(st.yaw);
      var dx = 0, dz = 0;
      if (k['w'] || k['arrowup']) { dx += fx; dz += fz; }
      if (k['s'] || k['arrowdown']) { dx -= fx; dz -= fz; }
      if (k['a'] || k['arrowleft']) { dx += fz; dz -= fx; }
      if (k['d'] || k['arrowright']) { dx -= fz; dz += fx; }
      var n = Math.hypot(dx, dz);
      if (n > 0) { dx = dx / n * sp; dz = dz / n * sp; }
      st.x = Math.max(0.5, Math.min(W - 0.5, st.x + dx));
      st.z = Math.max(0.5, Math.min(H - 0.5, st.z + dz));
      if (k[' ']) st.y += sp;
      if (k['c']) st.y -= sp;
      var ground = support(st.x | 0, st.z | 0) + EYE;
      if (st.y < ground) st.y = ground;
      if (st.y > st.maxLayer + 8) st.y = st.maxLayer + 8;
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
      var n = Number(key);
      if (n >= 1 && n <= 8) { hooks.onSlot && hooks.onSlot(n - 1); return; }
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
      hooks.onSlot && hooks.onSlot((st.slot + (ev.deltaY > 0 ? 1 : -1) + 8) % 8);
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
    /** 一次编辑动作：0 = 拆（释放归属），2 = 造（签一条 claim 记录） */
    function act(kind) {
      var t = st.target;
      if (!t) { hooks.onEmpty && hooks.onEmpty(kind); return; }
      if (kind === 2) {
        if (!t.place) { hooks.onBlocked && hooks.onBlocked(t); return; }
        hooks.onPlace && hooks.onPlace(t.place.cx, t.place.cz, t.place.layer, st.slot);
      } else {
        hooks.onBreak && hooks.onBreak(t.cx, t.cz, t.layer, t.entity);
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
      setEntities: function (list, address) {
        st.entities = list || []; st.address = address || '';
        st.byCell = {};
        for (var i = 0; i < st.entities.length; i++) {
          var e = st.entities[i];
          if (e.released) continue; // 无主 = 不占格（与 world.rs live_at 同一判据）
          var k = cellKey(e.x, e.y);
          if (!st.byCell[k]) st.byCell[k] = [];
          st.byCell[k].push(e);
        }
        st.dirty = true;
      },
      setColors: function (c) { st.colors = c; st.dirty = true; },
      setSlot: function (i) { st.slot = i; },
      setMaxLayer: function (n) { if (n > 0) st.maxLayer = n; },
      spawn: function (cx, cz) {
        st.x = cx + 0.5; st.z = cz + 0.5;
        st.y = Math.max(st.y, support(cx, cz) + EYE);
        st.dirty = true;
      },
      jumpTo: function (cx, cz, layer) {
        st.x = cx + 0.5; st.z = cz + 0.5; st.y = (layer == null ? support(cx, cz) : layer + 1) + EYE;
        st.dirty = true;
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
  };
})(window);
