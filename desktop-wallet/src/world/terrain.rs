//! 确定性地形场——把 §9.5 的整数规范移植进 Rust（五份参考实现之外的共识侧那一份，
//! 基准同 `tools/world/test_rules_v3.py`：高度场指纹 `88d303cd75dcc913`）。
//!
//! 为什么钱包里需要它：渲染器在 JS 里能画，但**矿脉必须被签进账本**——
//! 找矿、验矿、出块资格判定都在 Rust 侧，所以高度场/湿度场必须在这里也能算。
//! 一条铁律：任何"算矿"的代码都不许开本地 PRNG，也不许读墙钟；一切输入
//! 只有创世哈希与 `blocks.jsonl` 里的 VRF 序列，第三方拿同样的两个输入必须
//! 得到同一张图（§9.7 可验证随机源）。
//!
//! v3 的种子分道——大地与地表彻底分家，而且大地不再是算出来的：
//! - **大地 = 真实地球**。高度场是一次公开 DEM（SRTM30m / Copernicus GLO-30）
//!   在崂山东麓 8.96 km 窗口上的烘焙结果，`data/earth-height.bin` 65536 字节，
//!   SHA-256 直接写进 `RULES_CANON`。它既不由创世哈希决定，也不由链历史决定，
//!   所以「山为什么在这里」这个问题有了地图上的答案，而 `(x, y, layer)`
//!   依然是可被第三方复算的承诺——承诺的对象从"一次哈希"换成"一次卫星测量"。
//! - `words[4..8]` = 创世哈希后 16 字节 ⊕ 全部块 VRF 的 XOR 折叠 → **地表**
//!   （湿度/生物群系/矿脉）每个块重画一次。
//!
//! 后者正是"矿脉不可预挖"的来源：下一个块的 VRF 还不存在，谁都算不出下一轮
//! 哪里出矿——自动化矿工只能比谁在场更快，不能比谁算得更早。大地换成真实高程
//! 并没有削弱它：矿签仍然只从含历史的那 4 个字派生。

use crate::crypto;

/// 与 `tools/world/world.py` 同口径的世界尺寸（v3 才降级为纯渲染边界）
pub const W: usize = 256;
pub const H: usize = 256;

const SEA: u32 = 86;
const SAND: u32 = 98;
const SNOW: u32 = 190;
pub(crate) const M_FOREST: u32 = 120;
pub(crate) const M_DESERT: u32 = 60;
const DENS_FOREST: u32 = 225;
const DENS_DESERT: u32 = 240;
const DENS_GRASS: u32 = 232;
/// 种子分道点：前 4 字不再参与大地（v3 保留仅为与 Python/JS 参数字面同构）
const SEED_SPLIT: usize = 4;

/// 真实地球的高度场：`tools/world/fetch_earth_dem.py` 一次性烘焙，随程序分发。
/// 编译期就嵌进二进制，运行时无从篡改，也就没有"我这台机器山在别处"的可能。
pub const EARTH: &[u8] = include_bytes!("../../data/earth-height.bin");

/// 烘焙输入的 SHA-256——它进了 `RULES_CANON`，所以换一片土地等于换一套规则
pub const EARTH_SHA256: &str =
    "805868b90a88e3df2d43658cb8579dee9cebd1fe566794d52afabf05184248f8";

/// 这块地的身份，只用于 HUD 与文档展示，不参与任何哈希
pub const EARTH_PLACE: &str = "崂山东麓 · 青岛 · 黄海之滨";
/// 1 格 = 35 米真实地面（窗口 8.96 km ÷ 256 格）；方块与人仍是 1 米
pub const EARTH_METERS_PER_CELL: f64 = 35.0;

/// 规则版本 + 参数表 + 大地指纹：动任何一个数字、换一片土地都会换出另一个世界（§9.6）
const RULES_CANON: &str =
    "QL-WORLD-RULES-v3|256,256,86,98,190,120,60,225,240,232,4|805868b90a88e3df2d43658cb8579dee9cebd1fe566794d52afabf05184248f8";

const AMPS: [u32; 5] = [32, 16, 8, 4, 2];
const CELLS: [u32; 5] = [64, 32, 16, 8, 4];

/// 规则种子算一次就缓存：它在每个噪声采样里都要用到，重算 SHAKE 会把
/// 整片大地变成一次哈希马拉松（实测 65536 格 ×5 倍频程 = 数十万次调用）。
fn rules_seed() -> u32 {
    static SEED: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *SEED.get_or_init(|| {
        // 空前缀 = 与 `hashlib.shake_256(RULES_CANON).digest(4)` 逐位同构
        let d = crypto::shake256(b"", RULES_CANON.as_bytes(), 4);
        u32::from_be_bytes([d[0], d[1], d[2], d[3]])
    })
}

/// 本机算的是哪一套世界规则：`RULES_CANON` 的 32 位摘要。
///
/// 它压住了参数表和大地 SHA-256，所以「换一片土地」和「改一个阈值」都会换出
/// 不同的 tag。联邦公告必须带上它——两端的大地不一样，矿脉判定就不一样，
/// 而这在链上是静默的：各出各的块，各自都认为自己那条才是链。
pub fn rules_id() -> String {
    format!("{:08x}", rules_seed())
}

/// 32 位整数混合——替代一切随机源
pub fn hash2(ix: u32, iy: u32, seed: u32) -> u32 {
    let rs = rules_seed();
    let mut h = ix
        .wrapping_mul(374761393)
        .wrapping_add(iy.wrapping_mul(668265263))
        .wrapping_add(seed)
        .wrapping_add(rs);
    h ^= h >> 13;
    h = h.wrapping_mul(1274126177);
    h ^= h >> 16;
    h
}

/// t ∈ [0,256] → t²(3−2t)，整数，结果 ∈ [0,256]
fn smooth(t: u32) -> u32 {
    (t * t * (768 - 2 * t)) >> 16
}

/// 格点值噪声 + smoothstep 双线性插值，全程整数（无除法、无浮点）
///
/// 插值差值可正可负，必须用**有符号**算法：Python 侧 `(n10 - n00) * sx >> 8` 对负数
/// 是向下取整的算术右移，无符号 wrapping 写法会算出另一片地。
fn vnoise(x: u32, y: u32, cell: u32, seed: u32) -> u32 {
    let clog = cell.trailing_zeros();
    let (xi, yi) = (x >> clog, y >> clog);
    let fx = (x & (cell - 1)) << (8 - clog);
    let fy = (y & (cell - 1)) << (8 - clog);
    let (sx, sy) = (smooth(fx) as i32, smooth(fy) as i32);
    let n00 = ((hash2(xi, yi, seed) >> 16) & 255) as i32;
    let n10 = ((hash2(xi + 1, yi, seed) >> 16) & 255) as i32;
    let n01 = ((hash2(xi, yi + 1, seed) >> 16) & 255) as i32;
    let n11 = ((hash2(xi + 1, yi + 1, seed) >> 16) & 255) as i32;
    let a = n00 + (((n10 - n00) * sx) >> 8);
    let b = n01 + (((n11 - n01) * sx) >> 8);
    (a + (((b - a) * sy) >> 8)) as u32
}

/// 5 个倍频程，权重 32/16/8/4/2 → 结果 ∈ [0,247]，末尾右移而非归一化（避免除法）
fn fbm(x: u32, y: u32, words: &[u32]) -> u32 {
    let mut acc: u32 = 0;
    for k in 0..5 {
        acc += vnoise(x, y, CELLS[k], words[k & 3]) * AMPS[k];
    }
    acc >> 6
}

/// 种子 = 创世哈希（大地）⊕ 链历史（地表），8 个 32 位字
pub fn seed_words(genesis_hex: &str, vrfs: &[String]) -> Vec<u32> {
    let raw = hex::decode(genesis_hex).unwrap_or_else(|_| [0u8; 32].to_vec());
    let mut folded = raw.clone();
    for v in vrfs {
        if let Ok(b) = hex::decode(v) {
            // 与 Python 的 zip 同语义：只折进重叠的那段，多余的字节直接丢
            for (i, c) in b.iter().enumerate().take(folded.len()) {
                folded[i] ^= *c;
            }
        }
    }
    let mut stage = Vec::with_capacity(32);
    stage.extend_from_slice(&raw[..16]);
    stage.extend_from_slice(&folded[16..]);
    stage
        .chunks_exact(4)
        .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// 大地高度场（0..247）——真实地球高程，v3 起与种子、与链历史都无关
///
/// 索引 `y * W + x`，`y = 0` 是最北一排；行主序与 `earth-manifest.json` 的
/// `row_order` 一致，也和 JS/Python 渲染器逐字节对齐。调用方的坐标必须先过
/// `world.rs:749` 那道 `x < WORLD_W` 的边界检查——越界是本程序的 bug，不该被取模掩盖。
pub fn height_at(x: u32, y: u32) -> u32 {
    EARTH[y as usize * W + x as usize] as u32
}

/// 地表湿度场——混入全部块 VRF，每出一个块整片重画
pub fn moisture_at(x: u32, y: u32, words: &[u32]) -> u32 {
    fbm(x, y, &words[SEED_SPLIT..])
}

/// 天然实体位与创世世代判定共用的整数哈希（与 Python 同一对质数偏移）
pub fn life_hash(x: u32, y: u32, words: &[u32]) -> u32 {
    hash2(x + 7919, y + 104729, words[0])
}

/// 该格是否属于「创世纪元」：约 1/256，纯由创世哈希决定，全网一致、无人能挑
pub fn is_genesis_cell(x: u32, y: u32, words: &[u32]) -> bool {
    (life_hash(x, y, words) >> 24) & 0xFF == 0
}

/// 天然树木密度阈值（与 Python 同一条 if）
pub fn tree_density(moisture: u32) -> u32 {
    if moisture > M_FOREST {
        DENS_FOREST
    } else if moisture < M_DESERT {
        DENS_DESERT
    } else {
        DENS_GRASS
    }
}

pub const SEA_LEVEL: u32 = SEA;
pub const SAND_LEVEL: u32 = SAND;
pub const SNOW_LEVEL: u32 = SNOW;

/// 双 FNV-1a 32 位指纹 → 16 个十六进制位（与 `world.py::fingerprint` 同构：先异或后乘）
pub fn fingerprint(buf: &[u8]) -> String {
    let mut out = String::new();
    for (basis, prime) in [(0x811C9DC5u32, 16777619u32), (0x01000193u32, 3295579737u32)] {
        let mut d = basis;
        for byte in buf {
            d = (d ^ (*byte as u32)).wrapping_mul(prime);
        }
        out.push_str(&format!("{:08x}", d));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simchain::GENESIS_HASH_HEX;

    /// 跨语言基准：与 `world.js` / `world.py` 里的 `RULES_SEED` 同值。
    /// 这是五份实现之间最省话的一条对拍——只要它相等，湿度、群系、创世格、
    /// 矿签就全落在同一个混合常数上；不等就是共识分叉，没有第三种可能。
    #[test]
    fn rules_seed_matches_reference() {
        assert_eq!(rules_seed(), 0x50d2_96e0);
        assert_eq!(rules_id(), "50d296e0");
    }

    /// 跨语言基准：与 `tools/world/fetch_earth_dem.py` 写进 manifest 的两个值同真。
    /// 这条断言一挂，说明 Rust 侧算出的大地和 JS/Python 不是同一片土地——
    /// 那么"链只验坐标、像素各自本地算"整个主张当场失效。
    #[test]
    fn earth_field_is_the_pinned_land() {
        assert_eq!(EARTH.len(), W * H);
        // 这条指纹同时替 `RULES_CANON` 里那串 SHA-256 站台：重新烘焙过高度场
        // 却忘了同步常量，RULES_SEED 就会和 JS/Python 分叉，而分叉在这里
        // 不是"画面不一样"，是矿脉判定不一样。
        assert_eq!(fingerprint(EARTH), "88d303cd75dcc913");
    }

    /// 地表必改：链历史长 240 个块，湿度场就得重画——「矿脉不可预挖」靠这一半。
    /// 大地那一半在 v3 之后是结构性不变的：`height_at` 连种子都不接受，
    /// 漂移只能发生在 `earth-height.bin` 被换掉的时候，而那条路归 `earth_field_is_the_pinned_land` 管。
    #[test]
    fn surface_redraws_with_history() {
        let mk = |n: usize| -> Vec<String> {
            (0..n).map(|i| format!("{:064x}", i + 1)).collect()
        };
        let a = seed_words(GENESIS_HASH_HEX, &[]);
        let b = seed_words(GENESIS_HASH_HEX, &mk(240));
        let mut same = 0;
        for y in 0..H as u32 {
            for x in 0..W as u32 {
                if moisture_at(x, y, &a) == moisture_at(x, y, &b) {
                    same += 1;
                }
            }
        }
        assert!(same < W * H / 2, "地表完全没重画：矿脉就无法随链生长搬家");
    }

    /// 真实地球该有的样子：既有海也有山，且最高点的层差能撑起看得见的起伏
    #[test]
    fn earth_has_sea_and_relief() {
        let mut water = 0;
        let mut top = 0u8;
        for &v in EARTH {
            if v < SEA as u8 {
                water += 1;
            }
            top = top.max(v);
        }
        // 21068 个水格 = 32.1%，正是黄海之滨该有的比例；崂山主峰 1092 m
        assert!(
            (20000..23000).contains(&water),
            "海面格数 {water}：这片土地不像山海之滨"
        );
        assert_eq!(top, 247, "最高点没顶到标尺顶端，烘焙的分位写错了");
        assert_eq!(land_layers(), 20, "陆地起伏层数变了，HUD 上的比例说明得重写");
    }

    /// 陆地（含沙滩）实际占用的整层数——第一人称里"山有几阶"就是这个数。
    /// 口径与 `earth-manifest.json` 的 `land_layers` 一致：非水面即陆地。
    fn land_layers() -> usize {
        let mut used = [false; 64];
        for &v in EARTH {
            if v >= SEA as u8 {
                used[(v >> 3) as usize] = true;
            }
        }
        used.iter().filter(|&&u| u).count()
    }

    /// 创世世代格确实稀有（约 1/256），且与历史无关——它是有界矿脉的地基
    #[test]
    fn genesis_cells_are_rare_and_stable() {
        let words = seed_words(GENESIS_HASH_HEX, &[]);
        let mut n = 0;
        for y in 0..H as u32 {
            for x in 0..W as u32 {
                if is_genesis_cell(x, y, &words) {
                    n += 1;
                }
            }
        }
        assert!((150..450).contains(&n), "创世世代格数 {n} 偏离 1/256 太多");
    }
}
