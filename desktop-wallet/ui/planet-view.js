// 星球渲染：真实地球（自转）+ 星空。封面背景、星球选择页、太空俯冲进场共用这一份。
//
// 为什么单独成文件：这一层只管**画**球。字节口径（1 字节高程网格 ↔ 米）搬到
// earth-grid.js 了——那份不能只住在这里，否则 2D 世界地图和席位地块生成都得为了几个常量
// 去依赖 three.js。口径仍然只有一份，只是不再和渲染绑在一起。
import * as THREE from './vendor/three/three.module.js';
import { GW, unquantize, cellOf } from './earth-grid.js';

const R_EARTH_M = 6371000;

/** 经纬度 → 球面方向（与 three.js SphereGeometry 的 UV 同一定义）：
 *  u = (lon+180)/360，v = (90-lat)/180。 */
export function dirOf(lat, lon, out = new THREE.Vector3()) {
  const u = (lon + 180) / 360 * Math.PI * 2, v = (90 - lat) / 180 * Math.PI;
  return out.set(-Math.cos(u) * Math.sin(v), Math.cos(v), Math.sin(u) * Math.sin(v));
}

const assets = {};
/** 星球层两件文件各 ~1 MB，只取一次；封面和选择页会各自调用。
 *  路径按**本模块**解析，不按文档：对拍页可能住在别的目录下，写死相对文档的路径会 404。 */
export async function loadPlanetAssets() {
  if (!assets.H || !assets.tex) {
    const at = (f) => new URL(f, import.meta.url).href;
    const buf = await fetch(at('earth-global-h.bin')).then(r => r.arrayBuffer());
    assets.H = new Uint8Array(buf);
    assets.tex = await new Promise((res, rej) => {
      new THREE.TextureLoader().load(at('earth-global.jpg'), res, undefined, rej);
    });
    assets.tex.colorSpace = THREE.SRGBColorSpace;
    assets.tex.wrapS = THREE.RepeatWrapping;
  }
  return assets;
}

/** 顶点位移的球。网格密度按用途给：星球层 1 格 ≈ 19.6 公里，球面再细也只是把同一格
 *  多采几次，而顶点数直接决定封面第一次打开要等多久（1024×512 是 52 万顶点）。
 *  陆按夸张后的真高度抬起来；海**只沉到 4 公里量级**：按真实深度（最深 -6072 米）
 *  ×16 沉下去等于在每段大陆架造一道 96 公里高的悬崖，崖壁法线背对太阳，
 *  海岸线就全糊成黑裂缝（对拍帧里翻过车）。 */
function buildEarthMesh(H, tex, renderer, o = {}) {
  tex.anisotropy = renderer.capabilities.getMaxAnisotropy();
  const [ws, hs] = o.segs ?? [1024, 512];
  const ex = o.ex ?? 16, seaEx = o.seaEx ?? 2, seaCapM = o.seaCapM ?? -2000;
  const seaTint = o.seaTint ?? [0.5, 0.78, 2.1];
  const geo = new THREE.SphereGeometry(1, ws, hs);
  const pos = geo.attributes.position;
  const col = new Float32Array(pos.count * 3);
  for (let k = 0; k < pos.count; k++) {
    const x = pos.getX(k), y = pos.getY(k), z = pos.getZ(k);
    const len = Math.hypot(x, y, z) || 1;
    const lat = Math.asin(y / len) * 180 / Math.PI;
    const lon = Math.atan2(z, -x) * 180 / Math.PI;          // dirOf 的反函数
    const [i, j] = cellOf(lat, lon);
    const hv = unquantize(H[j * GW + i]);
    const lift = (hv > 0 ? Math.max(hv, 200) * ex : Math.max(hv, seaCapM) * seaEx) / R_EARTH_M;
    pos.setXYZ(k, x * (1 + lift), y * (1 + lift), z * (1 + lift));
    // 顶点色只给海：BMNG 是真彩色合成，海在这套光照下近乎全黑，读起来像剪影不像水。
    // 蓝通道拉到 2 倍、红压一半，深蓝就回来了；陆保持 1.0，不碰实拍颜色。
    const t = hv <= 0 ? seaTint : null;
    col[k * 3] = t ? t[0] : 1; col[k * 3 + 1] = t ? t[1] : 1; col[k * 3 + 2] = t ? t[2] : 1;
  }
  geo.computeVertexNormals();
  geo.setAttribute('color', new THREE.BufferAttribute(col, 3));
  return new THREE.Mesh(geo, new THREE.MeshStandardMaterial({
    map: tex, vertexColors: true, roughness: 1, metalness: 0,
  }));
}

// 大气：背面球 + 边缘菲涅尔。纯装饰，但没有它星球就只是"一张纸贴在球上"。
function buildAtmosphere() {
  return new THREE.Mesh(new THREE.SphereGeometry(1.055, 96, 48), new THREE.ShaderMaterial({
    side: THREE.BackSide, transparent: true, depthWrite: false, blending: THREE.AdditiveBlending,
    vertexShader: 'varying vec3 vN;varying vec3 P;void main(){vN=normalize(normalMatrix*normal);P=(modelViewMatrix*vec4(position,1.)).xyz;gl_Position=projectionMatrix*modelViewMatrix*vec4(position,1.);}',
    fragmentShader: 'varying vec3 vN;varying vec3 P;void main(){float f=pow(1.0-abs(dot(normalize(vN),normalize(-P))),3.0);gl_FragColor=vec4(vec3(0.32,0.56,0.95)*f*0.62,f*0.7);}'
  }));
}

// 星空：固定在世界里（跟着球转的话星星会拖着重力跑）。半径 22 的球壳，肉眼看不出透视。
function buildStars(n, seed = 1) {
  const arr = new Float32Array(n * 3);
  let s = seed >>> 0;
  const rnd = () => { s = (s * 1664525 + 1013904223) >>> 0; return s / 4294967296; };
  const v = new THREE.Vector3();
  for (let k = 0; k < n; k++) {
    dirOf(Math.asin(rnd() * 2 - 1) * 180 / Math.PI, rnd() * 360 - 180, v)
      .multiplyScalar(20 + rnd() * 6);
    arr[k * 3] = v.x; arr[k * 3 + 1] = v.y; arr[k * 3 + 2] = v.z;
  }
  const geo = new THREE.BufferGeometry();
  geo.setAttribute('position', new THREE.BufferAttribute(arr, 3));
  return new THREE.Points(geo, new THREE.PointsMaterial({color: 0xcfe0ee, size: 0.055, sizeAttenuation: true}));
}

/** 在 canvas 上起一颗会自转的地球。返回控制器：spin 调速、biasX 把球推到版式要的位置、
 *  stop() 之后不再画帧（封面被主界面遮住时一帧都不该花）。 */
export async function createPlanet(canvas, opts = {}) {
  const t0 = performance.now();
  const renderer = new THREE.WebGLRenderer({canvas, antialias: true, alpha: false});
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.toneMapping = THREE.NoToneMapping;      // 贴图是公有领域实拍，不再二次压曲线
  const scene = new THREE.Scene();
  const cam = new THREE.PerspectiveCamera(38, 1, 0.0005, 60);
  const {H, tex} = await loadPlanetAssets();

  const ex = opts.exaggeration ?? 16;              // 真实比例下 8848 米只占半径 0.14%，看不见
  const earth = buildEarthMesh(H, tex, renderer, {ex, segs: opts.segs, seaEx: opts.seaEx, seaTint: opts.seaTint});
  const atmo = buildAtmosphere();
  scene.add(earth, atmo, buildStars(opts.stars ?? 2600));

  // 太阳固定在世界里，不跟相机走：跟着走就成了"处处正午"，球面立刻塌成一张纸
  const sun = new THREE.DirectionalLight(0xfff2df, opts.sunIntensity ?? 3.1);
  const slat = (opts.sunLat ?? 34), slon = (opts.sunLon ?? 84);
  sun.position.copy(dirOf(slat, slon));
  scene.add(sun, new THREE.AmbientLight(0x33465e, opts.ambient ?? 0.62));

  const view = {dist: opts.dist ?? 2.45, lat: opts.lat ?? 22, lon: opts.lon ?? 108};
  const tmp = new THREE.Vector3();
  const ctl = {
    renderer, scene, cam, earth, atmo, view, spin: opts.spin ?? 0.05,
    biasX: opts.biasX ?? 0, biasY: opts.biasY ?? 0, running: false,
    place(w = innerWidth, h = innerHeight) {
      const dpr = Math.min(2, devicePixelRatio || 1);
      renderer.setPixelRatio(dpr);
      renderer.setSize(w, h, false);
      cam.aspect = w / h;
      // 版式偏置用 setViewOffset，不用 CSS 挪画布：星星得铺满整屏，球挪位置就行
      if (ctl.biasX || ctl.biasY) cam.setViewOffset(w, h, -ctl.biasX * w, -ctl.biasY * h, w, h);
      else cam.clearViewOffset();
      cam.near = Math.max(0.0004, view.dist * 0.0004);
      cam.far = Math.max(30, view.dist * 12);
      cam.updateProjectionMatrix();
      dirOf(view.lat, view.lon, tmp).multiplyScalar(view.dist);
      cam.position.copy(tmp);
      cam.up.set(0, 1, 0);
      cam.lookAt(0, 0, 0);
    },
    frame(dt, w, h) {
      earth.rotation.y += ctl.spin * dt;           // 自转轴 = 地理北极，视觉上就是"地球在转"
      atmo.rotation.y = earth.rotation.y;
      ctl.place(w, h);
      renderer.render(scene, cam);
    },
    start() {
      if (ctl.running) return;
      ctl.running = true;
      let last = performance.now();
      const loop = () => {
        if (!ctl.running) return;
        const now = performance.now();
        ctl.frame(Math.min(0.05, (now - last) / 1000));
        last = now;
        requestAnimationFrame(loop);
      };
      requestAnimationFrame(loop);
    },
    stop() { ctl.running = false; },
    dispose() { ctl.stop(); renderer.dispose(); },
  };
  ctl.place();      // 先按视口量好画布：rAF 一帧不走的场合（面板被遮住）也不能停在 300×150
  ctl.buildMs = Math.round(performance.now() - t0);   // 封面是第一次打开就要看见的东西，成本得能被量出来
  addEventListener('resize', () => { if (!ctl.running) ctl.place(); });
  return ctl;
}
