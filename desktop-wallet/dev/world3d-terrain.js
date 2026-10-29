// 高度场大地渲染器：一整张连续网格 + 真实水面 + 大气透视。
//
// 为什么不是方块网格器：大地是 65536 格真实高程（1 格 = 35 米）。用"一格一层"
// 的体面去画它，一条 300 米的缓坡会变成 6 级 58 米的断崖，面与面之间还会漏缝——
// 玩家看到的"地浮在空中""像在水里"就是这么来的，调颜色调不掉。
// 高程表每个字节本来就有 8 档亚层分辨率（raw/8），把它当连续高度场画，
// 水平 1 格 = 35 米、垂直 1 单位 = 35 米，两个方向同尺，山就是那座山的形状。
//
// 方块只留给人手碰过的地方：挖过的坑、放下的方块仍然按整数层成方，
// 因为"我挖掉一层"必须看得见、量得出、签得了名。
import * as THREE from '../ui/vendor/three/three.module.js';

const SIZE = 256;
const SEA_RAW = 86;          // 海面在 8 位高程表上的分位（与 consensus 的 RULES.SEA 同值）
const SAND_RAW = 98;
const Y_PER_RAW = 1 / 8;     // raw/8：水平与垂直同尺
export const SEA_Y = SEA_RAW * Y_PER_RAW;

const clamp = (v, a, b) => (v < a ? a : v > b ? b : v);
const lerp = (a, b, t) => a + (b - a) * t;
const smooth = (t) => t * t * (3 - 2 * t);

// 太阳方向：与旧渲染器同一套"傍晚偏西偏低"的读感，但色温不再走品红
const SUN_DIR = new THREE.Vector3(-0.52, 0.34, 0.78).normalize();

/** 光照项（不是颜色）：地面本身的颜色来自真实卫星影像，这里只算"这一格被光照到多少"。
 *  朝上的面吃满太阳，朝下与背阴面只剩冷色天光；AO 从邻域高差估出来。 */
function lightTerm(out, lam, up, occ) {
  const k = (v) => clamp(v, 0, 1.35);
  out[0] = k((0.42 + 0.62 * lam) * occ * (0.94 + 0.06 * up));
  out[1] = k((0.45 + 0.58 * lam) * occ);
  out[2] = k((0.56 + 0.40 * lam) * occ * (1.04 - 0.04 * up));
}

export class TerrainWorld {
  constructor(canvas, opts = {}) {
    this.canvas = canvas;
    this.renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: false });
    this.renderer.setPixelRatio(Math.min(2, window.devicePixelRatio || 1));
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.renderer.toneMapping = THREE.NoToneMapping;   // 曲线已经在顶点色里算过，不再二次压

    this.scene = new THREE.Scene();
    this.camera = new THREE.PerspectiveCamera(68, 1, 0.08, 1400);
    this.exaggeration = opts.exaggeration ?? 1.35;    // 垂直夸大：1 是地理真值，>1 让山更像山
    this.far = opts.far ?? 260;

    this.terrain = null;
    this.water = null;
    this.sky = null;
    this.heights = null;
    this.blocks = null;
    this.spawn = { x: 128, z: 128 };
    this._col = [0, 0, 0];
  }

  setWorld(heights, blocks) {
    this.heights = heights; this.blocks = blocks;
    this.rebuild();
  }

  setSpawn(x, z) { this.spawn = { x, z }; }

  /** 采样点的大地顶面（单位：格）。连续高度场，所以脚不会踩进台阶缝里。 */
  groundY(x, z) {
    const H = this.heights; if (!H) return 0;
    const cx = clamp(x, 0, SIZE - 1), cz = clamp(z, 0, SIZE - 1);
    const x0 = Math.floor(cx), z0 = Math.floor(cz);
    const x1 = Math.min(SIZE - 1, x0 + 1), z1 = Math.min(SIZE - 1, z0 + 1);
    const fx = cx - x0, fz = cz - z0;
    const h = (a, b) => H[b * SIZE + a] * Y_PER_RAW * this.exaggeration;
    return lerp(lerp(h(x0, z0), h(x1, z0), fx), lerp(h(x0, z1), h(x1, z1), fx), fz);
  }

  level(x, z) {
    return Math.floor(this.groundY(clamp(x, 0, SIZE - 1), clamp(z, 0, SIZE - 1)) / this.exaggeration);
  }

  raw(x, z) {
    const H = this.heights; if (!H) return 0;
    return H[clamp(Math.floor(z), 0, SIZE - 1) * SIZE + clamp(Math.floor(x), 0, SIZE - 1)];
  }

  rebuild() {
    if (!this.heights) return;
    if (this.terrain) { this.scene.remove(this.terrain); this.terrain.geometry.dispose(); }
    const N = SIZE;
    const pos = new Float32Array(N * N * 3);
    const nor = new Float32Array(N * N * 3);
    const col = new Float32Array(N * N * 3);
    const H = this.heights;
    const yOf = (x, z) => H[z * N + x] * Y_PER_RAW * this.exaggeration;

    // 法线用中心差分：水平 1 格 = 35 米、垂直同尺，所以坡度是真的坡度
    for (let z = 0; z < N; z++) {
      for (let x = 0; x < N; x++) {
        const i = z * N + x;
        const y = yOf(x, z);
        pos[i * 3] = x; pos[i * 3 + 1] = y; pos[i * 3 + 2] = z;
        const l = yOf(Math.max(0, x - 1), z), r = yOf(Math.min(N - 1, x + 1), z);
        const n = yOf(x, Math.max(0, z - 1)), s = yOf(x, Math.min(N - 1, z + 1));
        const nx = (l - r) / 2, ny = 1, nz = (n - s) / 2;
        const inv = 1 / Math.hypot(nx, ny, nz);
        nor[i * 3] = nx * inv; nor[i * 3 + 1] = ny * inv; nor[i * 3 + 2] = nz * inv;
      }
    }
    // 光照 + 环境光遮蔽烘进顶点色：一张网格一个 draw call，颜色完全可控
    const ao = new Float32Array(N * N);
    for (let z = 0; z < N; z++) {
      for (let x = 0; x < N; x++) {
        const i = z * N + x, y = yOf(x, z);
        let occ = 0;
        for (let dz = -2; dz <= 2; dz++) for (let dx = -2; dx <= 2; dx++) {
          if (!dx && !dz) continue;
          const ax = clamp(x + dx, 0, N - 1), az = clamp(z + dz, 0, N - 1);
          const d = Math.hypot(dx, dz);
          const over = (yOf(ax, az) - y) / d;
          if (over > 0) occ += Math.min(1, over * 1.4);
        }
        ao[i] = clamp(1 - occ * 0.055, 0.52, 1);
      }
    }
    const sun = SUN_DIR;
    const c = this._col;
    const uv = new Float32Array(N * N * 2);
    for (let z = 0; z < N; z++) {
      for (let x = 0; x < N; x++) {
        const i = z * N + x;
        const slope = 1 - nor[i * 3 + 1];
        const occ = ao[i];
        const lam = clamp(nor[i * 3] * sun.x + nor[i * 3 + 1] * sun.y + nor[i * 3 + 2] * sun.z, 0, 1);
        lightTerm(c, lam, slope, occ);
        col[i * 3] = c[0]; col[i * 3 + 1] = c[1]; col[i * 3 + 2] = c[2];
        uv[i * 2] = x / (N - 1);
        uv[i * 2 + 1] = 1 - z / (N - 1);
      }
    }
    const idx = new Uint32Array((N - 1) * (N - 1) * 6);
    let o = 0;
    for (let z = 0; z < N - 1; z++) for (let x = 0; x < N - 1; x++) {
      const a = z * N + x, b = a + 1, d = a + N, e = d + 1;
      idx[o++] = a; idx[o++] = d; idx[o++] = e;
      idx[o++] = a; idx[o++] = e; idx[o++] = b;
    }
    const g = new THREE.BufferGeometry();
    g.setAttribute('position', new THREE.BufferAttribute(pos, 3));
    g.setAttribute('normal', new THREE.BufferAttribute(nor, 3));
    g.setAttribute('color', new THREE.BufferAttribute(col, 3));
    g.setAttribute('uv', new THREE.BufferAttribute(uv, 2));
    g.setIndex(new THREE.BufferAttribute(idx, 1));
    const mat = new THREE.MeshBasicMaterial({ vertexColors: true });
    if (this.sat) mat.map = this.sat;
    this.terrain = new THREE.Mesh(g, mat);
    this.terrain.frustumCulled = false;
    this.scene.add(this.terrain);
    this.makeWater();
    this.makeSky();
    this.scene.fog = new THREE.FogExp2(0x9fb4c4, 0.0042);
  }

  makeWater() {
    if (this.water) { this.scene.remove(this.water); this.water.geometry.dispose(); this.water.material.dispose(); }
    const g = new THREE.PlaneGeometry(1600, 1600, 1, 1);
    const m = new THREE.ShaderMaterial({
      transparent: true,
      depthWrite: false,
      uniforms: {
        uTime: { value: 0 },
        uDeep: { value: new THREE.Color(0x0d2a3a) },
        uShallow: { value: new THREE.Color(0x2f6f7d) },
        uSky: { value: new THREE.Color(0x8fb2cc) },
        uSun: { value: SUN_DIR.clone() },
        uFog: { value: new THREE.Color(0x9fb4c4) },
        uFogD: { value: 0.0042 },
      },
      vertexShader: `
        varying vec3 vW;
        void main() {
          vec4 wp = modelMatrix * vec4(position, 1.0);
          vW = wp.xyz;
          gl_Position = projectionMatrix * viewMatrix * wp;
        }`,
      fragmentShader: `
        precision highp float;
        varying vec3 vW;
        uniform float uTime, uFogD;
        uniform vec3 uDeep, uShallow, uSky, uSun;
        void main() {
          vec3 V = normalize(cameraPosition - vW);
          // 两层交叉的正弦：远看是缓缓起伏的涌，近看是细碎的风纹
          float a = sin(vW.x * 0.42 + uTime * 0.7) + sin(vW.z * 0.37 - uTime * 0.55);
          float b = sin(vW.x * 1.9 - uTime * 1.4) + sin(vW.z * 2.3 + uTime * 1.1);
          vec3 n = normalize(vec3((a * 0.012 + b * 0.005), 1.0, (a * 0.009 - b * 0.006)));
          float fres = pow(1.0 - clamp(dot(n, V), 0.0, 1.0), 3.2);
          float spec = pow(max(dot(reflect(-uSun, n), V), 0.0), 90.0);
          float chop = pow(max(dot(reflect(-uSun, n), V), 0.0), 9.0) * 0.16;
          vec3 col = mix(uDeep, uShallow, 0.30 + 0.42 * clamp(dot(n, uSun), 0.0, 1.0));
          col = mix(col, uSky, clamp(fres * 1.25, 0.0, 0.86));
          col += vec3(1.0, 0.94, 0.82) * (spec * 0.9 + chop * 0.5);
          float d = length(cameraPosition - vW);
          float f = 1.0 - exp(-uFogD * uFogD * d * d);
          gl_FragColor = vec4(mix(col, uFog, clamp(f, 0.0, 1.0)), 0.94);
        }`,
    });
    this.water = new THREE.Mesh(g, m);
    this.water.rotation.x = -Math.PI / 2;
    this.water.position.y = SEA_RAW * Y_PER_RAW * this.exaggeration;
    this.water.frustumCulled = false;
    this.water.renderOrder = 1;
    this.scene.add(this.water);
  }

  /** 天空：地平线暖、天顶冷，中间只用蓝-橙两端过渡。
   *  旧的那条 ramp 会路过品红——黄昏整片天发紫，就是"很花、不高级"的第一来源。 */
  makeSky() {
    if (this.sky) { this.scene.remove(this.sky); this.sky.geometry.dispose(); this.sky.material.dispose(); }
    const g = new THREE.SphereGeometry(900, 32, 16);
    const m = new THREE.ShaderMaterial({
      side: THREE.BackSide,
      depthWrite: false,
      fog: false,
      uniforms: {
        uHorizon: { value: new THREE.Color(0xf0d3ac) },
        uZenith: { value: new THREE.Color(0x2c5f92) },
        uSun: { value: SUN_DIR.clone() },
        uSunCol: { value: new THREE.Color(0xfff0d2) },
      },
      vertexShader: `
        varying vec3 vDir;
        void main() { vDir = normalize(position); gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }`,
      fragmentShader: `
        precision highp float;
        varying vec3 vDir;
        uniform vec3 uHorizon, uZenith, uSun, uSunCol;
        void main() {
          float h = clamp(vDir.y * 0.5 + 0.5, 0.0, 1.0);
          vec3 col = mix(uHorizon, uZenith, pow(h, 0.72));
          float sd = max(dot(normalize(vDir), uSun), 0.0);
          col += uSunCol * (pow(sd, 420.0) * 1.6 + pow(sd, 7.0) * 0.22);
          // 地平线以下压向雾色：远山与天相接那一圈不能是两条边
          col = mix(col, uHorizon * 0.86, smoothstep(0.06, -0.25, vDir.y));
          gl_FragColor = vec4(col, 1.0);
        }`,
    });
    this.sky = new THREE.Mesh(g, m);
    this.sky.frustumCulled = false;
    this.sky.renderOrder = -1;
    this.scene.add(this.sky);
  }

  resize() {
    const c = this.canvas;
    const w = c.clientWidth || 800, h = c.clientHeight || 600;
    if (c.width !== w || c.height !== h) {
      c.width = w; c.height = h;
      this.renderer.setSize(w, h, false);
      this.camera.aspect = w / h;
      this.camera.updateProjectionMatrix();
    }
  }

  /** s = { px, py, pz, yaw, pitch, day, dusk }：与旧渲染器同一份状态，
   *  但 py 不再由调用方给整数层——脚底贴在这张连续地面上走。 */
  render(s) {
    this.resize();
    const t = performance.now() / 1000;
    const gy = this.groundY(s.px, s.pz);
    const eye = gy + 1.62 + (s.feet ? s.feet - Math.floor(s.feet) : 0);
    this.camera.position.set(s.px, eye, s.pz);
    this.camera.rotation.set(0, 0, 0, 'YXZ');
    this.camera.rotation.order = 'YXZ';
    this.camera.rotation.y = -(s.yaw - Math.PI / 2);
    this.camera.rotation.x = s.pitch;
    if (this.sky) this.sky.position.copy(this.camera.position);
    if (this.water) this.water.material.uniforms.uTime.value = t;
    const day = s.day == null ? 1 : s.day;
    if (this.sky) {
      const u = this.sky.material.uniforms;
      u.uZenith.value.setHex(0x2c5f92).lerp(new THREE.Color(0x0a1526), 1 - day);
      u.uHorizon.value.setHex(0xf0d3ac).lerp(new THREE.Color(0x243449), 1 - day);
    }
    if (this.water) {
      const u = this.water.material.uniforms;
      u.uSky.value.setHex(0x8fb2cc).lerp(new THREE.Color(0x1b2740), 1 - day);
      u.uFog.value.setHex(0x9fb4c4).lerp(new THREE.Color(0x16202f), 1 - day);
    }
    if (this.scene.fog) this.scene.fog.color.setHex(0x9fb4c4).lerp(new THREE.Color(0x16202f), 1 - day);
    this.renderer.render(this.scene, this.camera);
  }

  dispose() {
    for (const k of ['terrain', 'water', 'sky']) {
      if (this[k]) { this.scene.remove(this[k]); this[k].geometry.dispose(); this[k].material.dispose(); }
    }
    this.renderer.dispose();
  }
}
