import * as THREE from './vendor/three/three.module.js';

// 纯视觉层：账本与交互判定仍在 world3d.js。这里只画同一片地形，
// 额外的草簇/碎石/远山都不占账本格子（§9.11：像素不可验，位置由脚本可算）。
const SIZE = 256;
const REGION = 96;
const TILE = window.QLSprites?.TERRAIN;
const SEA_Y = Math.floor((window.QLWorld?.RULES?.SEA ?? 86) / 8) + 1;
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

export class ThreeWorld {
  constructor(canvas) {
    if (!TILE) throw new Error('Three.js 地形图集未加载');
    this.canvas = canvas;
    this.renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: false, preserveDrawingBuffer: true, powerPreference: 'high-performance' });
    this.renderer.setPixelRatio(Math.min(devicePixelRatio || 1, 1.7));
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.renderer.toneMapping = THREE.ACESFilmicToneMapping;
    this.renderer.toneMappingExposure = 1.45;
    this.renderer.shadowMap.enabled = true;
    this.renderer.shadowMap.type = THREE.PCFSoftShadowMap;
    this.scene = new THREE.Scene();
    this.scene.fog = new THREE.FogExp2(0xb9cde0, 0.0055);
    this.camera = new THREE.PerspectiveCamera(69.9, 1, 0.05, 700);
    this.clock = 0;
    this.drift = 0;
    this.heights = null;
    this.blocks = null;
    this.dug = new Map();
    this.entities = [];
    this.resources = [];
    this.region = null;
    // 固定出生地清场：第一眼不能是树干贴脸（旧版相机正好站在树里）
    this.spawn = { x: 128, z: 128 };
    this.dirty = true;
    this.terrain = new THREE.Group();
    this.props = new THREE.Group();
    this.scatter = new THREE.Group();
    this.scene.add(this.terrain, this.props, this.scatter);
    this.makeAtmosphere();
    this.makeEnvironment();
    this.makeMaterials();
    this.makeBackdrop();
    this.makeLandmark();
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

  makeAtmosphere() {
    this.sky = new THREE.Mesh(new THREE.SphereGeometry(380, 32, 16), new THREE.ShaderMaterial({
      side: THREE.BackSide, depthWrite: false, fog: false,
      uniforms: { sunHeight: { value: 0.25 }, night: { value: 0 } },
      vertexShader: 'varying vec3 vDir; void main(){vDir=position; gl_Position=projectionMatrix*modelViewMatrix*vec4(position,1.0);}',
      fragmentShader: `varying vec3 vDir; uniform float sunHeight; uniform float night;
        void main(){vec3 d=normalize(vDir); float h=smoothstep(-0.16,0.62,d.y);
          vec3 low=mix(vec3(1.0,0.53,0.26),vec3(0.55,0.74,0.97),sunHeight);
          vec3 high=mix(vec3(0.17,0.13,0.40),vec3(0.11,0.20,0.50),sunHeight);
          vec3 c=mix(low,high,h); c=mix(c,vec3(0.010,0.016,0.062),night);
          vec3 sd=normalize(vec3(-0.62,0.17,-0.76));
          float sun=pow(max(dot(d,sd),0.0),130.0);
          float glow=pow(max(dot(d,sd),0.0),7.0);
          c+=vec3(1.0,0.60,0.24)*sun*(1.0-night);
          c+=vec3(1.0,0.44,0.12)*glow*(1.0-sunHeight)*0.2*(1.0-night*0.8);
          // 一点点抖动，8bit 渐变才不会有色带
          float dith=fract(sin(dot(gl_FragCoord.xy,vec2(12.9898,78.233)))*43758.5453)-0.5;
          c+=dith*0.012;
          gl_FragColor=vec4(c,1.0); }`,
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

    this.hemi = new THREE.HemisphereLight(0x9dc0e8, 0x6d6650, 1.6);
    this.scene.add(this.hemi);
    this.sun = new THREE.DirectionalLight(0xffd0a0, 1.9);
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
    const tile = (name) => {
      const t = loader.load(`./assets/world-tiles/${name}.png`);
      t.colorSpace = THREE.SRGBColorSpace;
      t.magFilter = THREE.NearestFilter;
      t.minFilter = THREE.LinearMipmapLinearFilter;
      t.anisotropy = Math.min(8, this.renderer.capabilities.getMaxAnisotropy());
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
    for (const id of Object.values(TILE)) {
      const water = id === TILE.WATER;
      const texture = tile(names[id] || 'stone');
      if (water) {
        texture.wrapS = texture.wrapT = THREE.RepeatWrapping;
        this.waterMat = new THREE.MeshStandardMaterial({ map: texture, vertexColors: true, color: 0xc9e0e8,
          roughness: 0.13, metalness: 0.2, transparent: true, opacity: 0.87, side: THREE.DoubleSide });
        this.materials.set(id, this.waterMat);
        continue;
      }
      this.materials.set(id, new THREE.MeshStandardMaterial({ map: texture, vertexColors: true, roughness: 0.94, metalness: 0 }));
    }
    this.bark = new THREE.MeshStandardMaterial({ map: tile('bark'), roughness: 1 });
    this.leaves = new THREE.MeshStandardMaterial({ map: tile('leaves'), vertexColors: true, roughness: 1 });
    this.roof = new THREE.MeshStandardMaterial({ map: tile('roof'), roughness: 1 });
    this.planks = new THREE.MeshStandardMaterial({ map: tile('planks'), roughness: 1 });
    this.lantern = new THREE.MeshStandardMaterial({ color: 0xffb43e, emissive: 0xff9a22, emissiveIntensity: 2.6 });
    this.cactus = new THREE.MeshStandardMaterial({ color: 0xffffff, vertexColors: true, roughness: 0.95 });
    this.stone = new THREE.MeshStandardMaterial({ map: tile('stone'), roughness: 1 });
    this.soil = new THREE.MeshStandardMaterial({ map: tile('dirt'), roughness: 1 });
    this.grassMat = new THREE.MeshStandardMaterial({ vertexColors: true, side: THREE.DoubleSide, roughness: 1 });
    this.trunkMat = new THREE.MeshStandardMaterial({ map: tile('bark'), vertexColors: true, roughness: 1 });
    this.rockMat = new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 1 });
    this.stemMat = new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 1 });
    this.capMat = new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 0.9 });
    this.lilyMat = new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 0.55 });
    this.reedMat = new THREE.MeshStandardMaterial({ vertexColors: true, side: THREE.DoubleSide, roughness: 1 });
    this.flowerTexture = new THREE.CanvasTexture(window.QLSprites.icon(2, 32));
    this.flowerTexture.magFilter = THREE.NearestFilter;
    this.flower = new THREE.MeshBasicMaterial({ map: this.flowerTexture, alphaTest: 0.45, side: THREE.DoubleSide, transparent: true });
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

    // 五棱、平直着色：四面锥在远处就是一张纸片，五棱才有山脊的明暗
    const peak = tintable(new THREE.ConeGeometry(0.5, 1, 5));
    const rock = new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 1, flatShading: true });
    const far = [], line = [], caps = [], ridge = [], greens = [0x4f7568, 0x5a7f72, 0x436a60, 0x668d80];
    for (let i = 0; i < 150; i++) {
      const a = (i / 150) * Math.PI * 2;
      const r = 128 + (hash(i, 31) % 46);
      const hgt = 7 + (hash(i, 47) % 20);
      const w = 9 + (hash(i, 53) % 12);
      far.push({ p: [Math.cos(a) * r, SEA_Y - 2 + hgt / 2, Math.sin(a) * r], s: [w * 2.2, hgt, w * 2.2], r: [0, i * 0.7, 0], c: greens[i % greens.length] });
    }
    for (let i = 0; i < 90; i++) {
      const a = (i / 90) * Math.PI * 2 + 0.021;
      const r = 196 + (hash(i, 61) % 58);
      const hgt = 18 + (hash(i, 67) % 40) + (i % 7 === 0 ? 18 : 0); // 少数主峰拔高，天际线才有节奏
      const w = 16 + (hash(i, 71) % 22);
      const px = Math.cos(a) * r, pz = Math.sin(a) * r;
      far.push({ p: [px, SEA_Y - 3 + hgt / 2, pz], s: [w * 2.4, hgt, w * 2.4], r: [0, i * 0.37, 0], c: 0x556f7d });
      // 雪线：只有主峰顶盖一小块白，其余保持岩石的本色——满屏白山 = 纸片
      if (hgt > 46) {
        const ch = hgt * 0.26;
        caps.push({ p: [px, SEA_Y - 3 + hgt - ch / 2, pz], s: [w * 1.02, ch, w * 1.02], r: [0, i * 0.37, 0], c: 0xd9e4eb });
      }
    }
    // 再退一圈：只留剪影，交给雾把它压成一层淡蓝，远山不再是"一圈贴着地平线的白三角"
    for (let i = 0; i < 70; i++) {
      const a = (i / 70) * Math.PI * 2 + 0.019;
      const r = 286 + (hash(i, 73) % 74);
      const hgt = 30 + (hash(i, 79) % 46);
      const w = 22 + (hash(i, 83) % 30);
      ridge.push({ p: [Math.cos(a) * r, SEA_Y - 4 + hgt / 2, Math.sin(a) * r], s: [w * 2.6, hgt, w * 2.6], r: [0, i * 0.53, 0], c: 0x8399a9 });
    }
    // 近一圈树线：让远山和脚下之间有层次，而不是一整片雾
    for (let i = 0; i < 260; i++) {
      const a = (i / 260) * Math.PI * 2;
      const r = 104 + (hash(i, 83) % 16);
      const hgt = 2.6 + (hash(i, 89) % 26) / 10;
      line.push({ p: [Math.cos(a) * r, SEA_Y + hgt / 2 - 0.6, Math.sin(a) * r], s: [2.6, hgt, 2.6], r: [0, i * 0.9, 0], c: 0x32573d });
    }
    const farMesh = this.instanced(peak, rock, far);
    const ridgeMesh = this.instanced(peak, rock, ridge, { cast: false });
    const lineMesh = this.instanced(peak, rock, line);
    const capMesh = this.instanced(peak, rock, caps, { cast: false });
    if (ridgeMesh) this.backdrop.add(ridgeMesh);
    if (farMesh) this.backdrop.add(farMesh);
    if (lineMesh) this.backdrop.add(lineMesh);
    if (capMesh) this.backdrop.add(capMesh);

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

  // 封面里的地标：一座钉在世界坐标里的远山城堡。它不跟相机走（跟走就成了贴片），
  // 也不吃雾（那么远，标准光照会被雾整片吃掉），只用平涂色 + 窗火。
  makeLandmark() {
    this.landmarks = new THREE.Group();
    this.landmarkMat = new THREE.MeshBasicMaterial({ vertexColors: true, fog: false });
    this.landmarkGlow = new THREE.MeshBasicMaterial({ vertexColors: true, fog: false, transparent: true, opacity: 0.6 });
    const stone = [], glow = [];
    const cx = 128, cz = -238, base = SEA_Y - 6;
    for (let i = 0; i < 9; i++) {
      stone.push({ p: [cx, base + 5 + i * 6.5, cz], s: [150 - i * 14, 13, 120 - i * 11], c: i % 2 ? 0x33455c : 0x3a4d66 });
    }
    const top = base + 5 + 9 * 6.5;
    stone.push({ p: [cx, top + 23, cz], s: [26, 46, 26], c: 0x596b84 });
    for (const [dx, dz] of [[-19, -12], [19, -12], [-19, 13], [19, 13]]) {
      stone.push({ p: [cx + dx, top + 30, cz + dz], s: [10, 60, 10], c: 0x5f7288 });
      stone.push({ p: [cx + dx, top + 66, cz + dz], s: [7.5, 14, 7.5], c: 0x44566e });
      stone.push({ p: [cx + dx, top + 75, cz + dz], s: [3.4, 10, 3.4], c: 0x3d4f66 });
    }
    stone.push({ p: [cx, top + 8, cz + 14], s: [52, 16, 6], c: 0x50627a });
    stone.push({ p: [cx, top + 18, cz + 14], s: [11, 22, 8], c: 0x5a6d85 });
    for (let i = -2; i <= 2; i++) {
      glow.push({ p: [cx + i * 5, top + 26, cz + 13.2], s: [1.5, 2.6, 0.4], c: 0xffc067 });
      glow.push({ p: [cx + i * 5, top + 38, cz + 13.2], s: [1.3, 2.2, 0.4], c: 0xffb657 });
    }
    for (const [dx, dz] of [[-19, -12], [19, -12], [-19, 13], [19, 13]]) {
      glow.push({ p: [cx + dx, top + 42, cz + dz + 5.2], s: [1.1, 2.2, 0.4], c: 0xffc067 });
    }
    const box = tintable(new THREE.BoxGeometry(1, 1, 1));
    const a = this.instanced(box, this.landmarkMat, stone, { cast: false, receive: false });
    const b = this.instanced(box, this.landmarkGlow, glow, { cast: false, receive: false });
    if (a) this.landmarks.add(a);
    if (b) this.landmarks.add(b);
    this.scene.add(this.landmarks);
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
      child.traverse((o) => { if (o.geometry) o.geometry.dispose(); });
    }
  }

  // 体素 AO：相邻列比顶面高就在对应的角上压暗，坑壁与树下立刻有体积感
  ao(s1, s2, corner) {
    if (s1 && s2) return 0.68;
    return 1 - 0.1 * ((s1 ? 1 : 0) + (s2 ? 1 : 0)) - 0.05 * (corner ? 1 : 0);
  }

  rebuild(cx, cz) {
    if (!this.heights || !this.blocks) return;
    this.clearGroup(this.terrain);
    this.clearGroup(this.props);
    this.clearGroup(this.scatter);
    const x0 = clamp(Math.floor(cx) - REGION / 2, 0, SIZE - REGION);
    const z0 = clamp(Math.floor(cz) - REGION / 2, 0, SIZE - REGION);
    this.region = [x0, z0]; this.dirty = false;
    const buckets = new Map();
    const bucket = (id) => {
      if (!buckets.has(id)) buckets.set(id, { pos: [], normal: [], uv: [], color: [] });
      return buckets.get(id);
    };
    const face = (id, p, n, shades, variant) => {
      const b = bucket(id), order = [0, 1, 2, 0, 2, 3];
      const usesArray = Array.isArray(shades);
      for (const i of order) {
        b.pos.push(...p[i]); b.normal.push(...n);
        b.uv.push(i === 1 || i === 2 ? 1 : 0, i >= 2 ? 1 : 0);
        const c = (usesArray ? shades[i] : shades) * variant;
        b.color.push(c, c, c);
      }
    };
    const treeCells = [], flowerCells = [], tuftCells = [], pebbleCells = [];
    const mushroomCells = [], logCells = [], reedCells = [], lilyCells = [];
    const entityCells = new Set();
    for (const e of this.entities) if (!e.released) entityCells.add(`${e.x}:${e.y}:${e.layer}`);
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
      const y = lv + 1, h = hash(x, z), variant = 0.95 + (h % 11) / 100;
      const hiW = this.level(x - 1, z) > lv ? 1 : 0, hiE = this.level(x + 1, z) > lv ? 1 : 0;
      const hiN = this.level(x, z - 1) > lv ? 1 : 0, hiS = this.level(x, z + 1) > lv ? 1 : 0;
      const aoNW = this.ao(hiW, hiN, this.level(x - 1, z - 1) > lv), aoSW = this.ao(hiW, hiS, this.level(x - 1, z + 1) > lv);
      const aoNE = this.ao(hiE, hiN, this.level(x + 1, z - 1) > lv), aoSE = this.ao(hiE, hiS, this.level(x + 1, z + 1) > lv);
      // 顶点顺序：0 西北、1 东北、2 东南、3 西南
      face(this.tile(x, z, surface - lv, false), [[x, y, z], [x + 1, y, z], [x + 1, y, z + 1], [x, y, z + 1]], [0, 1, 0], [aoNW, aoNE, aoSE, aoSW], variant);
      const neighbors = [[x, z - 1, [0, 0, -1]], [x, z + 1, [0, 0, 1]], [x + 1, z, [1, 0, 0]], [x - 1, z, [-1, 0, 0]]];
      for (const [nx, nz, n] of neighbors) {
        const low = this.level(nx, nz);
        for (let layer = low + 1; layer <= lv; layer++) {
          const p = n[2] === -1 ? [[x, layer, z], [x + 1, layer, z], [x + 1, layer + 1, z], [x, layer + 1, z]]
            : n[2] === 1 ? [[x + 1, layer, z + 1], [x, layer, z + 1], [x, layer + 1, z + 1], [x + 1, layer + 1, z + 1]]
            : n[0] === 1 ? [[x + 1, layer, z], [x + 1, layer, z + 1], [x + 1, layer + 1, z + 1], [x + 1, layer + 1, z]]
            : [[x, layer, z + 1], [x, layer, z], [x, layer + 1, z], [x, layer + 1, z + 1]];
          const base = n[0] ? 0.78 : 0.88;
          // 侧面下沿压暗：坑壁与台阶不再是均匀一块色
          face(this.tile(x, z, surface - layer, true), p, n, [base * 0.9, base * 0.9, base, base], variant);
        }
      }
      const onTop = lv === surface && !(bits & 16);
      const covered = entityCells.has(`${x}:${z}:${lv}`);
      // 树不能按出生点删：这一层只是化妆，碰撞和挖取仍在 world3d.js——
      // 画面上少一棵树，玩家就会走进一棵看不见的树。出生点由 pickSpawn 让开。
      const tree = (bits & 8) && lv === surface;
      if (tree) treeCells.push([x, y, z, h, kind]);
      else if (onTop && !covered && !nearSpawn(x, z, 2)) {
        const grass = kind === 3 || kind === 5;
        // 细节是点缀不是地毯：宁可少一半，也不能每格都有东西
        if (grass && hash3(x, z, 5) % 100 < 8) tuftCells.push([x, y, z, h]);
        if ((kind === 3 || kind === 5) && h % 97 === 0) flowerCells.push([x, y, z, h]);
        if (kind === 3 && hash3(x, z, 9) % 613 === 0) mushroomCells.push([x, y, z, h]);
        if (kind === 3 && hash3(x, z, 11) % 2203 === 0) logCells.push([x, y, z, h]);
        if (hash3(x, z, 13) % 307 === 0) pebbleCells.push([x, y, z, h]);
        if (grass && nearWater(x, z) && hash3(x, z, 15) % 137 === 0) reedCells.push([x, y, z, h]);
      }
      if (kind === 0 && onTop && hash3(x, z, 17) % 179 === 0) lilyCells.push([x, y, z, h]);
    }
    for (const [id, b] of buckets) {
      const g = new THREE.BufferGeometry();
      g.setAttribute('position', new THREE.Float32BufferAttribute(b.pos, 3));
      g.setAttribute('normal', new THREE.Float32BufferAttribute(b.normal, 3));
      g.setAttribute('uv', new THREE.Float32BufferAttribute(b.uv, 2));
      g.setAttribute('color', new THREE.Float32BufferAttribute(b.color, 3));
      g.computeBoundingSphere();
      const mesh = new THREE.Mesh(g, this.materials.get(id));
      mesh.receiveShadow = true;
      this.terrain.add(mesh);
    }
    this.buildTrees(treeCells);
    this.buildScatter({ tuftCells, flowerCells, pebbleCells, mushroomCells, logCells, reedCells, lilyCells });
    this.buildEntities(x0, z0);
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
    // 贴图本身已经是中间调，实例色只做色相微调（接近白），否则两层相乘会把树压成黑块。
    // 色相只留极小的差异：一片林子如果每棵都不同色，看起来就"花"。
    const brown = [0xd8c9ba, 0xd8c9ba, 0xd2c3b2, 0xd8c9ba];
    const oak = [0xc4cfb4, 0xbfcbb0, 0xc9d3ba, 0xc1ccb2];
    const spruce = [0xaebfb4, 0xb3c3b8, 0xa9bbaf];
    const green = new THREE.Color();
    cells.forEach(([x, y, z, h, kind], i) => {
      const off = (h % 5) / 10;
      dummy.rotation.set(0, (h % 7) * 0.9, 0);
      if (kind === 4) {
        // 沙漠：仙人掌，矮、粗、带一只侧臂
        const ht = 1.5 + (h % 4) * 0.22;
        dummy.position.set(x + 0.5, y + ht / 2, z + 0.5);
        dummy.scale.set(0.3, ht, 0.3); dummy.updateMatrix();
        trunk.setMatrixAt(i, dummy.matrix); trunk.setColorAt(i, green.set(0x4f7f3a));
        cacti.push({ p: [x + 0.74, y + ht * 0.55, z + 0.5], s: [0.22, 0.72, 0.22], c: 0x4a7736 });
        return;
      }
      const ht = kind === 2 ? 3.2 + (h % 5) * 0.22 : 2.5 + (h % 5) * 0.18;
      dummy.position.set(x + 0.5, y + ht / 2, z + 0.5);
      dummy.scale.set(0.16 + off * 0.14, ht, 0.16 + off * 0.14); dummy.updateMatrix();
      trunk.setMatrixAt(i, dummy.matrix); trunk.setColorAt(i, green.set(brown[h % brown.length]));
      const pal = kind === 2 ? spruce : oak;
      // 层叠树冠：只留四层，层数一多就像一摞箱子
      const tiers = kind === 2
        ? [[1.62, 0.56, -0.55], [1.26, 0.54, 0.02], [0.9, 0.5, 0.56], [0.5, 0.46, 1.06]]
        : [[2.0, 0.7, -0.66], [1.56, 0.64, -0.02], [1.06, 0.56, 0.58], [0.56, 0.5, 1.06]];
      tiers.forEach(([w, hh, dy], t) => {
        crowns.push({
          p: [x + 0.5 + ((h >> (t * 2)) & 1 ? 0.07 : -0.07), y + ht + dy, z + 0.5],
          s: [w, hh, w * (0.92 + (t % 2) * 0.1)],
          c: pal[(h + t) % pal.length],
        });
      });
    });
    const crown = new THREE.InstancedMesh(crownGeo, this.leaves, crowns.length);
    crowns.forEach((it, i) => {
      dummy.rotation.set(0, 0, 0);
      dummy.position.set(it.p[0], it.p[1], it.p[2]);
      dummy.scale.set(it.s[0], it.s[1], it.s[2]); dummy.updateMatrix();
      crown.setMatrixAt(i, dummy.matrix);
      crown.setColorAt(i, green.set(it.c));
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
        tufts.push({ p: [x + 0.5 + ((h % 10) - 5) / 20, y, z + 0.5 + ((h >> 3) % 10 - 5) / 20], s: [1, 0.7 + (h % 7) / 16, 1], r: [0, (h % 12) * 0.52, 0], c: tuftPal[h % tuftPal.length] });
      });
      const mesh = this.instanced(geo, this.grassMat, tufts, { cast: false });
      if (mesh) this.scatter.add(mesh);
    }

    if (pebbleCells.length) {
      const geo = tintable(new THREE.BoxGeometry(1, 1, 1));
      const rocks = pebbleCells.map(([x, y, z, h]) => ({
        p: [x + 0.5 + ((h % 8) - 4) / 12, y + 0.025, z + 0.5 + ((h >> 2) % 8 - 4) / 12],
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
        stems.push({ p: [x + 0.5, y + 0.09 * s, z + 0.5], s: [0.09 * s, 0.18 * s, 0.09 * s], c: 0xe8dcc2 });
        caps.push({ p: [x + 0.5, y + 0.21 * s, z + 0.5], s: [0.24 * s, 0.09 * s, 0.24 * s], c: h % 3 === 0 ? 0xb23b32 : 0x8f6a45 });
      });
      const a = this.instanced(stemGeo, this.stemMat, stems, { cast: false });
      const b = this.instanced(capGeo, this.capMat, caps, { cast: false });
      if (a) this.scatter.add(a);
      if (b) this.scatter.add(b);
    }

    if (logCells.length) {
      const geo = new THREE.BoxGeometry(1, 1, 1);
      const logs = logCells.map(([x, y, z, h]) => ({
        p: [x + 0.5, y + 0.16, z + 0.5], s: [1.05 + (h % 4) * 0.14, 0.3, 0.3], r: [0, (h % 8) * 0.4, 0],
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
          reeds.push({
            p: [x + 0.5 + (hh % 9 - 4) / 10, y + 0.26, z + 0.5 + ((hh >> 3) % 9 - 4) / 10],
            s: [0.045, 0.5 + (hh % 6) / 12, 0.045], r: [0, hh * 0.7, 0],
            c: [0x6d8f48, 0x7c9e50, 0x5c7d3d][hh % 3],
          });
        }
      });
      const mesh = this.instanced(geo, this.reedMat, reeds, { cast: false });
      if (mesh) this.scatter.add(mesh);
    }

    if (lilyCells.length) {
      const geo = tintable(new THREE.CylinderGeometry(0.3, 0.3, 0.035, 7));
      const pads = lilyCells.map(([x, y, z, h]) => ({ p: [x + 0.5, y + 0.02, z + 0.5], s: 0.8 + (h % 5) * 0.09, r: [0, (h % 8) * 0.6, 0], c: 0x4f7d3c }));
      const mesh = this.instanced(geo, this.lilyMat, pads, { cast: false });
      if (mesh) this.scatter.add(mesh);
    }

    if (flowerCells.length) {
      const g = new THREE.PlaneGeometry(0.28, 0.3), a = new THREE.InstancedMesh(g, this.flower, flowerCells.length * 2);
      const stemGeo = tintable(new THREE.BoxGeometry(0.03, 0.2, 0.03));
      const stems = [];
      flowerCells.forEach(([x, y, z, h], i) => {
        dummy.position.set(x + 0.5, y + 0.17, z + 0.5);
        dummy.rotation.set(0, (h % 11) * 0.28, 0); dummy.updateMatrix(); a.setMatrixAt(i * 2, dummy.matrix);
        dummy.rotation.y += Math.PI / 2; dummy.updateMatrix(); a.setMatrixAt(i * 2 + 1, dummy.matrix);
        stems.push({ p: [x + 0.5, y + 0.1, z + 0.5], c: 0x4e7a38 });
      });
      a.receiveShadow = false;
      this.scatter.add(a);
      const stemMesh = this.instanced(stemGeo, this.stemMat, stems, { cast: false });
      if (stemMesh) this.scatter.add(stemMesh);
    }
  }

  box(group, x, y, z, sx, sy, sz, mat, glow = false) {
    const geo = new THREE.BoxGeometry(sx, sy, sz);
    // 带 vertexColors 的材质必须配一份全白 color 属性，否则缺省属性是 0 = 全黑
    if (mat.vertexColors) tintable(geo);
    const mesh = new THREE.Mesh(geo, mat);
    mesh.position.set(x, y, z); mesh.castShadow = !glow; mesh.receiveShadow = !glow;
    group.add(mesh); return mesh;
  }

  buildEntities(x0, z0) {
    for (const e of this.entities) {
      if (e.released || e.x < x0 || e.x >= x0 + REGION || e.y < z0 || e.y >= z0 + REGION) continue;
      const x = e.x + 0.5, z = e.y + 0.5, y = e.layer;
      if (e.kind === 3) {
        // 小屋：石基 + 木墙 + 出檐屋顶 + 门与窗
        this.box(this.props, x, y + 0.16, z, 0.98, 0.32, 0.98, this.stone);
        this.box(this.props, x, y + 0.72, z, 0.9, 0.8, 0.9, this.planks);
        this.box(this.props, x, y + 1.2, z, 1.2, 0.3, 1.2, this.roof);
        this.box(this.props, x, y + 0.42, z + 0.47, 0.34, 0.56, 0.08, this.soil);
        this.box(this.props, x - 0.25, y + 0.8, z + 0.462, 0.18, 0.18, 0.04, this.lantern, true);
        this.box(this.props, x + 0.25, y + 0.8, z + 0.462, 0.18, 0.18, 0.04, this.lantern, true);
      } else if (e.kind === 1) {
        this.box(this.props, x, y + 0.85, z, 0.24, 1.7, 0.24, this.bark);
        this.box(this.props, x, y + 1.95, z, 0.94, 1, 0.94, this.leaves);
        this.box(this.props, x, y + 2.42, z, 0.6, 0.45, 0.6, this.leaves);
      } else if (e.kind === 4) {
        this.box(this.props, x, y + 0.2, z, 1, 0.2, 0.8, this.planks);
        this.box(this.props, x - 0.4, y + 0.48, z, 0.08, 0.55, 0.8, this.bark);
        this.box(this.props, x + 0.4, y + 0.48, z, 0.08, 0.55, 0.8, this.bark);
      } else if (e.kind === 6 || e.kind === 5) {
        this.box(this.props, x, y + 0.32, z, 0.5, 0.64, 0.5, this.planks);
        this.box(this.props, x, y + 0.78, z, 0.3, 0.35, 0.3, this.lantern, true);
        this.box(this.props, x, y + 1.02, z, 0.44, 0.06, 0.44, this.roof);
      } else {
        const mat = e.kind === 0 ? this.stone : this.planks;
        this.box(this.props, x, y + 0.48, z, 0.9, 0.96, 0.9, mat);
      }
    }
  }

  render(s) {
    if (!this.heights || !s) return;
    const w = this.canvas.clientWidth, h = this.canvas.clientHeight;
    if (!w || !h) return;
    if (this.canvas.width !== Math.round(w * this.renderer.getPixelRatio()) || this.canvas.height !== Math.round(h * this.renderer.getPixelRatio())) {
      this.renderer.setSize(w, h, false); this.camera.aspect = w / h; this.camera.updateProjectionMatrix();
    }
    if (this.dirty || !this.region || Math.abs(s.px - (this.region[0] + REGION / 2)) > 16 || Math.abs(s.pz - (this.region[1] + REGION / 2)) > 16) this.rebuild(s.px, s.pz);
    const t = performance.now() / 1000;
    const cp = Math.cos(s.pitch || 0);
    this.camera.position.set(s.px, s.py + 1.62, s.pz);
    this.camera.lookAt(s.px + Math.sin(s.yaw) * cp, s.py + 1.62 + Math.sin(s.pitch || 0), s.pz + Math.cos(s.yaw) * cp);
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
    const day = clamp(s.day ?? 1, 0, 1), dusk = clamp(s.dusk ?? 0, 0, 1), night = 1 - day;
    this.scene.environmentIntensity = 0.16 + day * 0.5;
    if (this.waterMat) this.waterMat.envMapIntensity = 0.3 + day * 0.85;
    this.sky.material.uniforms.sunHeight.value = 1 - dusk;
    this.sky.material.uniforms.night.value = night;
    this.sunSprite.material.opacity = day * (1 - dusk * 0.5) * 0.9;
    this.stars.material.opacity = Math.min(0.9, night * night * night * 1.15);
    this.sun.color.setRGB(1, 0.6 + day * 0.2, 0.36 + day * 0.3);
    this.sun.intensity = 0.45 + day * 2.3;
    this.hemi.intensity = 0.32 + day * 0.9;
    this.sun.position.set(s.px - 40, s.py + 34, s.pz - 58);
    this.sunTarget.position.set(s.px, s.py, s.pz); this.sunTarget.updateMatrixWorld();
    // 雾的颜色跟着时段走：早晨偏蓝、黄昏偏橙、夜里偏深蓝，远景才会融进去
    const fog = this.scene.fog.color;
    fog.setHex(0xa9c6da).lerp(new THREE.Color(0xe6b083), dusk * 0.8).lerp(new THREE.Color(0x0c1728), night);
    this.cloudMat.color.setHex(0xffffff).lerp(new THREE.Color(0xf2c9a6), dusk * 0.85).lerp(new THREE.Color(0x22304e), night);
    // 天光在黄昏也会偏暖：否则地面的绿会显得塑料
    this.hemi.color.setHex(0x9dc0e8).lerp(new THREE.Color(0xe9b48f), dusk * 0.7).lerp(new THREE.Color(0x2c3a5c), night);
    const grassMat = this.materials.get(TILE.GRASS_TOP), grassSide = this.materials.get(TILE.GRASS_SIDE);
    for (const m of [grassMat, grassSide]) if (m) m.color.setHex(0xffffff).lerp(new THREE.Color(0xf2ddbd), dusk * 0.55);
    // 远景城堡同样要被时段压暗；窗火入夜才亮
    this.landmarkMat.color.setScalar(0.32 + day * 0.62);
    this.landmarkGlow.opacity = 0.25 + night * 0.7;
    this.highlight.visible = !!s.target;
    if (s.target) this.highlight.position.set(s.target.cx + 0.5, s.target.layer + 0.5, s.target.cz + 0.5);
    this.renderer.render(this.scene, this.camera);
  }

  snapshot() { return this.canvas.toDataURL('image/png'); }
  dispose() {
    this.clearGroup(this.terrain); this.clearGroup(this.props); this.clearGroup(this.scatter);
    this.renderer.dispose();
  }
}
