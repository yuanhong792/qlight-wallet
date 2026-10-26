//! 矿脉谓词 `QL-ORE-v1`（DESIGN.md §9.13「世界即矿场」的可计算内核）。
//!
//! 一句话：**挖矿 = 在世界里落一个签过名的方块**，而"哪里能挖"由两条场共同决定，
//! 全部输入只有创世哈希与 `blocks.jsonl` 里的 VRF 序列——所以任何一个节点、
//! 任何一份实现（Rust / Python / JS）拿同样的输入必须算出同一张矿脉图。
//! 自动化矿工因此不需要被信任，只需要被验证。
//!
//! 三条自洽要求，缺一条整个经济就塌：
//! 1. **不可预挖**：矿脉混了地表场（`words[4..8]` = 创世哈希 ⊕ 全部历史 VRF），
//!    下一个块的 VRF 还不存在，谁都算不出下一轮哪里出矿；
//! 2. **越来越难**：命中配额 `ore_quota(h)` 随块高按 2 的幂收窄（与减半同构），
//!    且已占用的格子永久少一格——世界越满，抢中越难；
//! 3. **成本真实**：挖中一条记录要付手续费、要持续付租（§9.9 断租不删除），
//!    所以自动化程序的"算力"被换算成真金白银的速度与租金，而不是免费脚本。
//!
//! 铁律：不许 PRNG、不许墙钟、不许读浮点；命中顺序按**湿度降序、并列再按 (x, y)**，
//! 三方实现因此对同一个 `h` 必然给出同一个列表，而不是"同一个集合"。

use super::terrain;

pub const ORE_RULE_VERSION: &str = "QL-ORE-v1";
/// 命中配额域：24 位均匀值，`ore_seed < quota` 即中矿
pub const ORE_SEED_SPACE: u32 = 1 << 24;
/// 初始配额 = 1/8 的创世纪元格 → 每回合约 32 个候选（实测，见对拍脚本）
pub const ORE_QUOTA_BASE: u32 = ORE_SEED_SPACE / 8;
/// 每这么多块，配额减半——与排放减半同构，后期矿脉自己变稀
pub const ORE_DECAY_INTERVAL: u64 = 50;
/// 配额下限：矿可以稀有，但不能绝迹（约每回合 2 格）
pub const ORE_QUOTA_MIN: u32 = ORE_SEED_SPACE / 128;
/// 一次最多取回这么多个候选再排序：全图 65536 次哈希本身够快，
/// 但候选数被配额限死，排序开销因此也有界
pub const SCAN_CAP: usize = 4096;

/// 该块高下的命中配额（2 的幂衰减）
pub fn ore_quota(h: u64) -> u32 {
    let halvings = (h / ORE_DECAY_INTERVAL).min(8);
    (ORE_QUOTA_BASE >> halvings).max(ORE_QUOTA_MIN)
}

/// 矿签：由**地表种子**（`words[4..8]` = 创世哈希 ⊕ 全部历史 VRF）派生的 24 位均匀值。
///
/// 用它而不是湿度绝对值，是因为湿度的分布谁也说不准——上一版拿"湿度 ≥ 240"当门槛，
/// 结果整片世界一个矿都扫不出来，测试当场抓住。均匀配额让"每回合有几个矿"变成
/// 可预算的量，衰减才谈得上是难度。
pub fn ore_seed(x: u32, y: u32, words: &[u32]) -> u32 {
    (terrain::hash2(x * 3 + 17, y * 5 + 31, words[4]) >> 8) & (ORE_SEED_SPACE - 1)
}

/// 某一格在这一块高下是不是矿。
///
/// 两个条件都要满足，一个都不能少：
/// - **创世纪元格**（约 1/256，纯由创世哈希决定）：矿脉的地基是大地，不是地表，
///   否则每个块重画时所有矿脉整片蒸发，已签的塔就成了无根之水；
/// - **矿签中配额**：这条混历史，因此不可预挖。
pub fn is_ore(x: u32, y: u32, words: &[u32], h: u64) -> bool {
    terrain::is_genesis_cell(x, y, words) && ore_seed(x, y, words) < ore_quota(h)
}

/// 矿格应当落在哪一层：**贴地**（`layer = 地表高度 >> 3`，与 §9.5 同一口径）。
///
/// 挖矿必须把方块真正放到地上，不许悬空——否则脚本可以把整个 64 层空间刷满。
pub fn ore_layer(x: u32, y: u32, words: &[u32]) -> u16 {
    let l = terrain::height_at(x, y, words) >> 3;
    if l >= super::MAX_LAYER as u32 {
        super::MAX_LAYER - 1
    } else {
        l as u16
    }
}

/// 已被占用的格子集合（来自实体账本重放，`released` 的不算：断租即让位）
pub type Taken = std::collections::BTreeSet<(u32, u32, u32)>;

/// 扫一遍世界，返回本回合还可挖的矿格 `(x, y, layer)`。
///
/// 排序口径三边必须一致，否则对拍只能对"集合"而对不上"列表"：
/// **湿度降序（富矿先挖）→ 再按 (x, y) 字典序**。湿度来自地表场，
/// 所以"哪个矿更肥"本身也随链历史重画。
pub fn scan(words: &[u32], h: u64, taken: &Taken, limit: usize) -> Vec<(u16, u16, u16)> {
    let mut hits: Vec<(u32, u32, u32, u32)> = Vec::new();
    if limit == 0 {
        return Vec::new();
    }
    for y in 0..terrain::H as u32 {
        for x in 0..terrain::W as u32 {
            if !is_ore(x, y, words, h) {
                continue;
            }
            let layer = ore_layer(x, y, words) as u32;
            if taken.contains(&(x, y, layer)) {
                continue;
            }
            hits.push((terrain::moisture_at(x, y, words), x, y, layer));
            if hits.len() >= SCAN_CAP {
                break;
            }
        }
        if hits.len() >= SCAN_CAP {
            break;
        }
    }
    // 湿度大者优先；并列时按坐标，保证与 Python/JS 逐位同序
    hits.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });
    hits.into_iter()
        .take(limit)
        .map(|(_, x, y, l)| (x as u16, y as u16, l as u16))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simchain::GENESIS_HASH_HEX;

    /// 与 Python/JS 对拍同口径的历史序列：**真随机 32 字节** VRF。
    /// 早先用 `format!("{:064x}", i+1)` 造假历史，相邻两条只有最后一字节不同，
    /// 而 `seed_words` 把折叠结果的第 16..20 字节交给 `words[4]`（矿签种子）——
    /// 于是"换历史"根本没换到矿签，测试看着像挂了，其实测的是假数据。
    fn synth(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| hex::encode(crate::crypto::shake256(b"QL-WORLD-PARITY", &i.to_be_bytes(), 32)))
            .collect()
    }

    /// 配额本身：2 的幂衰减且永不归零
    #[test]
    fn quota_decays_but_never_vanishes() {
        assert_eq!(ore_quota(0), ORE_SEED_SPACE / 8);
        assert_eq!(ore_quota(49), ORE_SEED_SPACE / 8);
        assert_eq!(ore_quota(50), ORE_SEED_SPACE / 16);
        assert_eq!(ore_quota(100), ORE_SEED_SPACE / 32);
        assert_eq!(ore_quota(9_999), ORE_QUOTA_MIN);
    }

    /// **不可预挖**：同一块高、只有链历史不同，矿脉图必须搬家。
    /// 如果这条挂了，说明矿脉其实只由创世哈希决定 → 脚本可以提前算光所有矿。
    /// （注意别拿"换块高"当对照：配额每 50 块才减半，50 块以内矿格本来就该一样。）
    #[test]
    fn ore_moves_when_chain_grows() {
        let words_short = terrain::seed_words(GENESIS_HASH_HEX, &synth(3));
        let words_long = terrain::seed_words(GENESIS_HASH_HEX, &synth(4));
        let a = scan(&words_short, 3, &Taken::new(), 64);
        let b = scan(&words_long, 3, &Taken::new(), 64);
        assert!(a.len() > 4, "h=3 只扫到 {} 个候选：配额定得太小", a.len());
        assert_ne!(a, b, "矿脉不随链历史搬家 = 可以被预挖");
    }

    /// **越来越难**：块高越大候选越少；占用过的格子直接让位。
    #[test]
    fn later_is_harder_and_occupancy_bites() {
        let words = terrain::seed_words(GENESIS_HASH_HEX, &synth(20));
        let early = scan(&words, 0, &Taken::new(), 4096).len();
        let late = scan(&words, 150, &Taken::new(), 4096).len();
        assert!(late <= early, "后期反而更多矿：{early} → {late}");
        assert!(early > 0 && late > 0, "矿脉不能绝迹：early={early} late={late}");

        let mut taken = Taken::new();
        let first = scan(&words, 0, &taken, 3);
        assert_eq!(first.len(), 3, "候选太少，挖不动：{first:?}");
        for (x, y, l) in &first {
            taken.insert((*x as u32, *y as u32, *l as u32));
        }
        let after = scan(&words, 0, &taken, 3);
        for c in &first {
            assert!(!after.contains(c), "挖过的格子 {c:?} 还在候选里");
        }
    }

    /// 命中顺序：湿度降序，并列按坐标——三边实现靠这一条才能对成同一个**列表**
    #[test]
    fn scan_order_is_moisture_then_coords() {
        let words = terrain::seed_words(GENESIS_HASH_HEX, &synth(7));
        let cells = scan(&words, 0, &Taken::new(), 64);
        let moist: Vec<u32> = cells
            .iter()
            .map(|(x, y, _)| terrain::moisture_at(*x as u32, *y as u32, &words))
            .collect();
        for w in moist.windows(2) {
            assert!(w[0] >= w[1], "排序不是湿度降序：{moist:?}");
        }
    }

    /// 矿必须落地：贴地层由高度场唯一决定，且在 MAX_LAYER 之内
    #[test]
    fn ore_lands_on_the_ground() {
        let words = terrain::seed_words(GENESIS_HASH_HEX, &synth(1));
        for (x, y, l) in scan(&words, 0, &Taken::new(), 32) {
            assert_eq!(
                l as u32,
                (terrain::height_at(x as u32, y as u32, &words) >> 3).min(crate::world::MAX_LAYER as u32 - 1)
            );
            assert!((l as u32) < crate::world::MAX_LAYER as u32);
        }
    }

    /// 黄金基准：`tools/world/test_ore_v1.py` 由 Python 现场算出、并与 Node 逐位对拍后写盘。
    /// 这里用 Rust 再复算一遍——三份实现（Rust / Python / Node）对同一份历史必须
    /// 给出同一个**有序列表**，否则钱包挖中的格子别人无法复算。
    #[test]
    fn ore_matches_cross_language_golden() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tools/world/ore_golden.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            panic!("缺少黄金基准 {}：先跑 python3 tools/world/test_ore_v1.py", path.display());
        };
        let v: serde_json::Value = serde_json::from_str(&text).expect("黄金基准不是合法 JSON");
        assert_eq!(v["rule"], "QL-ORE-v1");
        let genesis = v["genesis"].as_str().expect("genesis");
        let limit = v["limit"].as_u64().expect("limit") as usize;
        let cases = v["cases"].as_array().expect("cases");
        assert!(!cases.is_empty(), "黄金基准没有用例");

        for c in cases {
            let h = c["height"].as_u64().expect("height");
            let vrfs: Vec<String> = c["vrfs"]
                .as_array()
                .expect("vrfs")
                .iter()
                .map(|x| x.as_str().expect("vrf").to_string())
                .collect();
            let words = terrain::seed_words(genesis, &vrfs);
            let got: Vec<String> = scan(&words, h, &Taken::new(), limit)
                .iter()
                .map(|(x, y, l)| format!("{x},{y},{l}"))
                .collect();
            let want: Vec<String> = c["cells"]
                .as_array()
                .expect("cells")
                .iter()
                .map(|x| x.as_str().expect("cell").to_string())
                .collect();
            assert_eq!(got, want, "h={h} n={} 的矿脉与黄金基准不一致", vrfs.len());
            assert_eq!(
                ore_quota(h),
                c["quota"].as_u64().expect("quota") as u32,
                "h={h} 的配额不一致"
            );
        }
    }
}
