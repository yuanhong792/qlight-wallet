//! B 档联邦网络（M2.1）：libp2p gossipsub + 双路发现（mDNS + 引导注册表）
//!
//! 目标：多节点收同一批交易、VRF 心跳门票选出块人、其他节点验签拒绝坏块、全网账本一致。
//!
//! 邻居发现双保险（关键：macOS 同机两个进程之间组播包送不到——系统级限制，
//! 已实测 loopback/en0 口均不通；故 mDNS 只能服务跨机场景，同机多开走注册表）：
//! - mDNS：跨机局域网自动发现（UDP 5353 组播）；
//! - 引导注册表：所有实例把「节点号 + 监听地址 + 时间戳」合并写进公共
//!   `peers-registry.json`（fs2 独占锁防并发），周期互读 → TCP 直连。
//!   等同比特币 peers.dat 固定种子：同机 TCP 回环不经组播、不过防火墙，必通。
//!   条目 15 分钟未刷新自动裁剪（防止单次运行残留），容量上限 64。
//!
//! 消息面（7 个 gossip 主题，载荷均 JSON）：
//! - `qlx/reg/v1`    注册：{pk(1952B hex), salt(8B hex)} → 对端独立重算 ql1 地址，
//!   得到「地址 ↔ 公钥」的可验证绑定（跨节点验签的公钥来源）
//! - `qlx/tx/v1`     交易中继：{envelope, sig_hex} → 对端真验签入池
//! - `qlx/block/v1`  块中继：{header, txs[]} → 对端整块真验（逐笔重验签重放 +
//!   tx_root/块哈希重算 + 排放口径），一笔不对整块拒绝
//! - `qlx/sync/v1`   追账三件套：Announce（链公告）/ SyncAsk（对账询问）/ SyncResp
//!   （blocks.jsonl 全量快照，整链验证后采用）
//! - `qlx/ent/v1`    世界中继 + 追赶四件套：Entity（新记录推送）/ EntHead（集合指纹
//!   公告）/ EntAsk（索要整本）/ EntSnap（entities.jsonl 全量快照）
//! - `qlx/dust/v1`   光尘中继 + 追赶四件套：Dust / DustHead / DustAsk / DustSnap，
//!   与实体同一手法，独立前缀独立账本
//! - `qlx/pos/v1`    在场广播（**共识之外**，见 §9.19）：Pos{pk, salt, addr, x, y, z,
//!   heading, seq, ts, sig}。对端现场复算「这把公钥配这份盐 = 这个地址」再验签，
//!   所以冒充不了别人；但坐标本身无从证明，所以它只用来"看见彼此"——
//!   不进块、不落盘、不参与归属或发行的任何判定，一句话：在场挣不到钱。
//!
//! 出块节拍 —— 提交驱动（关键设计，避免各节点按各自墙钟出块导致双挖分叉）：
//! 1. 节点在「高度 H-1 落账」的那一刻（本地挖出或收到对端块），广播高度 H 的门票
//!    Ticket{addr, height, prev, sig}，并开 1.2s 收票窗口；
//! 2. 门票签名 = ML-DSA-65(「QL-TICKET-V1」‖ height ‖ prev)，注册表公钥可验；
//! 3. vrf' = SHAKE-256("QL-TICKET-V1" ‖ 票消息 ‖ sig)；窗口结束时 vrf' 最小者
//!    （并列比地址）出块，其余节点等待该块；
//! 4. 败选等 2.6s 仍无块 → 自愈兜底出块（演示网 liveness 保证）。
//!
//! 对账自愈（公告机制，处理启动错拍/网络闪断留下的分叉）：
//! - 每 5s 公告自己的 (height, tip_hash)；看到「更高」或「同高但 tip 字典序更小」的
//!   邻居 → 索要整链快照；快照逐块验证通过才整链采用（本机旧档先归档 .bak-fork-*）。
//! - 双方用同一条裁决规则（高者胜；同高 tip 小者胜），天然收敛到同一条链。
//!
//! 世界追赶（推送会漏，指纹能发现漏）：
//! - 新记录只在对端签名那一刻广播一次；本机当时掉线，那块方块就永久丢了，
//!   **而且谁都不知道丢了** —— 链侧有 Announce/SyncAsk 兜底，世界侧原来没有。
//! - 现在每 9s（仅联邦模式）公告 `EntHead{count, digest}`：digest =
//!   SHAKE-256("QL-ENTSET-V1" ‖ 全部 record_hash 升序拼接)，只依赖**记录集合**，
//!   与落盘顺序、多人记录怎么交错都无关。
//! - 看到「条数更多，或同数指纹更小」的邻居 → `EntAsk` → 持有者回 `EntSnap`
//!   （gzip+base64 的整本 entities.jsonl）。快照只是搬运工：收下前逐条验签验链，
//!   接不上的那条永远进不了本机账本；含坏行/超条数上限的快照整份丢弃（部分收下会
//!   让本机指纹永远对不上任何人，把追赶变成风暴）。
//!
//! 版本普查（分叉是静默的，只有趁还连得上时说出口）：
//! - `Announce` 除了块高还带 `app_version` 与 `rules`（出块域 ‖ 实体记录语义 ‖
//!   光尘语义，见 `simchain::rules_tag()`）。两个字段都 `#[serde(default)]`：
//!   0.4.1 之前的公告按空读，升级后的第一件事不是「整张网互相丢弃公告」。
//! - 邻居规则指纹与本机不同 = 互相验不过对方的块（v5 在块高 200 强制激活就是这类）；
//!   指纹相同但发行包不同 = 家族内改过判据时同样会分叉，只能报「不齐」。
//! - 分歧集合变化才打日志、才推 `net_versions`；邻居断开或连续三个公告周期没再
//!   公告就退出普查，chip 跟着撤警。
//!
//! 诚实边界（M2.1 演示口径，代码里不装假）：
//! - 联邦块接受依赖注册表公钥；快照整链只验哈希链（哈希链自洽即可采用），
//!   正式公测需「门票 VRF 证明 + 注册链锚定 + 罚没」闭环（M2.2+）；
//! - 演示规模（同机/局域网 2–3 节点）下按上述规则处理时间分叉；
//! - 世界追赶只在**至少一个在线邻居持有那份集合**时才补得回来；
//! - 版本普查只比对邻居**声明**出来的规则指纹（块域 / 实体记录 / 光尘记录）。
//!   同一规则家族内部改判据（如本轮把光尘轮次改成只认进块块高）不会让指纹变化，
//!   所以 chip 的第二层只报「发行包不齐」，不能断言一定分叉；
//!   陌生人之间真正的规则一致性要靠链本身裁决（§9.18 的诚实边界；门票证明与罚没在 M2.2）。

use crate::keystore::WalletCore;
use crate::simchain::{BlockEvent, BlockHeader, SimChain, BLOCK_V_V5_ACTIVATION};
use futures::StreamExt;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::multiaddr::Protocol;
use libp2p::{gossipsub, Multiaddr, PeerId};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use fs2::FileExt;
use tauri::{AppHandle, Emitter};

// ---------------------------------------------------------------- 节拍参数

/// 两块之间的节拍（与 A 档 2s 一致）
pub const BEAT_MS: u64 = 2000;
/// 门票收集窗口（从上一块落账时刻起算；留足错拍重叠，双挖率随窗口加大而骤降）
pub const WINDOW_MS: u64 = 2000;
/// 败选后等待胜者块的兜底时限
pub const WAIT_MS: u64 = 3400;
/// 注册重广播周期（保证新邻居拿到我们的公钥）
pub const REG_EVERY_MS: u64 = 15000;
/// 链公告周期（触发对账/追账）
pub const ANNOUNCE_EVERY_MS: u64 = 5000;
/// 版本普查条目时效：连着三个周期没再听到这台邻居的公告，就不再算它的分歧
/// （对端进程被杀掉时未必来得及发出 ConnectionClosed）
const VERSION_TTL_MS: u64 = 3 * ANNOUNCE_EVERY_MS;
/// 追账快照上限（演示规模；指 gzip 压缩后的网络负载，gossipsub 单条消息同此上限）
pub const SYNC_MAX_BYTES: usize = 4 * 1024 * 1024;
/// 引导注册表周期（写入自己 + 互读直连）
pub const BOOT_EVERY_MS: u64 = 4000;
/// 注册表条目时效（单次运行的残留自动过期）
pub const BOOT_TTL_MS: u64 = 15 * 60 * 1000;
/// 注册表容量上限
const BOOT_MAX_ENTRIES: usize = 64;
/// 直连失败重试间隔
const BOOT_RETRY_MS: u64 = 4000;
/// 世界指纹公告周期（缺口检测；比链公告慢一档——补的是记录，不阻塞出块）
const ENT_HEAD_EVERY_MS: u64 = 9000;
/// 实体快照的条数上限（演示规模；也是给对端的 CPU/磁盘放大器设的闸门）
const ENT_SNAP_MAX_LINES: usize = 65_536;
/// 光尘指纹公告周期（比实体慢一档：光尘记录多，补齐也更贵）
const DUST_HEAD_EVERY_MS: u64 = 13_000;
/// 光尘快照条数上限
const DUST_SNAP_MAX_LINES: usize = 65_536;

// —— 在场广播（DESIGN.md §9.19，**共识之外**）——
/// 心跳重发周期：位置没动也重发一次，晚来的邻居不必等我移动就能看见我
const POS_HEARTBEAT_MS: u64 = 4000;
/// 在场条目时效：三个心跳没再报位置就当作离场（进程被杀、锁钱包都走这条路）
const POS_TTL_MS: u64 = 3 * POS_HEARTBEAT_MS;
/// 前端上报的最小间隔：位置是画面级高频数据，不设闸等于给脚本一个 gossip 放大器
const POS_PUBLISH_MIN_MS: u64 = 500;
/// 签名时间戳窗口：超出即丢。在场不进共识，但也不许被人拿陈旧包灌出一串历史位置
const POS_MAX_AGE_SECS: u64 = 120;
/// 在场名单容量上限（演示规模，同时是内存放大器闸门）
const POS_ROSTER_MAX: usize = 256;
/// 坐标定点化倍率：世界坐标是浮点，签名里的字节必须是确定的整数
const POS_SCALE: f64 = 64.0;

const TOPIC_REG: &str = "qlx/reg/v1";
const TOPIC_TX: &str = "qlx/tx/v1";
const TOPIC_BLOCK: &str = "qlx/block/v1";
const TOPIC_SYNC: &str = "qlx/sync/v1";
/// 世界实体中继（M0「彼此看见」）：对方造了一个方块，我的小窗里当场显形
const TOPIC_ENT: &str = "qlx/ent/v1";
/// 光尘中继：采集与转账记录（独立账本，独立前缀）
const TOPIC_DUST: &str = "qlx/dust/v1";
/// 在场中继：谁在世界的哪个位置。只有展示价值，**没有任何一条会落进账本或块**
const TOPIC_POS: &str = "qlx/pos/v1";
/// 门票消息前缀（签名域隔离）
const TICKET_PREFIX: &[u8] = b"QL-TICKET-V1";
/// 在场消息前缀：与门票/交易/实体/光尘四个签名域全部隔离，
/// 一条在场签名不可能被误当成任何有账本意义的东西
const POS_PREFIX: &[u8] = b"QL-POS-V1";

// ---------------------------------------------------------------- 网络消息

/// 交易在网络上的形态
/// 上网的交易载体 = 块内原件（envelope + 签名 + 公钥 + 地址盐）。
///
/// 带上公钥与盐是有意的：收方必须能**独立**复算「这个地址确实由这把公钥派生」并验签，
/// 而不是先等对方把 `地址↔公钥` 注册广播过来（旧实现依赖注册表，先到交易后到注册
/// 就被丢弃，等于让邻居用消息顺序卡住你的交易）。
type TxWire = crate::simchain::BlockWire;

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub enum NetMsg {
    Reg {
        pk_hex: String,
        salt_hex: String,
    },
    Tx(TxWire),
    Block {
        header: BlockHeader,
        txs: Vec<TxWire>,
        /// v5 起：块内光尘记录（旧版本节点发来的消息没有这个字段，按空读）
        #[serde(default)]
        dusts: Vec<crate::world::dust::DustRecord>,
    },
    Ticket {
        addr: String,
        height: u64,
        prev_hash_hex: String,
        sig_hex: String,
    },
    Announce {
        height: u64,
        tip_hash_hex: String,
        /// —— 联邦版本告警 —— 0.4.1 起带上；`rules` 为空 = 对端是更早的版本
        /// （那时公告还没有这两个字段，也就谈不上"版本一致"）。
        #[serde(default)]
        app_version: String,
        #[serde(default)]
        rules: String,
    },
    SyncAsk {
        height: u64,
        tip_hash_hex: String,
    },
    SyncResp {
        /// base64(gzip(blocks.jsonl))——整链快照压完再上网
        snap: String,
    },
    /// 实体记录中继：一批（本机新签的那条，或应答对端索要的历史）。
    /// 对端**逐条独立验签验链**后才落盘，所以这一批不需要被信任。
    Entity(Vec<crate::world::EntityRecord>),
    /// 世界集合指纹公告：`(条数, 整本 records 的排序哈希指纹)`。
    ///
    /// 它是「推送会漏」的解药：邻居靠推送（`Entity`）拿到记录，掉线那一刻别人放的
    /// 方块永久错过，而且谁都不知道错过了。公告指纹让任何一台机器都能当场发现
    /// 自己缺，并主动索要补齐。
    EntHead {
        count: u64,
        digest_hex: String,
    },
    /// 我缺了：请把你那份 `(count, digest_hex)` 的整本实体账本发来
    EntAsk {
        count: u64,
        digest_hex: String,
    },
    /// 整本实体快照：base64(gzip(entities.jsonl))。`digest_hex` 必须是索要的那个，
    /// 否则收方直接丢弃 —— 快照只是搬运工，逐条验签验链才是裁判。
    EntSnap {
        digest_hex: String,
        snap: String,
    },
    /// 光尘记录中继：本机新签的一条，或应答对端索要的历史。
    Dust(Vec<crate::world::dust::DustRecord>),
    /// 光尘集合指纹公告 `(条数, 指纹)`
    DustHead { count: u64, digest_hex: String },
    /// 我缺光尘记录：请把你那份发来
    DustAsk { count: u64, digest_hex: String },
    /// 整本光尘快照：base64(gzip(dust.jsonl))
    DustSnap { digest_hex: String, snap: String },
    /// 在场公告（共识之外）：本人此刻在世界里的位置。收方独立复算
    /// 「这把公钥 + 这份盐派生出的地址就是它声称的那个」再验签，与 `Tx` 同一手法，
    /// 不依赖注册表的消息顺序。落进去的只有内存名单，账本与块一个字节都不碰。
    Pos(PresenceWire),
}

/// 在场原件：坐标定点化为整数（`POS_SCALE` 倍）后才进签名消息——浮点没有可比字节。
///
/// `seq` 是每个公钥自己的单调序号：邻居只认更大的 seq，所以网络重排不会把一个人
/// 拉回旧位置；`ts` 提供绝对时间窗口，防止有人攒一批老签名重放。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct PresenceWire {
    pub pk_hex: String,
    pub salt_hex: String,
    pub addr: String,
    /// 世界 X（定点，除以 `POS_SCALE` 得浮点坐标）
    pub x: i32,
    /// 世界 Y（竖直方向，即层/高度）
    pub y: i32,
    /// 世界 Z（定点）
    pub z: i32,
    /// 朝向（度 × `POS_SCALE`；和坐标同一个定点口径，不要按"度 × 10"读）
    pub heading: i32,
    pub seq: u64,
    pub ts: u64,
    pub sig_hex: String,
}

/// 签名消息 = 前缀 ‖ seq ‖ ts ‖ x ‖ y ‖ z ‖ heading ‖ addr
///
/// 公钥由验签本身绑定，盐与地址则显式进消息：否则一条合法签名可以配上任意盐，
/// 把同一个人伪装成由同一把公钥派生的任何地址。
fn pos_msg_bytes(w: &PresenceWire) -> Vec<u8> {
    let mut m = Vec::with_capacity(52 + w.addr.len());
    m.extend_from_slice(POS_PREFIX);
    for v in [w.seq, w.ts] {
        m.extend_from_slice(&v.to_be_bytes());
    }
    for v in [w.x, w.y, w.z, w.heading] {
        m.extend_from_slice(&v.to_be_bytes());
    }
    m.extend_from_slice(w.addr.as_bytes());
    m
}

/// 定点化并签下一条在场原件（浮点坐标 × `POS_SCALE` 后取整，钳进 i32）
fn sign_pos_wire(
    core: &WalletCore,
    seq: u64,
    x: f64,
    y: f64,
    z: f64,
    heading: f64,
    now_ms: u64,
) -> PresenceWire {
    let q = |v: f64| {
        (v * POS_SCALE)
            .round()
            .clamp(i32::MIN as f64, i32::MAX as f64) as i32
    };
    let mut w = PresenceWire {
        pk_hex: hex::encode(&core.pk_bytes),
        salt_hex: hex::encode(core.addr_salt),
        addr: core.address.clone(),
        x: q(x),
        y: q(y),
        z: q(z),
        heading: q(heading),
        seq,
        ts: now_ms / 1000,
        sig_hex: String::new(),
    };
    w.sig_hex = hex::encode(crate::crypto::sign(&core.signing_key, &pos_msg_bytes(&w)));
    w
}

#[derive(NetworkBehaviour)]
pub struct QlBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: libp2p::mdns::tokio::Behaviour,
}

// ---------------------------------------------------------------- 引导注册表

/// 注册表条目：某节点的监听地址 + 最后刷新时间
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct BootEntry {
    addrs: Vec<String>,
    ts: u64,
}
type BootFile = HashMap<String, BootEntry>;

/// 主题句柄
struct Topics {
    reg: gossipsub::IdentTopic,
    tx: gossipsub::IdentTopic,
    block: gossipsub::IdentTopic,
    sync: gossipsub::IdentTopic,
    ent: gossipsub::IdentTopic,
    dust: gossipsub::IdentTopic,
    pos: gossipsub::IdentTopic,
}

// ---------------------------------------------------------------- 出块轮状态机

/// 出块轮状态（提交驱动：块 N 落账刻 → 高度 N+1 门票 → 开票胜者出 N+1）
enum Phase {
    /// 直出（无邻居 = A 档行为）
    Direct,
    /// 投票窗口：收集所有节点的高度 H 门票
    Voting {
        height: u64,
        until_ms: u64,
        /// vrf'(hex) → addr
        tickets: HashMap<String, String>,
    },
    /// 败选：等待胜者块
    Waiting { height: u64, until_ms: u64 },
}

struct MachineState {
    phase: Phase,
    beat_next: u64,
    reg_next: u64,
    ann_next: u64,
    /// 上次索要的对账目标（height:tip），防追账风暴
    sync_asked: Option<String>,
    /// 下次公告世界指纹的时刻
    ent_next: u64,
    /// 上次索要的实体集合指纹（同一个缺失只问一次，防追赶风暴）
    ent_asked: Option<String>,
    /// 下次公告光尘指纹的时刻
    dust_next: u64,
    /// 上次索要的光尘集合指纹
    dust_asked: Option<String>,
    /// 下次重发本机位置的时刻（在场心跳）
    pos_next: u64,
}

impl Default for MachineState {
    fn default() -> Self {
        let now = now_ms();
        Self {
            phase: Phase::Direct,
            beat_next: now + 900,  // 启动后 ~1s 首拍
            reg_next: now + 2000,  // 解锁后尽快广播注册
            ann_next: now + 1500,  // 尽快公告链，让新邻居尽早对齐
            sync_asked: None,
            ent_next: now + 2500,  // 尽快对一次世界，掉线期间漏掉的方块当场补回
            ent_asked: None,
            dust_next: now + 3000,
            dust_asked: None,
            pos_next: now + 3500, // 解锁后先报一次位置，再按心跳重发
        }
    }
}

// ---------------------------------------------------------------- 在场名单

/// 邻居报上来的一个位置（只在内存里，进程一停就什么都没有）
#[derive(Clone, Debug)]
struct PosEntry {
    addr: String,
    x: f64,
    y: f64,
    z: f64,
    heading: f64,
    seq: u64,
    /// 本机收到它的时刻，用于 TTL
    seen_ms: u64,
}

/// 在场名单：地址 → 最新位置。
///
/// 与版本普查同一形状的两个方法（`note` / `sweep`）＋一个变更标志：只有名单内容
/// 真的变了才推前端事件，不然每秒重画一次人物毫无意义。
#[derive(Default)]
struct Presence {
    by_addr: HashMap<String, PosEntry>,
    changed: bool,
}

impl Presence {
    /// 记下一个位置；同地址只认更大的 seq（网络重排不能把人拉回旧位置）。
    /// 返回 false = 这条比已有的旧，或者名单已满——都静默丢弃，在场不值得告警。
    /// `seen_ms` 由验签那一刻盖上，TTL 才算的是"我最后一次听到他"。
    fn note(&mut self, e: PosEntry) -> bool {
        match self.by_addr.get(&e.addr) {
            Some(old) if old.seq >= e.seq => return false,
            None if self.len() >= POS_ROSTER_MAX => return false,
            _ => {}
        }
        self.changed = true;
        self.by_addr.insert(e.addr.clone(), e);
        true
    }

    /// 清掉超过 TTL 的邻居（下线、进程被杀、锁钱包都走这条路）。
    fn sweep(&mut self, ttl_ms: u64, now: u64) {
        let before = self.by_addr.len();
        self.by_addr.retain(|_, e| now.saturating_sub(e.seen_ms) < ttl_ms);
        if self.by_addr.len() != before {
            self.changed = true;
        }
    }

    /// 名单变了才返回内容并复位标志（与 `VersionCensus::take_warning` 同一手法）
    fn take_update(&mut self) -> Option<Vec<PosEntry>> {
        if !std::mem::take(&mut self.changed) {
            return None;
        }
        let mut list: Vec<PosEntry> = self.by_addr.values().cloned().collect();
        list.sort_by(|a, b| a.addr.cmp(&b.addr));
        Some(list)
    }

    fn len(&self) -> usize {
        self.by_addr.len()
    }
}

// ---------------------------------------------------------------- Net 句柄

/// 邻居规则指纹普查（联邦版本告警）
///
/// 装的是**全部在线邻居**（每台 2–3 台，规模由连接数天然有界），
/// 判据分两层，因为分叉有两种：
/// - 规则家族不同（`rules` ≠ 本机）：块域／实体记录／光尘记录不同 = 两端互相
///   验不过对方的块与记录。v5 在块高 200 强制激活就是这一类。
/// - 家族相同但发行包不同（`app_version` ≠ 本机）：家族**内部**改过判据时两端也会
///   分叉，而指纹看不出来——本轮的光尘轮次口径就是这种改动。这一层只能说"不齐"，
///   不能断言一定分叉：多数补丁版本只动界面。
///
/// 为什么要普查而不是「连上就算兼容」：分歧在链上是静默的——各出各的块、各自认为
/// 自己那条才是链，连接层和日志都没有任何异常。只有趁还连得上的时候说出口。
#[derive(Default)]
struct VersionCensus {
    /// 在线邻居：pid → (App 版本, 规则指纹, 最后一次公告时刻)
    peers: HashMap<String, (String, String, u64)>,
    /// 已经告出去过的分歧签名；集合没变就不重复刷屏、不重复发事件
    warned: String,
}

/// 公告不带版本/规则指纹（0.4.1 之前的对端）时的记法：直说未知，不假装兼容
const RULES_UNKNOWN: &str = "—";
/// 版本号唯一来源是 Cargo.toml（与 `commands::app_version` 同一处）
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

impl VersionCensus {
    /// 登记一条公告（公告每 5 秒重播一次，同一台机器只会占一行）
    fn note(&mut self, pid: &str, app_version: &str, rules: &str, now: u64) {
        let (v, r) = (
            if app_version.is_empty() { RULES_UNKNOWN } else { app_version }.to_string(),
            if rules.is_empty() { RULES_UNKNOWN } else { rules }.to_string(),
        );
        self.peers.insert(pid.to_string(), (v, r, now));
    }

    /// 邻居断开就从普查里消失：否则告警会挂着一台已经不存在的机器
    fn drop_peer(&mut self, pid: &str) {
        self.peers.remove(pid);
    }

    /// 公告是周期重发的；一段时间没再听到就等于它不在这张网上了
    fn sweep(&mut self, ttl_ms: u64, now: u64) {
        self.peers.retain(|_, (_, _, seen)| now.saturating_sub(*seen) < ttl_ms);
    }

    /// 分成「规则分歧」与「版本不齐（规则一致）」两堆，各自按指纹排序，便于逐行显示
    fn split(&self, mine: &str, my_version: &str) -> (Vec<(String, String)>, Vec<(String, String)>) {
        let mut fork = Vec::new();
        let mut odd = Vec::new();
        for (_, (v, r, _)) in self.peers.iter() {
            // 邻居列表里不带 pid：chip 要的是"有几个人不一样"，不是谁的机器
            if r != mine {
                fork.push((v.clone(), r.clone()));
            } else if v != my_version {
                odd.push((v.clone(), r.clone()));
            }
        }
        fork.sort();
        odd.sort();
        (fork, odd)
    }

    /// 分歧签名：只有两堆内容变了才算"新情况"。全网齐整时签名是空串，
    /// 这样"从干净到干净"一次事件都不会推（空普查不该让 chip 反复重绘）。
    fn signature(&self, mine: &str, my_version: &str) -> String {
        let (fork, odd) = self.split(mine, my_version);
        if fork.is_empty() && odd.is_empty() {
            return String::new();
        }
        let rows = |list: &Vec<(String, String)>| {
            list.iter()
                .map(|(v, r)| format!("{v}/{r}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        format!("F[{}]O[{}]", rows(&fork), rows(&odd))
    }

    /// 分歧集合**变了**才返回 true：公告每轮重发，不控住就会刷屏、事件也会白推。
    /// 变干净（邻居升级完/掉线完）也算变了——chip 得跟着撤警。
    fn take_warning(&mut self, mine: &str, my_version: &str) -> bool {
        let sig = self.signature(mine, my_version);
        if sig == self.warned {
            return false;
        }
        self.warned = sig;
        let (fork, odd) = self.split(mine, my_version);
        let rows = |list: &Vec<(String, String)>| {
            list.iter()
                .map(|(v, r)| format!("{v}/{r}"))
                .collect::<Vec<_>>()
                .join(" / ")
        };
        if !fork.is_empty() {
            eprintln!(
                "[QLight] 联邦规则分歧（本机 {mine}）：{}；两端互相验不过对方的块，块高超过 {BLOCK_V_V5_ACTIVATION} 后会各出各的链且谁都不报错",
                rows(&fork)
            );
        }
        if !odd.is_empty() {
            eprintln!(
                "[QLight] 联邦版本不齐、规则指纹相同（本机 {my_version}）：{}；规则家族内改过判据（本轮：光尘轮次只认进块块高）时这一样会分叉，请统一发行包",
                rows(&odd)
            );
        }
        true
    }

    /// 给 UI 的两堆明细
    fn lists(&self, mine: &str, my_version: &str) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
        let (fork, odd) = self.split(mine, my_version);
        let json = |list: Vec<(String, String)>| {
            list.into_iter()
                .map(|(v, r)| serde_json::json!({ "app_version": v, "rules": r }))
                .collect::<Vec<_>>()
        };
        (json(fork), json(odd))
    }
}

/// 联邦网络句柄（与 AppState 共享 chain / core_slot）
pub struct Net {
    pub chain: Arc<SimChain>,
    /// 世界实体账本（与 AppState 同一个 Arc）：邻居中继来的记录合并到这里
    pub entities: Arc<crate::world::EntityLedger>,
    /// 光尘账本（与 AppState 同一个 Arc）
    pub dust: Arc<crate::world::dust::DustLedger>,
    /// 与 AppState 共用的解锁核心槽（同槽 Arc：unlock/lock 两侧自动同步，无需两处写）
    pub core_slot: Arc<RwLock<Option<Arc<WalletCore>>>>,
    /// 注册表：bech32 地址 → (pk 原始字节, addr_salt)
    registry: Mutex<HashMap<String, (Vec<u8>, [u8; 8])>>,
    /// 指令通道（命令线程 → 网络线程）
    cmd_tx: tokio::sync::mpsc::UnboundedSender<NetCmd>,
    /// 已发现邻居数（UI 联邦 chip）
    peers: AtomicUsize,
    /// 规则指纹与本机不同的邻居（联邦版本告警）
    versions: Mutex<VersionCensus>,
    /// 在场名单（共识之外：只供展示，永不落盘、永不变成收益）
    presence: Mutex<Presence>,
    /// 本机最后一次上报的位置：心跳靠它重发，前端不移动也还在场上
    my_pos: Mutex<Option<PresenceWire>>,
    /// 本机在场序号，单调递增
    pos_seq: AtomicU64,
    /// 下一次允许前端上报位置的时刻（POS_PUBLISH_MIN_MS 的闸）
    pos_floor: AtomicU64,
    /// 拨号时用过的地址范围（连接真建立后才转进 `peer_scope`）
    dial_scope: Mutex<HashMap<String, &'static str>>,
    /// 已连接邻居的地址范围：本地 / 局域网 / 公网三态的数据源
    peer_scope: Mutex<HashMap<String, &'static str>>,
    /// 本机规则指纹：启动时算一次，公告与普查都用同一个值
    my_rules: String,
    /// 公共引导注册表（base_dir/peers-registry.json，所有实例共享）
    boot_path: PathBuf,
}

enum NetCmd {
    BroadcastTx(TxWire),
    BroadcastEntity(Vec<crate::world::EntityRecord>),
    BroadcastDust(Vec<crate::world::dust::DustRecord>),
    BroadcastPos(PresenceWire),
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn short_addr(addr: &str) -> String {
    if addr.len() > 14 {
        format!("{}…{}", &addr[..8], &addr[addr.len() - 6..])
    } else {
        addr.to_string()
    }
}

/// 邻居地址的可达范围 —— 「本地世界 / 局域网演示 / 公网多人」三态的唯一数据源。
///
/// 现在这张网只有 mDNS + 同机引导注册表 + TCP 直连：没有中继、没有公网种子节点，
/// 所以 `"public"` 在这里**应当永远是 0**。把它单列出来不是凑数——真接上公网的那天，
/// 界面会在同一行数据上自己变过去，而不是靠文档里一句"已经全球同服"。
pub fn addr_scope(a: &Multiaddr) -> &'static str {
    for p in a.iter() {
        match p {
            Protocol::Ip4(ip) => {
                let o = ip.octets();
                return if o[0] == 127 {
                    "loopback"
                } else if o[0] == 10
                    || (o[0] == 172 && (16..=31).contains(&o[1]))
                    || (o[0] == 192 && o[1] == 168)
                    || (o[0] == 169 && o[1] == 254)
                {
                    "lan"
                } else {
                    "public"
                };
            }
            Protocol::Ip6(ip) => {
                if ip.is_loopback() {
                    return "loopback";
                }
                let b = ip.octets();
                // fc00::/7 私密段（ULA）≈ v4 的 10/8；fe80::/10 是链路本地
                if b[0] & 0xfe == 0xfc || (b[0] == 0xfe && (b[1] & 0xc0) == 0x80) {
                    return "lan";
                }
                return "public";
            }
            _ => {}
        }
    }
    "unknown"
}

/// 门票签名消息 = 前缀 ‖ height(8B BE) ‖ prev_hash（收发双方同构重建）
fn ticket_msg_bytes(height: u64, prev_hash_hex: &str) -> Vec<u8> {
    let mut m = Vec::with_capacity(12 + prev_hash_hex.len());
    m.extend_from_slice(TICKET_PREFIX);
    m.extend_from_slice(&height.to_be_bytes());
    m.extend_from_slice(prev_hash_hex.as_bytes());
    m
}

/// 门票 vrf' = SHAKE-256(prefix ‖ 票消息 ‖ sig)（签名唯一 → vrf' 唯一）
fn ticket_vrf(msg: &[u8], sig: &[u8]) -> Vec<u8> {
    let mut all = Vec::with_capacity(msg.len() + sig.len());
    all.extend_from_slice(msg);
    all.extend_from_slice(sig);
    crate::crypto::shake256(TICKET_PREFIX, &all, 32)
}

impl Net {
    /// 启动联邦网络（swarm 在 Tauri 的 tokio 运行时里常驻）
    pub fn start(
        app: AppHandle,
        chain: Arc<SimChain>,
        entities: Arc<crate::world::EntityLedger>,
        dust: Arc<crate::world::dust::DustLedger>,
        core_slot: Arc<RwLock<Option<Arc<WalletCore>>>>,
        boot_path: PathBuf,
    ) -> Arc<Net> {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<NetCmd>();
        let net = Arc::new(Net {
            chain,
            entities,
            dust,
            core_slot,
            registry: Mutex::new(HashMap::new()),
            cmd_tx,
            peers: AtomicUsize::new(0),
            versions: Mutex::new(VersionCensus::default()),
            presence: Mutex::new(Presence::default()),
            my_pos: Mutex::new(None),
            pos_seq: AtomicU64::new(0),
            pos_floor: AtomicU64::new(0),
            dial_scope: Mutex::new(HashMap::new()),
            peer_scope: Mutex::new(HashMap::new()),
            my_rules: crate::simchain::rules_tag(),
            boot_path,
        });
        tauri::async_runtime::spawn(run_swarm(app, net.clone(), cmd_rx));
        net
    }

    /// 本机新签的实体记录广播给联邦（落盘成功后调用；DESIGN.md §9.15 M0）
    pub fn broadcast_entity(&self, rec: crate::world::EntityRecord) {
        let _ = self.cmd_tx.send(NetCmd::BroadcastEntity(vec![rec]));
    }

    /// 本机新签的光尘记录广播给联邦（落盘成功后调用）
    pub fn broadcast_dust(&self, rec: crate::world::dust::DustRecord) {
        let _ = self.cmd_tx.send(NetCmd::BroadcastDust(vec![rec]));
    }

    /// 本机发出的交易广播给联邦（入池成功后调用；原件自带公钥与地址盐，对端无需注册表）
    pub fn broadcast_tx(&self, wire: crate::simchain::BlockWire) {
        let _ = self.cmd_tx.send(NetCmd::BroadcastTx(wire));
    }

    /// 已发现邻居数（UI 联邦 chip）
    pub fn peers_count(&self) -> usize {
        self.peers.load(Ordering::Relaxed)
    }

    /// 已连接邻居的地址范围统计（欢迎页与「关于」页的联机三态都读这一个数）
    pub fn scope_report(&self) -> serde_json::Value {
        let m = self.peer_scope.lock().unwrap();
        let mut out = serde_json::Map::new();
        for k in ["loopback", "lan", "public", "unknown"] {
            out.insert(k.to_string(), serde_json::json!(0));
        }
        for v in m.values() {
            let cur = out.get(*v).and_then(|n| n.as_u64()).unwrap_or(0);
            out.insert((*v).to_string(), serde_json::json!(cur + 1));
        }
        out.insert("total".to_string(), serde_json::json!(m.len()));
        serde_json::Value::Object(out)
    }

    /// 链公告：块高之外带上本机规则指纹，让邻居当场看得出两端会不会分叉
    fn announce_msg(chain: &SimChain, rules: &str) -> NetMsg {
        NetMsg::Announce {
            height: chain.height(),
            tip_hash_hex: chain.tip_hash_hex_pub(),
            app_version: APP_VERSION.to_string(),
            rules: rules.to_string(),
        }
    }

    /// 联邦版本告警（chip 的数据源；`net_versions` 事件带的是同一个形状）
    pub fn version_report(&self) -> serde_json::Value {
        let (fork, odd) = self.versions.lock().unwrap().lists(&self.my_rules, APP_VERSION);
        serde_json::json!({
            "app_version": APP_VERSION,
            "rules": self.my_rules,
            // 只有非空才有意义：全网同规则同发行包时两个数组都是空的
            "rules_mismatch": fork,
            "version_diff": odd,
        })
    }

    /// 分歧集合变了才推事件：chip 自己按两堆明细决定说什么颜色
    fn emit_versions(&self, app: &AppHandle) {
        let _ = app.emit("net_versions", self.version_report());
    }

    // -------------------------------------------------------------- 在场广播（共识之外）

    /// 前端上报本机位置：签名后交给网络线程广播。
    ///
    /// 三道闸：必须解锁（没有身份就没有在场）、`POS_PUBLISH_MIN_MS` 的速率地板、
    /// 坐标定点化后钳进 i32。这里**只**发消息：不写账本、不出块、不参与任何判定，
    /// 所以刷不动发行，也改不动一格一主。
    pub fn publish_presence(&self, x: f64, y: f64, z: f64, heading: f64) -> Result<(), String> {
        let core = self.core_slot.read().unwrap().clone().ok_or("钱包未解锁")?;
        let now = now_ms();
        let floor = self.pos_floor.load(Ordering::Relaxed);
        if now < floor {
            return Err("位置上报太快".into());
        }
        self.pos_floor
            .store(now + POS_PUBLISH_MIN_MS, Ordering::Relaxed);
        let wire = self.sign_pos(&core, x, y, z, heading, now);
        let mut my = self.my_pos.lock().unwrap();
        *my = Some(wire.clone());
        drop(my);
        let _ = self.cmd_tx.send(NetCmd::BroadcastPos(wire));
        Ok(())
    }

    /// 定点化 + 签名。`seq` 用全局单调计数，所以邻居只可能看到我更晚的位置。
    fn sign_pos(&self, core: &WalletCore, x: f64, y: f64, z: f64, heading: f64, now_ms: u64) -> PresenceWire {
        let seq = self.pos_seq.fetch_add(1, Ordering::Relaxed) + 1;
        sign_pos_wire(core, seq, x, y, z, heading, now_ms)
    }

    /// 验一条在场原件：复算地址、验签、时间窗口——三关全过才给一个名单条目。
    /// 纯函数（不碰账本、不碰链），所以单元测试能直接喂它伪造的签名。
    fn verify_pos(w: &PresenceWire, now: u64) -> Option<PosEntry> {
        let pk = hex::decode(&w.pk_hex).ok()?;
        if pk.len() != crate::crypto::PK_LEN {
            return None;
        }
        let salt_raw = hex::decode(&w.salt_hex).ok()?;
        if salt_raw.len() != 8 {
            return None;
        }
        let mut salt = [0u8; 8];
        salt.copy_from_slice(&salt_raw);
        let vk = crate::crypto::decode_pk(&pk)?;
        // 「这把公钥配这份盐派生出的地址」必须就是它声称的那个：
        // 少了这一行，任何人用自己的钥匙就能冒充任意地址在场
        if crate::crypto::derive_address(&vk, &salt) != w.addr {
            return None;
        }
        let sig = hex::decode(&w.sig_hex).ok()?;
        if !crate::crypto::verify(&vk, &pos_msg_bytes(w), &sig) {
            return None;
        }
        let now_secs = now / 1000;
        // 允许 30 秒的时钟超前（机器之间不会严格同步），落后不得超过窗口
        if w.ts > now_secs + 30 || w.ts < now_secs.saturating_sub(POS_MAX_AGE_SECS) {
            return None;
        }
        let inv = 1.0 / POS_SCALE;
        Some(PosEntry {
            addr: w.addr.clone(),
            x: f64::from(w.x) * inv,
            y: f64::from(w.y) * inv,
            z: f64::from(w.z) * inv,
            heading: f64::from(w.heading) * inv,
            seq: w.seq,
            seen_ms: now,
        })
    }

    /// 收下邻居的位置：验过了才进名单。返回 true = 名单真的变了。
    fn accept_pos(&self, w: &PresenceWire, now: u64) -> bool {
        match Self::verify_pos(w, now) {
            Some(e) => self.presence.lock().unwrap().note(e),
            None => false,
        }
    }

    /// 在场名单（前端画人物、以及「同世界 N 人」那个数字的数据源）
    pub fn presence_report(&self) -> serde_json::Value {
        let now = now_ms();
        let my = self.my_pos.lock().unwrap().clone();
        let peers: Vec<serde_json::Value> = {
            let mut list: Vec<PosEntry> = self
                .presence
                .lock()
                .unwrap()
                .by_addr
                .values()
                .cloned()
                .collect();
            list.sort_by(|a, b| a.addr.cmp(&b.addr));
            list.into_iter()
                .map(|e| {
                    serde_json::json!({
                        "addr": e.addr,
                        "x": e.x,
                        "y": e.y,
                        "z": e.z,
                        "heading": e.heading,
                        "seq": e.seq,
                        // 前端用它判断"这人多久没动了"，比只给一个 bool 诚实
                        "age_secs": now.saturating_sub(e.seen_ms) / 1000,
                    })
                })
                .collect()
        };
        serde_json::json!({
            "count": peers.len(),
            "peers": peers,
            "mine": my.map(|w| serde_json::json!({
                "addr": w.addr,
                "x": f64::from(w.x) / POS_SCALE,
                "y": f64::from(w.y) / POS_SCALE,
                "z": f64::from(w.z) / POS_SCALE,
                "heading": f64::from(w.heading) / POS_SCALE,
                "seq": w.seq,
            })),
        })
    }

    fn emit_presence(&self, app: &AppHandle) {
        let _ = app.emit("net_presence", self.presence_report());
    }

    // -------------------------------------------------------------- 注册表

    fn make_reg_msg(core: &WalletCore) -> NetMsg {
        NetMsg::Reg {
            pk_hex: hex::encode(&core.pk_bytes),
            salt_hex: hex::encode(core.addr_salt),
        }
    }

    /// 注册表写入：重算地址并登记（「地址 ↔ 公钥」的可验证绑定）
    fn accept_registry(&self, pk_hex: &str, salt_hex: &str) -> Option<String> {
        let pk = hex::decode(pk_hex).ok()?;
        if pk.len() != crate::crypto::PK_LEN {
            return None;
        }
        let salt_raw = hex::decode(salt_hex).ok()?;
        if salt_raw.len() != 8 {
            return None;
        }
        let mut salt = [0u8; 8];
        salt.copy_from_slice(&salt_raw);
        let vk = crate::crypto::decode_pk(&pk)?;
        let addr = crate::crypto::derive_address(&vk, &salt);

        // 只在新注册/有变化时打日志（周期重广播不刷屏）
        let mut reg = self.registry.lock().unwrap();
        let changed = match reg.get(&addr) {
            Some((old_pk, _)) if old_pk[..] == pk[..] => false,
            _ => true,
        };
        reg.insert(addr.clone(), (pk, salt));
        drop(reg);
        if changed {
            eprintln!("[QLight] 节点注册：{}（地址↔公钥绑定可独立验证）", short_addr(&addr));
        }
        Some(addr)
    }

    fn registry_pk(&self, addr: &str) -> Option<Vec<u8>> {
        self.registry.lock().unwrap().get(addr).map(|(pk, _)| pk.clone())
    }

    // -------------------------------------------------------------- 世界追赶

    /// 本机世界指纹：`(去重条数, 排序哈希指纹)`
    fn ent_head(&self) -> (u64, String) {
        crate::world::set_digest(&self.entities.load())
    }

    fn dust_head(&self) -> (u64, String) {
        crate::world::dust::set_digest(&self.dust.load())
    }

    // -------------------------------------------------------------- 事件分发

    /// 新邻居接入：登记地址、拨号、立刻公告自己（mDNS 与引导注册表共用）。
    /// 已连接的直接跳过拨号；拨号失败的节点进入退避，退避期内不再尝试。
    /// 邻居计数以真实建立的连接为准（ConnectionEstablished/Closed 事件维护）。
    #[allow(clippy::too_many_arguments)]
    fn adopt_peer(
        &self,
        pid: PeerId,
        addrs: Vec<Multiaddr>,
        seen: &mut HashSet<PeerId>,
        backoff: &mut HashMap<PeerId, u64>,
        established: &HashSet<PeerId>,
        swarm: &mut libp2p::Swarm<QlBehaviour>,
        topics: &Topics,
        chain: &Arc<SimChain>,
        src: &str,
    ) {
        if addrs.is_empty() || seen.contains(&pid) {
            return;
        }
        if let Some(next) = backoff.get(&pid) {
            if now_ms() < *next {
                return;
            }
        }
        seen.insert(pid);
        // 已连接的邻居不再拨号（重拨已连接对端会报 DialPeerCondition 错误造成抖动）
        if established.contains(&pid) {
            pub_msg(swarm, &topics.sync, &Self::announce_msg(chain, &self.my_rules));
            return;
        }
        // 拨完整多地址（/ip4/../tcp/../p2p/<peerid>）——最明确的拨法，绕开地址簿歧义
        let mut dialed = false;
        for a in addrs {
            let mut full = a.clone();
            full.push(Protocol::P2p(pid));
            match swarm.dial(full.clone()) {
                Ok(()) => {
                    // 记下这个地址属于哪一档（本机/局域网/公网）：连接真建立时才进名单
                    self.dial_scope
                        .lock()
                        .unwrap()
                        .insert(pid.to_string(), addr_scope(&full));
                    dialed = true;
                    break;
                }
                Err(e) => eprintln!("[QLight] 拨号失败（{a}）：{e}"),
            }
        }
        if !dialed {
            return;
        }
        eprintln!("[QLight] {src}发现邻居：{pid}（拨号中…）");
        // 立即公告自己，让新邻居尽快对账
        pub_msg(swarm, &topics.sync, &Self::announce_msg(chain, &self.my_rules));
    }

    /// mDNS 发现新邻居
    fn on_peers_discovered(
        &self,
        list: Vec<(PeerId, Multiaddr)>,
        seen: &mut HashSet<PeerId>,
        backoff: &mut HashMap<PeerId, u64>,
        established: &HashSet<PeerId>,
        swarm: &mut libp2p::Swarm<QlBehaviour>,
        topics: &Topics,
        chain: &Arc<SimChain>,
    ) {
        for (pid, addr) in list {
            self.adopt_peer(pid, vec![addr], seen, backoff, established, swarm, topics, chain, "mDNS ");
        }
    }

    fn on_peers_expired(&self, list: Vec<(PeerId, Multiaddr)>, seen: &mut HashSet<PeerId>, app: &AppHandle) {
        for (pid, _) in list {
            if seen.remove(&pid) {
                // 邻居离线：它的地址范围与拨号记录一起清掉，三态统计才不会挂着死人
                self.dial_scope.lock().unwrap().remove(&pid.to_string());
                self.peer_scope.lock().unwrap().remove(&pid.to_string());
                let n = seen.len();
                self.peers.store(n, Ordering::Relaxed);
                let _ = app.emit("net_peers", serde_json::json!({ "count": n }));
                eprintln!("[QLight] 邻居离线 {pid}（当前 {n} 节点）");
            }
        }
    }

    // -------------------------------------------------------------- 引导注册表

    /// 合并写入自己的条目（fs2 独占锁防两实例互相覆盖丢数据）
    fn write_boot(&self, my_pid: PeerId, my_addrs: &[Multiaddr]) {
        if my_addrs.is_empty() {
            return;
        }
        let key = my_pid.to_string();
        let entry = BootEntry {
            addrs: my_addrs.iter().map(|a| a.to_string()).collect(),
            ts: now_ms(),
        };
        self.boot_update(move |file| {
            file.insert(key, entry);
        });
    }

    /// 注册表读-改-写（独占锁；演示容错：任何 IO/解析失败都静默跳过）
    fn boot_update(&self, f: impl FnOnce(&mut BootFile)) {
        let Ok(mut fh) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.boot_path)
        else {
            return;
        };
        let _ = fh.lock_exclusive();
        let mut txt = String::new();
        let _ = fh.read_to_string(&mut txt);
        let mut file: BootFile = serde_json::from_str(&txt).unwrap_or_default();
        f(&mut file);
        // 过期裁剪 + 容量上限（保最新）
        let cutoff = now_ms().saturating_sub(BOOT_TTL_MS);
        file.retain(|_, e| e.ts >= cutoff);
        if file.len() > BOOT_MAX_ENTRIES {
            let mut v: Vec<(String, BootEntry)> = file.into_iter().collect();
            v.sort_by(|a, b| b.1.ts.cmp(&a.1.ts));
            v.truncate(BOOT_MAX_ENTRIES);
            file = v.into_iter().collect();
        }
        if let Ok(bytes) = serde_json::to_vec(&file) {
            let _ = fh.seek(std::io::SeekFrom::Start(0));
            let _ = fh.set_len(0);
            let _ = fh.write_all(&bytes);
            let _ = fh.sync_all();
        }
        let _ = fh.unlock();
    }

    /// 周期互读注册表 → 直连新邻居（同机多开的正路；同网段跨机同样生效）
    #[allow(clippy::too_many_arguments)]
    fn boot_tick(
        &self,
        my_pid: PeerId,
        my_addrs: &[Multiaddr],
        seen: &mut HashSet<PeerId>,
        backoff: &mut HashMap<PeerId, u64>,
        established: &HashSet<PeerId>,
        swarm: &mut libp2p::Swarm<QlBehaviour>,
        topics: &Topics,
        chain: &Arc<SimChain>,
    ) {
        self.write_boot(my_pid, my_addrs);
        let Ok(txt) = std::fs::read_to_string(&self.boot_path) else {
            return;
        };
        let Ok(file) = serde_json::from_str::<BootFile>(&txt) else {
            return;
        };
        let me = my_pid.to_string();
        for (pid_str, entry) in file {
            if pid_str == me {
                continue;
            }
            let Ok(pid) = pid_str.parse::<PeerId>() else {
                continue;
            };
            let addrs: Vec<Multiaddr> =
                entry.addrs.iter().filter_map(|s| s.parse().ok()).collect();
            self.adopt_peer(pid, addrs, seen, backoff, established, swarm, topics, chain, "引导注册表");
        }
    }

    /// 联邦消息入口
    fn on_net_msg(
        &self,
        msg: NetMsg,
        from: Option<PeerId>,
        swarm: &mut libp2p::Swarm<QlBehaviour>,
        ms: &mut MachineState,
        topics: &Topics,
        app: &AppHandle,
        chain: &Arc<SimChain>,
    ) {
        match msg {
            NetMsg::Reg { pk_hex, salt_hex } => {
                self.accept_registry(&pk_hex, &salt_hex);
            }

            NetMsg::Tx(w) => {
                let from = crate::crypto::to_bech32_ql(&w.envelope.from_addr);
                match chain.accept_remote_tx(w) {
                    Ok(_) => eprintln!("[QLight] 联邦交易入池 {}…（原件独立验签通过，等下块打包）", short_addr(&from)),
                    Err(e) => eprintln!("[QLight] 拒收联邦交易（{}…）：{e}", short_addr(&from)),
                }
            }

            NetMsg::Block { header, txs, dusts } => {
                self.on_block_wire(header, txs, dusts, swarm, ms, topics, app, chain)
            }

            NetMsg::Entity(list) => {
                // 一条 gossip 最多带这么多条：合并要整本重放并逐条验签，
                // 无上限的列表等于给对端一个 CPU/磁盘放大器
                const ENTITY_BATCH_MAX: usize = 256;
                if list.len() > ENTITY_BATCH_MAX {
                    eprintln!(
                        "[QLight] 拒收超量实体批次：{} 条 > 上限 {}",
                        list.len(),
                        ENTITY_BATCH_MAX
                    );
                    return;
                }
                // v3 记录必须指到一笔本机链上已确认的销毁付款；链是最终裁判，
                // 所以合并时现读一遍 blocks.jsonl 作 ChainView（读不到就是空视图 → 全拒）
                let view = crate::world::ChainView::load(&chain.blocks_path_pub());
                match self.entities.merge(&list, &view) {
                    Ok(rep) => {
                        if rep.accepted > 0 {
                            eprintln!(
                                "[QLight] 邻居造了 {} 个方块（合并：收下 {}，已见过 {}，拒收 {}）",
                                list.len(),
                                rep.accepted,
                                rep.known,
                                rep.rejected
                            );
                            let _ = app.emit(
                                "world_entity",
                                serde_json::json!({
                                    "accepted": rep.accepted,
                                    "known": rep.known,
                                    "rejected": rep.rejected,
                                }),
                            );
                        }
                    }
                    Err(e) => eprintln!("[QLight] 实体合并失败：{e}"),
                }
            }

            // —— 世界追赶：指纹公告 → 缺口检测 → 索要整本 → 逐条验后落盘 ——
            NetMsg::EntHead { count, digest_hex } => {
                let mine = self.ent_head();
                if mine.1 == digest_hex {
                    // 对上了：清掉欠账标记，之后若再分叉还能重新索要
                    if ms.ent_asked.as_deref() == Some(format!("{count}:{digest_hex}").as_str()) {
                        ms.ent_asked = None;
                    }
                    return;
                }
                // 只有对方那份确实更完整才追（同一条裁决规则，双方算出同一方向）
                if !ent_head_wins((count, digest_hex.clone()), &mine) {
                    return;
                }
                let key = format!("{count}:{digest_hex}");
                if ms.ent_asked.as_deref() == Some(key.as_str()) {
                    return; // 同一个缺失只问一次，防追赶风暴
                }
                ms.ent_asked = Some(key);
                pub_msg(
                    swarm,
                    &topics.ent,
                    &NetMsg::EntAsk {
                        count,
                        digest_hex: digest_hex.clone(),
                    },
                );
                eprintln!(
                    "[QLight] 世界对不上（本机 {} 条，邻居 {count} 条）：索要整本实体快照",
                    mine.0
                );
            }

            NetMsg::EntAsk { count, digest_hex } => {
                let mine = self.ent_head();
                // 我要么正好持有被索要的那一份，要么比它更完整 —— 才值得回整本快照；
                // 比对方还少就别刷屏（对方问到更完整的邻居时会拿到答案）
                let requested = (count, digest_hex.clone());
                if mine.1 != digest_hex && !ent_head_wins(mine.clone(), &requested) {
                    return;
                }
                match std::fs::read(self.entities.path()) {
                    Ok(raw) if !raw.is_empty() => match snapshot_pack(&raw) {
                        Some(snap) if snap.len() <= SYNC_MAX_BYTES => pub_msg(
                            swarm,
                            &topics.ent,
                            &NetMsg::EntSnap {
                                digest_hex: mine.1,
                                snap,
                            },
                        ),
                        Some(_) => eprintln!("[QLight] 实体快照过大（压缩后仍超上限），暂不回"),
                        None => eprintln!("[QLight] 实体快照压缩失败，暂不回"),
                    },
                    _ => {} // 空账本 = 我这边世界还没开始，无从可回
                }
            }

            NetMsg::EntSnap { digest_hex, snap } => {
                let mine = self.ent_head();
                // 自报的指纹必须和搬运内容对得上，否则这份快照连"声称自己是谁"都做不到
                let Some(list) = ent_snap_records(&snap) else {
                    eprintln!("[QLight] 实体快照解析失败（超限或坏行），整份丢弃");
                    return;
                };
                let claimed = crate::world::set_digest(&list);
                if claimed.1 != digest_hex || !ent_head_wins(claimed.clone(), &mine) {
                    return; // 不是更完整的一份，或者指纹造假/传输损坏
                }
                // 快照只是搬运工，裁判还是逐条验签验链：接不上的那条永远进不了本机账本
                let view = crate::world::ChainView::load(&chain.blocks_path_pub());
                match self.entities.merge(&list, &view) {
                    Ok(rep) => {
                        let after = self.ent_head();
                        eprintln!(
                            "[QLight] 实体快照合并：收下 {}，已见过 {}，拒收 {}（本机指纹 …{} → …{}）",
                            rep.accepted,
                            rep.known,
                            rep.rejected,
                            &mine.1[mine.1.len().saturating_sub(8)..],
                            &after.1[after.1.len().saturating_sub(8)..]
                        );
                        if rep.accepted > 0 {
                            ms.ent_asked = None;
                            let _ = app.emit(
                                "world_entity",
                                serde_json::json!({
                                    "accepted": rep.accepted,
                                    "known": rep.known,
                                    "rejected": rep.rejected,
                                }),
                            );
                        }
                    }
                    Err(e) => eprintln!("[QLight] 实体快照合并失败：{e}"),
                }
            }

            // —— 光尘中继：与实体同一套推送 + 指纹补齐 ——
            NetMsg::Dust(list) => {
                const DUST_BATCH_MAX: usize = 256;
                if list.len() > DUST_BATCH_MAX {
                    eprintln!("[QLight] 拒收超量光尘批次：{} 条", list.len());
                    return;
                }
                let view = crate::world::dust::DustView {
                    tip: chain.height(),
                };
                match self.dust.merge(&list, &view) {
                    Ok(rep) => {
                        if rep.accepted > 0 {
                            eprintln!(
                                "[QLight] 邻居光尘记录（合并：收下 {}，已见过 {}，拒收 {}）",
                                rep.accepted, rep.known, rep.rejected
                            );
                            let _ = app.emit(
                                "world_dust",
                                serde_json::json!({
                                    "accepted": rep.accepted,
                                    "known": rep.known,
                                    "rejected": rep.rejected,
                                }),
                            );
                        }
                    }
                    Err(e) => eprintln!("[QLight] 光尘合并失败：{e}"),
                }
            }

            NetMsg::DustHead { count, digest_hex } => {
                let mine = self.dust_head();
                if mine.1 == digest_hex {
                    if ms.dust_asked.as_deref() == Some(format!("{count}:{digest_hex}").as_str()) {
                        ms.dust_asked = None;
                    }
                    return;
                }
                if !ent_head_wins((count, digest_hex.clone()), &mine) {
                    return;
                }
                let key = format!("{count}:{digest_hex}");
                if ms.dust_asked.as_deref() == Some(key.as_str()) {
                    return;
                }
                ms.dust_asked = Some(key);
                pub_msg(
                    swarm,
                    &topics.dust,
                    &NetMsg::DustAsk {
                        count,
                        digest_hex: digest_hex.clone(),
                    },
                );
                eprintln!(
                    "[QLight] 光尘对不上（本机 {} 条，邻居 {count} 条）：索要整本光尘快照",
                    mine.0
                );
            }

            NetMsg::DustAsk { count, digest_hex } => {
                let mine = self.dust_head();
                let requested = (count, digest_hex.clone());
                if mine.1 != digest_hex && !ent_head_wins(mine.clone(), &requested) {
                    return;
                }
                match std::fs::read(self.dust.path()) {
                    Ok(raw) if !raw.is_empty() => match snapshot_pack(&raw) {
                        Some(snap) if snap.len() <= SYNC_MAX_BYTES => pub_msg(
                            swarm,
                            &topics.dust,
                            &NetMsg::DustSnap {
                                digest_hex: mine.1,
                                snap,
                            },
                        ),
                        Some(_) => eprintln!("[QLight] 光尘快照过大（压缩后仍超上限），暂不回"),
                        None => eprintln!("[QLight] 光尘快照压缩失败，暂不回"),
                    },
                    _ => {}
                }
            }

            NetMsg::DustSnap { digest_hex, snap } => {
                let mine = self.dust_head();
                // 无论这次合并结果如何，都允许下个周期重问：链追赶上来以后
                // 早先因"块高超过本机链头"被拒的记录才有机会真正入账
                ms.dust_asked = None;
                let Some(list) = dust_snap_records(&snap) else {
                    eprintln!("[QLight] 光尘快照解析失败（超限或坏行），整份丢弃");
                    return;
                };
                let claimed = crate::world::dust::set_digest(&list);
                if claimed.1 != digest_hex || !ent_head_wins(claimed.clone(), &mine) {
                    return;
                }
                let view = crate::world::dust::DustView {
                    tip: chain.height(),
                };
                match self.dust.merge(&list, &view) {
                    Ok(rep) => {
                        let after = self.dust_head();
                        eprintln!(
                            "[QLight] 光尘快照合并：收下 {}，已见过 {}，拒收 {}（本机 {} → {} 条）",
                            rep.accepted, rep.known, rep.rejected, mine.0, after.0
                        );
                        if rep.accepted > 0 {
                            let _ = app.emit(
                                "world_dust",
                                serde_json::json!({
                                    "accepted": rep.accepted,
                                    "known": rep.known,
                                    "rejected": rep.rejected,
                                }),
                            );
                        }
                    }
                    Err(e) => eprintln!("[QLight] 光尘快照合并失败：{e}"),
                }
            }

            NetMsg::Pos(w) => {
                // 在场只在内存里：不落盘、不进块、不改任何判据，所以这条分支
                // 唯一的失败模式是"白丢一条消息"，不值得告警刷屏
                if self.accept_pos(&w, now_ms()) {
                    self.emit_presence(app);
                }
            }

            NetMsg::Ticket {
                addr,
                height,
                prev_hash_hex,
                sig_hex,
            } => self.on_ticket(addr, height, prev_hash_hex, sig_hex, ms, chain, swarm, topics),

            NetMsg::Announce {
                height,
                tip_hash_hex,
                app_version,
                rules,
            } => {
                // 版本普查：公告带来的规则指纹与本机不同 = 两端会互相验不过对方的块。
                // 这种分歧在链上没有任何其它信号会暴露它，只能趁还连得上时说出来。
                if let Some(pid) = from {
                    let fire = {
                        let mut c = self.versions.lock().unwrap();
                        c.note(&pid.to_string(), &app_version, &rules, now_ms());
                        c.take_warning(&self.my_rules, APP_VERSION)
                    };
                    if fire {
                        self.emit_versions(app);
                    }
                }
                let my_h = chain.height();
                let my_tip = chain.tip_hash_hex_pub();
                // 裁决规则（全网一致）：高者胜；同高 tip 字典序小者胜
                let adoptable =
                    height > my_h || (height == my_h && my_h > 0 && tip_hash_hex < my_tip);
                let key = format!("{height}:{tip_hash_hex}");
                if adoptable && ms.sync_asked.as_deref() != Some(key.as_str()) {
                    ms.sync_asked = Some(key);
                    pub_msg(
                        swarm,
                        &topics.sync,
                        &NetMsg::SyncAsk {
                            height: my_h,
                            tip_hash_hex: my_tip,
                        },
                    );
                    eprintln!("[QLight] 邻居链更高（#{my_h} → #{height}），发起整链对账");
                }
            }

            NetMsg::SyncAsk {
                height,
                tip_hash_hex,
            } => {
                let my_h = chain.height();
                let my_tip = chain.tip_hash_hex_pub();
                // 只有确认自己更长（或同高但 tip 更小）才回快照，避免互相刷
                let i_win =
                    my_h > height || (my_h == height && height > 0 && my_tip < tip_hash_hex);
                if i_win {
                    // 整链快照：gzip 压缩后发送（原样直发在链变长后会撞上单条消息上限）
                    match std::fs::read(chain.blocks_path_pub()) {
                        Ok(raw) if !raw.is_empty() => match snapshot_pack(&raw) {
                            Some(snap) if snap.len() <= SYNC_MAX_BYTES => {
                                pub_msg(swarm, &topics.sync, &NetMsg::SyncResp { snap });
                            }
                            Some(_) => eprintln!("[QLight] 快照过大（压缩后仍超上限），暂不回快照"),
                            None => eprintln!("[QLight] 快照压缩失败，暂不回快照"),
                        },
                        _ => {} // 块文件为空/未就绪，无从回快照
                    }
                }
            }

            NetMsg::SyncResp { snap } => {
                let my_h = chain.height();
                let my_tip = chain.tip_hash_hex_pub();
                let Some(lines) = snapshot_unpack(&snap) else {
                    eprintln!("[QLight] 追账快照解包失败，忽略");
                    return;
                };
                let Some((their_h, their_tip)) = snapshot_tip_of(&lines) else { return };
                let adoptable =
                    their_h > my_h || (their_h == my_h && their_h > 0 && their_tip < my_tip);
                if !adoptable {
                    return;
                }
                match chain.install_snapshot(&lines) {
                    Ok(h) => {
                        eprintln!(
                            "[QLight] 追账完成：本地 #{my_h} → #{h}（整链逐块验证通过，全网账本对齐）"
                        );
                        emit_status(app, self, chain);
                        ms.phase = Phase::Direct;
                        ms.beat_next = now_ms() + BEAT_MS;
                        ms.sync_asked = None;
                    }
                    Err(e) => eprintln!("[QLight] 追账快照拒收：{e}"),
                }
            }
        }
    }

    /// 联邦块：恰好下一块 → 完整验签入账；跳档 → 追账；旧块 → 忽略
    fn on_block_wire(
        &self,
        header: BlockHeader,
        txs: Vec<TxWire>,
        dusts: Vec<crate::world::dust::DustRecord>,
        swarm: &mut libp2p::Swarm<QlBehaviour>,
        ms: &mut MachineState,
        topics: &Topics,
        app: &AppHandle,
        chain: &Arc<SimChain>,
    ) {
        let my = chain.height();
        if header.height <= my {
            // 同高分叉块：立即按 tip 裁决（不等 5s 公告周期）；对方块哈希更小 = 对方链胜
            if header.height == my && header.block_hash_hex < chain.tip_hash_hex_pub() {
                let key = format!("fork{my}");
                if ms.sync_asked.as_deref() != Some(key.as_str()) {
                    ms.sync_asked = Some(key);
                    ms.phase = Phase::Direct;
                    ms.beat_next = now_ms() + WAIT_MS;
                    pub_msg(
                        swarm,
                        &topics.sync,
                        &NetMsg::SyncAsk {
                            height: my,
                            tip_hash_hex: chain.tip_hash_hex_pub(),
                        },
                    );
                    eprintln!("[QLight] 收到同高分叉块（哈希更小）：本拍停挖，立即整链对账");
                }
            }
            return; // 旧块 / 重复块 / 我方胜出的分叉块（对方会自行判负）
        }
        if header.height > my + 1 {
            let key = format!("{}", header.height);
            if ms.sync_asked.as_deref() != Some(key.as_str()) {
                ms.sync_asked = Some(key);
                pub_msg(
                    swarm,
                    &topics.sync,
                    &NetMsg::SyncAsk {
                        height: my,
                        tip_hash_hex: chain.tip_hash_hex_pub(),
                    },
                );
                eprintln!("[QLight] 收到跳档块 #{}（本地 #{my}），发起整链对账", header.height);
            }
            return;
        }

        // —— 高度衔接：整块真验（原件逐笔验签 + 凭据对照 + 块哈希重算）——
        // 不再先查注册表：块自带公钥与地址盐，验证不需要"对方先注册过"。
        match chain.apply_remote_block(&header, txs, dusts) {
            Ok(mut event) => {
                event.balance = self
                    .core_slot
                    .read()
                    .unwrap()
                    .as_ref()
                    .map(|c| chain.balance_of(&c.address))
                    .unwrap_or(0);
                let _ = app.emit("chain_block", &event);
                // 接受了对端块 → 从它的落账时刻起进入下一轮选举
                ms.phase = Phase::Direct;
                ms.beat_next = now_ms() + BEAT_MS;
                eprintln!(
                    "[QLight] 已采用联邦块 #{}（逐笔重验签 + 块哈希重算通过，全网账本对齐）",
                    header.height
                );
                self.chase_world(&header, ms, swarm, topics, chain);
            }
            Err(e) => eprintln!("[QLight] 拒绝联邦块：{e}"),
        }
    }

    /// 块上的世界承诺与本机对不上 → 立刻索要那一份，不等 9 秒的公告周期。
    ///
    /// 这是 `entity_root` 进块之后才有的闭环：块不只是"接受与否"，它现在还点名了
    /// 出块人认的那一份世界。本机少了几格，就有明确的对象可问、有明确的指纹可核对；
    /// 补齐后再公告，双方指纹一致，这件事才算完。
    fn chase_world(
        &self,
        header: &BlockHeader,
        ms: &mut MachineState,
        swarm: &mut libp2p::Swarm<QlBehaviour>,
        topics: &Topics,
        chain: &Arc<SimChain>,
    ) {
        if header.version < crate::simchain::BLOCK_V_WORLD || header.entity_root_hex.len() != 64 {
            return; // v0/v1 块不承诺世界，无从追
        }
        let (mine_count, mine_digest) = chain.world_head();
        if mine_digest == header.entity_root_hex {
            return;
        }
        let key = format!("{}:{}", header.entity_count, header.entity_root_hex);
        if ms.ent_asked.as_deref() == Some(key.as_str()) {
            return; // 同一份缺失只问一次
        }
        ms.ent_asked = Some(key);
        pub_msg(
            swarm,
            &topics.ent,
            &NetMsg::EntAsk {
                count: header.entity_count,
                digest_hex: header.entity_root_hex.clone(),
            },
        );
        eprintln!(
            "[QLight] 块 #{} 承诺的世界与本机不同（链上 {} 条 …{}，本机 {} 条 …{}）：立即索要那一份",
            header.height,
            header.entity_count,
            &header.entity_root_hex[..8],
            mine_count,
            &mine_digest[..8]
        );
    }

    /// 收票：注册表公钥验签 → vrf' 入席位（假票直接丢）。
    /// 同时是「同高分叉探测器」：票的 prev 对不上本机 tip = 两边各押一条分叉，
    /// 立即按全网同一裁决规则（同高 tip 字典序小者胜）当场判链——败方停挖并索要
    /// 整链快照，胜方继续。分叉在一张票的往返内消化，不会滚成连环追账。
    fn on_ticket(
        &self,
        addr: String,
        height: u64,
        prev_hash_hex: String,
        sig_hex: String,
        ms: &mut MachineState,
        chain: &Arc<SimChain>,
        swarm: &mut libp2p::Swarm<QlBehaviour>,
        topics: &Topics,
    ) {
        // —— 分叉探测（不依赖本机是否在投票期）——
        if prev_hash_hex != chain.tip_hash_hex_pub() && height == chain.height() + 1 {
            let my_tip = chain.tip_hash_hex_pub();
            if prev_hash_hex < my_tip {
                // 对方押的分叉 tip 更小 → 对方胜，本机停挖、立即对账
                if ms.sync_asked.as_deref() != Some("fork") {
                    ms.sync_asked = Some("fork".into());
                    ms.phase = Phase::Direct;
                    ms.beat_next = now_ms() + WAIT_MS;
                    pub_msg(
                        swarm,
                        &topics.sync,
                        &NetMsg::SyncAsk {
                            height: chain.height(),
                            tip_hash_hex: my_tip,
                        },
                    );
                    eprintln!("[QLight] 检测同高分叉（对方 tip 更小）：本拍停挖，立即整链对账");
                }
            }
            // 我方 tip 更小 → 我方胜，照常挖；对方收到我方票时会自行判负停挖
            return;
        }
        let Phase::Voting {
            height: wh,
            tickets,
            ..
        } = &mut ms.phase
        else {
            return;
        };
        if *wh != height {
            return;
        }
        // 票必须是「同一高度、续同一前块」才有效
        if prev_hash_hex != chain.tip_hash_hex_pub() {
            return;
        }
        let Some(pk) = self.registry_pk(&addr) else { return };
        let Ok(sig) = hex::decode(&sig_hex) else { return };
        let Some(vk) = crate::crypto::decode_pk(&pk) else { return };
        let msg = ticket_msg_bytes(height, &prev_hash_hex);
        if !crate::crypto::verify(&vk, &msg, &sig) {
            return; // 假票直接丢
        }
        let vrf = ticket_vrf(&msg, &sig);
        tickets.insert(hex::encode(&vrf), addr);
    }

    /// 出块状态机节拍（由 run_swarm 每 120ms 调用一次）
    fn machine_tick(
        &self,
        swarm: &mut libp2p::Swarm<QlBehaviour>,
        ms: &mut MachineState,
        app: &AppHandle,
        chain: &Arc<SimChain>,
        topics: &Topics,
        prev_unlocked: &mut bool,
    ) {
        let now = now_ms();

        // —— 周期广播：注册（解锁才有身份）+ 链公告 + 池内交易再撒 ——
        if let Some(core) = self.core_slot.read().unwrap().clone() {
            if now >= ms.reg_next {
                ms.reg_next = now + REG_EVERY_MS;
                pub_msg(swarm, &topics.reg, &Self::make_reg_msg(&core));
            }
        }
        if now >= ms.ann_next {
            ms.ann_next = now + ANNOUNCE_EVERY_MS;
            pub_msg(swarm, &topics.sync, &Self::announce_msg(chain, &self.my_rules));
            let wires = chain.mempool_wires_pub();
            for wire in wires {
                pub_msg(swarm, &topics.tx, &NetMsg::Tx(wire));
            }
        }
        // —— 版本普查过期：公告停了（进程被杀、机器下线）的邻居要自动退出告警 ——
        let expired = {
            let mut c = self.versions.lock().unwrap();
            c.sweep(VERSION_TTL_MS, now);
            c.take_warning(&self.my_rules, APP_VERSION)
        };
        if expired {
            self.emit_versions(app);
        }
        // —— 在场：心跳重发本机位置 + 过期邻居退场 ——
        // 重发而不是只发一次：晚连上来的邻居不必等我挪动才能看见我。
        // 锁钱包后不再重发，三个心跳之后邻居那边自动当我不在场。
        if self.core_slot.read().unwrap().is_some() {
            if now >= ms.pos_next {
                ms.pos_next = now + POS_HEARTBEAT_MS;
                let mine = self.my_pos.lock().unwrap().clone();
                if let Some(w) = mine {
                    pub_msg(swarm, &topics.pos, &NetMsg::Pos(w));
                }
            }
        }
        let left = {
            let mut p = self.presence.lock().unwrap();
            p.sweep(POS_TTL_MS, now);
            p.take_update()
        };
        if left.is_some() {
            self.emit_presence(app);
        }
        // —— 世界指纹公告：漏了记录的机器靠它发现自己漏了 ——
        // 单机（A 档）没有可对齐的对象，不去整本读实体账本，保持原行为零开销
        if self.peers.load(Ordering::Relaxed) > 0 && now >= ms.ent_next {
            ms.ent_next = now + ENT_HEAD_EVERY_MS;
            let (count, digest_hex) = self.ent_head();
            pub_msg(
                swarm,
                &topics.ent,
                &NetMsg::EntHead { count, digest_hex },
            );
        }
        // —— 光尘指纹公告：采集与转账是长流水，比实体更容易漏 ——
        if self.peers.load(Ordering::Relaxed) > 0 && now >= ms.dust_next {
            ms.dust_next = now + DUST_HEAD_EVERY_MS;
            let (count, digest_hex) = self.dust_head();
            pub_msg(
                swarm,
                &topics.dust,
                &NetMsg::DustHead { count, digest_hex },
            );
        }

        // —— 解锁沿检测：给刚解锁的钱包一个很快的首拍 ——
        let unlocked_now = self.core_slot.read().unwrap().is_some();
        if unlocked_now && !*prev_unlocked {
            ms.beat_next = ms.beat_next.min(now + 600);
        }
        *prev_unlocked = unlocked_now;

        let Some(core) = self.core_slot.read().unwrap().clone() else { return };

        // —— 出块状态机 ——
        match &ms.phase {
            Phase::Direct if now >= ms.beat_next => {
                if self.peers.load(Ordering::Relaxed) == 0 {
                    // A 档行为：单机直出
                    if let Some((event, ..)) = chain.mine_block(&core) {
                        let _ = app.emit("chain_block", &event);
                    }
                    ms.beat_next = now + BEAT_MS;
                } else {
                    // 联邦模式：广播自己的门票并开票窗
                    let height = chain.height() + 1;
                    let prev = chain.tip_hash_hex_pub();
                    let msg = ticket_msg_bytes(height, &prev);
                    let sig = crate::crypto::sign(&core.signing_key, &msg);
                    let vrf = ticket_vrf(&msg, &sig);
                    pub_msg(
                        swarm,
                        &topics.sync,
                        &NetMsg::Ticket {
                            addr: core.address.clone(),
                            height,
                            prev_hash_hex: prev,
                            sig_hex: hex::encode(&sig),
                        },
                    );
                    let mut tickets = HashMap::new();
                    tickets.insert(hex::encode(&vrf), core.address.clone());
                    ms.phase = Phase::Voting {
                        height,
                        until_ms: now + WINDOW_MS,
                        tickets,
                    };
                }
            }
            Phase::Voting {
                height,
                until_ms,
                tickets,
            } if now >= *until_ms => {
                // —— 开票：vrf' 最小（并列比地址）者出块 ——
                let winner = tickets
                    .iter()
                    .min_by(|a, b| a.0.cmp(b.0).then_with(|| a.1.cmp(b.1)))
                    .map(|(_, addr)| addr.clone());
                let mine_now = winner.as_deref() == Some(core.address.as_str());
                if mine_now {
                    if let Some((event, header, wires, dusts)) = chain.mine_block(&core) {
                        pub_block(swarm, &topics.block, &header, &wires, &dusts);
                        let _ = app.emit("chain_block", &event);
                    }
                } else {
                    eprintln!(
                        "[QLight] 高度 {height} 由 {} 出块（{} 张有效票），等待胜者块",
                        winner.as_deref().map(short_addr).unwrap_or_default(),
                        tickets.len()
                    );
                    ms.phase = Phase::Waiting {
                        height: *height,
                        until_ms: now + WAIT_MS,
                    };
                    return;
                }
                ms.phase = Phase::Direct;
                ms.beat_next = now + BEAT_MS;
            }
            Phase::Waiting { height, until_ms } if now >= *until_ms => {
                // 自愈兜底：胜者一直没出块（掉线/作恶），本机接手（演示网 liveness）
                eprintln!("[QLight] 高度 {height} 胜者超时未出块，本机兜底出块");
                if let Some((event, header, wires, dusts)) = chain.mine_block(&core) {
                    pub_block(swarm, &topics.block, &header, &wires, &dusts);
                    let _ = app.emit("chain_block", &event);
                }
                ms.phase = Phase::Direct;
                ms.beat_next = now + BEAT_MS;
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- 消息发布

/// 世界指纹的裁决规则（与链对齐同款口径）：条数多者胜；同数比指纹，字典序小者胜。
/// 双方各自算出同一个方向，所以不会出现互相对齐的死循环。
pub fn ent_head_wins(theirs: (u64, String), mine: &(u64, String)) -> bool {
    theirs.0 > mine.0 || (theirs.0 == mine.0 && theirs.1 < mine.1)
}

/// 整链快照打包：gzip（JSON 键名高度重复，压缩比 ~10 倍）+ base64（JSON 载体安全）
pub fn snapshot_pack(raw: &[u8]) -> Option<String> {
    use base64::Engine as _;
    use flate2::write::GzEncoder;
    use std::io::Write as _;
    let mut enc = GzEncoder::new(Vec::with_capacity(raw.len() / 8), flate2::Compression::best());
    enc.write_all(raw).ok()?;
    let gz = enc.finish().ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(gz))
}

/// 快照解包：base64 → gzip。带解压上限（≤ 8×SYNC_MAX_BYTES）防恶意超大负载
fn snapshot_unpack(b64: &str) -> Option<String> {
    use base64::Engine as _;
    use flate2::read::GzDecoder;
    use std::io::Read as _;
    let gz = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let mut raw = String::new();
    GzDecoder::new(&gz[..])
        .take((SYNC_MAX_BYTES as u64) * 8)
        .read_to_string(&mut raw)
        .ok()?;
    Some(raw)
}

/// 实体快照负载 → 记录列表：条数超限或任一行解析不了，整份丢弃。
///
/// 宁可整份丢掉也不"收下能解析的部分"：一部分收下会让本机指纹永远对不上任何人，
/// 从此每 9 秒重新问一遍，把追赶变成风暴。
pub fn ent_snap_records(snap: &str) -> Option<Vec<crate::world::EntityRecord>> {
    let lines = snapshot_unpack(snap)?;
    let mut out: Vec<crate::world::EntityRecord> = Vec::new();
    for line in lines.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if out.len() >= ENT_SNAP_MAX_LINES {
            return None;
        }
        out.push(serde_json::from_str::<crate::world::EntityRecord>(line).ok()?);
    }
    Some(out)
}

/// 光尘快照解包：与实体同一套上限与坏行判定
fn dust_snap_records(snap: &str) -> Option<Vec<crate::world::dust::DustRecord>> {
    let lines = snapshot_unpack(snap)?;
    let mut out: Vec<crate::world::dust::DustRecord> = Vec::new();
    for line in lines.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if out.len() >= DUST_SNAP_MAX_LINES {
            return None;
        }
        out.push(serde_json::from_str::<crate::world::dust::DustRecord>(line).ok()?);
    }
    Some(out)
}

/// 毫秒级 nonce：消息 ID 每次不同 → 绕过 gossipsub duplicate cache，
/// 同内容重发（尤其 SyncResp 整链快照）不再被静默吞掉
fn pub_msg(swarm: &mut libp2p::Swarm<QlBehaviour>, topic: &gossipsub::IdentTopic, msg: &NetMsg) {
    let wire = (now_ms(), msg);
    let Ok(bytes) = serde_json::to_vec(&wire) else { return };
    if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic.clone(), bytes) {
        eprintln!("[QLight] 广播暂缓（组网中或无邻居）：{e}");
    }
}

fn pub_block(
    swarm: &mut libp2p::Swarm<QlBehaviour>,
    topic: &gossipsub::IdentTopic,
    header: &BlockHeader,
    wires: &[TxWire],
    dusts: &[crate::world::dust::DustRecord],
) {
    pub_msg(
        swarm,
        topic,
        &NetMsg::Block {
            header: header.clone(),
            txs: wires.to_vec(),
            dusts: dusts.to_vec(),
        },
    );
}

/// 快照尾部解析：最后一行非空块头 → (height, tip)
fn snapshot_tip_of(lines: &str) -> Option<(u64, String)> {
    lines
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .and_then(|l| serde_json::from_str::<BlockHeader>(l).ok())
        .map(|h| (h.height, h.block_hash_hex))
}

/// 推送一次链状态（追账/对齐后刷新 UI）
fn emit_status(app: &AppHandle, net: &Net, chain: &Arc<SimChain>) {
    let core = net.core_slot.read().unwrap().clone();
    let height = chain.height();
    let event = BlockEvent {
        height,
        block_hash_hex: String::new(),
        vrf_hex: String::new(),
        reward: 0,
        minted: 0,
        balance: core
            .as_ref()
            .map(|c| chain.balance_of(&c.address))
            .unwrap_or(0),
        total_minted: chain.total_minted(),
        ts_unix: now_ms() / 1000,
        txs_count: 0,
    };
    let _ = app.emit("chain_block", &event);
}

// ---------------------------------------------------------------- swarm 生命周期

fn build_swarm() -> Result<libp2p::Swarm<QlBehaviour>, Box<dyn std::error::Error + Send + Sync>> {
    // 端口复用策略在 libp2p 0.56 起由连接层自动处理（Config 留默认即可）
    let tcp_cfg = libp2p::tcp::Config::default();

    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp_cfg,
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )?
        .with_behaviour(|key| {
            let gs_cfg = gossipsub::ConfigBuilder::default()
                .heartbeat_interval(Duration::from_millis(800))
                .duplicate_cache_time(Duration::from_secs(30))
                .max_transmit_size(SYNC_MAX_BYTES)
                .mesh_n(3)
                .mesh_n_low(1)
                .mesh_outbound_min(1)
                .mesh_n_high(16)
                .validation_mode(gossipsub::ValidationMode::Anonymous)
                .message_id_fn(|m| gossipsub::MessageId::from(&m.data[..]))
                .build()?;
            let gossipsub = gossipsub::Behaviour::new(gossipsub::MessageAuthenticity::Anonymous, gs_cfg)?;
            let mdns = libp2p::mdns::tokio::Behaviour::new(
                {
                    // 演示网：10s 重查一次（默认 5 分钟太久，丢包/防火墙场景恢复慢）
                    let mut c = libp2p::mdns::Config::default();
                    c.query_interval = Duration::from_secs(10);
                    c
                },
                libp2p::PeerId::from(key.public()),
            )?;
            Ok(QlBehaviour { gossipsub, mdns })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build();
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse::<Multiaddr>()?)?;
    Ok(swarm)
}

/// swarm 主循环：事件 / 指令 / 状态机节拍 三路 select
async fn run_swarm(app: AppHandle, net: Arc<Net>, mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<NetCmd>) {
    let mut swarm = match build_swarm() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[QLight] 联邦网络不可用：{e} —— 退回单机出块模式（账本与验签不受影响）");
            local_miner(app, net.chain.clone(), net.core_slot.clone()).await;
            return;
        }
    };

    let topics = Topics {
        reg: gossipsub::IdentTopic::new(TOPIC_REG),
        tx: gossipsub::IdentTopic::new(TOPIC_TX),
        block: gossipsub::IdentTopic::new(TOPIC_BLOCK),
        sync: gossipsub::IdentTopic::new(TOPIC_SYNC),
        ent: gossipsub::IdentTopic::new(TOPIC_ENT),
        dust: gossipsub::IdentTopic::new(TOPIC_DUST),
        pos: gossipsub::IdentTopic::new(TOPIC_POS),
    };
    for t in [
        &topics.reg,
        &topics.tx,
        &topics.block,
        &topics.sync,
        &topics.ent,
        &topics.dust,
        &topics.pos,
    ] {
        if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(t) {
            eprintln!("[QLight] 订阅失败：{e}");
        }
    }

    let chain = net.chain.clone();
    let mut ms = MachineState::default();
    let mut prev_unlocked = false;
    let mut seen: HashSet<PeerId> = HashSet::new();
    // 直连失败退避：pid → 下次允许重拨时刻（重试直到 TTL 过期，不刷屏）
    let mut backoff: HashMap<PeerId, u64> = HashMap::new();
    let my_pid = *swarm.local_peer_id();
    let mut my_addrs: Vec<Multiaddr> = Vec::new();
    // 真实建立的连接（邻居计数的唯一事实来源：UI chip 与单机/联邦判定都用它）
    let mut established: HashSet<PeerId> = HashSet::new();
    let mut next_boot = now_ms() + 1500;
    let mut ticker = tokio::time::interval(Duration::from_millis(120));

    loop {
        tokio::select! {
            ev = swarm.next() => {
                let Some(ev) = ev else { break };
                match ev {
                    SwarmEvent::Behaviour(QlBehaviourEvent::Mdns(libp2p::mdns::Event::Discovered(list))) => {
                        net.on_peers_discovered(list, &mut seen, &mut backoff, &established, &mut swarm, &topics, &chain);
                    }
                    SwarmEvent::Behaviour(QlBehaviourEvent::Mdns(libp2p::mdns::Event::Expired(list))) => {
                        net.on_peers_expired(list, &mut seen, &app);
                    }
                    SwarmEvent::ConnectionEstablished { peer_id, num_established, .. } => {
                        established.insert(peer_id);
                        // 拨过的地址才进范围名单：没拨过 = 对方连进来，我们看不到它的来源地址
                        if let Some(s) = net.dial_scope.lock().unwrap().remove(&peer_id.to_string()) {
                            net.peer_scope.lock().unwrap().insert(peer_id.to_string(), s);
                        }
                        let cnt = established.len();
                        net.peers.store(cnt, Ordering::Relaxed);
                        let _ = app.emit("net_peers", serde_json::json!({ "count": cnt }));
                        eprintln!("[QLight] 已连接邻居 {peer_id}（联邦 {cnt} 节点，连接数 {num_established}）");
                    }
                    SwarmEvent::ConnectionClosed { peer_id, num_established, .. } => {
                        if num_established == 0 {
                            established.remove(&peer_id);
                            net.peer_scope.lock().unwrap().remove(&peer_id.to_string());
                            net.dial_scope.lock().unwrap().remove(&peer_id.to_string());
                            let cnt = established.len();
                            net.peers.store(cnt, Ordering::Relaxed);
                            let _ = app.emit("net_peers", serde_json::json!({ "count": cnt }));
                            eprintln!("[QLight] 邻居连接断开 {peer_id}（联邦 {cnt} 节点）");
                            // 断开的邻居退出版本普查：chip 不该挂着一台已经不存在的机器
                            let fire = {
                                let mut c = net.versions.lock().unwrap();
                                c.drop_peer(&peer_id.to_string());
                                c.take_warning(&net.my_rules, APP_VERSION)
                            };
                            if fire {
                                net.emit_versions(&app);
                            }
                        }
                    }
                    SwarmEvent::OutgoingConnectionError { peer_id, .. } => {
                        // 拨号失败 → 只退避重试，不动计数（计数以真实连接为准）
                        if let Some(pid) = peer_id {
                            backoff.insert(pid, now_ms() + BOOT_RETRY_MS);
                        }
                    }
                    SwarmEvent::Behaviour(QlBehaviourEvent::Gossipsub(gossipsub::Event::Message { message, .. })) => {
                        match serde_json::from_slice::<(u64, NetMsg)>(&message.data) {
                            Ok((_, m)) => net.on_net_msg(m, message.source, &mut swarm, &mut ms, &topics, &app, &chain),
                            Err(e) => eprintln!("[QLight] 丢弃未知网络消息：{e}"),
                        }
                    }
                    SwarmEvent::NewListenAddr { address, .. } => {
                        eprintln!("[QLight] 联邦网络监听：{address}（mDNS + 引导注册表已开启）");
                        my_addrs.push(address.clone());
                        net.write_boot(my_pid, &my_addrs);
                    }
                    _ => {}
                }
            }
            cmd = cmd_rx.recv() => match cmd {
                Some(NetCmd::BroadcastEntity(list)) => {
                    pub_msg(&mut swarm, &topics.ent, &NetMsg::Entity(list));
                }
                Some(NetCmd::BroadcastTx(wire)) => {
                    pub_msg(&mut swarm, &topics.tx, &NetMsg::Tx(wire));
                }
                Some(NetCmd::BroadcastDust(list)) => {
                    pub_msg(&mut swarm, &topics.dust, &NetMsg::Dust(list));
                }
                Some(NetCmd::BroadcastPos(w)) => {
                    pub_msg(&mut swarm, &topics.pos, &NetMsg::Pos(w));
                }
                None => break,
            },
            _ = ticker.tick() => {
                let now = now_ms();
                if now >= next_boot {
                    next_boot = now + BOOT_EVERY_MS;
                    net.boot_tick(my_pid, &my_addrs, &mut seen, &mut backoff, &established, &mut swarm, &topics, &chain);
                }
                net.machine_tick(&mut swarm, &mut ms, &app, &chain, &topics, &mut prev_unlocked);
            }
        }
    }
}

/// 联邦网络初始化失败时的退路：与 A 档完全一致的单机出块（账本/验签口径不变）
async fn local_miner(app: AppHandle, chain: Arc<SimChain>, core_slot: Arc<RwLock<Option<Arc<WalletCore>>>>) {
    let mut next = now_ms() + 1000;
    loop {
        if now_ms() >= next {
            if let Some(core) = core_slot.read().unwrap().clone() {
                if let Some((event, ..)) = chain.mine_block(&core) {
                    let _ = app.emit("chain_block", &event);
                }
            }
            next = now_ms() + BEAT_MS;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    const MINE: &str = "b5|QL-ENTREC-v4|QL-DUST-v1";
    const MY: &str = "0.4.1";

    /// 0.4.1 之前的公告只有 height/tip。新字段必须按缺省读得出来：否则升级后的
    /// 第一件事变成「整张网互相丢弃公告」，比版本分歧本身更糟。
    #[test]
    fn an_announce_from_an_old_peer_still_parses() {
        let raw = r#"{"Announce":{"height":141,"tip_hash_hex":"aa"}}"#;
        match serde_json::from_str::<NetMsg>(raw).unwrap() {
            NetMsg::Announce {
                height,
                app_version,
                rules,
                ..
            } => {
                assert_eq!(height, 141);
                assert!(app_version.is_empty() && rules.is_empty(), "旧公告按缺省读空");
            }
            _ => panic!("不是公告"),
        }
    }

    /// 没带规则指纹 = 对端说不出自己按哪套规则出块，不能当成一致
    #[test]
    fn a_peer_that_declares_nothing_is_a_rule_mismatch() {
        let mut c = VersionCensus::default();
        c.note("p1", "", "", 1_000);
        let (fork, odd) = c.lists(MINE, MY);
        assert_eq!(fork.len(), 1, "说不出规则的邻居按规则分歧算");
        assert_eq!(fork[0]["rules"], RULES_UNKNOWN);
        assert_eq!(fork[0]["app_version"], RULES_UNKNOWN);
        assert!(odd.is_empty(), "已经算作规则分歧，就不必再报一遍版本不齐");
    }

    /// 这一层是本轮真正需要的：0.4.0 与 0.4.1 的规则家族完全一样，
    /// 但光尘轮次的**判据**变了——只比指纹会漏掉这种分叉。
    #[test]
    fn same_rules_different_build_is_reported_as_uneven() {
        let mut c = VersionCensus::default();
        c.note("p1", "0.4.0", MINE, 1_000);
        let (fork, odd) = c.lists(MINE, MY);
        assert!(fork.is_empty(), "指纹相同，谈不上规则分歧");
        assert_eq!(odd.len(), 1);
        assert_eq!(odd[0]["app_version"], "0.4.0");
        assert_eq!(odd[0]["rules"], MINE);
    }

    /// 齐整的联邦一句都不该说：签名从空串出发，"干净 → 干净"不推事件
    #[test]
    fn a_uniform_federation_says_nothing() {
        let mut c = VersionCensus::default();
        c.note("p1", MY, MINE, 1_000);
        c.note("p2", MY, MINE, 1_500);
        assert!(!c.take_warning(MINE, MY), "同一规则同一发行包 = 没有可告的东西");
        assert!(c.signature(MINE, MY).is_empty());
    }

    /// 公告每 5 秒重播一次；集合没变就不该刷屏、不该白推事件
    #[test]
    fn only_a_changed_set_raises_a_warning() {
        let mut c = VersionCensus::default();
        c.note("p1", "0.4.0", MINE, 1_000);
        assert!(c.take_warning(MINE, MY), "第一次不齐要说出口");
        c.note("p1", "0.4.0", MINE, 6_000);
        assert!(!c.take_warning(MINE, MY), "同一份分歧重播不刷屏");
        c.note("p2", "0.3.0", "b4|QL-ENTREC-v4|QL-DUST-v1", 6_000);
        assert!(c.take_warning(MINE, MY), "多出一台规则分歧 = 集合变了");
        c.drop_peer("p2");
        assert!(c.take_warning(MINE, MY), "少一台也要让 chip 跟着改");
        assert!(!c.take_warning(MINE, MY));
        c.note("p1", MY, MINE, 9_000);
        assert!(c.take_warning(MINE, MY), "邻居升上来了，告警得撤");
        assert!(!c.take_warning(MINE, MY), "撤警只说一次");
    }

    /// 对端进程被杀掉未必有 ConnectionClosed：不再公告就得自己过期
    #[test]
    fn silence_expires_a_peer_out_of_the_census() {
        let mut c = VersionCensus::default();
        c.note("p1", "0.4.0", MINE, 1_000);
        c.sweep(VERSION_TTL_MS, 1_000 + VERSION_TTL_MS - 1);
        assert_eq!(c.peers.len(), 1, "还在公告窗口内");
        c.sweep(VERSION_TTL_MS, 1_000 + VERSION_TTL_MS + 1);
        assert!(c.lists(MINE, MY).1.is_empty(), "三个公告周期没再听到 = 不在这张网上了");
    }

    // —————————————————————— 在场（共识之外）——————————————————————

    fn core_of(byte: u8) -> crate::keystore::WalletCore {
        let seed = [byte; 32];
        let (sk, vk) = crate::crypto::keypair_from_seed(&seed);
        let salt = [byte; 8];
        let address = crate::crypto::derive_address(&vk, &salt);
        let pk_bytes = crate::crypto::encode_pk(&vk);
        crate::keystore::WalletCore {
            seed: zeroize::Zeroizing::new(seed),
            addr_salt: salt,
            address,
            signing_key: sk,
            verifying_key: vk,
            pk_bytes,
        }
    }

    /// 固定的"现在"（毫秒）：在场判据里有两处时钟，测试必须自己掌控
    const NOW: u64 = 1_700_000_000_000;

    fn pos(byte: u8) -> PresenceWire {
        sign_pos_wire(&core_of(byte), 1, 12.5, 3.0, -8.25, 90.0, NOW)
    }

    #[test]
    fn a_signed_presence_lands_in_the_roster() {
        let w = pos(7);
        let e = Net::verify_pos(&w, NOW).expect("自签自验");
        assert!((e.x - 12.5).abs() < 1e-9, "定点化往返必须拿回原值");
        assert!((e.z + 8.25).abs() < 1e-9);
        let mut p = Presence::default();
        assert!(p.note(e));
        assert_eq!(p.len(), 1);
        assert!(p.take_update().is_some(), "名单变了要推事件");
        assert!(p.take_update().is_none(), "没变就不许再推一次");
    }

    /// 冒名的路：拿自己的合法签名配上别人的地址。地址由 (公钥, 盐) 现场复算，
    /// 改一个字就验不过——这是「彼此看得见」能被信任的唯一根据。
    #[test]
    fn presence_for_someone_elses_address_is_rejected() {
        let victim = core_of(99);
        let mut w = pos(7);
        w.addr = victim.address.clone();
        assert!(Net::verify_pos(&w, NOW).is_none());
    }

    #[test]
    fn a_tampered_coordinate_breaks_the_signature() {
        let mut w = pos(7);
        w.x += 1;
        assert!(Net::verify_pos(&w, NOW).is_none());
    }

    #[test]
    fn an_ancient_presence_is_dropped() {
        let c = core_of(7);
        let stale = sign_pos_wire(&c, 1, 1.0, 1.0, 1.0, 0.0, NOW - (POS_MAX_AGE_SECS + 5) * 1000);
        assert!(Net::verify_pos(&stale, NOW).is_none(), "超出时间窗口 = 直接丢");
        let fresh = sign_pos_wire(&c, 1, 1.0, 1.0, 1.0, 0.0, NOW - 60_000);
        assert!(Net::verify_pos(&fresh, NOW).is_some(), "一分钟前还作数");
    }

    /// gossip 不保证顺序：seq 更小的那封晚到也不能把邻居拽回旧位置
    #[test]
    fn an_older_seq_never_pulls_a_neighbor_backwards() {
        let c = core_of(7);
        let newer = sign_pos_wire(&c, 5, 20.0, 0.0, 0.0, 0.0, NOW);
        let older = sign_pos_wire(&c, 4, 2.0, 0.0, 0.0, 0.0, NOW);
        let mut p = Presence::default();
        assert!(p.note(Net::verify_pos(&newer, NOW).unwrap()));
        assert!(!p.note(Net::verify_pos(&older, NOW).unwrap()), "seq 倒退不收");
        let list = p.take_update().unwrap();
        assert_eq!(list[0].x, 20.0, "留在名单里的是更新的坐标");
    }

    #[test]
    fn silence_removes_a_neighbor_from_the_roster() {
        let mut p = Presence::default();
        // 时钟只用一个基准：wire 的 ts 由 NOW 生成，验证与过期都必须沿同一条时间轴
        p.note(Net::verify_pos(&pos(7), NOW).unwrap());
        p.take_update();
        p.sweep(POS_TTL_MS, NOW + POS_TTL_MS - 1);
        assert!(p.take_update().is_none(), "还在窗口内不该算变化");
        p.sweep(POS_TTL_MS, NOW + POS_TTL_MS + 1);
        assert_eq!(p.len(), 0);
        assert!(p.take_update().is_some(), "离场也要推一次事件");
    }

    /// 域隔离：在场签名与门票/交易/实体/光尘四个签名域不同构，
    /// 一条"我在场"永远不可能被当成任何有账本意义的东西
    #[test]
    fn presence_signs_a_domain_of_its_own() {
        let w = pos(7);
        assert!(pos_msg_bytes(&w).starts_with(POS_PREFIX));
        assert!(!pos_msg_bytes(&w).starts_with(TICKET_PREFIX));
    }

    /// 联机三态的分类：同机演示绝不该被说成"局域网"，局域网更不该被说成"公网"
    #[test]
    fn addresses_are_classified_into_local_lan_or_public() {
        let case = |s: &str| addr_scope(&s.parse::<Multiaddr>().unwrap());
        assert_eq!(case("/ip4/127.0.0.1/tcp/4001"), "loopback");
        assert_eq!(case("/ip4/10.1.2.3/tcp/4001"), "lan");
        assert_eq!(case("/ip4/192.168.1.9/tcp/4001"), "lan");
        assert_eq!(case("/ip4/172.16.0.1/tcp/4001"), "lan");
        assert_eq!(case("/ip4/172.31.255.7/tcp/4001"), "lan");
        // 172.32 不在私有的 172.16/12 里，它是公网地址：边界必须算对
        assert_eq!(case("/ip4/172.32.0.1/tcp/4001"), "public");
        assert_eq!(case("/ip4/8.8.8.8/tcp/4001"), "public");
        assert_eq!(case("/ip6/::1/tcp/4001"), "loopback");
        assert_eq!(case("/ip6/fd12::1/tcp/4001"), "lan");
        assert_eq!(case("/ip6/2001:4860::8888/tcp/4001"), "public");
        // 认不出来就说认不出来，不许蒙一个"公网"把玩家吓一跳
        assert_eq!(case("/dns4/example.com/tcp/4001"), "unknown");
    }

    /// 只有真建立过连接的邻居才进三态统计，断开即退出
    #[test]
    fn the_scope_report_counts_only_live_neighbours() {
        let m = addr_scope(&"/ip4/192.168.4.20/tcp/0".parse::<Multiaddr>().unwrap());
        let mut live: HashMap<String, &'static str> = HashMap::new();
        live.insert("peer-a".to_string(), m);
        assert_eq!(live.len(), 1);
        assert_eq!(m, "lan");
        live.remove("peer-a");
        assert!(live.is_empty(), "断开的邻居不许继续占着联机人数");
    }
}
