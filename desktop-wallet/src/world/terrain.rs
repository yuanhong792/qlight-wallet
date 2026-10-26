//! 确定性地形场——把 §9.5 的整数规范移植进 Rust（四份参考实现之外的第五份，
//! 基准同 `tools/world/test_rules_v2.py`：高度场指纹 `7b2863f076bce5fa`）。
//!
//! 为什么钱包里需要它：渲染器在 JS 里能画，但**矿脉必须被签进账本**——
//! 找矿、验矿、出块资格判定都在 Rust 侧，所以高度场/湿度场必须在这里也能算。
//! 一条铁律：任何"算矿"的代码都不许开本地 PRNG，也不许读墙钟；一切输入
//! 只有创世哈希与 `blocks.jsonl` 里的 VRF 序列，第三方拿同样的两个输入必须
//! 得到同一张图（§9.7 可验证随机源）。
//!
//! 种子分道（v2 的全部要点）：
//! - `words[0..4]` 只由创世哈希决定 → **大地**（高度场、天然实体位）永不动，
//!   `(x, y, layer)` 才是可以被签名、被第三方复算的承诺；
//! - `words[4..8]` = 创世哈希后 16 字节 ⊕ 全部块 VRF 的 XOR 折叠 → **地表**
//!   （湿度/生物群系）每个块重画一次。
//!
//! 后者正是"矿脉不可预挖"的来源：下一个块的 VRF 还不存在，谁都算不出下一轮
//! 哪里出矿——自动化矿工只能比谁在场更快，不能比谁算得更早。

use crate::crypto;

/// 与 `tools/world/world.py` 同口径的世界尺寸（v3 才降级为纯渲染边界）
pub const W: usize = 256;
pub const H: usize = 256;

const SEA: u32 = 86;
const SAND: u32 = 98;
const SNOW: u32 = 190;
const M_FOREST: u32 = 120;
const M_DESERT: u32 = 60;
const DENS_FOREST: u32 = 225;
const DENS_DESERT: u32 = 240;
const DENS_GRASS: u32 = 232;
/// 种子分道点：前 4 字 = 大地，后 4 字 = 地表
const SEED_SPLIT: usize = 4;

/// 规则版本 + 参数表：动任何一个数字都会换出另一个世界（§9.6）
const RULES_CANON: &str = "QL-WORLD-RULES-v2|256,256,86,98,190,120,60,225,240,232,4";

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

/// 大地高度场（0..247）——只由 `words[0..SEED_SPLIT]` 决定，与链历史无关
pub fn height_at(x: u32, y: u32, words: &[u32]) -> u32 {
    fbm(x, y, &words[..SEED_SPLIT])
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

    /// 跨语言基准：与 `tools/world/test_rules_v2.py` 的 `HEIGHTS_FP` 同值。
    /// 这条断言一挂，说明 Rust 侧算出的大地和 JS/Python 不是同一片土地——
    /// 那么"链只验坐标、像素各自本地算"整个主张当场失效。
    #[test]
    fn heights_field_matches_reference() {
        let words = seed_words(GENESIS_HASH_HEX, &[]);
        let mut hg = Vec::with_capacity(W * H);
        for y in 0..H as u32 {
            for x in 0..W as u32 {
                hg.push(height_at(x, y, &words) as u8);
            }
        }
        assert_eq!(fingerprint(&hg), "7b2863f076bce5fa");
    }

    /// 大地永不动：链历史长 240 个块，高度场逐位不变；地表必改。
    #[test]
    fn heights_field_is_history_free_but_surface_moves() {
        let mk = |n: usize| -> Vec<String> {
            (0..n).map(|i| format!("{:064x}", i + 1)).collect()
        };
        let a = seed_words(GENESIS_HASH_HEX, &[]);
        let b = seed_words(GENESIS_HASH_HEX, &mk(240));
        for y in 0..H as u32 {
            for x in 0..W as u32 {
                assert_eq!(height_at(x, y, &a), height_at(x, y, &b), "大地漂移了 ({x},{y})");
            }
        }
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
