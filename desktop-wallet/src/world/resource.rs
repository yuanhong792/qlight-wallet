//! 资源层 `QL-RES-v1`：可采集的自然物件。
//!
//! 地形高度场永远不动（§9.10），能采的只是**长在地上的东西**：树、石、花。
//! 它们是 Nature 不是数据——位置由种子算出，采伐记录才进账本。
//!
//! 为什么这样切：如果让玩家改高度场，`(x, z, layer)` 这条被签名、被第三方复算的
//! 承诺就不再有意义；而"树被砍了"完全可以只记一条 `(x, y, epoch)` 的采集记录，
//! 重放时按 `f(种子, 规则, 本 epoch 的采集记录)` 算还剩什么。
//!
//! 一轮 = 600 块 = 一昼夜（与钱包里的天色同一口径）。下一轮资源点全部长回来，
//! 所以不需要任何可变世界状态，掉线的机器重放历史也能得到同一片林子。

use super::terrain;

pub const RES_RULE_VERSION: &str = "QL-RES-v1";

/// 一轮的块数：与 `ui/world3d.js` 的 600 块一昼夜同一口径。
pub const EPOCH_BLOCKS: u64 = 600;
/// 一轮里最后 200 块算夜里：采集产出减半。
pub const NIGHT_START: u64 = 400;
/// 每人每轮最多采这么多个点：挑战来自取舍，不来自怪物。
pub const HARVEST_QUOTA: u32 = 60;

/// 复用的是物品栏里的三种既有槽位，不新增贴图。
pub const NODE_STONE: u8 = 0;
pub const NODE_TREE: u8 = 1;
pub const NODE_FLOWER: u8 = 2;

pub fn epoch_of(height: u64) -> u64 {
    height / EPOCH_BLOCKS
}

pub fn is_night(height: u64) -> bool {
    height % EPOCH_BLOCKS >= NIGHT_START
}

pub fn base_reward(kind: u8) -> u64 {
    match kind {
        NODE_TREE => 60,
        NODE_STONE => 15,
        NODE_FLOWER => 5,
        _ => 0,
    }
}

/// 采集实际产出：夜间减半。金额由块高唯一决定，记录里只能照抄，不能自定。
pub fn reward_at(kind: u8, height: u64) -> u64 {
    let base = base_reward(kind);
    if base == 0 {
        return 0;
    }
    if is_night(height) {
        base / 2
    } else {
        base
    }
}

/// 某一格长的是什么。`None` = 水、沙滩或空地。
///
/// 三种物件的密度由同一个 10 位抽样分桶决定（互斥、无重叠），
/// 所以同一格永远只有一种东西，重放不会出现"两个人采到不同的树"。
pub fn node_at(x: u32, y: u32, words: &[u32]) -> Option<u8> {
    if x >= terrain::W as u32 || y >= terrain::H as u32 {
        return None;
    }
    let hgt = terrain::height_at(x, y);
    if hgt <= terrain::SAND_LEVEL {
        return None;
    }
    let m = terrain::moisture_at(x, y, words);
    let roll = (terrain::life_hash(x, y, words) >> 8) & 1023;
    if m > terrain::M_FOREST && roll < 16 {
        return Some(NODE_TREE);
    }
    if roll < 28 {
        return Some(NODE_STONE);
    }
    if m >= terrain::M_DESERT && m <= terrain::M_FOREST && roll < 38 {
        return Some(NODE_FLOWER);
    }
    None
}

/// 扫描一块区域里的资源点：`(x, y, kind)`，按 (y, x) 排序。
///
/// 只扫调用方给的窗口，不扫全世界——资源点比矿脉密得多，全图扫会拖慢每个块。
pub fn scan(x0: u32, y0: u32, x1: u32, y1: u32, words: &[u32]) -> Vec<(u16, u16, u8)> {
    let mut out = Vec::new();
    let x1 = x1.min(terrain::W as u32);
    let y1 = y1.min(terrain::H as u32);
    for y in y0..y1 {
        for x in x0..x1 {
            if let Some(kind) = node_at(x, y, words) {
                out.push((x as u16, y as u16, kind));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simchain::GENESIS_HASH_HEX;

    fn words() -> Vec<u32> {
        terrain::seed_words(GENESIS_HASH_HEX, &[])
    }

    #[test]
    fn nodes_are_rare_enough_to_be_special() {
        let w = words();
        let mut n = 0usize;
        for y in 0..terrain::H as u32 {
            for x in 0..terrain::W as u32 {
                if node_at(x, y, &w).is_some() {
                    n += 1;
                }
            }
        }
        let total = terrain::W * terrain::H;
        assert!(
            n > total / 100 && n < total / 10,
            "资源点 {n}/{total} 密度偏离 1%~10% 太多"
        );
    }

    #[test]
    fn tree_nodes_never_collide_with_the_decorative_forest() {
        // world.js 画装饰树的条件是 (life_hash>>8)&0xFF > 密度阈值（最小 225），
        // 而可采树要求 (life_hash>>8)&1023 < 16——两者天然互斥。3D 视图因此可以
        // 直接给资源树画完整几何，不会和装饰林叠成两棵穿模的树。
        let w = words();
        for y in 0..terrain::H as u32 {
            for x in 0..terrain::W as u32 {
                if node_at(x, y, &w) != Some(NODE_TREE) {
                    continue;
                }
                let t = (terrain::life_hash(x, y, &w) >> 8) & 0xFF;
                let m = terrain::moisture_at(x, y, &w);
                assert!(
                    t <= terrain::tree_density(m),
                    "({x},{y}) 既是可采树又是装饰林：3D 里会画成两棵树"
                );
            }
        }
    }

    #[test]
    fn nodes_never_grow_in_water_or_on_the_beach() {
        let w = words();
        for y in 0..terrain::H as u32 {
            for x in 0..terrain::W as u32 {
                if terrain::height_at(x, y) <= terrain::SAND_LEVEL {
                    assert!(node_at(x, y, &w).is_none(), "({x},{y}) 水里长出了东西");
                }
            }
        }
    }

    #[test]
    fn reward_is_half_at_night_and_stable_by_height() {
        assert_eq!(reward_at(NODE_TREE, 0), 60);
        assert_eq!(reward_at(NODE_TREE, 400), 30);
        assert_eq!(reward_at(NODE_TREE, 599), 30);
        assert_eq!(reward_at(NODE_TREE, 600), 60);
        assert_eq!(reward_at(9, 0), 0);
    }

    #[test]
    fn epoch_and_night_follow_the_day_cycle() {
        assert_eq!(epoch_of(599), 0);
        assert_eq!(epoch_of(600), 1);
        assert!(!is_night(399));
        assert!(is_night(400));
        assert!(!is_night(600));
    }
}
