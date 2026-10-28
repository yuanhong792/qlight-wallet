//! 旅人摊位 `QL-TRADER-v1`：世界里唯一一个**卖情报**的人。
//!
//! 为什么卖情报而不是卖资源：树、石、花自己长在地里，谁都能去采；矿脉每两秒
//! 重画一次，卖坐标等于卖废纸。真正稀缺的不是资源，是**把整片世界算一遍的工夫**——
//! 哪一条路走一趟最值、哪块地方没人跟你抢、这一轮谁在采。旅人卖的就是这份工夫。
//!
//! 三条自洽要求：
//! 1. **摊位可复算**：坐标只由创世种子与「第几程」（每 50 块一程，与矿脉衰减同节奏）
//!    决定。地表每出一个块就重画，拿它摆摊会让摊位满地乱跳，所以只用创世种子。
//! 2. **先算后收钱**：情报算不出来就不该付钱；付钱用的是一条普通 `send` 记录，
//!    收款地址是烧毁地址（没有私钥），所以光尘是真被烧掉的——这是软币的回流阀。
//! 3. **只卖工夫，不卖秘密**：情报全部可被任何人独立复算，旅人的价值是算得快、
//!    算得全。任何一个改过的客户端都能白算，这一层防不住也不需要防。
//!
//! 情报的时效力：路线与余量按**当前块高**算（夜里减半已经算进去），下一轮资源
//! 全部长回来、余量重算，所以同一件情报过一轮就该重买。

use super::{resource, terrain};
use crate::world::dust::DustState;
use serde::Serialize;
use std::collections::BTreeMap;

pub const TRADER_RULE_VERSION: &str = "QL-TRADER-v1";
/// 每 50 块换一处摊位：与 `ore::ORE_DECAY_INTERVAL` 同节奏，世界一次重画一程
pub const TRADER_PERIOD: u64 = 50;

pub const ROUTE: &str = "route";
pub const FRONTIER: &str = "frontier";
pub const LEDGER: &str = "ledger";

/// 一路最多串这么多站：够走一趟，又不至于让面板变成一张坐标表
pub const ROUTE_STOPS: usize = 12;
/// 路线只在摊位周围这么大的一圈里挑，走远了不叫「一程」
pub const ROUTE_RADIUS: u32 = 96;
/// 先按产出挑出这么多候选，再按最近邻串起来
const ROUTE_POOL: usize = 32;
/// 「冷清之地」把世界切成 32×32 的小块来比
pub const AREA_SIZE: u16 = 32;

pub fn period_of(height: u64) -> u64 {
    height / TRADER_PERIOD
}

/// 旅人这一程摆在哪。纯函数：同一条链、同一个回合，任何机器都指向同一格。
pub fn stall_at(height: u64) -> (u16, u16) {
    let words = crate::world::resource_words();
    let p = period_of(height);
    let seed = words[0] ^ (p as u32).wrapping_mul(0x9E37_79B9);
    let mut x = ((terrain::hash2(101, p as u32, seed) >> 8) % terrain::W as u32) as u32;
    let y = ((terrain::hash2(211, p as u32, seed) >> 8) % terrain::H as u32) as u32;
    // 摊子不摆在水里：落在水面/沙滩就往东挪一格，仍是纯函数
    for _ in 0..terrain::W {
        if terrain::height_at(x, y, &words) > terrain::SAND_LEVEL {
            break;
        }
        x = (x + 1) % terrain::W as u32;
    }
    (x as u16, y as u16)
}

/// 报价表。价格是光尘最小单位，采一棵树 60 ✦ —— 第一件情报正好值一棵树。
pub fn price_of(id: &str) -> Option<u64> {
    match id {
        ROUTE => Some(60),
        FRONTIER => Some(40),
        LEDGER => Some(20),
        _ => None,
    }
}

/// 光尘的收款方：由常量派生、没有对应私钥的烧毁地址。
/// 它只能是合法的 32 字节 bech32，否则 `send` 的语义层会拒收这条记录。
pub fn sink_address() -> String {
    let d = crate::crypto::shake256(b"QL-TRADER-SINK-V1", b"", 32);
    let mut a = [0u8; 32];
    a.copy_from_slice(&d);
    crate::crypto::to_bech32_ql(&a)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Stop {
    pub x: u16,
    pub y: u16,
    pub kind: u8,
    pub reward: u64,
    /// 从上一站（第一站从摊位）走过来的曼哈顿格数
    pub steps: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Area {
    pub x: u16,
    pub y: u16,
    pub size: u16,
    pub nodes: u64,
    pub reward: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Harvester {
    pub addr: String,
    pub count: u32,
}

/// 一件情报。三种情报共用一个信封，用不到的部分留空：
/// 前端只看 `id` 决定怎么讲，数值全部由后端算好。
#[derive(Debug, Clone, Serialize)]
pub struct Quote {
    pub id: String,
    pub price: u64,
    pub height: u64,
    pub epoch: u64,
    pub night: bool,
    /// 路线站点（`route`）
    pub stops: Vec<Stop>,
    /// 冷清之地（`frontier`）
    pub area: Option<Area>,
    /// 本轮采集排行（`ledger`）
    pub top: Vec<Harvester>,
    /// 主数字：路线总产出 / 区域余值 / 本轮已采格数
    pub total: u64,
    /// 副数字：站点数 / 区域余格 / 采集人数
    pub count: u64,
    /// 全世界本轮还剩多少格、值多少（三种情报都附送，用来判断值不值）
    pub world_nodes: u64,
    pub world_reward: u64,
}

fn dist(a: (u16, u16), b: (u16, u16)) -> u32 {
    (a.0 as i32 - b.0 as i32).unsigned_abs() + (a.1 as i32 - b.1 as i32).unsigned_abs()
}

/// 全图扫一遍：本轮还长在地上、且没被采走的资源点，产量按当前块高算好。
fn survey(height: u64, words: &[u32], settled: &DustState) -> Vec<(u16, u16, u8, u64)> {
    let epoch = resource::epoch_of(height);
    resource::scan(0, 0, terrain::W as u32, terrain::H as u32, words)
        .into_iter()
        .filter(|(x, y, _)| !settled.harvested.contains(&(*x, *y, epoch)))
        .map(|(x, y, k)| (x, y, k, resource::reward_at(k, height)))
        .collect()
}

/// 算一件情报。`settled` 必须是**当前链头**重放出来的光尘状态：
/// 余量与排行都建立在「谁已经采过哪一格」上，旧状态会给出过期情报。
pub fn quote(id: &str, height: u64, settled: &DustState) -> Result<Quote, String> {
    let price = price_of(id).ok_or_else(|| format!("旅人没有这件情报：{id}"))?;
    let words = crate::world::resource_words();
    let epoch = resource::epoch_of(height);
    let pool = survey(height, &words, settled);
    let world_nodes = pool.len() as u64;
    let world_reward: u64 = pool.iter().map(|c| c.3).sum();

    let mut q = Quote {
        id: id.to_string(),
        price,
        height,
        epoch,
        night: resource::is_night(height),
        stops: Vec::new(),
        area: None,
        top: Vec::new(),
        total: 0,
        count: 0,
        world_nodes,
        world_reward,
    };

    match id {
        ROUTE => {
            let (stops, total) = best_route(&pool, stall_at(height));
            q.count = stops.len() as u64;
            q.total = total;
            q.stops = stops;
        }
        FRONTIER => {
            let (area, nodes) = quietest_area(&pool);
            q.count = nodes;
            q.total = area.reward;
            q.area = Some(area);
        }
        LEDGER => {
            let (people, cells, top) = this_epoch(settled, epoch);
            q.count = people;
            q.total = cells;
            q.top = top;
        }
        _ => return Err(format!("旅人没有这件情报：{id}")),
    }
    Ok(q)
}

/// 先按产出挑最值钱的一批（同价取离摊位近的），再按最近邻串成一程。
/// 「值钱」优先于「近」：走 20 格去砍一棵树，好过在脚边捡三朵花。
fn best_route(pool: &[(u16, u16, u8, u64)], stall: (u16, u16)) -> (Vec<Stop>, u64) {
    let mut cand: Vec<(u16, u16, u8, u64)> = pool
        .iter()
        .copied()
        .filter(|c| dist(stall, (c.0, c.1)) <= ROUTE_RADIUS)
        .collect();
    cand.sort_by(|a, b| {
        b.3.cmp(&a.3)
            .then(dist(stall, (a.0, a.1)).cmp(&dist(stall, (b.0, b.1))))
            .then(a.0.cmp(&b.0))
            .then(a.1.cmp(&b.1))
    });
    cand.truncate(ROUTE_POOL);

    let mut cur = stall;
    let mut stops = Vec::with_capacity(ROUTE_STOPS);
    while stops.len() < ROUTE_STOPS && !cand.is_empty() {
        let idx = cand
            .iter()
            .enumerate()
            .min_by_key(|(_, c)| (dist(cur, (c.0, c.1)), c.0, c.1))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let c = cand.remove(idx);
        stops.push(Stop {
            x: c.0,
            y: c.1,
            kind: c.2,
            reward: c.3,
            steps: dist(cur, (c.0, c.1)),
        });
        cur = (c.0, c.1);
    }
    let total = stops.iter().map(|s| s.reward).sum();
    (stops, total)
}

/// 把世界切成 `AREA_SIZE` 见方的小块，挑本轮余值最大的一块。
/// 并列时取坐标小的（先上后右），保证不同实现给出同一块地。
fn quietest_area(pool: &[(u16, u16, u8, u64)]) -> (Area, u64) {
    let mut buckets: BTreeMap<(u16, u16), (u64, u64)> = BTreeMap::new();
    for (x, y, _, r) in pool {
        let key = (y / AREA_SIZE, x / AREA_SIZE);
        let e = buckets.entry(key).or_insert((0, 0));
        e.0 += 1;
        e.1 += r;
    }
    let mut best = ((0u16, 0u16), (0u64, 0u64));
    for (i, (key, v)) in buckets.iter().enumerate() {
        if i == 0 || v.1 > best.1 .1 {
            best = (*key, *v);
        }
    }
    let (by, bx) = best.0;
    (
        Area {
            x: bx * AREA_SIZE,
            y: by * AREA_SIZE,
            size: AREA_SIZE,
            nodes: best.1 .0,
            reward: best.1 .1,
        },
        best.1 .0,
    )
}

/// 本轮的战况：几个人在采、共采了多少格、谁采得最多。
fn this_epoch(settled: &DustState, epoch: u64) -> (u64, u64, Vec<Harvester>) {
    let mut people = 0u64;
    let mut cells = 0u64;
    let mut top: Vec<Harvester> = Vec::new();
    for ((addr, e), n) in &settled.epoch_counts {
        if *e != epoch {
            continue;
        }
        people += 1;
        cells += *n as u64;
        top.push(Harvester {
            addr: addr.clone(),
            count: *n,
        });
    }
    top.sort_by(|a, b| b.count.cmp(&a.count).then(a.addr.cmp(&b.addr)));
    top.truncate(3);
    (people, cells, top)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore::WalletCore;
    use crate::simchain::GENESIS_HASH_HEX;
    use crate::world::dust::{self, DustView};

    fn core() -> WalletCore {
        crate::keystore::open_core_from_seed(&[5u8; 32], &[2u8; 8]).unwrap()
    }

    fn words() -> Vec<u32> {
        terrain::seed_words(GENESIS_HASH_HEX, &[])
    }

    /// 摊位在一程之内不动，跨过 50 块才换地方
    #[test]
    fn stall_holds_a_period_then_moves() {
        assert_eq!(stall_at(0), stall_at(49));
        assert_eq!(stall_at(50), stall_at(99));
        assert_eq!(stall_at(120), stall_at(149));
        let mut seen = std::collections::BTreeSet::new();
        for p in 0..8u64 {
            seen.insert(stall_at(p * TRADER_PERIOD));
        }
        assert!(seen.len() >= 6, "八程里只换了 {} 处摊位", seen.len());
        let (x, y) = stall_at(0);
        assert!((x as usize) < terrain::W && (y as usize) < terrain::H);
    }

    /// 摊子不摆在水里（水/沙滩高度 ≤ SAND_LEVEL）
    #[test]
    fn stall_stands_on_dry_land() {
        let w = words();
        for p in 0..8u64 {
            let (x, y) = stall_at(p * TRADER_PERIOD);
            assert!(
                terrain::height_at(x as u32, y as u32, &w) > terrain::SAND_LEVEL,
                "第 {p} 程摊位 ({x},{y}) 落在水里"
            );
        }
    }

    /// 路线：每一站都真的长着东西、没被采走、不重复，步数是曼哈顿距离
    #[test]
    fn route_is_connected_and_harvestable() {
        let h = 10;
        let st = DustState::default();
        let q = quote(ROUTE, h, &st).unwrap();
        assert!(!q.stops.is_empty(), "路线一站都没有");
        assert!(q.stops.len() <= ROUTE_STOPS);
        let mut cur = stall_at(h);
        let mut seen = std::collections::BTreeSet::new();
        for s in &q.stops {
            assert!(seen.insert((s.x, s.y)), "路线重复经过 ({},{})", s.x, s.y);
            assert_eq!(s.steps, dist(cur, (s.x, s.y)), "步数与路线接不上");
            assert_eq!(
                resource::node_at(s.x as u32, s.y as u32, &words()),
                Some(s.kind),
                "({},{}) 上并没有这种资源",
                s.x,
                s.y
            );
            assert_eq!(s.reward, resource::reward_at(s.kind, h));
            cur = (s.x, s.y);
        }
        assert_eq!(q.total, q.stops.iter().map(|s| s.reward).sum::<u64>());
        assert!(!st.harvested.contains(&(q.stops[0].x, q.stops[0].y, 0)));
    }

    /// 采过的格子不该再出现在路线里
    #[test]
    fn route_skips_what_is_already_picked() {
        let h = 10;
        let mut st = DustState::default();
        let q = quote(ROUTE, h, &st).unwrap();
        for s in &q.stops {
            st.harvested.insert((s.x, s.y, 0));
        }
        let q2 = quote(ROUTE, h, &st).unwrap();
        for s in &q2.stops {
            assert!(!st.harvested.contains(&(s.x, s.y, 0)), "路线里还有采过的格子");
        }
    }

    /// 冷清之地：跟独立扫出来的分块最大值一致
    #[test]
    fn frontier_matches_an_independent_bucket_scan() {
        let h = 10;
        let q = quote(FRONTIER, h, &DustState::default()).unwrap();
        let area = q.area.expect("frontier 必须带区域");

        let w = words();
        let mut best = (0u64, (0u16, 0u16));
        for by in 0..(terrain::H as u16 / AREA_SIZE) {
            for bx in 0..(terrain::W as u16 / AREA_SIZE) {
                let mut sum = 0u64;
                for y in by * AREA_SIZE..(by + 1) * AREA_SIZE {
                    for x in bx * AREA_SIZE..(bx + 1) * AREA_SIZE {
                        if let Some(k) = resource::node_at(x as u32, y as u32, &w) {
                            sum += resource::reward_at(k, h);
                        }
                    }
                }
                if sum > best.0 {
                    best = (sum, (bx, by));
                }
            }
        }
        assert_eq!((area.x, area.y), (best.1 .0 * AREA_SIZE, best.1 .1 * AREA_SIZE));
        assert_eq!(area.reward, best.0);
        assert_eq!(area.nodes, q.count);
        assert!(area.reward > 0);
    }

    /// 账本情报：只统计本轮，按格数降序，最多三条
    #[test]
    fn ledger_counts_this_epoch_only() {
        let mut st = DustState::default();
        st.epoch_counts.insert(("a".into(), 0), 5);
        st.epoch_counts.insert(("b".into(), 0), 9);
        st.epoch_counts.insert(("c".into(), 0), 1);
        st.epoch_counts.insert(("d".into(), 0), 7);
        st.epoch_counts.insert(("a".into(), 1), 100); // 上一轮，不该计入
        let q = quote(LEDGER, 10, &st).unwrap();
        assert_eq!(q.count, 4, "本轮采集人数");
        assert_eq!(q.total, 22, "本轮采集格数");
        assert_eq!(q.top.len(), 3);
        assert_eq!(q.top[0].addr, "b");
        assert_eq!(q.top[1].addr, "d");
        assert_eq!(q.top[2].addr, "a");
    }

    #[test]
    fn unknown_offer_is_refused() {
        assert!(quote("free-lunch", 10, &DustState::default()).is_err());
        assert_eq!(price_of(ROUTE), Some(60));
        assert_eq!(price_of(FRONTIER), Some(40));
        assert_eq!(price_of(LEDGER), Some(20));
    }

    /// 烧毁地址：合法的 32 字节 ql1 地址，且与任何钱包地址都不同
    #[test]
    fn sink_is_a_valid_burn_address() {
        let sink = sink_address();
        assert!(sink.starts_with("ql1"));
        let raw = crate::simchain::bech32_decode_addr(&sink).expect("烧毁地址必须能解码");
        assert_eq!(raw.len(), 32);
        assert_ne!(sink, core().address);
        assert_eq!(sink, sink_address(), "烧毁地址必须每次算出来都一样");
    }

    /// 买一件情报 = 一条普通 send 记录：光尘从买家到烧毁地址，买家余额正好少掉报价
    #[test]
    fn paying_burns_dust_at_the_sink() {
        let c = core();
        let w = words();
        let h = 10;
        let (x, y, k) = {
            let mut found = None;
            'outer: for yy in 0..128u32 {
                for xx in 0..128u32 {
                    if let Some(k) = resource::node_at(xx, yy, &w) {
                        found = Some((xx as u16, yy as u16, k));
                        break 'outer;
                    }
                }
            }
            found.expect("前 128×128 里没有资源点")
        };
        let reward = resource::reward_at(k, h);
        let harvest = dust::sign_record(
            &c,
            1,
            &[0u8; 32],
            &dust::DustDraft {
                op: dust::op::HARVEST,
                epoch: resource::epoch_of(h),
                x,
                y,
                kind: k,
                amount: reward,
                to: "",
                height: h,
                ts: h,
            },
        );
        let price = price_of(LEDGER).unwrap();
        let sink = sink_address();
        let send = dust::sign_record(
            &c,
            2,
            &dust::record_hash(&harvest),
            &dust::DustDraft {
                op: dust::op::SEND,
                epoch: 0,
                x: 0,
                y: 0,
                kind: 0,
                amount: price,
                to: &sink,
                height: h + 1,
                ts: h + 1,
            },
        );
        assert!(reward >= price, "一棵树的产出应当买得起最便宜的情报");
        let (st, rep) = dust::replay(&[harvest, send], &DustView { tip: h + 1 });
        assert!(rep.ok, "{}", rep.reason);
        assert_eq!(rep.accepted, 2);
        assert_eq!(st.balance_of(&c.address), reward - price);
        assert_eq!(st.balance_of(&sink), price);
    }

    /// 余额不够时 `quote` 照算，但购买层必须先拒（这里只验证价格可判定的那一半）
    #[test]
    fn every_offer_costs_more_than_zero() {
        for id in [ROUTE, FRONTIER, LEDGER] {
            let p = price_of(id).unwrap();
            assert!(p > 0 && p <= resource::base_reward(resource::NODE_TREE));
        }
    }
}
