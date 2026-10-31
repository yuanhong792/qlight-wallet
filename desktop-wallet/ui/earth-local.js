// 席位与局部大地：把「一枚币站在地球哪一块、那块地长什么样」算成纯函数。
//
// 真身在 tools/world/seat.py 与 tools/world/gen_planet_local.py。这份是镜像，
// 不是重写：逐算子照抄，连浮点累加顺序都对齐，最后拿 fingerprint 对拍。
// 三方（Python 参考 / 浏览器 JS / 共识 Rust）必须烤出同一份 65536 字节——
// 只要有一方差一个字节，「矿脉不可作假」这句话就作废。
//
// 两处抄错的坑，都在这份里显式处理了：
//   1. numpy 的 np.rint 是**四舍六入五取偶**，不是 JS 的 Math.round（五永远进）。
//      所以有 rint()，直接用 Math.round 会在半档处差 1 字节。
//   2. RELIEF_FOLD 与 MIN_COS_LAT 含 pow/cos，跨 libm 可能差 1 ULP。
//      这两个数是常量，写成参考实现打印出的全精度字面量，运行时不再算超越函数。
//      运行时唯一的超越函数是 cos(radians(lat))（算经度跨度用），对拍覆盖多席位验过。
import { GW, GH, SEA_CODES, unquantize } from './earth-grid.js';

export const N = 256;
export const CELL_M = 35.0;
export const SEAT_RULE = 'QL-SEAT-v1';
export const LOCAL_RULE = 'QL-EARTH-LOCAL-v1';
export const TRIES_MAX = 4096;

const WIDTH_KM = N * CELL_M / 1000.0;          // 8.96
// 地图渲染要按真实海拔铺色（不是按字节），所以这几个共识常量得能被外面拿到
export const SEA_FIELD = 86, SAND_FIELD = 98;
export const BEACH_M = 4.0, SHELF_MAX_M = 40.0;
const TOP_FIELD = 247;
const SHELF_M_PER_CELL = 0.5;
const M_PER_DEG = 111320.0;
const LAND_BYTES = TOP_FIELD - SAND_FIELD;     // 149
const MAX_EXAG = 2.5;
const GAIN_MIN = (CELL_M / 8.0) / MAX_EXAG;    // 1.75
const NOISE_L = 280.0;
const NOISE_GAIN = [1.0, 0.5, 0.25];
const RUGGED_SIGMA_M = 800.0;
const AMP_FLOOR_M = 4.0;
const RELIEF_FOLD = 0.38314597069681794;        // (8.96/39.2)**0.65
const SEA_DETAIL = 0.35;
const MIN_COS_LAT = 0.0174524064372836;         // cos(radians(89))
const DEG2RAD = 0.017453292519943295;           // CPython math.radians 用的那个乘数

/** 网格只读一次：2 MiB，封面、地图、世界层共用同一份拷贝 */
let gridCache = null;
export async function loadHeightGrid() {
  if (!gridCache) {
    const buf = await fetch(new URL('earth-global-h.bin', import.meta.url).href)
      .then((r) => r.arrayBuffer());
    gridCache = new Uint8Array(buf);
    if (gridCache.length !== GW * GH) throw new Error(`星球层尺寸 ${gridCache.length} != ${GW * GH}`);
  }
  return gridCache;
}

/** numpy 的 rint：四舍六入五取偶。floor 对负数也向下，所以 d ∈ [0,1)。 */
function rint(x) {
  const f = Math.floor(x), d = x - f;
  if (d > 0.5) return f + 1;
  if (d < 0.5) return f;
  return f % 2 === 0 ? f : f + 1;
}

/** 32 位整数哈希（Murmur3 finalizer）。Python 侧全程 mod 2^32，JS 用 Math.imul 取低 32 位。 */
function h32(x, y, seed) {
  let h = ((seed >>> 0) ^ Math.imul(x >>> 0, 0x27D4EB2D) ^ Math.imul(y >>> 0, 0x165667B1)) >>> 0;
  h = (h ^ (h >>> 16)) >>> 0;
  h = Math.imul(h, 0x85EBCA6B) >>> 0;
  h = (h ^ (h >>> 13)) >>> 0;
  h = Math.imul(h, 0xC2B2AE35) >>> 0;
  return (h ^ (h >>> 16)) >>> 0;
}

/** SHA-256（WebCrypto），席位链要用它，浏览器里只有这一条路 */
async function sha256(bytes) {
  const d = await crypto.subtle.digest('SHA-256', bytes);
  return new Uint8Array(d);
}

const enc = new TextEncoder();

/** 席位链的前 `max` 个陆地格——**这条链只写这一遍**。
 *  `seatOf` 取第 1 个，漫游清单（QL-ROAM-v1）取前 1+64 个：如果两处各抄一份循环，
 *  「家园那一格」和「漫游列表第 0 项」迟早对不上，而那正是"我到底站在哪"的凭据。
 *  去重按格号：4096 轮在 209 万格里有放回地抽，重复抽到同一格是正常事件；
 *  在席位里无所谓（第一个就是它），在清单里会变成"两个目的地长在同一处"，那是噪声。 */
export async function seatPath(address, genesisHex, H, max) {
  const canon = `${SEAT_RULE}|${address.trim().toLowerCase()}|${genesisHex.trim().toLowerCase()}`;
  let h = await sha256(enc.encode(canon));
  const out = [], seen = new Set();
  for (let tries = 0; tries < TRIES_MAX && out.length < max; tries++) {
    if (tries) h = await sha256(h);
    // 每轮都要重挂 DataView：sha256() 换了 buffer，旧视图会一直读第一次的摘要
    const view = new DataView(h.buffer);
    const i = view.getUint32(0, false) % GW;   // false = 大端
    const j = view.getUint32(4, false) % GH;
    const code = H[j * GW + i];                // JS 这份是平铺的，Python 那份是 [j, i]
    if (code >= SEA_CODES) {
      const cell = j * GW + i;
      if (seen.has(cell)) continue;
      seen.add(cell);
      out.push({ i, j, cell, code,
                 lat: 90.0 - (j + 0.5) / GH * 180.0,
                 lon: -180.0 + (i + 0.5) / GW * 360.0,
                 tries, canon });
    }
  }
  return out;
}

/** 席位：地址 ⊕ 创世 → SHA-256 链 → 全球陆地格拒绝采样。
 *  与 seat.py::seat_of 逐字同构——大端取 4 字节、先取格再判陆、陆地码 ≥128。
 *  同地址同创世永远同一个格（不可重掷）；但哈希不保证全球唯一，唯一要上链记名。 */
export async function seatOf(address, genesisHex, H) {
  const p = await seatPath(address, genesisHex, H, 1);
  return p.length ? p[0] : null;              // 4096 轮全是海：不可能，调用方要报错
}

/** 一块 256×256 窗口的每一行/每一列落在地球哪条经纬线上。
 *  与 genBlock 用同一个 gridCoords，不另立公式：HUD 报的坐标必须和烤出这块地的
 *  那套算法逐字一致，否则"你站在北纬 X 度"这句话在屏幕上就是编的。 */
export function parcelCoords(latC, lonC) {
  return gridCoords(latC, lonC);
}

// ── 骨架 ────────────────────────────────────────────────────────────────────
function gridCoords(latC, lonC) {
  const halfM = WIDTH_KM * 1000.0 / 2.0;
  const dlat = halfM / M_PER_DEG;
  const dlon = halfM / (M_PER_DEG * Math.max(Math.cos(latC * DEG2RAD), MIN_COS_LAT));
  const lats = new Float64Array(N), lons = new Float64Array(N);
  for (let k = 0; k < N; k++) {
    lats[k] = latC + dlat - (k + 0.5) * (2 * dlat / N);
    lons[k] = lonC - dlon + (k + 0.5) * (2 * dlon / N);
  }
  return { lats, lons };
}

function regionalRelief(H, latC, lonC, r = 1) {
  const i = Math.min(GW - 1, Math.max(0, Math.floor((lonC + 180) / 360 * GW)));
  const j = Math.min(GH - 1, Math.max(0, Math.floor((90 - latC) / 180 * GH)));
  let mn = Infinity, mx = -Infinity, any = false;
  for (let jj = Math.max(j - r, 0); jj < Math.min(j + r + 1, GH); jj++) {
    for (let ii = Math.max(i - r, 0); ii < Math.min(i + r + 1, GW); ii++) {
      const c = H[jj * GW + ii];
      if (c < SEA_CODES) continue;              // 海格不参与：-6000 米混进极差会把平原烤成高山
      const m = unquantize(c);
      any = true;
      if (m < mn) mn = m;
      if (m > mx) mx = m;
    }
  }
  return any ? mx - mn : 0.0;
}

/** 离岸距离（格，四邻曼哈顿）。Python 那份是迭代扩散，结果是同一 thing：到最近陆地格的
 *  曼哈顿距离，所以这里用 BFS 队列一次算完——同算法，只是不必跑 512 遍全图。 */
function offshoreCells(land) {
  const total = N * N, INF = 0x7fffffff;
  const d = new Int32Array(total).fill(INF);
  const q = new Int32Array(total);
  let head = 0, tail = 0;
  for (let k = 0; k < total; k++) if (land[k]) { d[k] = 0; q[tail++] = k; }
  while (head < tail) {
    const p = q[head++], base = d[p] + 1;
    const x = p % N, y = (p / N) | 0;
    if (x > 0 && d[p - 1] > base) { d[p - 1] = base; q[tail++] = p - 1; }
    if (x < N - 1 && d[p + 1] > base) { d[p + 1] = base; q[tail++] = p + 1; }
    if (y > 0 && d[p - N] > base) { d[p - N] = base; q[tail++] = p - N; }
    if (y < N - 1 && d[p + N] > base) { d[p + N] = base; q[tail++] = p + N; }
  }
  return d;
}

function baseField(H, lats, lons) {
  const hh = new Float64Array(N * N), land = new Uint8Array(N * N);
  const offshore = new Uint8Array(N * N);
  // 逐格：4 角只累陆地权，顺序与 Python 的 (dj,di) 二重循环一致
  for (let row = 0; row < N; row++) {
    const fy = (90.0 - lats[row]) / 180.0 * GH - 0.5;
    const j0 = Math.floor(fy), ty = fy - j0;
    for (let col = 0; col < N; col++) {
      const fx = (lons[col] + 180.0) / 360.0 * GW - 0.5;
      const i0 = Math.floor(fx), tx = fx - i0;
      let num = 0.0, den = 0.0, has = 0;
      for (const [dj, wy] of [[0, 1 - ty], [1, ty]]) {
        const jj = Math.min(GH - 1, Math.max(0, j0 + dj));
        for (const [di, wx] of [[0, 1 - tx], [1, tx]]) {
          const ii = Math.min(GW - 1, Math.max(0, i0 + di));
          const code = H[jj * GW + ii];
          if (code < SEA_CODES) continue;
          const w = wx * wy;
          num += unquantize(code) * w;          // 与 where(l,unq,0)*(w*l) 同值
          den += w;
          if (w > 0.25) has = 1;
        }
      }
      const k = row * N + col;
      land[k] = has;
      hh[k] = den > 0 ? num / Math.max(den, 1e-12) : 0.0;
    }
  }
  // 海格： shelf = max(-0.5·离岸, -40)
  const off = offshoreCells(land);
  for (let k = 0; k < N * N; k++) if (!land[k]) hh[k] = Math.max(-SHELF_M_PER_CELL * off[k], -SHELF_MAX_M);
  return { h: hh, land };
}

// ── 微地形 ──────────────────────────────────────────────────────────────────
function valueNoise(lats, lons, seed, stepM) {
  const out = new Float64Array(N * N);
  const gy0 = new Int32Array(N), fyt = new Float64Array(N);
  for (let row = 0; row < N; row++) {
    const ym = (90.0 - lats[row]) * M_PER_DEG / stepM;
    const g = Math.floor(ym);
    gy0[row] = g;
    const f = ym - g;
    fyt[row] = f * f * (3 - 2 * f);
  }
  const gx0 = new Int32Array(N), fxt = new Float64Array(N);
  for (let col = 0; col < N; col++) {
    const xm = (lons[col] + 180.0) * M_PER_DEG / stepM;
    const g = Math.floor(xm);
    gx0[col] = g;
    const f = xm - g;
    fxt[col] = f * f * (3 - 2 * f);
  }
  for (let row = 0; row < N; row++) {
    const sy = fyt[row];
    for (let col = 0; col < N; col++) {
      const sx = fxt[col];
      let v = 0.0;
      for (const [dy, wy] of [[0, 1 - sy], [1, sy]]) {
        for (const [dx, wx] of [[0, 1 - sx], [1, sx]]) {
          v += h32((gx0[col] + dx) >>> 0, (gy0[row] + dy) >>> 0, seed) * (wx * wy);
        }
      }
      out[row * N + col] = v / 4294967296.0;
    }
  }
  return out;
}

function fbm(lats, lons, seed, octaves = 3) {
  const acc = new Float64Array(N * N);
  let den = 0.0;
  for (let o = 0; o < octaves; o++) {
    const a = NOISE_GAIN[o];
    const nv = valueNoise(lats, lons, (seed + Math.imul(o, 0x9E3779B9)) >>> 0, NOISE_L / (1 << o));
    for (let k = 0; k < N * N; k++) acc[k] += a * nv[k];
    den += a;
  }
  for (let k = 0; k < N * N; k++) acc[k] /= den;
  return acc;
}

function detailField(sigma, seed, lats, lons, baseMinM) {
  const r = Math.min(1.0, sigma / RUGGED_SIGMA_M);
  let amp = Math.max(AMP_FLOOR_M, 0.5 * sigma * RELIEF_FOLD);
  if (baseMinM !== null && baseMinM !== undefined && amp > baseMinM && baseMinM > 0) amp = baseMinM;
  const n = fbm(lats, lons, seed);
  const det = new Float64Array(N * N);
  for (let k = 0; k < N * N; k++) {
    const shape = (1.0 - r) * (n[k] - 0.5) + r * ((1.0 - Math.abs(2.0 * n[k] - 1.0)) - 0.45);
    det[k] = amp * 2.0 * shape;
  }
  return { det, amp, rugged: r };
}

// ── 量化 ────────────────────────────────────────────────────────────────────
function landBasis(h, land) {
  let mn = Infinity, mx = -Infinity;
  for (let k = 0; k < N * N; k++) if (land[k]) { if (h[k] < mn) mn = h[k]; if (h[k] > mx) mx = h[k]; }
  const ref = Math.max(BEACH_M, mn);
  return { ref, gain: Math.max((mx - ref) / LAND_BYTES, GAIN_MIN) };
}

function quantizeLocal(h, land) {
  const f = new Uint8Array(N * N);
  const { ref, gain } = landBasis(h, land);
  for (let k = 0; k < N * N; k++) {
    let v;
    if (!land[k]) {
      const depth = Math.min(1.0, Math.max(0.0, h[k] / -SHELF_MAX_M));
      v = SEA_FIELD - 1 - depth * (SEA_FIELD - 2);
    } else if (h[k] < BEACH_M) {
      v = SEA_FIELD + (Math.min(BEACH_M, Math.max(0.0, h[k])) / BEACH_M) * (SAND_FIELD - SEA_FIELD);
    } else {
      v = SAND_FIELD + (h[k] - ref) / gain;
    }
    f[k] = Math.min(TOP_FIELD, Math.max(1, rint(v)));
  }
  return { f, ref, gain };
}

/** 一块地：席位中心 (lat, lon) + 种子 → 256×256 高度场。与 gen_planet_local.gen 同构。
 *  种子用共识的 RULES_SEED，所以大地不随链长高而挪动：签出去的 (x, y, layer) 才复算得回。 */
export function genBlock(H, latC, lonC, seed) {
  const { lats, lons } = gridCoords(latC, lonC);
  const sigma = regionalRelief(H, latC, lonC);
  const { h: hEff, land } = baseField(H, lats, lons);
  let baseMin = null;
  for (let k = 0; k < N * N; k++)
    if (land[k] && (baseMin === null || hEff[k] < baseMin)) baseMin = hEff[k];
  const { det, amp, rugged } = detailField(sigma, seed, lats, lons, baseMin);
  const h = new Float64Array(N * N);
  for (let k = 0; k < N * N; k++) h[k] = hEff[k] + (land[k] ? det[k] : -Math.abs(det[k]) * SEA_DETAIL);
  const { f, ref, gain } = quantizeLocal(h, land);
  let landCells = 0;
  for (let k = 0; k < N * N; k++) landCells += land[k];
  let used = 0;
  for (let k = 0; k < N * N; k++) if (f[k] >= SAND_FIELD) used = Math.max(used, f[k]);
  const info = { lat: latC, lon: lonC, sigma_m: Math.round(sigma * 10) / 10,
                 rugged: Math.round(rugged * 1000) / 1000, land_cells: landCells,
                 amp_m: Math.round(amp * 10) / 10, base_m: Math.round(ref * 10) / 10,
                 gain_m_per_byte: Math.round(gain * 100) / 100,
                 exag: Math.round((CELL_M / 8.0) / gain * 100) / 100,
                 land_bytes_used: used ? used - SAND_FIELD : 0, rule: LOCAL_RULE };
  return { field: f, info };
}

/** 两路独立折叠拼成 16 位十六进制——改一格就变，三方对拍就看它 */
export function fingerprint(buf) {
  let out = '';
  for (const [basis, prime] of [[0x811C9DC5, 16777619], [0x01000193, 3295579737]]) {
    let d = basis >>> 0;
    for (let k = 0; k < buf.length; k++) d = Math.imul(d ^ buf[k], prime) >>> 0;
    out += d.toString(16).padStart(8, '0');
  }
  return out;
}
