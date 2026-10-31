//! 席位与局部大地——`QL-SEAT-v1` + `QL-EARTH-LOCAL-v1` 的共识侧镜像。
//!
//! 真身在 `tools/world/seat.py` 与 `tools/world/gen_planet_local.py`，浏览器那份是
//! `ui/earth-local.js`。三份必须烤出**同一份 65536 字节**：高度场进 `surface_layer`，
//! 而 `surface_layer` 进 DIG 记录的合法性与矿格贴地判定，最后折进 `entity_root`
//! 和块哈希。哪一份差一个字节，那台机器眼里的世界就和别人不是一个世界。
//!
//! 为什么 Rust 里需要它：钱包可以只在前端画，但**别人签来的记录要能被本机重验**——
//! 校验一条 `(x, y, layer)` 的挖地承诺，必须知道"这个地址站在地球哪一格、
//! 那一格的地长什么样"。所以席位与烤块都不是渲染细节，是共识输入。
//!
//! 移植时最容易抄错的三处，这里都做了显式处理：
//! - `np.rint` 是**四舍六入五取偶**，`f64::round` 是四舍六入五**进位**：负半档能差 1 字节。
//! - 累加顺序：四角混合按 `(dj, di)` 的 `((0,·),(1,·)) × ((0,·),(1,·))` 顺序相加，
//!   浮点加法不满足结合律，换个顺序就能在 `floor()` 边界上翻出一个格。
//! - `RELIEF_FOLD` / `MIN_COS_LAT` 含 `pow`/`cos`，跨 libm 可能差 1 ULP，
//!   所以写成参考实现打印出的全精度字面量，运行时不再算这两个超越函数。
//!   运行时只剩 `cos(lat·π/180)` 一处超越函数：CPython 与本 Rust 都走同一套系统 libm，
//!   V8 用的是自带的 fdlibm；16 例夹具逐字节对过，未见分叉。

use sha2::{Digest, Sha256};

/// 星球层网格：等经纬，1 格 ≈ 19.6 公里。地球就是被它分成 2,097,152 份。
pub const GW: usize = 2048;
pub const GH: usize = 1024;
/// 高程码：<128 是海，≥128 是陆，0 是"这里没有数据"（不当陆地接受，否则造出查无此地的席位）
pub const SEA_CODES: u8 = 128;
const OCEAN_STEP: f64 = 48.0;
const LAND_BUCKETS: [(f64, f64, usize); 7] = [
    (0.0, 4.0, 32), (128.0, 8.0, 16), (256.0, 16.0, 16), (512.0, 32.0, 16),
    (1024.0, 64.0, 16), (2048.0, 128.0, 16), (4096.0, 256.0, 16),
];

pub const SEAT_RULE: &str = "QL-SEAT-v1";
pub const LOCAL_RULE: &str = "QL-EARTH-LOCAL-v1";
pub const TRIES_MAX: u32 = 4096;

/// 局部块尺寸与比例：一格 35 米，一块 8.96 公里见方
pub const N: usize = 256;
pub const CELL_M: f64 = 35.0;
const WIDTH_KM: f64 = (N as f64) * CELL_M / 1000.0;
const SEA_FIELD: f64 = 86.0;
const SAND_FIELD: f64 = 98.0;
const TOP_FIELD: f64 = 247.0;
const BEACH_M: f64 = 4.0;
const SHELF_M_PER_CELL: f64 = 0.5;
const SHELF_MAX_M: f64 = 40.0;
const M_PER_DEG: f64 = 111320.0;
const LAND_BYTES: f64 = TOP_FIELD - SAND_FIELD;
const MAX_EXAG: f64 = 2.5;
const GAIN_MIN: f64 = (CELL_M / 8.0) / MAX_EXAG;
const NOISE_L: f64 = 280.0;
const NOISE_GAIN: [f64; 3] = [1.0, 0.5, 0.25];
const RUGGED_SIGMA_M: f64 = 800.0;
const AMP_FLOOR_M: f64 = 4.0;
/// `(8.96 / 39.2) ^ 0.65`——自仿射折算：星球层量到的 σ 摊到 8.96 公里的块上只剩这些
const RELIEF_FOLD: f64 = 0.38314597069681794;
const SEA_DETAIL: f64 = 0.35;
const MIN_COS_LAT: f64 = 0.0174524064372836;
/// CPython `math.radians` 乘的那个数，JS 与 Rust 都照它写，不再各自算 π/180
const DEG2RAD: f64 = 0.017453292519943295;

/// 星球层高程网格：随包那 2 MiB，与前端渲染器读同一个文件。
/// 编译期嵌进二进制——运行时可以改 `entities.jsonl`，改不了大地。
pub const GLOBAL: &[u8] = include_bytes!("../../ui/earth-global-h.bin");

/// 实测崂山块（`terrain::EARTH`）的覆盖窗口：落在这里面就用真测量，不用推导。
const MEASURED_RECT: (f64, f64, f64, f64) = (120.615359, 36.112456, 120.715041, 36.192944);

/// numpy 的 rint：四舍六入五取偶。`floor` 对负数也向下，所以 `d` 恒在 [0,1)。
fn rint(x: f64) -> f64 {
    let f = x.floor();
    let d = x - f;
    if d > 0.5 {
        f + 1.0
    } else if d < 0.5 {
        f
    } else if f % 2.0 == 0.0 {
        f
    } else {
        f + 1.0
    }
}

/// 32 位整数哈希（Murmur3 finalizer）——与 JS 的 `Math.imul`、Python 的 mod 2³² 同余
fn h32(x: u32, y: u32, seed: u32) -> u32 {
    let mut h = seed ^ x.wrapping_mul(0x27D4_EB2D) ^ y.wrapping_mul(0x1656_67B1);
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}

/// 陆地码 128..255 → 米（档中值）。与 `ui/earth-grid.js` 那份逐字同义。
/// 逐档推进：进不了本档的把档宽扣掉再看下一档，落进本档的当场定值。
pub fn land_meters(code: u8) -> f64 {
    let mut c = code as i32 - SEA_CODES as i32; // 0..127
    for (start, step, count) in LAND_BUCKETS {
        if c < count as i32 {
            return start + (c as f64 + 0.5) * step;
        }
        c -= count as i32;
    }
    let (s, st, ct) = LAND_BUCKETS[LAND_BUCKETS.len() - 1];
    s + (ct as f64 - 0.5) * st
}

/// 星球层 1 字节 → 米；`None` = 这一格没有数据（0 码），绝不静默当 0 米
pub fn unquantize(b: u8) -> Option<f64> {
    if b == 0 {
        None
    } else if b < SEA_CODES {
        Some(-((SEA_CODES as i32 - 1 - b as i32) as f64 + 0.5) * OCEAN_STEP)
    } else {
        Some(land_meters(b))
    }
}

/// 一枚币的地块中心：地址 ⊕ 创世哈希 → SHA-256 链 → 陆地格拒绝采样。
///
/// 大端取字节是刻意的：小端在 Python/JS/Rust 之间没有第三种解释可用，只有大端
/// 三方都写成同一句。命中不了陆地就换下一轮哈希，4096 轮试不中返回 `None`——
/// 调用方必须报错，不许"随便找块地"：落点被偷偷改一次，此前签过的 `(x, z)` 全废。
pub fn seat_of(address: &str, genesis_hex: &str, h: &[u8]) -> Option<Seat> {
    assert_eq!(h.len(), GW * GH, "星球层尺寸 {} != {}", h.len(), GW * GH);
    let canon = format!(
        "{}|{}|{}",
        SEAT_RULE,
        address.trim().to_ascii_lowercase(),
        genesis_hex.trim().to_ascii_lowercase()
    );
    let mut digest: [u8; 32] = Sha256::digest(canon.as_bytes()).into();
    for tries in 0..TRIES_MAX {
        if tries > 0 {
            digest = Sha256::digest(digest).into();
        }
        let i = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) as usize % GW;
        let j = u32::from_be_bytes([digest[4], digest[5], digest[6], digest[7]]) as usize % GH;
        let code = h[j * GW + i];
        if code >= SEA_CODES {
            return Some(Seat {
                i,
                j,
                cell: j * GW + i,
                code,
                lon: -180.0 + (i as f64 + 0.5) / GW as f64 * 360.0,
                lat: 90.0 - (j as f64 + 0.5) / GH as f64 * 180.0,
                tries,
            });
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seat {
    pub i: usize,
    pub j: usize,
    pub cell: usize,
    pub code: u8,
    pub lat: f64,
    pub lon: f64,
    pub tries: u32,
}

/// 一块地的体检报告。`source` 区分"实测"与"推导"，这两块地在产品上是同一等公民，
/// 但在证据上是两等的：实测那块有卫星数据兜，推导那块只有规则兜。
#[derive(Debug, Clone, Copy)]
pub struct BlockInfo {
    pub lat: f64,
    pub lon: f64,
    pub sigma_m: f64,
    pub rugged: f64,
    pub land_cells: usize,
    pub amp_m: f64,
    pub base_m: f64,
    pub gain_m_per_byte: f64,
    pub source: &'static str,
}

fn grid_coords(lat_c: f64, lon_c: f64) -> ([f64; N], [f64; N]) {
    let half_m = WIDTH_KM * 1000.0 / 2.0;
    let dlat = half_m / M_PER_DEG;
    let dlon = half_m / (M_PER_DEG * ((lat_c * DEG2RAD).cos()).max(MIN_COS_LAT));
    let mut lats = [0.0f64; N];
    let mut lons = [0.0f64; N];
    for k in 0..N {
        lats[k] = lat_c + dlat - (k as f64 + 0.5) * (2.0 * dlat / N as f64);
        lons[k] = lon_c - dlon + (k as f64 + 0.5) * (2.0 * dlon / N as f64);
    }
    (lats, lons)
}

/// 周围 3×3 **陆地**格的极差：微地形幅度的唯一依据。海格不参与，
/// 否则海岸线上每一块平原都会被 -6000 米的海底烤成高山。
fn regional_relief(h: &[u8], lat_c: f64, lon_c: f64) -> f64 {
    let i = ((lon_c + 180.0) / 360.0 * GW as f64).floor().clamp(0.0, GW as f64 - 1.0) as usize;
    let j = ((90.0 - lat_c) / 180.0 * GH as f64).floor().clamp(0.0, GH as f64 - 1.0) as usize;
    let (mut mn, mut mx, mut any) = (f64::MAX, f64::MIN, false);
    for jj in j.saturating_sub(1)..=(j + 1).min(GH - 1) {
        for ii in i.saturating_sub(1)..=(i + 1).min(GW - 1) {
            let c = h[jj * GW + ii];
            if c < SEA_CODES {
                continue;
            }
            let m = land_meters(c);
            any = true;
            mn = mn.min(m);
            mx = mx.max(m);
        }
    }
    if any {
        mx - mn
    } else {
        0.0
    }
}

/// 离岸距离（格，四邻曼哈顿）。Python 那份是迭代扩散到不动点，结果就是本函数：
/// 到最近陆地格的曼哈顿距离——同一 thing，一次 BFS 算完，不必扫 512 遍全图。
fn offshore_cells(land: &[u8]) -> Vec<u32> {
    let total = N * N;
    let mut d = vec![u32::MAX; total];
    let mut q: Vec<u32> = Vec::with_capacity(total);
    for k in 0..total {
        if land[k] != 0 {
            d[k] = 0;
            q.push(k as u32);
        }
    }
    let mut head = 0usize;
    while head < q.len() {
        let p = q[head] as usize;
        head += 1;
        let base = d[p] + 1;
        let (x, y) = (p % N, p / N);
        let mut push = |np: usize| {
            if d[np] > base {
                d[np] = base;
                q.push(np as u32);
            }
        };
        if x > 0 {
            push(p - 1);
        }
        if x + 1 < N {
            push(p + 1);
        }
        if y > 0 {
            push(p - N);
        }
        if y + 1 < N {
            push(p + N);
        }
    }
    d
}

/// 窗口骨架：陆地-only 加权双线性 + 离岸下切的海底模型。
/// 直接双线性会把 -6000 米的海搅进地面高度，海岸线两侧各 19 公里都是假深坑。
fn base_field(h: &[u8], lats: &[f64; N], lons: &[f64; N]) -> (Vec<f64>, Vec<u8>) {
    let total = N * N;
    let mut hh = vec![0.0f64; total];
    let mut land = vec![0u8; total];
    for row in 0..N {
        let fy = (90.0 - lats[row]) / 180.0 * GH as f64 - 0.5;
        let j0 = fy.floor();
        let ty = fy - j0;
        for col in 0..N {
            let fx = (lons[col] + 180.0) / 360.0 * GW as f64 - 0.5;
            let i0 = fx.floor();
            let tx = fx - i0;
            let (mut num, mut den, mut has) = (0.0f64, 0.0f64, 0u8);
            for (dj, wy) in [(0i32, 1.0 - ty), (1, ty)] {
                let jj = (j0 + dj as f64).clamp(0.0, GH as f64 - 1.0) as usize;
                for (di, wx) in [(0i32, 1.0 - tx), (1, tx)] {
                    let ii = (i0 + di as f64).clamp(0.0, GW as f64 - 1.0) as usize;
                    let code = h[jj * GW + ii];
                    if code < SEA_CODES {
                        continue;
                    }
                    let w = wx * wy;
                    num += unquantize(code).unwrap_or(0.0) * w;
                    den += w;
                    if w > 0.25 {
                        has = 1;
                    }
                }
            }
            let k = row * N + col;
            land[k] = has;
            hh[k] = if den > 0.0 {
                num / den.max(1e-12)
            } else {
                0.0
            };
        }
    }
    let off = offshore_cells(&land);
    for k in 0..total {
        if land[k] == 0 {
            hh[k] = (-SHELF_M_PER_CELL * off[k] as f64).max(-SHELF_MAX_M);
        }
    }
    (hh, land)
}

/// 格值噪声：smoothstep 双线性。点阵坐标用等经纬"平面米"，所以相邻块落在同一套格点上。
fn value_noise(lats: &[f64; N], lons: &[f64; N], seed: u32, step_m: f64) -> Vec<f64> {
    let mut out = vec![0.0f64; N * N];
    let mut gx0 = [0i64; N];
    let mut sx = [0.0f64; N];
    for col in 0..N {
        let xm = (lons[col] + 180.0) * M_PER_DEG / step_m;
        let g = xm.floor();
        gx0[col] = g as i64;
        let f = xm - g;
        sx[col] = f * f * (3.0 - 2.0 * f);
    }
    for row in 0..N {
        let ym = (90.0 - lats[row]) * M_PER_DEG / step_m;
        let gy0 = ym.floor();
        let f = ym - gy0;
        let sy = f * f * (3.0 - 2.0 * f);
        for col in 0..N {
            let mut v = 0.0f64;
            for (dy, wy) in [(0i64, 1.0 - sy), (1, sy)] {
                for (dx, wx) in [(0i64, 1.0 - sx[col]), (1, sx[col])] {
                    let x = (gx0[col] + dx) as u32;
                    let y = (gy0 as i64 + dy) as u32;
                    v += h32(x, y, seed) as f64 * (wx * wy);
                }
            }
            out[row * N + col] = v / 4294967296.0;
        }
    }
    out
}

fn fbm(lats: &[f64; N], lons: &[f64; N], seed: u32) -> Vec<f64> {
    let mut acc = vec![0.0f64; N * N];
    let mut den = 0.0f64;
    for o in 0..3usize {
        let a = NOISE_GAIN[o];
        let sd = seed.wrapping_add((o as u32).wrapping_mul(0x9E37_79B9));
        let nv = value_noise(lats, lons, sd, NOISE_L / (1 << o) as f64);
        for k in 0..N * N {
            acc[k] += a * nv[k];
        }
        den += a;
    }
    for v in acc.iter_mut() {
        *v /= den;
    }
    acc
}

/// 微地形（米）。幅度跟着**真实**区域起伏走；平原走平滑起伏，山区走脊状
/// （`1-|2n-1|`）：山脊是尖的、谷是圆的，这是"像山"和"像麻点"的分别。
fn detail_field(
    sigma: f64,
    seed: u32,
    lats: &[f64; N],
    lons: &[f64; N],
    base_min_m: Option<f64>,
) -> (Vec<f64>, f64, f64) {
    let r = (sigma / RUGGED_SIGMA_M).min(1.0);
    let mut amp = (AMP_FLOOR_M).max(0.5 * sigma * RELIEF_FOLD);
    if let Some(bm) = base_min_m {
        if amp > bm && bm > 0.0 {
            amp = bm; // 谷底不许低于海平面：海岸块本就该是平的
        }
    }
    let n = fbm(lats, lons, seed);
    let det: Vec<f64> = n
        .iter()
        .map(|v| {
            let shape = (1.0 - r) * (v - 0.5) + r * ((1.0 - (2.0 * v - 1.0).abs()) - 0.45);
            amp * 2.0 * shape
        })
        .collect();
    (det, amp, r)
}

fn land_basis(h: &[f64], land: &[u8]) -> (f64, f64) {
    let (mut mn, mut mx) = (f64::MAX, f64::MIN);
    for k in 0..N * N {
        if land[k] != 0 {
            mn = mn.min(h[k]);
            mx = mx.max(h[k]);
        }
    }
    let ref_m = BEACH_M.max(mn);
    (ref_m, ((mx - ref_m) / LAND_BYTES).max(GAIN_MIN))
}

/// 有效高程（米）→ 高度场字节 1..247。
/// 海与沙用**绝对**高程：字节 86 必须是真海平面，这是共识常量，逐块归一化会把它冲掉。
fn quantize_local(h: &[f64], land: &[u8]) -> Vec<u8> {
    let (ref_m, gain) = land_basis(h, land);
    let mut f = vec![0u8; N * N];
    for k in 0..N * N {
        let v = if land[k] == 0 {
            let depth = (h[k] / -SHELF_MAX_M).clamp(0.0, 1.0);
            SEA_FIELD - 1.0 - depth * (SEA_FIELD - 2.0)
        } else if h[k] < BEACH_M {
            SEA_FIELD + (h[k].clamp(0.0, BEACH_M) / BEACH_M) * (SAND_FIELD - SEA_FIELD)
        } else {
            SAND_FIELD + (h[k] - ref_m) / gain
        };
        f[k] = rint(v).clamp(1.0, TOP_FIELD) as u8;
    }
    f
}

/// 席位中心 → 一块 256×256 的地。落在实测窗口里时直接用实测块（`measured = Some(...)`）。
///
/// `seed` 必须是共识的 `rules_id()` 那个数：换种子 = 换地貌 = 换世界，
/// 所以它不能是本地可调参数，否则签出去的 `(x, y, layer)` 承诺第二天就复算不回来。
pub fn gen_block(h: &[u8], lat_c: f64, lon_c: f64, seed: u32, measured: Option<&[u8]>) -> (Vec<u8>, BlockInfo) {
    let (lats, lons) = grid_coords(lat_c, lon_c);
    let sigma = regional_relief(h, lat_c, lon_c);
    let (h_eff, land) = base_field(h, &lats, &lons);
    let base_min = (0..N * N).fold(None::<f64>, |acc, k| {
        if land[k] != 0 {
            Some(acc.map_or(h_eff[k], |a| a.min(h_eff[k])))
        } else {
            acc
        }
    });
    let (det, amp, rugged) = detail_field(sigma, seed, &lats, &lons, base_min);
    let hh: Vec<f64> = (0..N * N)
        .map(|k| {
            if land[k] != 0 {
                h_eff[k] + det[k]
            } else {
                h_eff[k] - det[k].abs() * SEA_DETAIL
            }
        })
        .collect();
    let land_cells = (0..N * N).filter(|&k| land[k] != 0).count();
    let (ref_m, gain) = land_basis(&hh, &land);
    let mut f = quantize_local(&hh, &land);
    let mut source = "derived";
    if let Some(m) = measured {
        let (w, s, e, n) = MEASURED_RECT;
        if w <= lon_c && lon_c <= e && s <= lat_c && lat_c <= n {
            f = m.to_vec();
            source = "measured";
        }
    }
    (
        f,
        BlockInfo {
            lat: lat_c,
            lon: lon_c,
            sigma_m: sigma,
            rugged,
            land_cells,
            amp_m: amp,
            base_m: ref_m,
            gain_m_per_byte: gain,
            source,
        },
    )
}

/// 高度场指纹不另立一份：与 `terrain::fingerprint` 同一条（两路独立折叠拼成 16 位十六进制），
/// 改一格就变，三方对拍看的就是它。
pub use super::terrain::fingerprint;

/// 地球被分成多少份，其中多少份是陆地。随包 2 MiB 高程网格的真数，不是文案。
///
/// 这就是业主说的"把地球随机分成若干份"的可核对版本：份数由那份网格定义，
/// 谁拿到同一个二进制都会数出同一个 2,097,152 / 615,081。
pub fn partition() -> (usize, usize) {
    static P: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();
    *P.get_or_init(|| {
        let land = GLOBAL.iter().filter(|&&b| b >= SEA_CODES).count();
        (GLOBAL.len(), land)
    })
}

/// 脚下这块地的来源：实测窗口里就是 `measured`，窗口外是规则推导。
/// 它必须进快照——「这块地是量出来的还是算出来的」是两种不同的证据，产品上不该混为一谈。
pub fn walked_source(lat_c: f64, lon_c: f64) -> &'static str {
    let (w, s, e, n) = MEASURED_RECT;
    if w <= lon_c && lon_c <= e && s <= lat_c && lat_c <= n {
        "measured"
    } else {
        "derived"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    /// 夹具：Python 参考实现跑出来的 16 例席位 + 指纹，见 `tools/world/seat_crosscheck.py`。
    fn fixture() -> serde_json::Value {
        static DOC: OnceLock<serde_json::Value> = OnceLock::new();
        DOC.get_or_init(|| {
            let p = format!("{}/dev/seat-crosscheck.json", env!("CARGO_MANIFEST_DIR"));
            let s = std::fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("读不到夹具 {p}：{e}\n先跑 python3 tools/world/seat_crosscheck.py"));
            serde_json::from_str(&s).expect("夹具不是合法 JSON")
        })
        .clone()
    }

    #[test]
    fn 席位与地块与python参考逐字节一致() {
        let doc = fixture();
        let seed = doc["seed"].as_u64().unwrap() as u32;
        let genesis = doc["genesis"].as_str().unwrap();
        let cases = doc["cases"].as_array().unwrap();
        assert!(cases.len() >= 8, "夹具例数太少，覆盖不了码路");
        let mut seas = 0;
        for c in cases {
            let addr = c["address"].as_str().unwrap();
            let want = &c["seat"];
            let s = seat_of(addr, genesis, GLOBAL).expect("席位试满 4096 轮没落到陆地");
            assert_eq!(s.cell, want["cell"].as_u64().unwrap() as usize, "{addr} 席位格不对");
            assert_eq!(s.tries, want["tries"].as_u64().unwrap() as u32, "{addr} 试轮数不对");
            // 全精度比较：JSON 里的经纬是 Python repr 的最短往返表示，同double必须同bits
            assert_eq!(s.lat, want["lat"].as_f64().unwrap(), "{addr} 纬度差了一个 double");
            assert_eq!(s.lon, want["lon"].as_f64().unwrap(), "{addr} 经度差了一个 double");

            let (f, info) = gen_block(GLOBAL, s.lat, s.lon, seed, Some(crate::world::terrain::EARTH));
            assert_eq!(
                fingerprint(&f),
                c["heights_fingerprint"].as_str().unwrap(),
                "{addr} 高度场指纹不同——本机眼里的地和参考实现不是一片地"
            );
            assert_eq!(info.land_cells, c["block"]["land_cells"].as_u64().unwrap() as usize, "{addr} 陆地格数不对");
            assert_eq!(info.source, c["block"]["source"].as_str().unwrap(), "{addr} 地貌来源不对");
            let spot = c["spot"].as_array().unwrap();
            let pts = [(0usize, 0usize), (64, 192), (128, 128), (191, 37), (255, 255), (7, 240)];
            for (k, (y, x)) in pts.iter().enumerate() {
                assert_eq!(f[y * N + x] as usize, spot[k].as_u64().unwrap() as usize, "{addr} 第 {y},{x} 格字节不同");
            }
            if info.land_cells < N * N {
                seas += 1;
            }
        }
        assert!(seas >= 1, "夹具里没有跨海的块：海段量化/离岸扩散那条码路没被走过");
    }

    #[test]
    fn 同地址同创世不可重掷且换创世必换地() {
        let doc = fixture();
        let g = doc["genesis"].as_str().unwrap();
        let a = doc["cases"][0]["address"].as_str().unwrap();
        assert_eq!(
            seat_of(a, g, GLOBAL),
            seat_of(a.to_uppercase().as_str(), g.to_uppercase().as_str(), GLOBAL),
            "地址或创世的大小写换出了另一个席位"
        );
        let other = "5dc2205a8f0153a05f84e26ab94086009796bb68fdfddff439f97ebfd2aa3105";
        let s1 = seat_of(a, g, GLOBAL).unwrap();
        let s2 = seat_of(a, other, GLOBAL).unwrap();
        assert_ne!(s1.cell, s2.cell, "换了创世哈希席位没动——地形就没跟着链条变");
    }

    #[test]
    fn 实测窗口内的席位用实测块() {
        // 崂山中心：规则说"这里有真测量就用测量"，指纹必须退回 88d303cd75dcc913
        let (f, info) = gen_block(GLOBAL, 36.1527, 120.6652, 0x50D2_96E0, Some(crate::world::terrain::EARTH));
        assert_eq!(info.source, "measured");
        assert_eq!(fingerprint(&f), crate::world::terrain::fingerprint(&crate::world::terrain::EARTH));
        let (d, _) = gen_block(GLOBAL, 36.1527, 120.6652, 0x50D2_96E0, None);
        assert_ne!(fingerprint(&d), fingerprint(&f), "推导块与实测块同指纹——窗口判定等于没判");
    }

    #[test]
    fn 星球层随包且陆地占比对得上() {
        assert_eq!(GLOBAL.len(), GW * GH);
        let land = GLOBAL.iter().filter(|&&b| b >= SEA_CODES).count();
        // 参考实现量到 615,081 格；这一条把"换了一份星球层"这种事故挡在这里
        assert_eq!(land, 615_081, "星球层陆地格数变了：要么换数据要么换口径");
        assert_eq!(land_meters(128), 2.0);
        assert_eq!(land_meters(255), 4096.0 + 15.5 * 256.0);
        assert!(unquantize(0).is_none(), "0 码必须是无数据，不能当 0 米或当海");
    }

    #[test]
    fn rint是取偶不是进位() {
        assert_eq!(rint(0.5), 0.0);
        assert_eq!(rint(1.5), 2.0);
        assert_eq!(rint(-2.5), -2.0);
        assert_eq!(rint(-3.5), -4.0);
        assert_eq!(rint(2.4999), 2.0);
    }
}
