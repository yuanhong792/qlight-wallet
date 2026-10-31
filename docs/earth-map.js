// 世界地图 = 整颗地球，不是"一块地的地图"。
//
// 业主口径（2026-10-30）：「这里应该是世界地图，不应该是一个地方的地图，也不要具体在
// 某地，这样显得地图很小，没有什么可玩性」。所以这一层画的是随包那 2 MiB 全球高程网格
// （2048×1024，1 格 ≈ 19.6 公里），地球就是被它分成 **2,097,152 份**——"分成若干份"
// 有真身，不是切图；其中陆地 615,081 份，才是可以派给币的那一份。
//
// 两块地必须同时看得见、并且说清各是什么：
//   · 席位格（金环）——这枚币按 QL-SEAT-v1 分到的那一格，地貌由 QL-EARTH-LOCAL-v1 现算；
//   · 脚下这块（青框）——当前真正在走的实测窗口。席位地块接管脚下要改共识
//     （`surface_layer` 进 DIG 合法性与 `entity_root`），所以这里不许把两张说成一张。
//
// 全程 2D canvas：地图是"看一眼我在地球哪儿"，不是第二个 3D 视图，
// 为它再开一个 WebGL 上下文不值当（封面与俯冲已经各有一份）。
import { GW, GH, SEA_CODES, unquantize, cellOf } from './earth-grid.js';
import { loadHeightGrid, genBlock, fingerprint, N, CELL_M,
         SEA_FIELD, SAND_FIELD, BEACH_M, SHELF_MAX_M } from './earth-local.js';

const CELL_KM = 19.57;          // 赤道处 40075 km ÷ 2048
const M_PER_ROW = 111.32 * (180 / GH) * 1000;   // 一行跨的南北距离（米）
const SHADE_SPAN_M = 20000;   // 山体阴影采坡的地面跨度：约等于赤道处的一格

/** 高程 → 颜色：海按深度分蓝，陆按高度走 绿→黄绿→褐→灰→雪线白。
 *  配色不是审美选择，是"读得出山在哪"：海陆用色相分，高度用明度分。 */
function ramp(m) {
  if (!(m >= 0)) return [10, 24, 44];                       // 0 码 = 无数据
  if (m < 0) {
    const t = Math.min(1, -m / 4200);                       // 深海压暗
    return [34 - 22 * t, 96 - 60 * t, 156 - 92 * t];
  }
  const stops = [[0, 74, 112, 66], [200, 118, 142, 74], [600, 158, 150, 88],
                 [1500, 150, 112, 74], [3000, 128, 118, 112],
                 [5000, 214, 220, 228], [8100, 246, 248, 250]];
  for (let k = 0; k < stops.length - 1; k++) {
    const [a, r1, g1, b1] = stops[k], [bb, r2, g2, b2] = stops[k + 1];
    if (m < bb) {
      const t = (m - a) / (bb - a);
      return [r1 + (r2 - r1) * t, g1 + (g2 - g1) * t, b1 + (b2 - b1) * t];
    }
  }
  const [, r, g, b] = stops[stops.length - 1];
  return [r, g, b];
}

let baseLayer = null;
/** 底图算一次就留着：2M 格的高程 + 山体阴影，每次重画是几百毫秒的白等。
 *  数据是随包常量，永远不需要失效。 */
async function buildBase() {
  if (baseLayer) return baseLayer;
  const H = await loadHeightGrid();
  const el = new Float64Array(GW * GH);
  for (let k = 0; k < el.length; k++) el[k] = unquantize(H[k]) ?? -8000;
  const cv = document.createElement('canvas');
  cv.width = GW; cv.height = GH;
  const ctx = cv.getContext('2d');
  const img = ctx.createImageData(GW, GH), px = img.data;
  for (let j = 0; j < GH; j++) {
    const lat = 90 - (j + 0.5) / GH * 180;
    const mPerCol = M_PER_ROW * Math.max(Math.abs(Math.cos(lat * Math.PI / 180)), 0.02);
    // 东西向梯度按**固定地面距离**取样，不是固定格数。等经纬网格的经线在极地收敛：
    // 赤道一格 19.6 公里，−85° 只剩 1.4 公里，跨 1 格采坡把冰壁算成 0.9 的坡度
    // （约 42°）——南端那条黑白条纹带就是这么来的。|纬度| < 70° 时这一档仍然取 1 格，
    // 与改之前逐位相同；再往两极才摊开，因为那里"一格"已经不是同一个物理长度了。
    const S = Math.max(1, Math.round(SHADE_SPAN_M / (2 * mPerCol)));
    for (let i = 0; i < GW; i++) {
      const k = j * GW + i, m = el[k];
      const land = m >= 0;
      let [r, g, b] = ramp(m);
      if (land) {
        // 山体阴影：光从西北来，朝西北升的坡亮、朝东南降的坡暗。
        // 没有这一层，喜马拉雅和一块田都是同一团褐色——地图就读不出"山在哪"。
        const ir = Math.min(GW - 1, i + S), il = Math.max(0, i - S);
        const gx = (el[j * GW + ir] - el[j * GW + il]) / ((ir - il) * mPerCol);
        const gy = (el[k + (j + 1 < GH ? GW : 0)] - el[k - (j > 0 ? GW : 0)]) / (2 * M_PER_ROW);
        const sh = 0.74 + 0.42 * Math.tanh((gx + gy) * 0.5 * 2600);
        r *= sh; g *= sh; b *= sh;
      }
      const o = k * 4;
      px[o] = Math.min(255, r); px[o + 1] = Math.min(255, g);
      px[o + 2] = Math.min(255, b); px[o + 3] = 255;
    }
  }
  ctx.putImageData(img, 0, 0);
  baseLayer = cv;
  return cv;
}

let parcelCache = null;
/** 席位那一格里 8.96 公里见方的地，本地现算——这份字节场与 Rust、Python 逐位相同
 *  （dev/seat-crosscheck.html 就是对它做对拍的），所以这块预览不是示意图。 */
async function buildParcel(lat, lon, seed) {
  const key = `${lat}|${lon}|${seed}`;
  if (parcelCache && parcelCache.key === key) return parcelCache.cv;
  const H = await loadHeightGrid();
  const { field, info } = genBlock(H, lat, lon, seed);
  const cv = document.createElement('canvas');
  cv.width = N; cv.height = N;
  const ctx = cv.getContext('2d');
  const img = ctx.createImageData(N, N), px = img.data;
  // 字节 → 真实米（quantize_local 的逆）：86 档是真海平面（共识常量），98 档起是
  // info.base_m 这块地的基面，每档 info.gain_m_per_byte 米。
  // 按**真实海拔**铺色，山区块和滩涂块才不会长同一张脸：整块归一化会把 60 米的
  // 河滩拉成 900 米的落差，看着像高原，其实是平地——那是画出来的假象，不是数据。
  const gain = info.gain_m_per_byte, ref = info.base_m;
  const m = new Float64Array(N * N);
  for (let k = 0; k < N * N; k++) {
    const b = field[k];
    m[k] = b < SEA_FIELD ? -((SEA_FIELD - 1 - b) / (SEA_FIELD - 2)) * SHELF_MAX_M
      : b < SAND_FIELD ? (b - SEA_FIELD) / (SAND_FIELD - SEA_FIELD) * BEACH_M
        : ref + (b - SAND_FIELD) * gain;
  }
  // 阴影增益按这块地**自己的**坡度定标。写死一个数必然两头不讨好：平原块 tanh 不出
  // 值（等于没有阴影），山区块整片饱和成黑白噪点（σ=848 米那块就是这么糊的）。
  // 梯度跨 3 格 = 105 米：微地形最短一波是 280 米，跨 1 格采到的主要是抖动不是地形。
  const S = 3;
  const deriv = (i, j) => {
    const ir = Math.min(N - 1, i + S), il = Math.max(0, i - S);
    const jr = Math.min(N - 1, j + S), jl = Math.max(0, j - S);
    const gx = (m[j * N + ir] - m[j * N + il]) / ((ir - il) * CELL_M);
    const gy = (m[jr * N + i] - m[jl * N + i]) / ((jr - jl) * CELL_M);
    return (gx + gy) * 0.5;                        // 光从西北来，与全球底图同一套约定
  };
  let mad = 0;
  for (let j = 0; j < N; j++) for (let i = 0; i < N; i++) mad += Math.abs(deriv(i, j));
  mad /= N * N;
  // 0.8 是"典型坡落在 tanh 的线性段、最陡的一撮才饱和"；12 是平原块的上限——
  // 再放大就把米级抖动提成悬崖，那是画出来的假地形。
  const K = Math.min(12, 0.8 / Math.max(mad, 1e-9));
  for (let j = 0; j < N; j++) {
    for (let i = 0; i < N; i++) {
      const k = j * N + i, h = m[k];
      const t = h < 0 ? Math.min(1, -h / SHELF_MAX_M) : 0;
      const c = h < 0 ? [30 - 18 * t, 92 - 58 * t, 150 - 88 * t] : ramp(h);
      const sh = 0.74 + 0.42 * Math.tanh(deriv(i, j) * K);
      const o = k * 4;
      px[o] = Math.min(255, Math.max(0, c[0] * sh));
      px[o + 1] = Math.min(255, Math.max(0, c[1] * sh));
      px[o + 2] = Math.min(255, Math.max(0, c[2] * sh));
      px[o + 3] = 255;
    }
  }
  ctx.putImageData(img, 0, 0);
  parcelCache = { key, cv, info, fp: fingerprint(field) };
  return parcelCache.cv;
}

/** 画整颗地球：底图 + 席位金环 + 脚下青框 + 可去之处 + 经纬网格线（本初子午线与赤道）。
 *  `walked` 是当前真正在走的窗口中心；席位与它不同才需要两个标记。
 *  `roam` 是 QL-ROAM-v1 的目的地清单：解锁的用实圈加编号，没解锁的用空心灰圈——
 *  「锁着」必须看得见，点上去还要当场说清差几格，不许做成"点了没反应"。
 *  标记的屏幕位置同时写进 `lastMarkers`，供 earthHit() 做命中判定（同一套坐标，不各算一遍）。 */
let lastMarkers = [];
export async function paintEarth(canvas, seat, walked, seed, roam) {
  const base = await buildBase();
  const ctx = canvas.getContext('2d');
  const { width: w, height: h } = canvas;
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = 'high';
  ctx.clearRect(0, 0, w, h);
  ctx.drawImage(base, 0, 0, w, h);

  ctx.strokeStyle = 'rgba(255,255,255,.16)';
  ctx.lineWidth = 1;
  for (const lon of [-120, -60, 0, 60, 120]) {
    const x = (lon + 180) / 360 * w;
    ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, h); ctx.stroke();
  }
  for (const lat of [-60, -30, 0, 30, 60]) {
    const y = (90 - lat) / 180 * h;
    ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(w, y); ctx.stroke();
  }
  ctx.strokeStyle = 'rgba(240,214,158,.5)';
  ctx.beginPath(); ctx.moveTo(0, h / 2); ctx.lineTo(w, h / 2); ctx.stroke();

  if (walked) {
    const [wi, wj] = cellOf(walked.lat, walked.lon);
    const x = (wi + 0.5) / GW * w, y = (wj + 0.5) / GH * h;
    ctx.strokeStyle = '#7FD7E8';
    ctx.lineWidth = 1.5;
    ctx.strokeRect(x - 7, y - 7, 14, 14);
  }
  if (seat) {
    const x = (seat.i + 0.5) / GW * w, y = (seat.j + 0.5) / GH * h;
    ctx.strokeStyle = '#FFD479';
    ctx.lineWidth = 2;
    ctx.beginPath(); ctx.arc(x, y, 10, 0, Math.PI * 2); ctx.stroke();
    ctx.beginPath();
    ctx.moveTo(x - 16, y); ctx.lineTo(x - 12, y);
    ctx.moveTo(x + 12, y); ctx.lineTo(x + 16, y);
    ctx.moveTo(x, y - 16); ctx.lineTo(x, y - 12);
    ctx.moveTo(x, y + 12); ctx.lineTo(x, y + 16);
    ctx.lineWidth = 1.5; ctx.stroke();
  }

  // 可去之处：编号点。配色遵守 §9.27 的"彩度按面积分配"——地图是整颗地球，
  // 点位是小面积点缀，所以这里容得下金；未解锁的用低彩度灰蓝空心圈，读得出"有，但还进不去"。
  lastMarkers = [];
  const list = roam || [];
  ctx.textAlign = 'center'; ctx.textBaseline = 'middle';
  ctx.font = '700 8px ui-monospace, monospace';
  for (const r of list) {
    const x = (r.i + 0.5) / GW * w, y = (r.j + 0.5) / GH * h;
    lastMarkers.push({ kind: 'roam', n: r.n, unlocked: !!r.unlocked, x, y, cell: r.cell });
    if (r.unlocked) {
      ctx.fillStyle = 'rgba(18,22,31,.72)';
      ctx.beginPath(); ctx.arc(x, y, 6.5, 0, Math.PI * 2); ctx.fill();
      ctx.strokeStyle = '#F0D69E'; ctx.lineWidth = 1.4;
      ctx.beginPath(); ctx.arc(x, y, 6.5, 0, Math.PI * 2); ctx.stroke();
      ctx.fillStyle = '#F0D69E';
      ctx.fillText(String(r.n), x, y + 0.5);
    } else {
      ctx.strokeStyle = 'rgba(170,182,198,.62)'; ctx.lineWidth = 1;
      ctx.beginPath(); ctx.arc(x, y, 4, 0, Math.PI * 2); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(x - 2.4, y + 2.4); ctx.lineTo(x + 2.4, y - 2.4); ctx.stroke();
    }
  }
  if (seat) lastMarkers.push({ kind: 'home', x: (seat.i + 0.5) / GW * w, y: (seat.j + 0.5) / GH * h });
  return base;
}

/** 地图点击 → 命中的标记（9 像素内最近的一个），没有就 null。
 *  地球在 512×256 的面板上画 209 万格，一格只有 0.25 像素，所以命中判定只能按标记的
 *  屏幕半径来——按经纬度反查格子会点出一串"谁都没画在那里"的坐标。 */
export function earthHit(canvas, clientX, clientY, radius = 9) {
  const r = canvas.getBoundingClientRect();
  const x = (clientX - r.left) * (canvas.width / r.width);
  const y = (clientY - r.top) * (canvas.height / r.height);
  let best = null, bd = radius;
  for (const m of lastMarkers) {
    const d = Math.hypot(x - m.x, y - m.y);
    if (d < bd) { bd = d; best = m; }
  }
  return best;
}

/** 席位地块的缩略图（画进地球图右下角的方形凹槽里，与金环是同一块地） */
export async function parcelPreview(lat, lon, seed, size = 128) {
  const cv = await buildParcel(lat, lon, seed);
  const out = document.createElement('canvas');
  out.width = size; out.height = size;
  const ctx = out.getContext('2d');
  ctx.imageSmoothingEnabled = false;
  ctx.drawImage(cv, 0, 0, size, size);
  return { canvas: out, info: parcelCache.info, fp: parcelCache.fp };
}

export { GW, GH, SEA_CODES, CELL_KM };
