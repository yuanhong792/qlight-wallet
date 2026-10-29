import * as THREE from '../ui/vendor/three/three.module.js';

// 纯视觉层：账本与交互判定仍在 world3d.js。这里只画同一片地形，
// 额外的草簇/碎石/远山都不占账本格子（§9.11：像素不可验，位置由脚本可算）。
const SIZE = 256;
const REGION = 96;
const TILE = window.QLSprites?.TERRAIN;
const SEA_Y = Math.floor((window.QLWorld?.RULES?.SEA ?? 86) / 8) + 1;
const TILE_WATER_NEAR = 900;      // 近景水面专用槽位：不属于 QLSprites 的地形枚举，只给这一层用
const WATER_Y = SEA_Y - 0.36;     // 压在远景海平面之上 0.06，两层水面才不会互相闪
const hash = (x, z) => {
  let h = Math.imul(x + 79, 73856093) ^ Math.imul(z + 131, 19349663);
  h = Math.imul(h ^ (h >>> 16), 0x85ebca6b);
  return (h ^ (h >>> 13)) >>> 0;
};
const hash3 = (x, z, k) => hash(x + k * 1013, z - k * 791);
const clamp = (n, a, b) => Math.max(a, Math.min(b, n));

// setColorAt 只有在 USE_COLOR 打开时才乘进片元；没有贴图颜色的几何体补一层白，
// 这样一棵树的树冠/一丛草就能用同一个 InstancedMesh 上色，不必按颜色拆 draw call。
function tintable(geo) {
  const n = geo.attributes.position.count;
  const c = new Float32Array(n * 3).fill(1);
  geo.setAttribute('color', new THREE.BufferAttribute(c, 3));
  return geo;
}

// 建模用的单位形状：所有部件都是"单位网格 × 缩放 × 旋转"，共享同一份顶点，
// 构建时只在 CPU 侧克隆再烘焙到世界坐标，最后按材质归并。
const SHAPES = {
  box: () => new THREE.BoxGeometry(1, 1, 1),
  cyl: () => new THREE.CylinderGeometry(0.5, 0.5, 1, 8, 1),
  cyl6: () => new THREE.CylinderGeometry(0.5, 0.5, 1, 6, 1),
  taper: () => new THREE.CylinderGeometry(0.34, 0.5, 1, 8, 1),
  cone: () => new THREE.ConeGeometry(0.5, 1, 8),
  cone4: () => new THREE.ConeGeometry(0.5, 1, 4),
  ball: () => new THREE.IcosahedronGeometry(0.5, 0),
  ball2: () => new THREE.IcosahedronGeometry(0.5, 1),
  plane: () => new THREE.PlaneGeometry(1, 1),
  disc: () => new THREE.CircleGeometry(0.5, 8),
  basin: () => new THREE.LatheGeometry([
    new THREE.Vector2(0.02, 0.1), new THREE.Vector2(0.5, 0.1), new THREE.Vector2(0.56, 0.36),
    new THREE.Vector2(0.66, 0.37), new THREE.Vector2(0.68, 0.06), new THREE.Vector2(0.6, 0.0),
    new THREE.Vector2(0.02, 0.0),
  ], 8),
};

// 活物与建筑的固定色板：同一个键在两台机器上必须是同一个颜色（§9.11 像素可算）。
const SKIN = {
  deer: 0xb08356, deerPale: 0xd8c2a0, antler: 0xe6d9bd,
  wolf: 0x7b8087, wolfPale: 0xa9aeb3,
  sheep: 0xe7e1d3, sheepDark: 0x403a34,
  rabbit: 0xc8b498, bird: 0x5a83b4, birdPale: 0xdfc88c,
  fish: 0x8fb9cc, fin: 0xc6dfe8,
  dark: 0x35322e, hoof: 0x4a4238, gold: 0xd9a53a,
  wood: 0xa8875c, woodDark: 0x6f573a, stone: 0xc4c8cc, cloth: 0xb4483c, clothPale: 0xe9dcc2,
};
const FLOWER_COLORS = [0xd9564c, 0xe8b23a, 0xb274d0, 0xf0efe6, 0xe886a6];
const WING_COLORS = [0xe8604c, 0xd9a53a, 0x6fb4d9, 0xc77fd9];

// 真实米制定标：一格 = 一米，玩家眼睛高 1.62 米、身体 1.75 米。建模坐标一直按"自己那一格
// 里的 0–1"来写，读起来直观，但按比例整个世界就成了娃娃屋——屋脊 1.2 米、人 0.94 米、
// 树 3 米。这里给每一类物件一次性的放大倍数，把比例钉回现实：
//   立石 1.8 米 · 橡树 7 米 · 木屋脊高 4 米 / 门 1.8 米 · 灯塔 11.6 米 · 篱笆 1.4 米
//   桥面 0.6 米高 / 栏杆 0.9 米 · 旅人 1.75 米 · 鹿肩高 1.35 米 · 蝴蝶翼展 12 厘米
// 唯独玩家自己叠上去的方块不放大：它占的就是这一格，撑得住才叫踩得住（见 support()）。
const SCL = {
  stone: 2.6, oak: 2.5, flower: 1, cabin: 3.2, bridge: 2.4, light: 4.0, fence: 1.7,
  animal: { 8: 1.5, 9: 0.62, 10: 0.9, 11: 0.45, 12: 1, 13: 1.3, 14: 1.85, 15: 1.15 },
  station: { pool: 1.7, trader: 2.0, dust: 1.6 },
};

/** 部件归并器：静态件把整片区域的同类部件烘进一个几何体（几种材质 = 几个 draw call），
    活物则各自挂一个 group，动画只改 transform，绝不逐帧重建网格。 */
class Parts {
  constructor(owner, node) {
    this.owner = owner;
    this.node = node || null;
    this.world = !node;
    this.buckets = new Map();
    this.nodes = new Map();
    this.ox = 0; this.oy = 0; this.oz = 0; this.yaw = 0; this.us = 1;
    this._f = new THREE.Matrix4();
    this._m = new THREE.Matrix4();
    this._box = new THREE.Box3();
    this._q = new THREE.Quaternion();
    this._e = new THREE.Euler();
    this._p = new THREE.Vector3();
    this._s = new THREE.Vector3();
  }

  // us = 这件模型的真实米制缩放：账本给的是一个 1 米格子里的坐标，而一栋屋子的脊高
  // 是 4 米、一个人是 1.75 米。坐标全部按"格子"写死会让整个世界变成娃娃屋，
  // 所以定标只在这一处乘一遍，建模坐标继续按 0–1 的格子读。
  frame(x, y, z, yaw, us) {
    this.ox = x; this.oy = y; this.oz = z; this.yaw = yaw || 0; this.us = us || 1;
    return this;
  }

  /** 动画枢轴：之后带同一个 tag 的部件都写在这个节点的局部坐标里 */
  pivot(tag, x, y, z) {
    if (!this.node) return null;
    const g = new THREE.Group();
    g.position.set(x * this.us, y * this.us, z * this.us);
    this.node.add(g);
    this.nodes.set(tag, g);
    return g;
  }

  add(shape, mat, x, y, z, sx, sy, sz, o = {}) {
    let tag = '';
    if (this.node && o.tag && this.nodes.has(o.tag)) tag = o.tag;
    const key = mat.id + '|' + tag;
    let b = this.buckets.get(key);
    if (!b) {
      b = { mat, cast: true, chunks: [], verts: 0, node: tag ? this.nodes.get(tag) : this.node };
      this.buckets.set(key, b);
    }
    if (o.cast === false) b.cast = false;
    const src = this.owner.geo(shape);
    const u = this.us;
    this._e.set(o.rx || 0, o.ry || 0, o.rz || 0, 'XYZ');
    this._q.setFromEuler(this._e);
    this._m.compose(this._p.set(x * u, y * u, z * u), this._q, this._s.set(sx * u, sy * u, sz * u));
    // 一歪一斜，部件的包围盒下沿就会探进地里：按它自己的最低点顶回地面。
    // 地表平面以下不留顶点，玩家才看不到"穿模"，也不会和草皮共面闪。
    const floor = (o.floor || 0) * u - (b.node && b.node !== this.node ? b.node.position.y : 0);
    this._box.copy(src.boundingBox).applyMatrix4(this._m);
    const sink = this._box.min.y - floor;
    const g = src.clone();
    g.applyMatrix4(this._m);
    if (sink < 0) g.translate(0, -sink, 0);
    if (this.world) {
      this._f.makeRotationY(this.yaw);
      this._f.setPosition(this.ox, this.oy, this.oz);
      g.applyMatrix4(this._f);
    }
    const a = g.attributes;
    b.chunks.push([a.position.array, a.normal.array, a.uv.array]);
    b.verts += a.position.count;
    g.dispose();
    return null;
  }

  // 圆柱横放：绕 Z 转 90°，长度落在 X 上（圆木、栏杆、脖颈都靠它）
  log(shape, mat, x, y, z, r, len, o = {}) {
    return this.add(shape, mat, x, y, z, r * 2, len, r * 2, { ...o, rz: (o.rz || 0) + Math.PI / 2 });
  }

  finish(parent) {
    for (const b of this.buckets.values()) {
      if (!b.chunks.length) continue;
      const nv = b.verts * 3;
      const P = new Float32Array(nv), N = new Float32Array(nv), U = new Float32Array(b.verts * 2);
      let o3 = 0, o2 = 0;
      for (const [p, n, u] of b.chunks) { P.set(p, o3); N.set(n, o3); U.set(u, o2); o3 += p.length; o2 += u.length; }
      const g = new THREE.BufferGeometry();
      g.setAttribute('position', new THREE.BufferAttribute(P, 3));
      g.setAttribute('normal', new THREE.BufferAttribute(N, 3));
      g.setAttribute('uv', new THREE.BufferAttribute(U, 2));
      g.computeBoundingSphere();
      const mesh = new THREE.Mesh(g, b.mat);
      mesh.castShadow = b.cast;
      mesh.receiveShadow = true;
      (b.node || parent || this.owner.props).add(mesh);
    }
    this.buckets.clear();
  }
}

export class ThreeWorld {
  constructor(canvas) {
    if (!TILE) throw new Error('Three.js 地形图集未加载');
    this.canvas = canvas;
    this.renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: false, preserveDrawingBuffer: true, powerPreference: 'high-performance' });
    this.renderer.setPixelRatio(Math.min(devicePixelRatio || 1, 1.7));
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    // ACES 的高光肩会把中间调整体往白里推、顺手把饱和度吃掉——贴图本来 0.46 明度的草地
    // 被顶到 0.85 以上，水、石、草就全挤在同一档白里。Neutral 保饱和，光强另调。
    this.renderer.toneMapping = THREE.NeutralToneMapping;
    this.renderer.toneMappingExposure = 1.0;
    this.renderer.shadowMap.enabled = true;
    this.renderer.shadowMap.type = THREE.PCFSoftShadowMap;
    this.scene = new THREE.Scene();
    this.scene.fog = new THREE.FogExp2(0xb9cde0, 0.0105);
    this.camera = new THREE.PerspectiveCamera(69.9, 1, 0.05, 700);
    this.clock = 0;
    this.drift = 0;
    this.heights = null;
    this.blocks = null;
    this.dug = new Map();
    this.entities = [];
    this.resources = [];
    this.stations = [];
    this.stationSig = null;
    this.entityAt = new Map();
    this.anim = [];
    this.region = null;
    // 固定出生地清场：第一眼不能是树干贴脸（旧版相机正好站在树里）
    this.spawn = { x: 128, z: 128 };
    this.dirty = true;
    this.terrain = new THREE.Group();
    this.props = new THREE.Group();
    this.scatter = new THREE.Group();
    // 远景地表：整张 256×256 只铺顶面，一次画好、跟着世界数据重建，不跟着相机重建。
    // 没有它，96 格近景之外就是空的，相机里那片跟着走的"海"会一直顶在眼前——
    // 世界明明有陆地，第一眼却像被水泡着。
    this.farGround = new THREE.Group();
    this.farDirty = true;
    // 兑换池/摊位/光尘灯是账本里的地标，不是签名实体：单独一层，重建与释放都跟着区域走
    this.stationLayer = new THREE.Group();
    this.scene.add(this.terrain, this.props, this.scatter, this.farGround, this.stationLayer);
    this.makeAtmosphere();
    this.makeEnvironment();
    this.makeMaterials();
    this.makeEntityKit();
    this.makeBackdrop();
    this.makeHands();
    const edges = new THREE.EdgesGeometry(new THREE.BoxGeometry(1.025, 1.025, 1.025));
    this.highlight = new THREE.LineSegments(edges, new THREE.LineBasicMaterial({ color: 0xffdc83, depthTest: false, transparent: true, opacity: 0.86 }));
    this.highlight.renderOrder = 100;
    this.highlight.visible = false;
    this.scene.add(this.highlight);
  }

  setSpawn(x, z) {
    if (x == null || z == null) return;
    this.spawn = { x, z };
    this.dirty = true;
  }

  /** 只推实体的一条路：世界快照整体走 setWorld，这里给 UI 一个单独更新签名的入口。
   *  key 的算法与 setWorld 完全一致，两条路混用也不会互相打脸。 */
  setEntities(entities = [], address = '') {
    const key = entities.map((e) => `${e.entity_id_hex}:${e.x}:${e.y}:${e.layer}:${e.kind}:${!!e.released}`).join('|');
    if (key === this.entityKey && address === this.address) return;
    this.entities = entities;
    this.address = address;
    this.entityKey = key;
    this.dirty = true;
  }

  makeAtmosphere() {
    this.sky = new THREE.Mesh(new THREE.SphereGeometry(380, 32, 16), new THREE.ShaderMaterial({
      side: THREE.BackSide, depthWrite: false, fog: false,
      uniforms: { sunHeight: { value: 0.25 }, night: { value: 0 } },
      vertexShader: 'varying vec3 vDir; void main(){vDir=position; gl_Position=projectionMatrix*modelViewMatrix*vec4(position,1.0);}',
      fragmentShader: `varying vec3 vDir; uniform float sunHeight; uniform float night;
        // 天空原本绕过了渲染器的色彩管线：地形走 tone mapping、天空走原值，
        // 同一天光算两次，地面发白而天还饱和——玩家说的"两层皮"就是这个。
        // 现在把作者值转成线性、和地形共用同一条输出通道。
        vec3 s2l(vec3 s){return pow(s,vec3(2.2));}
        void main(){vec3 d=normalize(vDir); float h=smoothstep(-0.16,0.62,d.y);
          vec3 low=mix(s2l(vec3(1.0,0.53,0.26)),s2l(vec3(0.55,0.74,0.97)),sunHeight);
          vec3 high=mix(s2l(vec3(0.17,0.13,0.40)),s2l(vec3(0.11,0.20,0.50)),sunHeight);
          vec3 c=mix(low,high,h); c=mix(c,s2l(vec3(0.010,0.016,0.062)),night);
          vec3 sd=normalize(vec3(-0.62,0.17,-0.76));
          float sun=pow(max(dot(d,sd),0.0),130.0);
          float glow=pow(max(dot(d,sd),0.0),7.0);
          c+=s2l(vec3(1.0,0.60,0.24))*sun*2.6*(1.0-night);
          c+=s2l(vec3(1.0,0.44,0.12))*glow*(1.0-sunHeight)*0.35*(1.0-night*0.8);
          // 一点点抖动，8bit 渐变才不会有色带
          float dith=fract(sin(dot(gl_FragCoord.xy,vec2(12.9898,78.233)))*43758.5453)-0.5;
          c+=dith*0.004;
          gl_FragColor=vec4(c,1.0);
          #include <tonemapping_fragment>
          #include <colorspace_fragment> }`,
    }));
    this.sky.frustumCulled = false;
    this.scene.add(this.sky);

    // 太阳光斑：画在天空球里会被雾和裁剪吃掉，用一个跟随相机的 Sprite 更稳
    const c = document.createElement('canvas');
    c.width = c.height = 128;
    const g = c.getContext('2d');
    const grd = g.createRadialGradient(64, 64, 4, 64, 64, 64);
    grd.addColorStop(0, 'rgba(255,248,222,1)');
    grd.addColorStop(0.25, 'rgba(255,226,160,.95)');
    grd.addColorStop(0.6, 'rgba(255,190,110,.22)');
    grd.addColorStop(1, 'rgba(255,180,90,0)');
    g.fillStyle = grd;
    g.fillRect(0, 0, 128, 128);
    const sunTex = new THREE.CanvasTexture(c);
    sunTex.colorSpace = THREE.SRGBColorSpace;
    this.sunSprite = new THREE.Sprite(new THREE.SpriteMaterial({ map: sunTex, transparent: true, depthWrite: false, fog: false, blending: THREE.AdditiveBlending }));
    this.sunSprite.scale.set(38, 38, 1);
    this.sunSprite.renderOrder = -1;
    this.sunDir = new THREE.Vector3(-0.62, 0.17, -0.76).normalize();
    this.scene.add(this.sunSprite);

    // 星空：夜里只露一点点，白天完全隐藏
    const starPos = new Float32Array(520 * 3);
    for (let i = 0; i < 520; i++) {
      const a = hash(i, 7) / 4294967296 * Math.PI * 2;
      const b = 0.08 + (hash(i, 13) % 1000) / 1000 * 1.2;
      starPos[i * 3] = Math.cos(a) * Math.cos(b) * 350;
      starPos[i * 3 + 1] = Math.sin(b) * 350;
      starPos[i * 3 + 2] = Math.sin(a) * Math.cos(b) * 350;
    }
    const starGeo = new THREE.BufferGeometry();
    starGeo.setAttribute('position', new THREE.BufferAttribute(starPos, 3));
    this.stars = new THREE.Points(starGeo, new THREE.PointsMaterial({ color: 0xe8f0ff, size: 1.6, sizeAttenuation: false, transparent: true, opacity: 0, depthWrite: false, fog: false }));
    this.stars.frustumCulled = false;
    this.scene.add(this.stars);

    // 光强按 r155+ 的物理单位算：漫反射 = 反照率 × 光强 / π，所以"1.6 的太阳"
    // 只能打出老光照模型三分之一的光——整幅画面压在 sRGB 30 上下，
    // 树、草地、阴影全糊成一团黑绿，唯一还亮着的是反射天光的水面。
    // 玩家说的"地上有蓝色的""地面上空的""树分不清"其实是同一个 bug：场景欠曝 π 倍。
    // 补回 π 之后，材质的明度阶梯才重新可见（正午草地落在 100–160，不是 20–60）。
    this.hemi = new THREE.HemisphereLight(0xbcd0e2, 0x6d6650, 1.1 * Math.PI);
    this.scene.add(this.hemi);
    this.sun = new THREE.DirectionalLight(0xffd0a0, 1.6 * Math.PI);
    this.sun.position.set(-40, 44, -58);
    this.sun.castShadow = true;
    this.sun.shadow.mapSize.set(2048, 2048);
    this.sun.shadow.camera.left = this.sun.shadow.camera.bottom = -62;
    this.sun.shadow.camera.right = this.sun.shadow.camera.top = 62;
    this.sun.shadow.camera.near = 1;
    this.sun.shadow.camera.far = 200;
    this.sun.shadow.bias = -0.0005;
    this.sun.shadow.normalBias = 0.02;
    this.sunTarget = new THREE.Object3D();
    this.scene.add(this.sun, this.sunTarget);
    this.sun.target = this.sunTarget;
  }

  // 一张极小的天光渐变当环境贴图：没有它，水面只能反射黑色，
  // 黄昏的橙光进不到水里，整片水就是一块深蓝塑料。
  makeEnvironment() {
    const c = document.createElement('canvas');
    c.width = 128; c.height = 64;
    const g = c.getContext('2d');
    const grd = g.createLinearGradient(0, 0, 0, 64);
    grd.addColorStop(0, '#2c4574');
    grd.addColorStop(0.42, '#7f9cc2');
    grd.addColorStop(0.55, '#e6a877');
    grd.addColorStop(0.7, '#5b6a58');
    grd.addColorStop(1, '#2a3a33');
    g.fillStyle = grd;
    g.fillRect(0, 0, 128, 64);
    const tex = new THREE.CanvasTexture(c);
    tex.mapping = THREE.EquirectangularReflectionMapping;
    tex.colorSpace = THREE.SRGBColorSpace;
    const pm = new THREE.PMREMGenerator(this.renderer);
    this.envMap = pm.fromEquirectangular(tex).texture;
    pm.dispose();
    tex.dispose();
    this.scene.environment = this.envMap;
    this.scene.environmentIntensity = 0.5;
  }

  makeMaterials() {
    const loader = new THREE.TextureLoader();
    // 贴图是异步的：没落地就画，材质采样到空纹理，整片世界是黑的——这不是光照 bug，是时序。
    // 世界页的封面淡出等这个计数：玩家不该先看到一帧全黑再"淡入"进画面。
    this.texturesLeft = 0;
    this.texturesReady = false;
    this.warm = 0;
    const tile = (name) => {
      this.texturesLeft++;
      const t = loader.load(`./assets/world-tiles/${name}.png`, () => { if (--this.texturesLeft <= 0) this.texturesReady = true; });
      t.colorSpace = THREE.SRGBColorSpace;
      t.magFilter = THREE.NearestFilter;
      t.minFilter = THREE.LinearMipmapLinearFilter;
      t.anisotropy = Math.min(8, this.renderer.capabilities.getMaxAnisotropy());
      // 近看保持像素（magFilter 仍是 Nearest），远看交给 mipmap：不推 bias 的话，
      // 一格贴图在几十格外仍然铺出十几个碎点，玩家说的"隔远了还很具体"就是这个噪点
      t.bias = 0.5;
      return t;
    };
    const names = {
      [TILE.GRASS_TOP]: 'grass', [TILE.GRASS_SIDE]: 'grass_side',
      [TILE.DIRT]: 'dirt', [TILE.STONE]: 'stone', [TILE.COBBLE]: 'brick',
      [TILE.MOSSY]: 'brick', [TILE.SAND]: 'sand', [TILE.SNOW_TOP]: 'snow',
      [TILE.SNOW_SIDE]: 'snow', [TILE.WATER]: 'water', [TILE.DEEP]: 'deep',
      [TILE.ORE_GOLD]: 'stone', [TILE.ORE_IRON]: 'stone',
      [TILE.ORE_CRYSTAL]: 'stone', [TILE.ERA_GOLD]: 'stone', [TILE.BEDROCK]: 'deep',
    };
    this.materials = new Map();
    // 底色只做"这一类材质往哪个方向偏一点"，不能当滤镜用：
    // 贴图自己的明度阶梯（雪 > 沙 > 石 > 草 > 土 > 水）就是画面的骨架，再乘一层灰就全糊了
    const TINTS = new Map([
      [TILE.SAND, 0xf0e3c6], [TILE.SNOW_TOP, 0xeef4f8], [TILE.SNOW_SIDE, 0xe6eef4],
      [TILE.STONE, 0xe4e9ef], [TILE.COBBLE, 0xdde3ea], [TILE.MOSSY, 0xdcead8],
      [TILE.DIRT, 0xf3e6d6], [TILE.DEEP, 0xd8dde4], [TILE.BEDROCK, 0xcfd6de],
      [TILE.ORE_GOLD, 0xf6e3b4], [TILE.ORE_IRON, 0xece0d6], [TILE.ORE_CRYSTAL, 0xd8f0f8],
      [TILE.ERA_GOLD, 0xffe6a8],
    ]);
    for (const id of Object.values(TILE)) {
      const water = id === TILE.WATER;
      const texture = tile(names[id] || 'stone');
      if (water) {
        texture.wrapS = texture.wrapT = THREE.RepeatWrapping;
        // 水的底色必须压得住：贴图本身偏浅青，配白底 + 高环境反射，正午就镜成一片白雾
        // 掠射角那一道菲涅尔是"远处一整片白蓝镜子"的来源：specularIntensity 整体压低
        // 高光，水才留在"深蓝、有一点反光"这一档，而不是抢走地面的亮度。
        this.waterMat = new THREE.MeshStandardMaterial({ map: texture, vertexColors: true, color: 0x1b4457,
          roughness: 0.5, metalness: 0.02, specularIntensity: 0.45,
          transparent: true, opacity: 0.97, side: THREE.DoubleSide });
        this.materials.set(id, this.waterMat);
        continue;
      }
      this.materials.set(id, new THREE.MeshStandardMaterial({ map: texture, vertexColors: true,
        color: TINTS.get(id) ?? 0xffffff, roughness: id === TILE.SAND ? 0.86 : 0.94, metalness: 0 }));
    }
    this.bark = new THREE.MeshStandardMaterial({ map: tile('bark'), roughness: 1 });
    // 近景水面：不透明，深度越深顶点色越暗。半透明会和身后那张大海平面叠成两层，
    // 近处一片深、远处一片浅，玩家就会说"分不清哪里是水、哪里是地"。
    const nearWater = tile('water');
    nearWater.wrapS = nearWater.wrapT = THREE.RepeatWrapping;
    this.waterNearMat = new THREE.MeshStandardMaterial({ map: nearWater, vertexColors: true,
      color: 0x1f4c60, roughness: 0.32, metalness: 0.03, specularIntensity: 0.5 });
    this.materials.set(TILE_WATER_NEAR, this.waterNearMat);
    this.leaves = new THREE.MeshStandardMaterial({ map: tile('leaves'), vertexColors: true, roughness: 1 });
    // 树冠/树干的贴图按米平铺：整张贴布拉在一面 3 米宽的盒子上就是一块糊绿，
    // 玩家看到的"树"变成一摞绿色平板——贴图越具体，拉得越狠越假
    const leafTile = tile('leaves');
    leafTile.wrapS = leafTile.wrapT = THREE.RepeatWrapping;
    leafTile.repeat.set(2.6, 1.1);
    // 树冠必须比草地暗一档、偏冷：owner 的 grass.png 均值 (94,128,64)、leaves.png (78,122,58)
    // 是同一块绿，两层贴图本身分不开（也不该由我重画），能分的只有材质底色和几何。
    // 底色乘 0.6 把冠压成"叶层内部的阴影"，地面留原值——抬头是暗冠、低头是亮地。
    this.leavesBlock = new THREE.MeshStandardMaterial({ map: leafTile, vertexColors: true,
      color: 0x99a185, roughness: 1 });
    this.roof = new THREE.MeshStandardMaterial({ map: tile('roof'), roughness: 1 });
    this.planks = new THREE.MeshStandardMaterial({ map: tile('planks'), roughness: 1 });
    this.lantern = new THREE.MeshStandardMaterial({ color: 0xffb43e, emissive: 0xff9a22, emissiveIntensity: 2.6 });
    this.cactus = new THREE.MeshStandardMaterial({ color: 0xffffff, vertexColors: true, roughness: 0.95 });
    this.stone = new THREE.MeshStandardMaterial({ map: tile('stone'), roughness: 1 });
    this.soil = new THREE.MeshStandardMaterial({ map: tile('dirt'), roughness: 1 });
    this.grassMat = new THREE.MeshStandardMaterial({ vertexColors: true, side: THREE.DoubleSide, roughness: 1 });
    this.trunkMat = new THREE.MeshStandardMaterial({ map: tile('bark'), vertexColors: true, roughness: 1 });
    this.trunkMat.map.wrapS = this.trunkMat.map.wrapT = THREE.RepeatWrapping;
    this.trunkMat.map.repeat.set(1, 4);
    this.rockMat = new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 1 });
    this.stemMat = new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 1 });
    this.capMat = new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 0.9 });
    this.lilyMat = new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 0.55 });
    this.reedMat = new THREE.MeshStandardMaterial({ vertexColors: true, side: THREE.DoubleSide, roughness: 1 });
    this.flowerTexture = new THREE.CanvasTexture(window.QLSprites.icon(2, 32));
    this.flowerTexture.magFilter = THREE.NearestFilter;
    this.flower = new THREE.MeshBasicMaterial({ map: this.flowerTexture, alphaTest: 0.45, side: THREE.DoubleSide, transparent: true });
    this.tileOf = tile;
  }

  // 建模材质：贴图材质按色相克隆一份（关掉 vertexColors 才能逐件换色而不重刷顶点色），
  // 纯色材质按 hex 缓存——整个场景的材质/程序数量才是可控的。
  mat(base, hex) {
    const key = base.uuid + '|' + (hex == null ? '' : hex);
    let m = this.paintCache.get(key);
    if (!m) {
      m = base.clone();
      m.vertexColors = false;
      if (hex != null) m.color.setHex(hex);
      this.paintCache.set(key, m);
    }
    return m;
  }

  paint(hex, o = {}) {
    const key = `${hex}|${o.flat ? 1 : 0}|${o.rough ?? 0.86}|${o.side ? 1 : 0}|${o.em || 0}|${o.op ?? 1}|${o.metal || 0}`;
    let m = this.plainCache.get(key);
    if (!m) {
      m = new THREE.MeshStandardMaterial({ color: hex, roughness: o.rough ?? 0.86, metalness: o.metal ?? 0, flatShading: !!o.flat });
      if (o.side) m.side = THREE.DoubleSide;
      if (o.op < 1) { m.transparent = true; m.opacity = o.op; }
      if (o.em) { m.emissive.setHex(o.em); m.emissiveIntensity = o.emi ?? 1.6; }
      this.plainCache.set(key, m);
    }
    return m;
  }

  geo(name) {
    let g = this.entGeo.get(name);
    if (!g) {
      const raw = SHAPES[name]();
      g = raw.index ? raw.toNonIndexed() : raw;
      if (g !== raw) raw.dispose();
      g.userData.shared = true;
      g.computeBoundingBox();          // 落地前的包围盒：穿地检查靠它
      this.entGeo.set(name, g);
    }
    return g;
  }

  makeEntityKit() {
    this.entGeo = new Map();
    this.paintCache = new Map();
    this.plainCache = new Map();
    this.brickMat = new THREE.MeshStandardMaterial({ map: this.tileOf('brick'), roughness: 1 });
    this.kitMats = [
      this.brickMat,
      // 灯焰/窗火/晶体/池面：整层共用一份材质，render() 里一次脉冲就够，不逐实体记账
      (this.glass = new THREE.MeshStandardMaterial({ color: 0xfff2cf, emissive: 0xffa63a, emissiveIntensity: 2.2, roughness: 0.5 })),
      (this.ember = new THREE.MeshStandardMaterial({ color: 0xffb04a, emissive: 0xff6a1e, emissiveIntensity: 3.1, roughness: 0.7 })),
      (this.crystal = new THREE.MeshStandardMaterial({ color: 0xdcf7ff, emissive: 0x6fe0ff, emissiveIntensity: 2.5, roughness: 0.24, metalness: 0.1 })),
      (this.mirror = new THREE.MeshStandardMaterial({ color: 0xc3e6f0, emissive: 0x2f718c, emissiveIntensity: 0.8, roughness: 0.08, metalness: 0.6, transparent: true, opacity: 0.92, side: THREE.DoubleSide })),
      (this.mote = new THREE.PointsMaterial({ color: 0xc3f4ff, size: 0.1, sizeAttenuation: true, transparent: true, opacity: 0.85, depthWrite: false, blending: THREE.AdditiveBlending })),
    ];
  }

  /** 物件的脚在哪：support() 说玩家踩在哪就钉在哪。
   *  地表层及以下的签名件跟着当前地表走（挖了坑就一起落下去，不留悬空），
   *  地表以上自己叠的层保持绝对高度（空中的塔不会因脚下被挖而塌进坑里）。 */
  anchor(cx, cz, layer) {
    const ground = this.level(cx, cz) + 1;
    return layer > (this.heights[cz * SIZE + cx] >> 3) ? layer + 1 : ground;
  }

  makeBackdrop() {
    // 远景全部画在世界坐标里，跟着相机平移：地图只有 256×256，地平线不能空着
    this.backdrop = new THREE.Group();
    this.clouds = new THREE.Group();
    this.scene.add(this.backdrop, this.clouds);

    const sea = new THREE.Mesh(
      new THREE.PlaneGeometry(1800, 1800),
      new THREE.MeshStandardMaterial({ color: 0x173f52, roughness: 0.52, metalness: 0, envMapIntensity: 0.35, transparent: true, opacity: 0.97 })
    );
    sea.rotation.x = -Math.PI / 2;
    sea.position.y = SEA_Y - 0.42;
    sea.receiveShadow = false;
    this.backdrop.add(sea);
    this.sea = sea;

    // 这里原来站着 310 个我凭空造的圆锥：一圈远山、一圈主峰、一圈树线。
    // 它们不在链算出来的世界里，却把地平线占满了——玩家说"世界怎么被山围着"，
    // 因为那圈山确实是画的，不是长出来的。一个口径写着不可作假的世界，
    // 地平线上不能站假东西。地平线交给真实数据：整张 256×256 都由 buildFarGround
    // 用真贴图铺好，世界的边界就是它能被看见的样子。

    // 云：一簇低面数小球，而不是几块巨大的板子
    const puff = tintable(new THREE.IcosahedronGeometry(1, 0));
    const cloudMat = new THREE.MeshBasicMaterial({ vertexColors: true, color: 0xffffff, fog: false, transparent: true, opacity: 0.42 });
    this.cloudMat = cloudMat;
    const puffs = [];
    for (let i = 0; i < 10; i++) {
      const a = i * 2.39996 + (hash(i, 97) % 100) / 400;
      const r = 110 + (hash(i, 101) % 120);
      const y = 88 + (hash(i, 103) % 26);
      const n = 3 + (hash(i, 107) % 3);
      for (let j = 0; j < n; j++) {
        const s = 3.6 + (hash3(i, j, 1) % 26) / 10;
        puffs.push({
          p: [Math.cos(a) * r + (hash3(i, j, 2) % 90) / 10 - 4.5, y + (hash3(i, j, 3) % 12) / 10, Math.sin(a) * r + (hash3(i, j, 4) % 90) / 10 - 4.5],
          s: [s * 1.9, s * 0.6, s * 1.1], c: 0xffffff,
        });
      }
    }
    const clouds = this.instanced(puff, cloudMat, puffs, { receive: false });
    if (clouds) this.clouds.add(clouds);
  }

  instanced(geo, mat, items, opts = {}) {
    if (!items.length) return null;
    const mesh = new THREE.InstancedMesh(geo, mat, items.length);
    const d = new THREE.Object3D();
    const c = new THREE.Color();
    items.forEach((it, i) => {
      d.position.set(it.p[0], it.p[1], it.p[2]);
      const r = it.r || [0, 0, 0];
      d.rotation.set(r[0], r[1], r[2]);
      const s = it.s == null ? 1 : it.s;
      if (Array.isArray(s)) d.scale.set(s[0], s[1], s[2]); else d.scale.set(s, s, s);
      d.updateMatrix();
      mesh.setMatrixAt(i, d.matrix);
      if (it.c != null) mesh.setColorAt(i, c.set(it.c));
    });
    mesh.instanceMatrix.needsUpdate = true;
    if (mesh.instanceColor) mesh.instanceColor.needsUpdate = true;
    if (opts.cast !== false) mesh.castShadow = true;
    mesh.receiveShadow = opts.receive !== false;
    return mesh;
  }

  setWorld(heights, blocks, entities = [], dug = [], resources = []) {
    const dugKey = dug.map((d) => `${d.x}:${d.y}:${d.removed}`).join('|');
    const entityKey = entities.map((e) => `${e.entity_id_hex}:${e.x}:${e.y}:${e.layer}:${e.kind}:${!!e.released}`).join('|');
    const resourceKey = resources.map((r) => `${r.x}:${r.y}:${r.kind}:${!!r.taken}`).join('|');
    if (this.heights !== heights || this.blocks !== blocks || this.dugKey !== dugKey || this.entityKey !== entityKey || this.resourceKey !== resourceKey) {
      this.heights = heights; this.blocks = blocks; this.entities = entities;
      this.dugKey = dugKey; this.entityKey = entityKey; this.resourceKey = resourceKey;
      this.dug = new Map(dug.map((d) => [d.y * SIZE + d.x, d.removed | 0]));
      this.resources = resources;
      this.dirty = true;
      this.farDirty = true;
    }
  }

  level(x, z) {
    if (x < 0 || z < 0 || x >= SIZE || z >= SIZE) return -1;
    const k = z * SIZE + x;
    return Math.max(1, (this.heights[k] >> 3) - (this.dug.get(k) || 0));
  }

  tile(x, z, depth, side) {
    const kind = this.blocks[z * SIZE + x] & 7;
    if (this.blocks[z * SIZE + x] & 16) return TILE.ERA_GOLD;
    if (kind === 0) return TILE.WATER;
    if (depth > 9) return (hash(x, z + depth) % 11 === 0 ? TILE.ORE_GOLD : TILE.DEEP);
    if (depth > 3) return TILE.STONE;
    if (depth > 0) return TILE.DIRT;
    if (kind === 2) return side ? TILE.SNOW_SIDE : TILE.SNOW_TOP;
    if (kind === 1 || kind === 4) return TILE.SAND;
    return side ? TILE.GRASS_SIDE : TILE.GRASS_TOP;
  }

  clearGroup(group) {
    while (group.children.length) {
      const child = group.children[0]; group.remove(child);
      // 共享的单位几何体被到处复用，释放一次就会让后面所有模型重建缓冲
      child.traverse((o) => { if (o.geometry && !o.geometry.userData.shared) o.geometry.dispose(); });
    }
  }

  // 体素 AO：相邻列比顶面高就在对应的角上压暗，坑壁与树下立刻有体积感
  ao(s1, s2, corner) {
    if (s1 && s2) return 0.68;
    return 1 - 0.1 * ((s1 ? 1 : 0) + (s2 ? 1 : 0)) - 0.05 * (corner ? 1 : 0);
  }

  /** 面桶：把逐格生成的四边形按贴图槽位归组，最后每槽一个 draw call */
  bucketer() {
    const buckets = new Map();
    const order = [0, 1, 2, 0, 2, 3];
    return {
      buckets,
      face(id, p, n, shades, variant) {
        if (!buckets.has(id)) buckets.set(id, { pos: [], normal: [], uv: [], color: [] });
        const b = buckets.get(id);
        const usesArray = Array.isArray(shades);
        for (const i of order) {
          b.pos.push(...p[i]); b.normal.push(...n);
          b.uv.push(i === 1 || i === 2 ? 1 : 0, i >= 2 ? 1 : 0);
          const sh = (usesArray ? shades[i] : shades) * variant;
          // 只乘灰阶会把阴影闷成泥（像素画的 hue shift 规矩：暗的一档往冷、往饱和走，
          // 亮的一档往暖、往白走，中间那一档才是这块材质本来的颜色）。
          // 顶点色在线性空间相乘，等价于打一盏偏色的天光，材质之间才分得开。
          let r, g, bl;
          if (sh < 1) { const t = Math.min(1, (1 - sh) * 2.4); r = 1 - 0.22 * t; g = 1 - 0.08 * t; bl = 1 + 0.20 * t; }
          else { const t = Math.min(1, (sh - 1) * 3.2); r = 1 + 0.14 * t; g = 1 + 0.02 * t; bl = 1 - 0.16 * t; }
          b.color.push(r, g, bl);
        }
      },
    };
  }

  commit(buckets, group) {
    for (const [id, b] of buckets) {
      const g = new THREE.BufferGeometry();
      g.setAttribute('position', new THREE.Float32BufferAttribute(b.pos, 3));
      g.setAttribute('normal', new THREE.Float32BufferAttribute(b.normal, 3));
      g.setAttribute('uv', new THREE.Float32BufferAttribute(b.uv, 2));
      g.setAttribute('color', new THREE.Float32BufferAttribute(b.color, 3));
      g.computeBoundingSphere();
      const mesh = new THREE.Mesh(g, this.materials.get(id));
      mesh.receiveShadow = true;
      group.add(mesh);
    }
  }

  /** 远景地表：整张地图一次铺好。顶面 + 一格高的立面（远处看不出层数，也不需要挖坑细节），
      比近景低 5 厘米，所以近景永远盖住它。只铺顶面不行——地形一有台阶，缝里就漏出
      跟着相机走的那片海，玩家看到的就是"陆地泡在水里"。 */
  buildFarGround() {
    if (!this.heights || !this.blocks) return;
    this.clearGroup(this.farGround);
    this.farDirty = false;
    const bk = this.bucketer();
    const dirs = [[0, -1, [0, 0, -1]], [0, 1, [0, 0, 1]], [1, 0, [1, 0, 0]], [-1, 0, [-1, 0, 0]]];
    for (let z = 0; z < SIZE; z++) {
      for (let x = 0; x < SIZE; x++) {
        if (this.dug.has(z * SIZE + x)) continue; // 挖开的坑交给近景，远景铺顶会把坑盖平
        const lv = this.level(x, z);
        // 远景也只有一个水位：水格的顶面不是"它自己那层的地板"，而是全图共用的那条水位线
        const wet = (this.blocks[z * SIZE + x] & 7) === 0;
        // 比近景水位再低 3 厘米：两层水面重叠的正是相机周围那 96 格，不等高才不会闪
        const y = wet ? WATER_Y - 0.03 : lv + 1 - 0.05;
        // 海拔也进明度：越高的台地越亮越暖，越低越暗越冷——一眼就能读出地势，
        // 而不是靠贴图猜（远景整张地图都走这一档，山脊线自己就浮出来了）
        // 水面不参与海拔明度阶梯，也不参与每格随机：一格一档随机蓝铺开来就是
        // "地上有一块块蓝色"，而真实的水面在远处是一整块、只有深浅没有花斑。
        const variant = wet ? 0.6 : 0.9 + (hash(x, z) % 13) / 100 + Math.min(0.14, Math.max(0, lv - SEA_Y) * 0.009);
        bk.face(this.tile(x, z, 0, false), [[x, y, z], [x + 1, y, z], [x + 1, y, z + 1], [x, y, z + 1]],
          [0, 1, 0], 1, variant);
        for (const [dx, dz, n] of dirs) {
          const nx = x + dx, nz = z + dz;
          const lvN = this.level(nx, nz);
          const footY = (this.blocks[nz * SIZE + nx] & 7) === 0 ? WATER_Y - 0.03 : lvN + 1 - 0.05;
          if (footY >= y) continue;
          const h = y - footY;
          const p = dz === -1 ? [[x, footY, z], [x + 1, footY, z], [x + 1, y, z], [x, y, z]]
            : dz === 1 ? [[x + 1, footY, z + 1], [x, footY, z + 1], [x, y, z + 1], [x + 1, y, z + 1]]
            : dx === 1 ? [[x + 1, footY, z], [x + 1, footY, z + 1], [x + 1, y, z + 1], [x + 1, y, z]]
            : [[x, footY, z + 1], [x, footY, z], [x, y, z], [x, y, z + 1]];
          const base = n[2] === -1 ? 0.95 : n[2] === 1 ? 0.76 : n[0] === 1 ? 0.86 : 0.66;
          // 露得越深的崖脚越暗：一格的台阶和十几格的断崖不能是同一块色
          const sink = base * clamp(0.62 - h * 0.022, 0.4, 0.62);
          bk.face(this.tile(x, z, 0, true), p, n, [sink, sink, base, base], variant);
        }
      }
    }
    this.commit(bk.buckets, this.farGround);
  }

  rebuild(cx, cz) {
    if (!this.heights || !this.blocks) return;
    this.clearGroup(this.terrain);
    this.clearGroup(this.props);
    this.clearGroup(this.scatter);
    this.anim = [];
    const x0 = clamp(Math.floor(cx) - REGION / 2, 0, SIZE - REGION);
    const z0 = clamp(Math.floor(cz) - REGION / 2, 0, SIZE - REGION);
    this.region = [x0, z0]; this.dirty = false;
    if (this.farDirty) this.buildFarGround();
    const bk = this.bucketer();
    const { face } = bk;
    const treeCells = [], flowerCells = [], tuftCells = [], pebbleCells = [];
    const mushroomCells = [], logCells = [], reedCells = [], lilyCells = [];
    // 占格按"列"记：物件的脚钉在 level+1 的顶面上，若还按 layer 建 key，
    // 木屋/桥/动物脚下的草簇永远压不掉，草就从地板里长出来。
    const entityCells = new Set();
    for (const e of this.entities) if (!e.released) entityCells.add(`${e.x | 0}:${e.y | 0}`);
    for (const s of this.stations) entityCells.add(`${s.x | 0}:${s.z | 0}`);
    const spawn = this.spawn || { x: 128, z: 128 };
    const nearSpawn = (x, z, r) => (x - spawn.x) * (x - spawn.x) + (z - spawn.z) * (z - spawn.z) < r * r;
    const nearWater = (x, z) => {
      for (const [nx, nz] of [[x - 1, z], [x + 1, z], [x, z - 1], [x, z + 1]]) {
        if (nx >= 0 && nz >= 0 && nx < SIZE && nz < SIZE && (this.blocks[nz * SIZE + nx] & 7) === 0) return true;
      }
      return false;
    };

    for (let z = z0; z < z0 + REGION; z++) for (let x = x0; x < x0 + REGION; x++) {
      const lv = this.level(x, z), surface = this.heights[z * SIZE + x] >> 3;
      const bits = this.blocks[z * SIZE + x], kind = bits & 7;
      const y = lv + 1, h = hash(x, z), variant = 0.95 + (h % 11) / 100
        + Math.min(0.14, Math.max(0, lv - SEA_Y) * 0.009);
      const hiW = this.level(x - 1, z) > lv ? 1 : 0, hiE = this.level(x + 1, z) > lv ? 1 : 0;
      const hiN = this.level(x, z - 1) > lv ? 1 : 0, hiS = this.level(x, z + 1) > lv ? 1 : 0;
      const aoNW = this.ao(hiW, hiN, this.level(x - 1, z - 1) > lv), aoSW = this.ao(hiW, hiS, this.level(x - 1, z + 1) > lv);
      const aoNE = this.ao(hiE, hiN, this.level(x + 1, z - 1) > lv), aoSE = this.ao(hiE, hiS, this.level(x + 1, z + 1) > lv);
      // 顶点顺序：0 西北、1 东北、2 东南、3 西南
      const isWet = (nx, nz) => nx >= 0 && nz >= 0 && nx < SIZE && nz < SIZE && (this.blocks[nz * SIZE + nx] & 7) === 0;
      if (kind === 0) {
        // 水面只有一档：所有水格都铺在同一条水位上，包括紧贴海岸的那一圈。
        // 之前浅水格（hgt 80~85，层号和水位同为 10）走的是"陆地顶面"那条分支，
        // 顶面高度算成 lv+1 = 11，和旁边沙地的 11 完全齐平——
        // 于是水面成了另一块地板，树看着就是插在水里长。
        const depth = Math.max(0, (SEA_Y - 1) - lv);
        // 先铺河床：只画一张水面 = 一层飘在半空的蓝漆，底下什么都没有，
        // 玩家两句连着说的"地上还有蓝色的"和"感觉地面上空的"就是这一张纸。
        const bedY = Math.min(lv + 1, WATER_Y - 0.16);
        if (bedY < WATER_Y) {
          face(TILE.SAND, [[x, bedY, z], [x + 1, bedY, z], [x + 1, bedY, z + 1], [x, bedY, z + 1]], [0, 1, 0], 0.3, 1);
        }
        const w = clamp(1.06 - depth * 0.16, 0.4, 1.06);
        // 贴岸的角往暗走：真实浅水是"水薄到能看见湿沙"，湿沙是深色的。
        // 原来浅水角最亮（1.16），于是每一片水的边缘都镶一道亮蓝——
        // 那正是"地上有一块蓝色"的观感来源。
        const isW = (nx, nz) => isWet(nx, nz) ? 1 : 0;
        const eN = isW(x, z - 1), eS = isW(x, z + 1), eE = isW(x + 1, z), eW = isW(x - 1, z);
        const sh = (a, b) => w * (0.68 + 0.16 * (a + b));
        face(TILE_WATER_NEAR, [[x, WATER_Y, z], [x + 1, WATER_Y, z], [x + 1, WATER_Y, z + 1], [x, WATER_Y, z + 1]], [0, 1, 0],
          [sh(eW, eN), sh(eE, eN), sh(eE, eS), sh(eW, eS)], 1);
      } else {
        // 临水的岸：顶面靠水那一边的两个角提亮，海岸线自己会画出来
        const wN = kind !== 0 && isWet(x, z - 1), wS = kind !== 0 && isWet(x, z + 1);
        const wE = kind !== 0 && isWet(x + 1, z), wW = kind !== 0 && isWet(x - 1, z);
        const bump = (i, v) => v * (1 + 0.3 * Math.min(2, [wN + wW, wN + wE, wE + wS, wS + wW][i]));
        face(this.tile(x, z, surface - lv, false), [[x, y, z], [x + 1, y, z], [x + 1, y, z + 1], [x, y, z + 1]], [0, 1, 0],
          [bump(0, aoNW), bump(1, aoNE), bump(2, aoSE), bump(3, aoSW)], variant);
      }
      const neighbors = [[x, z - 1, [0, 0, -1]], [x, z + 1, [0, 0, 1]], [x + 1, z, [1, 0, 0]], [x - 1, z, [-1, 0, 0]]];
      for (const [nx, nz, n] of neighbors) {
        const low = this.level(nx, nz);
        // 岸脚：水面统一画在水位上，交界那 0.36 用整数层表达不出来，得单独补一段立面。
        // 不补就是沙面与水面对齐、中间一道悬空缝——玩家看到的正是"水面上长着树"。
        if (kind !== 0 && isWet(nx, nz)) {
          const top = Math.min(lv + 1, low + 1), bot = WATER_Y;
          if (bot < top) {
            const q = n[2] === -1 ? [[x, bot, z], [x + 1, bot, z], [x + 1, top, z], [x, top, z]]
              : n[2] === 1 ? [[x + 1, bot, z + 1], [x, bot, z + 1], [x, top, z + 1], [x + 1, top, z + 1]]
              : n[0] === 1 ? [[x + 1, bot, z], [x + 1, bot, z + 1], [x + 1, top, z + 1], [x + 1, top, z]]
              : [[x, bot, z + 1], [x, bot, z], [x, top, z], [x, top, z + 1]];
            const edge = n[2] === -1 ? 0.95 : n[2] === 1 ? 0.76 : n[0] === 1 ? 0.86 : 0.66;
            face(this.tile(x, z, surface - lv, true), q, n, [edge * 0.55, edge * 0.55, edge, edge], variant);
          }
        }
        for (let layer = low + 1; layer <= lv; layer++) {
          const p = n[2] === -1 ? [[x, layer, z], [x + 1, layer, z], [x + 1, layer + 1, z], [x, layer + 1, z]]
            : n[2] === 1 ? [[x + 1, layer, z + 1], [x, layer, z + 1], [x, layer + 1, z + 1], [x + 1, layer + 1, z + 1]]
            : n[0] === 1 ? [[x + 1, layer, z], [x + 1, layer, z + 1], [x + 1, layer + 1, z + 1], [x + 1, layer + 1, z]]
            : [[x, layer, z + 1], [x, layer, z], [x, layer + 1, z], [x, layer + 1, z + 1]];
          // 四个朝向四个明度：体素世界靠这一档才"立起来"，只交给一盏太阳不够——
          // 阴影里、树冠下、正午平光时，朝向明度是唯一还活着的高度线索
          const base = n[2] === -1 ? 0.95 : n[2] === 1 ? 0.76 : n[0] === 1 ? 0.86 : 0.66;
          // 崖壁从顶到底压一道渐变：露出 1 格和露出 12 格不能是同一块色，
          // 否则玩家说的"地形高度完全区分不开"就是字面意思。
          // 但一格高的台阶不能暗到 0.34：genesis 图九成地面都是这种一格的梯田，
          // 每一道都刷成深棕墙，前景就成了一地泥条——落差靠"顶面亮、立面中调"读，
          // 不靠把立面压黑。
          const foot = base * (0.68 + 0.27 * ((layer - low) / Math.max(1, lv - low)));
          face(this.tile(x, z, surface - layer, true), p, n, [foot, foot, base, base], variant);
        }
      }
      const onTop = lv === surface && !(bits & 16);
      const covered = entityCells.has(`${x}:${z}`);
      // 点缀只铺在近处：96 格区域一路铺到边上，远处就成了密密麻麻的碎点
      const ring = Math.max(Math.abs(x - cx), Math.abs(z - cz));
      const speckle = ring <= 22;
      // 树不能按出生点删：这一层只是化妆，碰撞和挖取仍在 world3d.js——
      // 画面上少一棵树，玩家就会走进一棵看不见的树。出生点由 pickSpawn 让开。
      const tree = (bits & 8) && lv === surface;
      if (tree) treeCells.push([x, y, z, h, kind]);
      else if (onTop && speckle && !covered && !nearSpawn(x, z, 2)) {
        const grass = kind === 3 || kind === 5;
        // 细节是点缀不是地毯：宁可少一半，也不能每格都有东西。
        // 但"脚下十米"是玩家读地面的那一段，太稀就成了"地上是空的"——
        // 所以 14 格内加密一档，14 格外照旧，远处仍然是干净的。
        const near = ring <= 14;
        if (grass && hash3(x, z, 5) % 100 < (near ? 14 : 7)) tuftCells.push([x, y, z, h]);
        if ((kind === 3 || kind === 5) && h % (near ? 61 : 97) === 0) flowerCells.push([x, y, z, h]);
        if (kind === 3 && hash3(x, z, 9) % 613 === 0) mushroomCells.push([x, y, z, h]);
        if (kind === 3 && hash3(x, z, 11) % 2203 === 0) logCells.push([x, y, z, h]);
        if (hash3(x, z, 13) % (near ? 181 : 307) === 0) pebbleCells.push([x, y, z, h]);
        if (grass && nearWater(x, z) && hash3(x, z, 15) % 137 === 0) reedCells.push([x, y, z, h]);
      }
      // 睡莲只漂在水面上：这一列的地表在水线以下，按 lv+1 放就飘在半空了
      if (kind === 0 && onTop && hash3(x, z, 17) % 179 === 0) lilyCells.push([x, WATER_Y - 0.005, z, h]);
    }
    this.commit(bk.buckets, this.terrain);
    this.buildTrees(treeCells);
    this.buildScatter({ tuftCells, flowerCells, pebbleCells, mushroomCells, logCells, reedCells, lilyCells });
    this.buildEntities(x0, z0);
    this.buildResources(x0, z0);
    this.buildStations(x0, z0);
  }

  buildTrees(cells) {
    if (!cells.length) return;
    const dummy = new THREE.Object3D();
    const trunkGeo = tintable(new THREE.BoxGeometry(1, 1, 1));
    const trunk = new THREE.InstancedMesh(trunkGeo, this.trunkMat, cells.length);
    const crownGeo = tintable(new THREE.BoxGeometry(1, 1, 1));
    const crowns = [];
    const cactusGeo = tintable(new THREE.BoxGeometry(1, 1, 1));
    const cacti = [];
    // 实例色只做同一棵树内部的层次（各档之间差 10–20%），不再负责把树和地分开：
    // 分开树和地靠材质底色（leavesBlock 压暗偏冷）+ 冠形。两层都往亮绿推，
    // 结果就是 owner 那句"树的颜色和草地地面区分不开"。
    const brown = [0xc8b8a8, 0xc8b8a8, 0xc2b3a2, 0xc8b8a8];
    const oak = [[1.02, 1.20, 0.66], [0.96, 1.16, 0.63], [1.08, 1.24, 0.72], [0.99, 1.18, 0.62]];
    const spruce = [[0.74, 1.06, 0.66], [0.78, 1.10, 0.70], [0.70, 1.02, 0.64]];
    const green = new THREE.Color();
    const tint = (c) => (c.length ? green.setRGB(c[0], c[1], c[2]) : green.set(c));
    cells.forEach(([x, y, z, h, kind], i) => {
      const off = (h % 5) / 10;
      dummy.rotation.set(0, (h % 7) * 0.9, 0);
      if (kind === 4) {
        // 沙漠：仙人掌要够得着人的视线——2.5–3.7 米高、半米粗，再加一只侧臂
        const ht = 2.4 + (h % 5) * 0.28;
        dummy.position.set(x + 0.5, y + ht / 2, z + 0.5);
        dummy.scale.set(0.5, ht, 0.5); dummy.updateMatrix();
        trunk.setMatrixAt(i, dummy.matrix); trunk.setColorAt(i, green.set(0x4f7f3a));
        cacti.push({ p: [x + 0.72, y + ht * 0.55, z + 0.5], s: [0.34, 1.1, 0.34], c: 0x4a7736 });
        return;
      }
      // 一格一米，树就要有树的个子：主干 4.4–10.6 米，人从树干底下走过去不用低头。
      // 旧值 5.0–8.0 太挤：高度档一窄，一格一棵的林子树冠自动拼成一块天花板，
      // owner 说的"树都糊在一起，根本区分不开远近"，前一半是几何问题不是雾的问题。
      const ht = kind === 2 ? 7.2 + (h % 7) * 0.72 : 4.4 + (h % 11) * 0.62;
      const tw = 0.34 + off * 0.16;
      dummy.position.set(x + 0.5, y + ht / 2, z + 0.5);
      dummy.scale.set(tw, ht, tw); dummy.updateMatrix();
      trunk.setMatrixAt(i, dummy.matrix); trunk.setColorAt(i, green.set(brown[h % brown.length]));
      const pal = kind === 2 ? spruce : oak;
      // 层叠树冠：三层，冠宽 2 米以内。一格一棵树，冠宽 3 米就是两两咬合成片，
      // 收进来天光和树与树之间的缝才透得进来——抬头一棵一棵数得清，低头看得见地。
      // 冠心再横向抖 ±0.22 米、纵向抖 ±0.24 米：整整齐齐的网格冠会连成一条绿带。
      // 层叠树冠：三层，冠宽 2 米以内，而且每层都要"高得接近宽"。
      // 2.0 米宽 × 1.3 米高是一台平板电视——树冠要立起来才像一蓬叶子。
      const tiers = kind === 2
        ? [[1.9, 1.8, -1.6], [1.45, 1.6, -0.4], [0.95, 1.4, 0.7]]
        : [[2.0, 1.75, -1.3], [1.65, 1.6, -0.2], [1.2, 1.5, 0.85]];
      const jx = (((h >> 3) % 5) - 2) * 0.11, jz = (((h >> 6) % 5) - 2) * 0.11;
      tiers.forEach(([w, hh, dy], t) => {
        crowns.push({
          p: [x + 0.5 + jx + ((h >> (t * 2)) & 1 ? 0.1 : -0.1),
              y + ht + dy + (((h >> t) % 5) - 2) * 0.12, z + 0.5 + jz],
          s: [w, hh, w * (0.92 + (t % 2) * 0.1)],
          r: ((h >> t) % 4) * 0.42,
          c: pal[(h + t) % pal.length],
        });
      });
    });
    const crown = new THREE.InstancedMesh(crownGeo, this.leavesBlock, crowns.length);
    crowns.forEach((it, i) => {
      dummy.rotation.set(0, it.r, 0);
      dummy.position.set(it.p[0], it.p[1], it.p[2]);
      dummy.scale.set(it.s[0], it.s[1], it.s[2]); dummy.updateMatrix();
      crown.setMatrixAt(i, dummy.matrix);
      crown.setColorAt(i, tint(it.c));
    });
    crown.count = crowns.length;
    trunk.castShadow = true; trunk.receiveShadow = true;
    crown.castShadow = true; crown.receiveShadow = true;
    trunk.instanceColor.needsUpdate = true; crown.instanceColor.needsUpdate = true;
    this.props.add(trunk, crown);
    if (cacti.length) {
      const mesh = this.instanced(cactusGeo, this.cactus, cacti);
      if (mesh) this.props.add(mesh);
    }
  }

  buildScatter(lists) {
    const { tuftCells, flowerCells, pebbleCells, mushroomCells, logCells, reedCells, lilyCells } = lists;
    const tuftPal = [0x5f8f3c, 0x6f9c45, 0x4f7d36, 0x86a24f];
    const dummy = new THREE.Object3D();
    const green = new THREE.Color();

    if (tuftCells.length) {
      // 三片交叉的细叶：几何体，不用新贴图（美术素材仍然只有你的图集）
      const pos = [], idx = [];
      for (let b = 0; b < 3; b++) {
        const a = (b * Math.PI) / 3, ca = Math.cos(a) * 0.1, sa = Math.sin(a) * 0.1, lean = (b - 1) * 0.03;
        const base = b * 4;
        pos.push(-ca, 0, -sa, ca, 0, sa, ca * 0.6 + lean, 0.34, sa * 0.6, -ca * 0.6 + lean, 0.34, -sa * 0.6);
        idx.push(base, base + 1, base + 2, base, base + 2, base + 3);
      }
      const geo = new THREE.BufferGeometry();
      geo.setAttribute('position', new THREE.Float32BufferAttribute(pos, 3));
      geo.setIndex(idx);
      geo.computeVertexNormals();
      tintable(geo);
      const tufts = [];
      tuftCells.forEach(([x, y, z, h], i) => {
        // 叶片的底边就在贴地那一个平面上：不抬起来就是和草皮顶面共面，走一步闪一次
        tufts.push({ p: [x + 0.5 + ((h % 10) - 5) / 20, y + 0.035, z + 0.5 + ((h >> 3) % 10 - 5) / 20], s: [1, 0.7 + (h % 7) / 16, 1], r: [0, (h % 12) * 0.52, 0], c: tuftPal[h % tuftPal.length] });
      });
      const mesh = this.instanced(geo, this.grassMat, tufts, { cast: false });
      if (mesh) this.scatter.add(mesh);
    }

    if (pebbleCells.length) {
      const geo = tintable(new THREE.BoxGeometry(1, 1, 1));
      const rocks = pebbleCells.map(([x, y, z, h]) => ({
        p: [x + 0.5 + ((h % 8) - 4) / 12, y + 0.055, z + 0.5 + ((h >> 2) % 8 - 4) / 12],
        s: [0.09 + (h % 5) * 0.02, 0.07, 0.09 + (h % 3) * 0.02], r: [0, (h % 9) * 0.7, 0],
        c: [0x8b8f96, 0x777d84, 0x9aa0a4][h % 3],
      }));
      const mesh = this.instanced(geo, this.rockMat, rocks, { cast: false });
      if (mesh) this.scatter.add(mesh);
    }

    if (mushroomCells.length) {
      const stemGeo = tintable(new THREE.BoxGeometry(1, 1, 1));
      const capGeo = tintable(new THREE.BoxGeometry(1, 1, 1));
      const stems = [], caps = [];
      mushroomCells.forEach(([x, y, z, h]) => {
        const s = 0.7 + (h % 4) * 0.14;
        stems.push({ p: [x + 0.5, y + 0.03 + 0.09 * s, z + 0.5], s: [0.09 * s, 0.18 * s, 0.09 * s], c: 0xe8dcc2 });
        caps.push({ p: [x + 0.5, y + 0.03 + 0.21 * s, z + 0.5], s: [0.24 * s, 0.09 * s, 0.24 * s], c: h % 3 === 0 ? 0xb23b32 : 0x8f6a45 });
      });
      const a = this.instanced(stemGeo, this.stemMat, stems, { cast: false });
      const b = this.instanced(capGeo, this.capMat, caps, { cast: false });
      if (a) this.scatter.add(a);
      if (b) this.scatter.add(b);
    }

    if (logCells.length) {
      const geo = new THREE.BoxGeometry(1, 1, 1);
      const logs = logCells.map(([x, y, z, h]) => ({
        p: [x + 0.5, y + 0.19, z + 0.5], s: [1.05 + (h % 4) * 0.14, 0.3, 0.3], r: [0, (h % 8) * 0.4, 0],
      }));
      const mesh = this.instanced(geo, this.bark, logs);
      if (mesh) this.scatter.add(mesh);
    }

    if (reedCells.length) {
      const geo = tintable(new THREE.BoxGeometry(1, 1, 1));
      const reeds = [];
      reedCells.forEach(([x, y, z, h]) => {
        for (let k = 0; k < 3; k++) {
          const hh = hash3(x, z, 19 + k);
          const rl = 0.85 + (hh % 8) / 9;
          reeds.push({
            p: [x + 0.5 + (hh % 9 - 4) / 10, y + 0.03 + rl / 2, z + 0.5 + ((hh >> 3) % 9 - 4) / 10],
            s: [0.045, rl, 0.045], r: [0, hh * 0.7, 0],
            c: [0x6d8f48, 0x7c9e50, 0x5c7d3d][hh % 3],
          });
        }
      });
      const mesh = this.instanced(geo, this.reedMat, reeds, { cast: false });
      if (mesh) this.scatter.add(mesh);
    }

    if (lilyCells.length) {
      const geo = tintable(new THREE.CylinderGeometry(0.3, 0.3, 0.035, 7));
      const pads = lilyCells.map(([x, y, z, h]) => ({ p: [x + 0.5, y + 0.035, z + 0.5], s: 0.8 + (h % 5) * 0.09, r: [0, (h % 8) * 0.6, 0], c: 0x4f7d3c }));
      const mesh = this.instanced(geo, this.lilyMat, pads, { cast: false });
      if (mesh) this.scatter.add(mesh);
    }

    if (flowerCells.length) {
      const g = new THREE.PlaneGeometry(0.28, 0.3), a = new THREE.InstancedMesh(g, this.flower, flowerCells.length * 2);
      const stemGeo = tintable(new THREE.BoxGeometry(0.03, 0.2, 0.03));
      const stems = [];
      flowerCells.forEach(([x, y, z, h], i) => {
        dummy.position.set(x + 0.5, y + 0.2, z + 0.5);
        dummy.rotation.set(0, (h % 11) * 0.28, 0); dummy.updateMatrix(); a.setMatrixAt(i * 2, dummy.matrix);
        dummy.rotation.y += Math.PI / 2; dummy.updateMatrix(); a.setMatrixAt(i * 2 + 1, dummy.matrix);
        stems.push({ p: [x + 0.5, y + 0.13, z + 0.5], c: 0x4e7a38 });
      });
      a.receiveShadow = false;
      this.scatter.add(a);
      const stemMesh = this.instanced(stemGeo, this.stemMat, stems, { cast: false });
      if (stemMesh) this.scatter.add(stemMesh);
    }
  }

  buildEntities(x0, z0) {
    // 落地件和建筑不必逐帧动：整片区域的同类部件烘进同一份几何，几种材质就几个 draw call；
    // 只有活物、火苗这类要动的东西各自留一个 group，动画只改 transform。
    const at = new Map();
    for (const e of this.entities) {
      if (e.released) continue;
      const key = `${e.x | 0}:${e.y | 0}`;
      const a = at.get(key);
      if (a) a.push(e); else at.set(key, [e]);
    }
    this.entityAt = at;
    const P = new Parts(this);
    for (const e of this.entities) {
      const cx = e.x | 0, cz = e.y | 0;
      if (e.released || cx < x0 || cx >= x0 + REGION || cz < z0 || cz >= z0 + REGION) continue;
      this.buildEntity(P, e, cx, cz);
    }
    P.finish(this.props);
  }

  buildEntity(P, e, cx, cz) {
    const h = hash(cx, cz);
    const base = this.anchor(cx, cz, e.layer);
    const ox = cx + 0.5, oz = cz + 0.5;
    // 朝向由坐标哈希决定：同一份账本在两台机器上必须摆出同一个姿势
    const yaw = (h % 8) * Math.PI / 4;
    const stacked = e.layer > (this.heights[cz * SIZE + cx] >> 3);
    switch (e.kind) {
      case 0: this.entStone(P.frame(ox, base, oz, yaw, stacked ? 1 : SCL.stone), h, stacked); break;
      case 1: this.entOak(P.frame(ox, base, oz, yaw, SCL.oak), h); break;
      case 2: this.entFlowerPatch(P.frame(ox, base, oz, yaw, SCL.flower), h); break;
      case 3: this.entCabin(P.frame(ox, base, oz, yaw, SCL.cabin), h); break;
      case 4: this.entBridge(P, cx, cz, base); break;
      case 5: this.entLighthouse(P.frame(ox, base, oz, yaw, SCL.light), h); break;
      case 6: this.entCampfire(P, ox, oz, base, h); break;
      case 7: this.entFence(P.frame(ox, base, oz, 0, SCL.fence), cx, cz, h); break;
      default: this.entCreature(e.kind, cx, cz, base, h); break;
    }
  }

  /** 资源层（QL-RES-v1）：可采的树 / 石 / 花。有没有、能不能采全由链上说了算，
   *  这一层只负责"看得见"。基础渲染层画过的东西这里必须同样画出来——
   *  画面少一棵可采的树，玩家就不知道该把准星对准哪里。 */
  buildResources(x0, z0) {
    if (!this.resources.length) return;
    const P = new Parts(this);
    const rock = this.mat(this.stone, 0xbcc1c6), bark = this.mat(this.bark, 0xc0a184);
    const leaf = this.mat(this.leaves, 0xa8c85e);
    for (const n of this.resources) {
      if (!n || n.taken) continue;
      const cx = n.x | 0, cz = n.y | 0;
      if (cx < x0 || cx >= x0 + REGION || cz < z0 || cz >= z0 + REGION) continue;
      if (cx < 0 || cz < 0 || cx >= SIZE || cz >= SIZE) continue;
      if (this.dug.has(cz * SIZE + cx)) continue;       // 挖过的地表：资源跟着表层一起没了
      const h = hash(cx, cz);
      const F = P.frame(cx + 0.5, this.level(cx, cz) + 1, cz + 0.5, (h % 8) * Math.PI / 4);
      if (n.kind === 0) {
        // 岩块比一格略小：看着是从地里长出来的，不是格子里塞了个箱子
        F.add('ball2', rock, 0, 0.26, 0, 0.92, 0.62, 0.86, { ry: (h % 12) * 0.5 });
        F.add('ball', rock, 0.3, 0.1, -0.24, 0.36, 0.26, 0.34, { ry: 1.1 });
      } else if (n.kind === 1) {
        // 可采的是"小树"：3.5 米，比天然林矮一半，一刀下去倒下的分量看得见
        F.add('taper', bark, 0, 1.6, 0, 0.34, 3.2, 0.34, { ry: (h % 6) * 0.5 });
        F.add('ball2', leaf, 0, 3.5, 0, 2.1, 1.3, 2.0, { ry: (h % 9) * 0.7 });
        F.add('ball', leaf, 0.5, 3.1, -0.3, 1.2, 0.9, 1.1);
      } else {
        for (const s of [1, -1]) F.add('plane', this.flower, 0, 0.42, 0, 0.86, 0.86, 0.86, { ry: s * 0.79, cast: false });
      }
    }
    P.finish(this.props);
  }

  legs4(P, mat, x, z, h, w, o = {}) {
    for (const a of [1, -1]) for (const b of [1, -1]) P.add('box', mat, a * x, h / 2, b * z, w, h, w, o);
  }

  entStone(P, h, stacked) {
    const a = this.mat(this.stone, 0xcfd3d6), b = this.mat(this.stone, 0xa9aeb3), c = this.mat(this.brickMat, 0xb9bec3);
    if (stacked) {
      // 地表以上叠起来的石头是实心砌块：它要填满自己那一格，玩家才踩得住（见 support()）
      P.add('box', c, 0, 0.5, 0, 0.98, 0.98, 0.98, { ry: 0.05 });
      P.add('box', a, 0.28, 0.9, 0.26, 0.32, 0.14, 0.3);
      P.add('box', b, -0.24, 0.12, -0.2, 0.26, 0.12, 0.24);
      return;
    }
    const s = 0.58 + (h % 5) * 0.03;
    P.add('ball', a, 0, s * 0.5, 0, s * 1.16, s, s * 1.04, { ry: (h % 24) * 0.26 });
    P.add('ball', b, 0.28, s * 0.32, -0.2, s * 0.6, s * 0.62, s * 0.56, { ry: 0.6 });
    P.add('ball', c, -0.26, s * 0.26, 0.22, s * 0.52, s * 0.5, s * 0.48, { ry: 1.7 });
    P.add('box', b, 0.06, 0.03, 0.34, 0.3, 0.06, 0.22, { ry: (h % 7) * 0.4 });
  }

  entOak(P, h) {
    const bark = this.mat(this.bark, 0xd8c9ba);
    const leafA = this.mat(this.leaves, 0x9dbd52), leafB = this.mat(this.leaves, 0x7fa845), leafC = this.mat(this.leaves, 0xb6cd63);
    const th = 1.5 + (h % 5) * 0.08;
    P.add('taper', bark, 0, th / 2, 0, 0.3, th, 0.3, { ry: (h % 6) * 0.5 });
    // 两根伸出去的枝：树冠不是浮在空中的方块
    P.add('cyl6', bark, 0.17, th * 0.8, 0, 0.07, 0.5, 0.07, { rz: -0.9 + (h % 3) * 0.1 });
    P.add('cyl6', bark, -0.14, th * 0.88, 0.1, 0.06, 0.42, 0.06, { rz: 0.8, rx: 0.4 });
    const cw = 1.04 + (h % 4) * 0.05;
    P.add('ball2', leafA, 0, th + 0.2, 0, cw, 0.62, cw * 0.94, { ry: (h % 21) * 0.3 });
    P.add('ball', leafB, 0.26, th + 0.48, -0.16, cw * 0.6, 0.5, cw * 0.58);
    P.add('ball', leafC, -0.2, th + 0.54, 0.2, cw * 0.56, 0.44, cw * 0.52);
    P.add('ball', leafB, 0.02, th + 0.82, 0.01, cw * 0.42, 0.36, cw * 0.4);
  }

  entFlowerPatch(P, h) {
    const stem = this.paint(0x5c8a3c, { rough: 1, side: 1 }), blade = this.paint(0x6f9c45, { rough: 1, side: 1 });
    const n = 4 + (h % 3);
    for (let i = 0; i < n; i++) {
      const hh = hash3(i, h, 23);
      const a = (i / n) * 6.283 + (h % 9) * 0.17, r = 0.07 + (hh % 4) * 0.05;
      const len = 0.2 + (hh % 7) * 0.045;
      const x = Math.cos(a) * r, z = Math.sin(a) * r, lean = ((hh % 5) - 2) * 0.09;
      P.add('cyl6', stem, x, len / 2, z, 0.05, len, 0.05, { rx: lean, rz: lean * 0.7 });
      P.add('ball', this.paint(FLOWER_COLORS[(hh + i) % FLOWER_COLORS.length], { rough: 0.9 }),
        x + lean * 0.2, len + 0.04, z + lean * 0.14, 0.12, 0.09, 0.12, { ry: hh % 12 });
      // 光有杆和花头就是一枚图钉：旁边配一片叶
      P.add('plane', blade, x * 1.5, len * 0.4, z * 1.5, 0.15, 0.1, 0.15, { rx: -1.25, ry: a });
    }
  }

  entCabin(P, h) {
    const log = this.mat(this.planks, 0xb99161), beam = this.mat(this.bark, 0xa08163);
    const stone = this.mat(this.stone, 0xbfc4c8), brick = this.mat(this.brickMat, 0xa9aeb3);
    const roofing = this.mat(this.roof, 0xc2a08a), dark = this.paint(SKIN.woodDark, { rough: 1 });
    P.add('box', stone, 0, 0.07, 0, 0.98, 0.14, 0.98);            // 石基：和地表严丝合缝，不留缝
    for (let i = 0; i < 3; i++) {
      const y = 0.26 + i * 0.19;
      P.log('cyl6', log, 0, y, 0.4, 0.11, 0.86);
      P.log('cyl6', log, 0, y, -0.4, 0.11, 0.86);
      P.add('cyl6', log, 0.4, y, 0, 0.11, 0.86, 0.11, { rx: Math.PI / 2 });
      P.add('cyl6', log, -0.4, y, 0, 0.11, 0.86, 0.11, { rx: Math.PI / 2 });
    }
    for (const sx of [1, -1]) for (const sz of [1, -1])
      P.add('box', beam, sx * 0.46, 0.45, sz * 0.46, 0.13, 0.62, 0.13);   // 转角柱略微出墙
    P.add('box', log, 0.4, 0.82, 0, 0.08, 0.14, 0.52);            // 山墙填两层，屋顶才有收口
    P.add('box', log, -0.4, 0.82, 0, 0.08, 0.14, 0.52);
    P.add('box', log, 0.4, 0.95, 0, 0.08, 0.12, 0.3);
    P.add('box', log, -0.4, 0.95, 0, 0.08, 0.12, 0.3);
    const a = 0.545;
    P.add('box', roofing, 0, 0.99, 0.315, 1.28, 0.07, 0.76, { rx: a });    // 双坡 + 出檐
    P.add('box', roofing, 0, 0.99, -0.315, 1.28, 0.07, 0.76, { rx: -a });
    P.add('box', roofing, 0, 1.2, 0, 1.32, 0.08, 0.14);
    P.add('box', brick, 0.27, 1.26, -0.18, 0.17, 0.52, 0.17);     // 烟囱
    P.add('box', stone, 0.27, 1.55, -0.18, 0.22, 0.06, 0.22);
    P.add('box', dark, 0, 0.42, 0.5, 0.32, 0.56, 0.07);           // 门洞凹进墙面
    P.add('box', beam, 0, 0.41, 0.53, 0.26, 0.5, 0.05);
    P.add('ball', this.paint(SKIN.gold, { rough: 0.4, metal: 0.6 }), 0.09, 0.42, 0.56, 0.05, 0.05, 0.05);
    for (const s of [1, -1]) {
      P.add('box', dark, -0.22, 0.52, s * 0.53, 0.26, 0.26, 0.05);
      P.add('box', this.glass, -0.22, 0.52, s * 0.56, 0.18, 0.18, 0.03, { cast: false });  // 窗里的暖光
    }
  }

  bridgeSide(cx, cz, dx, dz) {
    const nx = cx + dx, nz = cz + dz;
    if (nx < 0 || nz < 0 || nx >= SIZE || nz >= SIZE) return 0;
    if ((this.blocks[nz * SIZE + nx] & 7) === 0) return 2;
    return this.level(nx, nz) < this.level(cx, cz) ? 1 : 0;
  }

  entBridge(P, cx, cz, base) {
    const alongX = this.bridgeSide(cx, cz, 1, 0) + this.bridgeSide(cx, cz, -1, 0)
      >= this.bridgeSide(cx, cz, 0, 1) + this.bridgeSide(cx, cz, 0, -1);
    // 桥面顺着一个轴铺，local +X 落在哪个世界方向就由 yaw 决定
    const yaw = alongX ? 0 : Math.PI / 2;
    const dirs = alongX ? [[1, 0], [-1, 0]] : [[0, -1], [0, 1]];
    const extP = this.bridgeSide(cx, cz, ...dirs[0]) > 0 ? 0.8 : 0;
    const extN = this.bridgeSide(cx, cz, ...dirs[1]) > 0 ? 0.8 : 0;
    const deck = this.mat(this.planks, 0xa8875c), beam = this.mat(this.bark, 0x8a6f4d);
    const rail = this.paint(SKIN.woodDark, { rough: 0.95 });
    const F = P.frame(cx + 0.5, base, cz + 0.5, yaw, SCL.bridge);
    const span = 1 + extP + extN, dcx = (extP - extN) / 2;
    const planks = Math.max(5, Math.round(span / 0.17));
    for (let i = 0; i < planks; i++)
      F.add('box', deck, dcx - span / 2 + (i + 0.5) * (span / planks), 0.26, 0, (span / planks) * 0.8, 0.06, 0.74);
    F.add('box', beam, dcx, 0.19, 0.3, span, 0.08, 0.1);
    F.add('box', beam, dcx, 0.19, -0.3, span, 0.08, 0.1);
    for (const sz of [0.31, -0.31]) {
      for (const ex of [dcx - span / 2 + 0.08, dcx + span / 2 - 0.08]) F.add('box', beam, ex, 0.12, sz, 0.1, 0.24, 0.1);
      for (let i = 1; i < 4; i++) F.add('box', beam, dcx - span / 2 + (i * span) / 4, 0.44, sz, 0.06, 0.34, 0.06);
      F.add('box', rail, dcx, 0.62, sz, span * 0.98, 0.05, 0.06);
    }
    for (const ex of [dcx - span / 2 + 0.05, dcx + span / 2 - 0.05])
      F.add('box', rail, ex, 0.6, 0, 0.14, 0.05, 0.66);
  }

  fenceAt(cx, cz) {
    const a = this.entityAt.get(`${cx}:${cz}`);
    return !!(a && a.some((e) => !e.released && e.kind === 7));
  }

  entFence(P, cx, cz, h) {
    const post = this.mat(this.bark, 0x9b7d59), rail = this.mat(this.planks, 0x8a6f4d);
    P.add('box', post, 0, 0.36, 0, 0.14, 0.72, 0.14, { ry: (h % 4) * 0.12 });
    P.add('cone4', post, 0, 0.79, 0, 0.15, 0.12, 0.15, { ry: Math.PI / 4 });
    let links = 0;
    for (const [dx, dz] of [[1, 0], [-1, 0], [0, 1], [0, -1]]) {
      if (!this.fenceAt(cx + dx, cz + dz)) continue;
      links++;
      // 横杆只铺自己这半格，邻格补上另半格，一排篱笆才连成一条线而不是一堆积木
      for (const y of [0.32, 0.58]) P.add('box', rail, dx * 0.25, y, dz * 0.25, dx ? 0.5 : 0.05, 0.07, dx ? 0.05 : 0.5);
      P.add('box', rail, dx * 0.2, 0.14, dz * 0.2, dx ? 0.4 : 0.05, 0.05, dx ? 0.05 : 0.4);
    }
    if (!links) {
      for (const s of [1, -1]) P.add('box', rail, s * 0.24, 0.5, 0, 0.48, 0.07, 0.05);
      P.add('box', rail, 0.2, 0.16, 0, 0.4, 0.05, 0.05);
    }
  }

  entLighthouse(P, h) {
    const stone = this.mat(this.stone, 0xcfd4d8), brick = this.mat(this.brickMat, 0xb9bec3);
    const band = this.mat(this.brickMat, 0xc0584c), metal = this.paint(0x53585f, { rough: 0.6, metal: 0.4 });
    const roofing = this.mat(this.roof, 0x8fa2b0), beam = this.mat(this.bark, 0x8a6f4d);
    P.add('box', stone, 0, 0.11, 0, 0.94, 0.22, 0.94);
    P.add('taper', brick, 0, 0.57, 0, 0.74, 0.7, 0.74);
    P.add('taper', band, 0, 0.95, 0, 0.66, 0.16, 0.66);
    P.add('taper', brick, 0, 1.35, 0, 0.58, 0.64, 0.58);
    P.add('taper', band, 0, 1.63, 0, 0.52, 0.12, 0.52);
    P.add('taper', brick, 0, 1.97, 0, 0.46, 0.56, 0.46);
    P.add('cyl', stone, 0, 2.29, 0, 0.62, 0.08, 0.62);            // 灯台
    for (let i = 0; i < 8; i++) {
      const a = (i / 8) * 6.283;
      P.add('box', metal, Math.cos(a) * 0.24, 2.44, Math.sin(a) * 0.24, 0.05, 0.28, 0.05, { ry: -a });
    }
    P.add('cyl6', this.glass, 0, 2.44, 0, 0.42, 0.26, 0.42, { cast: false });   // 灯室里的光
    P.add('cone', roofing, 0, 2.66, 0, 0.66, 0.26, 0.66);
    P.add('ball', this.paint(SKIN.gold, { rough: 0.4, metal: 0.6 }), 0, 2.84, 0, 0.13, 0.13, 0.13);
    P.add('box', beam, 0, 0.44, 0.42, 0.26, 0.44, 0.07);
    P.add('box', this.glass, 0.02, 1.32, 0.32, 0.12, 0.16, 0.04, { cast: false });
  }

  entCampfire(P, ox, oz, base, h) {
    const stone = this.mat(this.stone, 0xb9bec3), ash = this.paint(0x4a443e, { rough: 1 });
    P.frame(ox, base, oz, 0).add('cyl', ash, 0, 0.02, 0, 0.64, 0.04, 0.64);
    for (let i = 0; i < 8; i++) {
      const a = (i / 8) * 6.283 + (h % 7) * 0.09;
      P.frame(ox + Math.cos(a) * 0.3, base, oz + Math.sin(a) * 0.3, a)
        .add('ball', i % 3 === 0 ? this.mat(this.stone, 0x9aa0a6) : stone, 0, 0, 0, 0.16, 0.13, 0.15, { ry: a });
    }
    const bark = this.mat(this.bark, 0xc0a184);
    for (let i = 0; i < 3; i++) {
      const a = (i / 3) * 3.1416 + (h % 5) * 0.12;
      P.frame(ox, base, oz, a).log('taper', bark, 0, 0.17, 0, 0.075, 0.6, { rz: -0.42 });
    }
    const g = new THREE.Group();
    g.position.set(ox, base, oz);
    g.rotation.y = (h % 8) * Math.PI / 4;
    this.props.add(g);
    const F = new Parts(this, g);
    const flame = F.pivot('flame', 0, 0.12, 0);
    F.add('cone', this.ember, 0, 0.2, 0, 0.27, 0.42, 0.27, { tag: 'flame', cast: false });
    F.add('cone', this.paint(0xffd98a, { em: 0xffb04a, emi: 3.2, rough: 0.6 }), 0.02, 0.14, -0.01, 0.15, 0.26, 0.15, { tag: 'flame', cast: false });
    F.add('ball', this.ember, -0.05, 0.06, 0.04, 0.08, 0.06, 0.08, { tag: 'flame', cast: false });
    F.finish();
    const ph = (h % 911) / 911 * 6.283;
    this.anim.push({ x: ox, z: oz, fn: (t) => {
      const f = 1 + Math.sin(t * 11 + ph) * 0.16 + Math.sin(t * 23.7 + ph * 2) * 0.08;
      flame.scale.set(2 - f, f, 2 - f);
      flame.rotation.y = t * 1.7 + ph;
    } });
  }

  entCreature(kind, cx, cz, base, h) {
    const g = new THREE.Group();
    const yaw = ((h >> 4) % 8) * Math.PI / 4;
    g.position.set(cx + 0.5, base, cz + 0.5);
    g.rotation.y = yaw;
    this.props.add(g);
    const P = new Parts(this, g);
    P.us = SCL.animal[kind] || 1;
    // 蝴蝶是飞在空中的：建模坐标按"贴地"写，落地时要抬到真实的高度带
    if (kind === 11) g.position.y += 0.42;
    const ph = (h % 997) / 997 * 6.283;
    const build = {
      8: (m, k, p) => this.entDeer(m, k, p), 9: (m, k, p) => this.entBird(m, k, p),
      10: (m, k, p) => this.entFish(m, k, p), 11: (m, k, p) => this.entButterfly(m, k, p),
      12: (m, k, p) => this.entRabbit(m, k, p), 13: (m, k, p) => this.entSheep(m, k, p),
      15: (m, k, p) => this.entWolf(m, k, p), 14: (m, k, p) => this.entTraveler(m, k, p),
    }[kind] || ((m, k, p) => this.entRabbit(m, k, p));
    const spec = build(P, h, ph) || {};
    P.finish();
    this.anim.push({
      x: cx + 0.5, z: cz + 0.5,
      fn: (t) => {
        // 起伏只往上走：负半周会把脚陷进草皮，看着就假了。
        if (spec.bob) g.position.y = base + (1 - Math.cos(t * (spec.speed || 1.3) + ph)) * 0.5 * spec.bob;
        if (spec.breathe) g.scale.y = 1 + Math.sin(t * (spec.bspeed || 2.1) + ph) * spec.breathe;
        if (spec.turn) g.rotation.y = yaw + Math.sin(t * 0.29 + ph) * spec.turn;
        if (spec.extra) spec.extra(t);
      },
    });
  }

  entDeer(P, h, ph) {
    const fur = this.paint(SKIN.deer), pale = this.paint(SKIN.deerPale, { rough: 1 });
    const hoof = this.paint(SKIN.hoof), horn = this.paint(SKIN.antler, { rough: 0.8 });
    this.legs4(P, hoof, 0.26, 0.1, 0.52, 0.06);
    P.add('ball2', fur, 0, 0.76, 0, 0.72, 0.4, 0.34);
    P.add('ball', fur, -0.26, 0.78, 0, 0.42, 0.44, 0.36);
    P.add('ball', fur, 0.22, 0.8, 0, 0.36, 0.38, 0.34);
    P.add('ball', pale, 0.02, 0.64, 0, 0.5, 0.24, 0.28);
    P.add('box', fur, 0.34, 1.0, 0, 0.16, 0.42, 0.16, { rz: -0.45 });
    P.add('box', fur, 0.5, 1.18, 0, 0.26, 0.18, 0.16, { rz: -0.2 });
    P.add('box', hoof, 0.64, 1.14, 0, 0.12, 0.1, 0.13);
    const tail = P.pivot('tail', -0.44, 0.9, 0);
    P.add('box', pale, -0.05, 0.02, 0, 0.09, 0.17, 0.06, { tag: 'tail', rz: 0.5 });
    for (const s of [1, -1]) {
      P.pivot('ear' + s, 0.44, 1.26, s * 0.13);
      P.add('box', fur, 0, 0.02, s * 0.02, 0.11, 0.05, 0.16, { tag: 'ear' + s, ry: s * 0.5, rz: 0.15 });
      P.pivot('horn' + s, 0.47, 1.3, s * 0.07);
      P.add('cyl6', horn, 0.02, 0.14, 0, 0.055, 0.3, 0.055, { tag: 'horn' + s, rx: s * 0.3, rz: -0.25 });
      P.add('cyl6', horn, 0.05, 0.2, s * 0.06, 0.045, 0.15, 0.045, { tag: 'horn' + s, rx: s * 0.7, rz: -0.5 });
      P.add('cyl6', horn, 0.09, 0.25, 0, 0.04, 0.13, 0.04, { tag: 'horn' + s, rz: -0.9 });
    }
    const e1 = P.nodes.get('ear1'), e2 = P.nodes.get('ear-1');
    const t1 = P.nodes.get('horn1'), t2 = P.nodes.get('horn-1');
    return { bob: 0.012, breathe: 0.014, turn: 0.12, extra: (t) => {
      const tw = Math.max(0, Math.sin(t * 2.3 + ph) ** 14 - 0.5) * 2;
      if (e1) e1.rotation.x = tw * 0.5;
      if (e2) e2.rotation.x = -tw * 0.35;
      tail.rotation.z = 0.0 + Math.sin(t * 1.6 + ph) * 0.2;
      if (t1) t1.rotation.z = -0.25 + Math.sin(t * 0.9 + ph) * 0.05;
      if (t2) t2.rotation.z = -0.25 - Math.sin(t * 0.9 + ph) * 0.05;
    } };
  }

  entWolf(P, h, ph) {
    const fur = this.paint(SKIN.wolf), pale = this.paint(SKIN.wolfPale), dark = this.paint(SKIN.dark);
    this.legs4(P, fur, 0.24, 0.09, 0.44, 0.075);
    P.add('ball2', fur, 0, 0.62, 0, 0.7, 0.36, 0.3);
    P.add('ball', fur, 0.2, 0.66, 0, 0.34, 0.32, 0.3);
    P.add('ball', pale, 0, 0.54, 0, 0.5, 0.2, 0.26);
    P.add('box', fur, 0.34, 0.74, 0, 0.2, 0.18, 0.18, { rz: -0.3 });
    P.add('box', fur, 0.47, 0.79, 0, 0.22, 0.19, 0.19);
    P.add('box', dark, 0.6, 0.75, 0, 0.11, 0.09, 0.12);
    for (const s of [1, -1]) P.add('cone4', fur, 0.4, 0.94, s * 0.09, 0.09, 0.16, 0.07, { tag: '', rx: s * 0.2 });
    const tail = P.pivot('tail', -0.36, 0.66, 0);
    const tuft = this.paint(0x8f959b);
    P.add('ball', fur, -0.1, 0.03, 0, 0.2, 0.14, 0.14, { tag: 'tail', rz: -0.5 });
    P.add('ball', tuft, -0.24, -0.02, 0, 0.16, 0.13, 0.13, { tag: 'tail' });
    return { bob: 0.01, breathe: 0.012, turn: 0.16, extra: (t) => {
      tail.rotation.y = Math.sin(t * 1.9 + ph) * 0.35;
      tail.rotation.z = Math.sin(t * 1.1 + ph) * 0.18;
    } };
  }

  entSheep(P, h, ph) {
    const wool = this.paint(SKIN.sheep, { rough: 1 }), dark = this.paint(SKIN.sheepDark, { rough: 0.95 });
    this.legs4(P, dark, 0.18, 0.1, 0.3, 0.06);
    P.add('ball2', wool, 0, 0.48, 0, 0.62, 0.44, 0.44);
    P.add('ball', wool, -0.2, 0.5, 0, 0.36, 0.36, 0.36);
    P.add('ball', wool, 0.18, 0.52, 0, 0.34, 0.34, 0.34);
    P.add('ball', wool, 0, 0.6, 0.14, 0.3, 0.26, 0.26);
    P.add('ball', wool, 0, 0.6, -0.14, 0.3, 0.26, 0.26);
    P.add('box', dark, 0.32, 0.58, 0, 0.22, 0.2, 0.18, { rz: -0.15 });
    P.add('ball', wool, 0.3, 0.71, 0, 0.22, 0.16, 0.2);
    const ears = [];
    for (const s of [1, -1]) {
      P.pivot('ear' + s, 0.3, 0.66, s * 0.13);
      P.add('box', dark, 0, 0, s * 0.03, 0.1, 0.04, 0.12, { tag: 'ear' + s, rz: s * 0.3 });
      ears.push(P.nodes.get('ear' + s));
    }
    return { bob: 0.01, breathe: 0.016, turn: 0.1, speed: 1.1, extra: (t) => {
      const tw = Math.max(0, Math.sin(t * 1.7 + ph) ** 12 - 0.5) * 2;
      for (const s of [1, -1]) if (ears[s > 0 ? 0 : 1]) ears[s > 0 ? 0 : 1].rotation.x = tw * 0.4 * s;
    } };
  }

  entRabbit(P, h, ph) {
    const fur = this.paint(SKIN.rabbit, { rough: 1 }), pale = this.paint(0xe4d8c4, { rough: 1 });
    P.add('ball', fur, -0.07, 0.11, 0, 0.22, 0.2, 0.19);
    P.add('ball2', fur, 0.05, 0.13, 0, 0.26, 0.2, 0.18);
    P.add('ball', fur, 0.19, 0.21, 0, 0.15, 0.14, 0.14);
    P.add('ball', pale, -0.16, 0.17, 0, 0.09, 0.09, 0.08);
    for (const s of [1, -1]) P.add('box', pale, 0.13, 0.02, s * 0.06, 0.09, 0.04, 0.06);
    for (const s of [1, -1]) {
      P.pivot('ear' + s, 0.17, 0.28, s * 0.045);
      P.add('box', fur, 0, 0.08, 0, 0.05, 0.17, 0.03, { tag: 'ear' + s, rx: s * 0.2, rz: -0.12 });
    }
    const l = P.nodes.get('ear1'), r = P.nodes.get('ear-1');
    return { bob: 0.008, breathe: 0.02, speed: 1.6, turn: 0.18, extra: (t) => {
      const tw = Math.max(0, Math.sin(t * 3.1 + ph) ** 10 - 0.5) * 2;
      l.rotation.x = 0 + tw * 0.6;
      r.rotation.x = -tw * 0.4;
    } };
  }

  entBird(P, h, ph) {
    const body = this.paint(SKIN.bird), pale = this.paint(SKIN.birdPale);
    const wing = this.paint(0x42648c, { rough: 0.9 }), beakm = this.paint(SKIN.gold, { rough: 0.7 });
    const dark = this.paint(SKIN.dark);
    for (const s of [1, -1]) {
      P.add('cyl6', dark, 0.01, 0.06, s * 0.04, 0.025, 0.12, 0.025);
      P.add('box', dark, 0.04, 0.015, s * 0.04, 0.06, 0.03, 0.05);
    }
    P.add('ball2', body, 0, 0.2, 0, 0.2, 0.18, 0.15);
    P.add('ball', pale, 0.03, 0.17, 0, 0.14, 0.12, 0.12);
    P.add('ball', body, 0.1, 0.3, 0, 0.13, 0.13, 0.12);
    P.add('cone4', beakm, 0.18, 0.29, 0, 0.06, 0.08, 0.05, { rz: -Math.PI / 2 });
    P.add('box', body, -0.17, 0.22, 0, 0.17, 0.03, 0.1, { rz: 0.35 });
    for (const s of [1, -1]) {
      P.pivot('w' + s, 0, 0.24, s * 0.055);
      P.add('box', wing, 0.01, 0.004, s * 0.07, 0.16, 0.02, 0.13, { tag: 'w' + s });
    }
    const wl = P.nodes.get('w1'), wr = P.nodes.get('w-1');
    return { bob: 0.014, speed: 2.2, breathe: 0.02, turn: 0.08, extra: (t) => {
      // 间歇性扇两下翅膀：一直扇的鸟像电风扇
      const f = Math.sin(t * 1.1 + ph);
      const amp = f > 0.72 ? (f - 0.72) * 3.6 : 0.02;
      wl.rotation.x = amp;
      wr.rotation.x = -amp;
    } };
  }

  entFish(P, h, ph) {
    const body = this.paint(SKIN.fish), fin = this.paint(SKIN.fin, { rough: 0.7, side: 1 });
    const pale = this.paint(0xdbeef3, { rough: 0.7 });
    P.add('ball2', body, 0.02, 0.13, 0, 0.34, 0.16, 0.13);
    P.add('ball', pale, 0.06, 0.09, 0, 0.22, 0.1, 0.1);
    P.pivot('tail', -0.16, 0.13, 0);
    P.add('plane', fin, -0.07, 0, 0, 0.16, 0.16, 0.16, { tag: 'tail', ry: 1.4, rx: 0.2 });
    P.add('plane', fin, -0.07, 0, 0, 0.15, 0.13, 0.15, { tag: 'tail', ry: 1.4, rx: -0.2 });
    P.add('plane', fin, 0.02, 0.21, 0, 0.15, 0.1, 0.15, { rx: 0.2 });
    for (const s of [1, -1]) P.add('plane', fin, 0.04, 0.11, s * 0.06, 0.1, 0.07, 0.1, { tag: '', rz: 0.3, ry: s * 0.9 });
    const tail = P.nodes.get('tail');
    return { bob: 0.02, speed: 1.7, turn: 0.14, extra: (t) => {
      tail.rotation.y = Math.sin(t * 4.2 + ph) * 0.4;
    } };
  }

  entButterfly(P, h, ph) {
    const dark = this.paint(0x4a4238);
    const pal = WING_COLORS.map((c) => this.paint(c, { rough: 0.9, side: 1 }));
    const pale = this.paint(0xf6eef7, { rough: 0.9, side: 1 });
    P.add('cyl6', dark, 0, 0.28, 0, 0.05, 0.15, 0.05, { rz: Math.PI / 2 });
    P.add('ball', dark, 0.08, 0.28, 0, 0.06, 0.06, 0.06);
    for (const s of [1, -1]) P.add('cyl6', dark, 0.12, 0.32, s * 0.02, 0.015, 0.09, 0.015, { rz: -0.7, rx: s * 0.4 });
    for (const s of [1, -1]) {
      P.pivot('w' + s, 0.01, 0.28, s * 0.02);
      P.add('plane', pal[h % pal.length], 0.02, 0, s * 0.07, 0.13, 0.11, 0.13, { tag: 'w' + s, rx: -Math.PI / 2 });
      P.add('plane', pale, -0.02, 0, s * 0.05, 0.08, 0.07, 0.08, { tag: 'w' + s, rx: -Math.PI / 2 });
    }
    const wl = P.nodes.get('w1'), wr = P.nodes.get('w-1');
    return { bob: 0.045, speed: 3.1, turn: 0.5, extra: (t) => {
      const a = 0.5 + Math.sin(t * 9.2 + ph) * 0.55;
      wl.rotation.x = -a;
      wr.rotation.x = a;
    } };
  }

  entTraveler(P, h, ph) {
    const cloak = this.paint(0x4c5b74, { rough: 0.95 }), trim = this.paint(0x8a5f3c, { rough: 0.9 });
    const skin = this.paint(0xd7b48f, { rough: 0.9 }), pack = this.mat(this.planks, 0x9b7d59);
    const staff = this.mat(this.bark, 0xa8875c), boot = this.paint(0x40372c, { rough: 1 });
    for (const s of [1, -1]) P.add('box', boot, s * 0.07, 0.05, s * 0.03, 0.11, 0.1, 0.14);
    // 摆幅挂在脚踝以上的枢轴上：整只身子转的话鞋底会犁进草皮。
    const body = P.pivot('body', 0, 0.1, 0);
    const b = (shape, mat, x, y, z, sx, sy, sz, o = {}) =>
      P.add(shape, mat, x, y - 0.1, z, sx, sy, sz, { ...o, tag: 'body' });
    b('taper', cloak, 0, 0.34, 0, 0.44, 0.6, 0.42);
    b('ball2', cloak, 0, 0.66, 0, 0.4, 0.22, 0.36);
    b('ball', cloak, 0.02, 0.82, 0, 0.26, 0.24, 0.25);
    b('ball', skin, 0.11, 0.8, 0, 0.14, 0.13, 0.14);
    b('box', pack, -0.19, 0.62, 0, 0.2, 0.28, 0.24);
    b('cyl6', trim, -0.19, 0.79, 0, 0.12, 0.22, 0.12, { rx: Math.PI / 2 });
    b('box', trim, -0.19, 0.5, 0, 0.22, 0.05, 0.26);
    b('cyl6', staff, 0.2, 0.5, 0.11, 0.04, 1.0, 0.04, { rz: -0.06, floor: 0.05 });
    b('ball', this.glass, 0.23, 1.0, 0.11, 0.1, 0.11, 0.1, { cast: false });
    return { bob: 0.012, speed: 1.2, turn: 0.22, breathe: 0.01, extra: (t) => {
      body.rotation.z = Math.sin(t * 1.4 + ph) * 0.03;
    } };
  }

  /** 地标层：兑换池 / 旅人摊位 / 光尘灯。UI 只送 {x,z,type}，画在哪一列由地形说了算。 */
  setStations(stations) {
    const list = (stations || []).filter((s) => s && Number.isFinite(s.x) && Number.isFinite(s.z));
    const sig = list.map((s) => `${s.x | 0}:${s.z | 0}:${s.type}`).join('|');
    if (sig === this.stationSig) return;
    this.stationSig = sig;
    this.stations = list;
    this.dirty = true;   // 地标要跟着所在列的当前地表走，只能走 rebuild
  }

  buildStations(x0, z0) {
    this.clearGroup(this.stationLayer);
    if (!this.stations.length) return;
    const P = new Parts(this);
    for (const s of this.stations) {
      const cx = s.x | 0, cz = s.z | 0;
      if (cx < x0 || cx >= x0 + REGION || cz < z0 || cz >= z0 + REGION) continue;
      if (cx < 0 || cz < 0 || cx >= SIZE || cz >= SIZE) continue;
      const base = this.level(cx, cz) + 1;
      const h = hash(cx * 3 + 11, cz * 5 + 7);
      if (s.type === 'pool') this.stationPool(P, cx, cz, base, h);
      else if (s.type === 'trader') this.stationTrader(P, cx, cz, base, h);
      else if (s.type === 'dust') this.stationDust(P, cx, cz, base, h);
    }
    P.finish(this.stationLayer);
  }

  stationFrame(cx, cz, base, h, us) {
    const g = new THREE.Group();
    g.position.set(cx + 0.5, base, cz + 0.5);
    g.rotation.y = (h % 8) * Math.PI / 4;
    this.stationLayer.add(g);
    const P = new Parts(this, g);
    P.us = us || 1;
    return { g, P, us: us || 1, ph: (h % 887) / 887 * 6.283, x: cx + 0.5, z: cz + 0.5 };
  }

  stationPool(P, cx, cz, base, h) {
    const f = this.stationFrame(cx, cz, base, h, SCL.station.pool);
    const stone = this.mat(this.stone, 0xc6cbd0), brick = this.mat(this.brickMat, 0xb4705a);
    const gold = this.paint(SKIN.gold, { rough: 0.4, metal: 0.6 });
    // 池壳是死的：并进区域几何；水面与池底的光才留自己的节点去动
    P.frame(cx + 0.5, base, cz + 0.5, f.g.rotation.y, f.us);
    P.add('basin', stone, 0, 0, 0, 1, 1, 1);
    P.add('cyl', brick, 0, 0.38, 0, 1.4, 0.05, 1.4);
    P.add('taper', this.paint(0x394851, { rough: 1 }), 0, 0.08, 0, 1.0, 0.16, 1.0);
    const M = f.P;
    const water = M.pivot('water', 0, 0.3, 0);
    M.add('disc', this.mirror, 0, 0, 0, 1.06, 1, 1.06, { tag: 'water', rx: -Math.PI / 2, cast: false });
    const rim = M.pivot('rim', 0, 0, 0);
    for (let i = 0; i < 4; i++) {
      const a = (i / 4) * 6.283 + 0.785;
      M.add('box', this.glass, Math.cos(a) * 0.66, 0.44, Math.sin(a) * 0.66, 0.12, 0.16, 0.12, { tag: 'rim', cast: false });
    }
    for (let i = 0; i < 3; i++) {
      const a = (i / 3) * 6.283 + 1.2;
      M.add('ball', gold, Math.cos(a) * 0.22, 0.15, Math.sin(a) * 0.22, 0.12, 0.06, 0.12, { tag: 'rim' });
    }
    M.finish();
    this.anim.push({ x: f.x, z: f.z, fn: (t) => {
      water.position.y = 0.3 * f.us + Math.sin(t * 1.4 + f.ph) * 0.012;
      water.scale.setScalar(1 + Math.sin(t * 2.2 + f.ph) * 0.02);
      rim.rotation.y = -t * 0.2;
    } });
  }

  stationTrader(P, cx, cz, base, h) {
    const f = this.stationFrame(cx, cz, base, h, SCL.station.trader);
    const beam = this.mat(this.bark, 0x9b7d59), plank = this.mat(this.planks, 0xb99161);
    const cloth = this.paint(SKIN.cloth, { rough: 1 }), pale = this.paint(SKIN.clothPale, { rough: 1 });
    const goods = [this.paint(0x8f6a45, { rough: 0.9 }), this.paint(0x6f9c45, { rough: 0.9 }), this.paint(0xd9564c, { rough: 0.9 })];
    P.frame(cx + 0.5, base, cz + 0.5, f.g.rotation.y);
    for (const sx of [1, -1]) for (const sz of [1, -1]) P.add('box', beam, sx * 0.44, 0.46, sz * 0.36, 0.08, 0.92, 0.08);
    P.add('box', plank, 0, 0.66, 0, 0.94, 0.08, 0.74);          // 木板台面
    P.add('box', plank, 0, 0.5, 0.36, 0.9, 0.24, 0.06);          // 挡板
    for (const s of [1, -1]) P.add('box', beam, 0, 0.98, s * 0.36, 0.9, 0.06, 0.06);
    for (let i = 0; i < 7; i++)                                  // 条纹雨棚：一深一浅才像布
      P.add('box', i % 2 ? pale : cloth, 0, 1.06 + (i % 2) * 0.008, -0.45 + i * 0.15, 1.04, 0.04, 0.15, { rx: 0.16 });
    P.add('box', cloth, 0, 1.0, 0.46, 1.06, 0.12, 0.04, { rx: 0.16 });
    for (let i = 0; i < 3; i++) P.add('ball', goods[(h + i) % 3], -0.26 + i * 0.26, 0.75, 0.02, 0.16, 0.12, 0.16);
    P.add('box', this.paint(SKIN.gold, { rough: 0.5, metal: 0.5 }), 0.2, 0.74, -0.2, 0.18, 0.08, 0.14);
    const M = f.P;
    const lamp = M.pivot('lamp', 0.4, 0.94, 0.3);
    M.add('box', beam, 0, 0.02, 0, 0.05, 0.06, 0.05, { tag: 'lamp' });
    M.add('ball', this.glass, 0, -0.08, 0, 0.12, 0.14, 0.12, { tag: 'lamp', cast: false });
    M.finish();
    this.anim.push({ x: f.x, z: f.z, fn: (t) => {
      lamp.rotation.z = Math.sin(t * 1.3 + f.ph) * 0.14;
      lamp.rotation.x = Math.sin(t * 0.9 + f.ph) * 0.08;
    } });
  }

  stationDust(P, cx, cz, base, h) {
    const f = this.stationFrame(cx, cz, base, h, SCL.station.dust);
    const stone = this.mat(this.stone, 0xbfc4c8), metal = this.paint(0x5a6068, { rough: 0.55, metal: 0.5 });
    P.frame(cx + 0.5, base, cz + 0.5, f.g.rotation.y, f.us);
    P.add('box', stone, 0, 0.06, 0, 0.6, 0.12, 0.6);
    P.add('cyl', stone, 0, 0.16, 0, 0.44, 0.08, 0.44);
    P.add('taper', metal, 0, 0.95, 0, 0.13, 1.5, 0.13);
    P.add('box', metal, 0, 1.52, 0, 0.26, 0.06, 0.26);
    const g = new THREE.BufferGeometry();
    const pos = new Float32Array(7 * 3);
    for (let i = 0; i < 7; i++) {
      const hh = hash3(cx + i, cz - i * 5, 41);
      pos[i * 3] = (((hh % 40) - 20) / 55) * f.us;
      pos[i * 3 + 1] = (1.35 + ((hh >> 3) % 60) / 45) * f.us;
      pos[i * 3 + 2] = ((((hh >> 6) % 40) - 20) / 55) * f.us;
    }
    g.setAttribute('position', new THREE.BufferAttribute(pos, 3));
    const motes = new THREE.Points(g, this.mote);
    motes.frustumCulled = false;
    f.g.add(motes);
    const M = f.P;
    const crystal = M.pivot('crystal', 0, 1.72, 0);
    M.add('cone', this.crystal, 0, 0.14, 0, 0.3, 0.3, 0.3, { tag: 'crystal', cast: false });
    M.add('cone', this.crystal, 0, -0.04, 0, 0.3, 0.22, 0.3, { tag: 'crystal', rx: Math.PI, cast: false });
    M.add('ball', this.crystal, 0, 0.05, 0, 0.16, 0.16, 0.16, { tag: 'crystal', cast: false });
    M.finish();
    this.anim.push({ x: f.x, z: f.z, fn: (t) => {
      crystal.rotation.y = t * 0.55 + f.ph;
      crystal.position.y = 1.72 * f.us + Math.sin(t * 1.6 + f.ph) * 0.03;
      motes.rotation.y = -t * 0.32;
    } });
  }

  // —— 第一人称的手 ——
  // 手是相机自己的孩子：它的存在只说明一件事——屏幕后面站着一个 1.75 米的人。
  // 视角一旦离开人的高度（刚进世界的俯瞰、拉远的观察），手必须消失：
  // 飘在三米高空的一双手不是"第一人称"，是穿帮。
  makeHands() {
    const G = new THREE.Group();
    this.scene.add(G);
    this.hands = G;
    const flat = { depthTest: false, depthWrite: false, transparent: true, roughness: 0.92, metalness: 0 };
    const sleeve = new THREE.MeshStandardMaterial({ color: 0x3a4557, ...flat });   // 袖子和旅人同一件斗篷
    const cuff = new THREE.MeshStandardMaterial({ color: 0x8a5f3c, ...flat });
    const skin = new THREE.MeshStandardMaterial({ color: 0xd7b48f, ...flat });
    this.handMats = [sleeve, cuff, skin];
    const arm = (side) => {
      const g = new THREE.Group();
      // 肩在画面下缘的外侧，小臂斜着往中心前方伸：这是"两只手在眼前甩"，不是两块
      // 挡路的木板。旧值 0.115 米见方、离眼 0.4 米，竖向占掉 20% 的画面；补了光照
      // 之后那块蓝木板更是直接糊在屏幕上——手臂要细下来，也要退到屏幕两个下角。
      g.position.set(0.34 * side, -0.44, -0.46);
      g.rotation.set(0.16, 0.30 * side, -0.20 * side);   // y 往内收：手朝屏幕中线来
      const parts = [
        [sleeve, 0, 0.01, -0.10, 0.088, 0.088, 0.20],              // 上臂
        [cuff, 0, 0.006, -0.212, 0.094, 0.094, 0.045],             // 袖口
        [skin, 0, -0.006, -0.282, 0.075, 0.082, 0.115],            // 手掌
      ];
      for (const [mat, x, y, z, sx, sy, sz] of parts) {
        const m = new THREE.Mesh(new THREE.BoxGeometry(sx, sy, sz), mat);
        m.position.set(x, y, z);
        m.renderOrder = 999;
        m.castShadow = m.receiveShadow = false;
        m.frustumCulled = false;
        g.add(m);
      }
      G.add(g);
      return g;
    };
    this.armR = arm(1);
    this.armL = arm(-1);
    this.armBase = { r: this.armR.position.clone(), l: this.armL.position.clone(), rx: 0.16, rz: 0.20 };
    this.held = new THREE.Group();
    // 手里的东西要退到画面右下角：0.24 米见方离眼 0.84 米 = 竖向吃掉 23% 画面，
    // 又是全帧最亮的白块，玩家第一眼看到的是"一块石头挡在世界上"而不是世界
    this.held.position.set(0.02, -0.055, -0.40);
    this.armR.add(this.held);
    this.heldMesh = null;
    this.heldSlot = -1;
    this.heldMats = new Map();
    this.punchSeen = -1;
    this.punchAt = -9;
    this.setHeld(0);
  }

  /** 手持格 → 一张和物品栏同一来源的像素精灵：手里的方块就是栏里的那一格，不另画美术 */
  setHeld(slot) {
    slot = Math.max(0, Math.min(15, slot | 0));
    if (slot === this.heldSlot && this.heldMesh) return;
    this.heldSlot = slot;
    let m = this.heldMats.get(slot);
    if (!m) {
      const icon = window.QLSprites && window.QLSprites.icon ? window.QLSprites.icon(slot, 64) : null;
      const map = icon ? new THREE.CanvasTexture(icon) : null;
      if (map) {
        map.magFilter = THREE.NearestFilter;
        map.colorSpace = THREE.SRGBColorSpace;
      }
      m = new THREE.MeshStandardMaterial({
        map, emissive: 0xffffff, emissiveMap: map, emissiveIntensity: 0.2,
        transparent: true, depthTest: false, depthWrite: false, side: THREE.DoubleSide, roughness: 1, metalness: 0,
      });
      this.heldMats.set(slot, m);
    }
    if (this.heldMesh) this.held.remove(this.heldMesh);
    this.heldMesh = new THREE.Mesh(new THREE.PlaneGeometry(0.185, 0.185), m);
    this.heldMesh.rotation.set(-0.46, -0.34, 0.16);
    this.heldMesh.renderOrder = 1000;
    this.heldMesh.frustumCulled = false;
    this.held.add(this.heldMesh);
  }

  updateHands(s, t, day) {
    const G = this.hands;
    if (!G) return;
    G.position.copy(this.camera.position);
    G.quaternion.copy(this.camera.quaternion);
    // 站在自己叠的楼顶上仍然有手（above 读的是脚下那一列的顶面）；
    // 俯瞰 / 高空镜头淡出，1.15 米起淡、3.2 米见底。
    const vis = clamp(1 - (Math.max(0, s.above || 0) - 1.15) / 2.05, 0, 1);
    G.visible = vis > 0.02;
    if (!G.visible) return;
    for (const m of this.handMats) m.opacity = vis;
    if (this.heldMesh) {
      this.heldMesh.material.opacity = vis;
      // 自发光只到"看得清是什么"就停：再亮，手里那块石头就成了全帧最亮的东西
      this.heldMesh.material.emissiveIntensity = (0.04 + day * 0.13) * vis;
    }
    const amp = Math.min(0.17, (s.speed || 0) * 0.055);    // 摆幅只认实测速度：站着不动手不甩
    const ph = s.bob || 0;
    const idle = Math.sin(t * 1.1) * 0.012;                // 静止时的呼吸起伏
    if (s.punch !== this.punchSeen) { this.punchSeen = s.punch; this.punchAt = t; }
    const k = 1 - Math.min(1, Math.max(0, (t - this.punchAt) / 0.24));
    const sw = Math.sin(k * Math.PI);                      // 一拳 0.24 秒：出去再收回来
    const B = this.armBase;
    this.armR.rotation.x = B.rx + Math.sin(ph) * amp + idle - sw * 0.62;
    this.armL.rotation.x = B.rx - Math.sin(ph) * amp - idle;
    this.armR.rotation.z = -B.rz + Math.cos(ph) * amp * 0.35;
    this.armL.rotation.z = B.rz - Math.cos(ph) * amp * 0.35;
    this.armR.position.z = B.r.z - sw * 0.14;
    this.armR.position.y = B.r.y + sw * 0.04;
    this.held.rotation.z = sw * 0.45;
  }

  render(s) {
    if (!this.heights || !s) return;
    // 兜底：某张贴图加载失败（404、离线资源缺件）也不能把封面永远按住，约 1.5 秒后照常淡出
    if (!this.texturesReady && ++this.warm > 90) this.texturesReady = true;
    const w = this.canvas.clientWidth, h = this.canvas.clientHeight;
    if (!w || !h) return;
    if (this.canvas.width !== Math.round(w * this.renderer.getPixelRatio()) || this.canvas.height !== Math.round(h * this.renderer.getPixelRatio())) {
      this.renderer.setSize(w, h, false); this.camera.aspect = w / h; this.camera.updateProjectionMatrix();
    }
    if (this.dirty || !this.region || Math.abs(s.px - (this.region[0] + REGION / 2)) > 16 || Math.abs(s.pz - (this.region[1] + REGION / 2)) > 16) this.rebuild(s.px, s.pz);
    const t = performance.now() / 1000;
    const cp = Math.cos(s.pitch || 0);
    const bobY = s.bobY || 0;
    this.camera.position.set(s.px, s.py + 1.62 + bobY, s.pz);
    this.camera.lookAt(s.px + Math.sin(s.yaw) * cp, s.py + 1.62 + bobY + Math.sin(s.pitch || 0), s.pz + Math.cos(s.yaw) * cp);
    this.sky.position.copy(this.camera.position);
    this.stars.position.copy(this.camera.position);
    this.sunSprite.position.copy(this.camera.position).addScaledVector(this.sunDir, 320);
    this.backdrop.position.set(s.px, 0, s.pz);
    this.clouds.position.set(s.px, 0, s.pz);
    this.drift += 0.0009;
    this.clouds.rotation.y = this.drift;
    if (this.waterMat && this.waterMat.map) {
      this.waterMat.map.offset.set((t * 0.012) % 1, Math.sin(t * 0.11) * 0.01);
    }
    if (this.waterNearMat && this.waterNearMat.map) {
      this.waterNearMat.map.offset.set((t * 0.014) % 1, Math.sin(t * 0.13 + 1.1) * 0.012);
    }
    const day = clamp(s.day ?? 1, 0, 1), dusk = clamp(s.dusk ?? 0, 0, 1), night = 1 - day;
    this.scene.environmentIntensity = 0.12 + day * 0.34;
    // 正午的水不能把整片天空镜回来：反射一强，水和一块白雾就没区别了。
    // 场景欠曝时水是唯一还在反天光的材质，于是它成了地面上最亮的一块蓝——
    // 光补回来之后，这一档必须同时收，否则"地上有蓝色的"会原样复现。
    if (this.waterMat) this.waterMat.envMapIntensity = 0.06 + day * 0.10;
    if (this.waterNearMat) this.waterNearMat.envMapIntensity = 0.08 + day * 0.14;
    this.sky.material.uniforms.sunHeight.value = 1 - dusk;
    this.sky.material.uniforms.night.value = night;
    this.sunSprite.material.opacity = day * (1 - dusk * 0.5) * 0.9;
    this.stars.material.opacity = Math.min(0.9, night * night * night * 1.15);
    // 光强要留余地：贴图自己的明度阶梯（雪 0.89 > 沙 0.74 > 石 0.52 > 草 0.46 > 水 0.39）
    // 就是画面的骨架，光打过头会把每一档都顶到同一片白里，材质就全糊了。
    // π 是物理单位的换算补偿（见 makeLights），不是"加亮"：不补这一档，
    // 曲线整条落在 sRGB 0–60 的死黑区里，阶梯再合理也看不出来。
    this.sun.color.setRGB(1, 0.6 + day * 0.2, 0.36 + day * 0.3);
    this.sun.intensity = (0.35 + day * 1.15) * Math.PI;
    this.hemi.intensity = (0.28 + day * 0.62) * Math.PI;
    this.sun.position.set(s.px - 40, s.py + 34, s.pz - 58);
    this.sunTarget.position.set(s.px, s.py, s.pz); this.sunTarget.updateMatrixWorld();
    // 空气透视的规矩：远处要收敛到"地平线处天空的值"，所以雾色不能自己编，
    // 必须和天空 shader 里那条 low 色同源（同一组作者值、同一条色彩管线）。
    const fog = this.scene.fog.color, sh = 1 - dusk;
    fog.setRGB(1 - 0.45 * sh, 0.53 + 0.21 * sh, 0.26 + 0.71 * sh, THREE.SRGBColorSpace);
    fog.lerp(new THREE.Color(0x0c1728), night * 0.92);
    // 白天雾气最厚：远处的树墙融进天光，画面才有前后层次；夜里收薄，灯才看得见。
    // 这一档是"远近分得开"的主力：正午 40 米起雾 16%、80 米 49%、地图边界 80%，
    // 树与树之间靠空气透视拉开，而不是靠颜色硬拼。
    // 上限要收着：假山拆掉之后地平线只剩真实地图的边缘，雾一浓就把它整条抹成白，
    // 玩家看到的又是一片没有层级的平面
    this.scene.fog.density = 0.005 + day * 0.0055;
    this.cloudMat.color.setHex(0xffffff).lerp(new THREE.Color(0xf2c9a6), dusk * 0.85).lerp(new THREE.Color(0x22304e), night);
    // 天光在黄昏也会偏暖：否则地面的绿会显得塑料
    this.hemi.color.setHex(0xdfe9f2).lerp(new THREE.Color(0xe9b48f), dusk * 0.75).lerp(new THREE.Color(0x2c3a5c), night);
    this.hemi.groundColor.setHex(0x6d6650).lerp(new THREE.Color(0x1a2434), night);
    // 草地底色只能做"时段"的微调色，不能当滤镜用：
    // 之前这里是蓝灰 0xa9b3c2，压在天光蓝上把绿地渲染成了水面，玩家看到的就是"树长在水里"
    const grassMat = this.materials.get(TILE.GRASS_TOP), grassSide = this.materials.get(TILE.GRASS_SIDE);
    for (const m of [grassMat, grassSide]) if (m) m.color.setHex(0xf4f7fb).lerp(new THREE.Color(0xffd9a8), dusk * 0.5);
    // 灯焰/窗火/晶体共用一份材质，一次脉冲就是整片世界的呼吸，不必逐实体记账
    const pulse = 0.7 + Math.sin(t * 1.7) * 0.2 + Math.sin(t * 5.3) * 0.06;
    this.glass.emissiveIntensity = (1.2 + pulse * 1.4) * (0.7 + night * 0.9);
    this.crystal.emissiveIntensity = 1.6 + pulse * 1.6;
    this.mirror.opacity = 0.86 + Math.sin(t * 0.9) * 0.06;
    // 活物只改已有节点的变换，且只管 45 格内：省的是每帧的 JS，不是显存
    this.clock = t;
    for (const a of this.anim) {
      const dx = a.x - s.px, dz = a.z - s.pz;
      if (dx * dx + dz * dz > 2025) continue;
      a.fn(t);
    }
    this.highlight.visible = !!s.target;
    if (s.target) this.highlight.position.set(s.target.cx + 0.5, s.target.layer + 0.5, s.target.cz + 0.5);
    if (s.held != null) this.setHeld(s.held);
    this.updateHands(s, t, day);
    this.renderer.render(this.scene, this.camera);
  }

  snapshot() { return this.canvas.toDataURL('image/png'); }
  dispose() {
    this.clearGroup(this.terrain); this.clearGroup(this.props);
    this.clearGroup(this.scatter); this.clearGroup(this.farGround);
    this.clearGroup(this.stationLayer);
    this.anim = [];
    this.stations = []; this.stationSig = null;
    for (const g of this.entGeo.values()) g.dispose();
    for (const m of this.kitMats) m.dispose();
    for (const m of this.handMats) m.dispose();
    for (const m of this.heldMats.values()) { if (m.map) m.map.dispose(); m.dispose(); }
    for (const m of this.paintCache.values()) m.dispose();
    for (const m of this.plainCache.values()) m.dispose();
    this.renderer.dispose();
  }
}
