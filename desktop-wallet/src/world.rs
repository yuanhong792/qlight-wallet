//! 世界实体账本（DESIGN.md §9.11「彼此看见：币即世界，世界即账本」）
//!
//! 一句话：链上现在只有"钱能从 A 到 B"这一条物理定律（`TxEnvelope.actions` 只有
//! Transfer）。本模块补上第二条——**东西可以被创造、被归属、被指认**。
//!
//! 为什么不塞进 QL-TX-v1：`encode_tx` 顶层是定长 9 元素 CBOR 数组，加一个字段就
//! 改变**每一笔历史交易的 digest**，而 blocks.jsonl 每行存的就是 digest_hex，启动
//! 重放会当场崩塌。所以实体走**独立账本 + 独立前缀**，将来要进块也只是把
//! `entity_root` 挂进新版本的块格式，不破坏既有链。
//!
//! 可验证性口径（每一条都能被第三方独立重算，不需要任何服务器）：
//! - 记录哈希 `record_hash = SHAKE-256("QL-ENTREC-H1", canon)`
//! - 签名     `sig = ML-DSA-65(sk, record_hash)`（FIPS 204，与转账同一把钥匙）
//! - 归属     `owner = bech32(derive_address(pk, salt))`，由记录内自带的 1952B 公钥推出
//! - 顺序     每条带 `prev_hash`，**每个归属者各自一条 append-only 哈希链**，
//!   撕一条只断那一个人的链（别人的记录照常生效）
//! - 内容寻址 `entity_id = SHAKE-256("QL-ENT-V1", prev_hash ‖ pk_hash ‖ seq)`
//!   id 由铸造内容（名称/坐标/槽位/块高/父实体）一并派生：没有注册机构、没有
//!   命名空间仲裁、没有白名单，谁先写谁得；创建者也无法事后自签改写这条历史。
//!
//! Minecraft 借来的四件事：格子坐标 + **高度层**（点击面即放置，可以叠塔）、
//! 物品栏（kind 8 种方块/生灵）、chunk 16×16（每个 chunk 一个承诺根，实体
//! "都可查"但不是每条都单独上链）、创造模式（站着看哪儿就能在那儿造东西）。

/// 世界可用的最大高度层（layer ∈ 0..MAX_LAYER）。
///
/// 地形本身是算出来的：`layer = 高度场 >> 3`（`ui/world.js` 与 `tools/world/*`
/// 同一口径），所以地表只到 28 层。留到 64 层是给玩家往天上造的；
/// 往地下则是**挖**——每挖一层落一条 `dig` 记录（v5），挖到第 1 层为止。
pub const MAX_LAYER: u16 = 64;

use crate::crypto;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 世界尺寸（与 tools/world 参考实现同一口径：256×256 格）
pub mod dust;
pub mod earth_local;
pub mod ore;
pub mod pool;
pub mod resource;
pub mod terrain;
pub mod trader;

pub const WORLD_W: u16 = 256;
pub const WORLD_H: u16 = 256;
/// Minecraft 口径：chunk 16×16 是加载/网络/承诺单位
pub const CHUNK: u16 = 16;
/// 创世纪元截止块高：此前诞生的实体永久标 era=1（§9.9 稀缺来自时间）
pub const ERA1_END_HEIGHT: u64 = 100_000;
/// 物品栏槽位数（kind ∈ 0..SLOT_COUNT）
pub const SLOT_COUNT: u8 = 16;
pub const NAME_MAX: usize = 64;

/// 免费写入的软限额：每个归属者、每 1024 个块高最多 4096 条记录。
///
/// 建造不再和币相关（§9.14）——不付费就没有"每写一条都花真钱"的成本，
/// 所以用**链高窗口**给一个确定、可复算的速率上限：心跳块每 2 秒推进一格，
/// 窗口因此一直向前走；正常建造永远碰不到它，只有脚本刷盘会撞墙。
pub const FREE_QUOTA_WINDOW: u64 = 1_024;
pub const FREE_QUOTA_MAX: u32 = 4_096;

/// 每列真正挖不动的那一层：第 0 层是岩床，第 1 层是最后一层可挖的地。
/// 地表层最高 28、最低 4，所以一列能挖 3..27 层（全图均值约 13 层）。
pub const DUG_FLOOR: u16 = 1;

/// 材料编号（`dig` 记录的 `kind`）：0 泥土、1 石头、2 矿。
/// 与 `claim` 的 `kind`（物品栏槽位）共用同一个字段，但语义按 `op` 分开——
/// 重放时先看操作、再看字段，绝不会把一件家具当成一筐土。
pub const MAT_SOIL: u8 = 0;
pub const MAT_STONE: u8 = 1;
pub const MAT_ORE: u8 = 2;
pub const MAT_COUNT: usize = 3;

/// 深度带（离地表的层数）：0..=3 是泥土，4..=9 是石头，10 层以下才出矿。
/// 矿必须靠真往下挖换来——否则"深度"就只是画面上的说法。
pub const DUG_SOIL_DEPTH: u16 = 3;
pub const DUG_STONE_DEPTH: u16 = 9;

const REC_PREFIX_V3: &[u8] = b"QL-ENTREC-v3";
const REC_PREFIX_V4: &[u8] = b"QL-ENTREC-v4";
const REC_PREFIX_V5: &[u8] = b"QL-ENTREC-v5";
const REC_HASH_PREFIX: &[u8] = b"QL-ENTREC-H1";
pub(crate) const RECORD_VERSION: &str = "QL-ENTREC-v5";
/// v4 = 免费的上一代语义（无挖地、无材料）。老账本必须原样重放——
/// 升版本换前缀是为了让新的字段语义（kind 在 dig 里指材料）不污染旧记录。
const RECORD_VERSION_V4: &str = "QL-ENTREC-v4";
const RECORD_VERSION_V3: &str = "QL-ENTREC-v3";
const ID_PREFIX: &[u8] = b"QL-ENT-V1";
const CHUNK_PREFIX: &[u8] = b"QL-CHUNK-V1";
const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// 世界铸造费的销毁地址（32 字节全零 commitment 的 bech32 形式）。
///
/// 没有对应私钥（SHAKE-256 求 29 字节零前像不可行），所以付进去的钱离开流通、
/// 永远不再回来。这既是"写入世界要付真金白银"的价格，也是最干净的销毁口径：
/// 没有任何人能把这笔钱再花出去。
pub fn fee_addr() -> String {
    crypto::to_bech32_ql(&[0u8; 32])
}

/// 每个操作的价格（QLX 最小单位，1 QLX = 1e9）。
///
/// **只服务 v3 老账本的重放**：v4 起建造免费（§9.14），价格表冻结在这里，
/// 让历史记录仍按当年的价格验得过去；新记录根本不看它。
pub fn entity_fee(op: &str) -> Option<u64> {
    match op {
        op::CLAIM => Some(1_000_000_000),
        op::RENAME | op::MOVE => Some(200_000_000),
        op::RELEASE => Some(100_000_000),
        _ => None,
    }
}

/// 操作类型（对应 §9.11 的四件事：铸、改名、挪动、释放归属）+ v5 的第五件：挖
pub mod op {
    pub const CLAIM: &str = "claim";
    pub const RENAME: &str = "rename";
    pub const MOVE: &str = "move";
    pub const RELEASE: &str = "release";
    /// v5：从某一列剥掉露出地面的那一层，产出一份材料（§9.20）
    pub const DIG: &str = "dig";
}

/// 一条实体记录（entities.jsonl 每行一条，字段全部参与哈希链，无冗余可篡改位）
///
/// 版本沿革：v1（无 layer，从未签名过任何一条）→ v2（加 layer）→
/// v3（加 `fee_tx_hex`：每条写入必须由一笔已确认的链上付款买单）→
/// **v4（建造免费：`fee_tx_hex` 必须为空，`height` 不得超过链头，按窗口配额防刷）** →
/// **v5（地形可挖：新增 `op = "dig"`，材料进重放状态，建造按材料扣费）**。
///
/// v5 的 `dig` 记录里字段各管什么：`x, y` = 列，`layer` = 被剥掉的那一层，
/// `kind` = 产出的材料（0 土 / 1 石 / 2 矿），`name`、`entity_id_hex`、
/// `parent_hex` 一律为空——挖不是造东西，账本里不留一个假实体。
///
/// v3 用钱解决了"刷盘免费"；v4 把这笔钱拿掉，是因为**创造不该被币控制**。
/// 防刷改为确定性的速率上限：同一归属者、同一 1024 块窗口最多 4096 条。
/// 代价要说清楚：没有链上付款锚定，`height` 在 v4 里只是**声明值**（诚实钱包取链头），
/// 第三方无法像 v3 那样从付款块高反推它；真正的创建时刻要等 `entity_root` 进块。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityRecord {
    pub v: String,
    /// 从 1 开始严格递增（重放时校验，杜绝插入与重排）
    pub seq: u64,
    pub op: String,
    /// claim 时为新派生 id；其余为被操作对象的 id
    pub entity_id_hex: String,
    /// 谱系：由谁造出（§9.9 无预挖下唯一给得出的稀缺性）；空串表示无
    pub parent_hex: String,
    pub name: String,
    pub x: u16,
    pub y: u16,
    /// 高度层（Minecraft 的 Y）：`layer = 地表高度 >> 3` 即贴地，往上叠塔
    pub layer: u16,
    pub kind: u8,
    /// 写入时声明的块高：决定 era、免费写入的窗口与先后。
    /// v3 必须等于 `fee_tx_hex` 那笔交易所在块高（由链上数据复算，不能自填）；
    /// v4 必须 ≤ 链头，且同一归属者的 v4 块高只能向前（见模块文档）
    pub height: u64,
    pub ts: u64,
    /// 归属地址；release 后状态机里置空，但记录永久保留
    pub owner: String,
    /// SHAKE-256("QL-PK-REG-V1", pk)，与 export_pk_hash 同口径
    pub pk_hash_hex: String,
    /// 地址派生盐（无它无法从 pk 复算 owner，故必须随记录公开）
    pub salt_hex: String,
    /// 上一条记录的 record_hash；首条为 64 个 0
    pub prev_hash_hex: String,
    /// v3：为这次写入买单的那笔已确认交易（QL-TX-v1 digest，64 hex）——
    /// 单笔转账、付款人 = owner、收款方 = `fee_addr()`、金额 ≥ `entity_fee(op)`，
    /// 且全账本只能被用一次。
    /// v4：**必须为空**（建造免费，见 §9.14）
    pub fee_tx_hex: String,
    /// 1952 字节 ML-DSA-65 公钥（独立验签所需，不信任任何索引）
    pub pk_hex: String,
    /// ML-DSA-65 对 record_hash 的签名
    pub sig_hex: String,
}

/// 参与签名的字段 → 确定性字节串（长度前缀，杜绝字段边界歧义）
///
/// 版本进前缀：v3 账本按 v3 前缀重算、v4 按 v4 前缀重算。升版本必须换前缀——
/// 否则改了字段语义（手续费凭据从"必需"变成"必须为空"）却仍共用同一个域，
/// 老记录会被新规则静默误读。
fn rec_prefix(v: &str) -> &'static [u8] {
    if v == RECORD_VERSION_V3 {
        REC_PREFIX_V3
    } else if v == RECORD_VERSION_V4 {
        REC_PREFIX_V4
    } else {
        REC_PREFIX_V5
    }
}

fn canon(r: &EntityRecord) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(256);
    fn u64b(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_be_bytes());
    }
    fn bytes(out: &mut Vec<u8>, b: &[u8]) {
        out.extend_from_slice(&(b.len() as u16).to_be_bytes());
        out.extend_from_slice(b);
    }
    out.extend_from_slice(rec_prefix(&r.v));
    u64b(&mut out, r.seq);
    bytes(&mut out, r.op.as_bytes());
    bytes(&mut out, &hex256(&r.entity_id_hex));
    bytes(&mut out, &hex256(&r.parent_hex));
    bytes(&mut out, r.name.as_bytes());
    out.extend_from_slice(&r.x.to_be_bytes());
    out.extend_from_slice(&r.y.to_be_bytes());
    out.extend_from_slice(&r.layer.to_be_bytes());
    out.push(r.kind);
    u64b(&mut out, r.height);
    u64b(&mut out, r.ts);
    bytes(&mut out, &hex256(&r.pk_hash_hex));
    bytes(&mut out, &hex::decode(&r.salt_hex).unwrap_or_default());
    bytes(&mut out, &hex256(&r.prev_hash_hex));
    // v3：手续费凭据也进签名——换掉凭据等于换掉整条记录，签名当场失效
    bytes(&mut out, &hex256(&r.fee_tx_hex));
    out
}

/// 宽松解析：非法/短填一律补零到 32 字节（校验阶段会因哈希不符而失败，不在此处 panic）
fn hex256(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    if let Ok(b) = hex::decode(s) {
        let n = b.len().min(32);
        out[..n].copy_from_slice(&b[..n]);
    }
    out
}

pub fn record_hash(r: &EntityRecord) -> [u8; 32] {
    let d = crypto::shake256(REC_HASH_PREFIX, &canon(r), 32);
    let mut out = [0u8; 32];
    out.copy_from_slice(&d);
    out
}

/// 实体「集合指纹」的域分隔符（DESIGN.md §9.15 缺口检测）
pub const ENT_SET_PREFIX: &[u8] = b"QL-ENTSET-V1";

/// `(条数, 指纹)`：指纹 = SHAKE-256(域 ‖ 全部 record_hash 升序拼接)。
///
/// 排序 + 去重让它只依赖**记录集合本身**，与落盘顺序、多个归属者的记录怎么交错
/// 都无关 —— 这正是缺口检测需要的不变量：两台机器指纹相同 ⇒ 它们眼里的世界是同
/// 一份。指纹不承诺"这些记录合法"，那仍由逐条验签验链负责。
pub fn set_digest(records: &[EntityRecord]) -> (u64, String) {
    let mut hashes: Vec<[u8; 32]> = records.iter().map(record_hash).collect();
    hashes.sort_unstable();
    hashes.dedup();
    let mut buf = Vec::with_capacity(hashes.len() * 32);
    for h in &hashes {
        buf.extend_from_slice(h);
    }
    (
        hashes.len() as u64,
        hex::encode(crypto::shake256(ENT_SET_PREFIX, &buf, 32)),
    )
}

/// 内容寻址的实体 id：由「上一条哈希 ‖ 创建者公钥摘要 ‖ 序号 ‖ 铸造内容」算出，
/// 不需要任何注册机构批准，也不可被抢注（同一 seq 位置只能有一条合法记录）。
///
/// 内容（名称/坐标/高度层/槽位/块高）也进 id：否则事后改一条 claim 再自签一次，
/// id 仍自洽，世界历史就能被创建者悄悄重写。
pub fn derive_entity_id(prev_hash: &[u8; 32], pk_hash: &[u8; 32], r: &EntityRecord) -> [u8; 32] {
    let mut buf = Vec::with_capacity(96);
    buf.extend_from_slice(prev_hash);
    buf.extend_from_slice(pk_hash);
    buf.extend_from_slice(&r.seq.to_be_bytes());
    buf.extend_from_slice(&(r.name.len() as u16).to_be_bytes());
    buf.extend_from_slice(r.name.as_bytes());
    buf.extend_from_slice(&r.parent_hex.as_bytes());
    buf.extend_from_slice(&r.x.to_be_bytes());
    buf.extend_from_slice(&r.y.to_be_bytes());
    buf.extend_from_slice(&r.layer.to_be_bytes());
    buf.push(r.kind);
    buf.extend_from_slice(&r.height.to_be_bytes());
    let d = crypto::shake256(ID_PREFIX, &buf, 32);
    let mut out = [0u8; 32];
    out.copy_from_slice(&d);
    out
}

// ---------------------------------------------------------------- 挖掘（v5）

/// 该列的大地高度（0..247）——v3 起是真实地球的高程表，与种子和历史彻底无关。
///
/// 这正是挖掘判定敢用它的理由：地表湿度（`chain_words`）每出一个块就重画一次，
/// 拿它判定"这一列地表在第几层"，昨天挖的那一格今天就会变成非法——
/// 一笔历史被后来的块改判，等于整个重放不可复现。而 `earth-height.bin`
/// 连一个字节都不随链生长，`(x, y, layer)` 才是能签进账本的承诺。
pub fn ground_height(x: u16, y: u16) -> u32 {
    terrain::height_at(x as u32, y as u32)
}

/// 该列原始地表所在的层：地表方块就占这一层，顶面在 `layer + 1`
pub fn surface_layer(x: u16, y: u16) -> u16 {
    (ground_height(x, y) >> 3) as u16
}

/// 水面不是方块：这一列挖不动（也挖不出东西）
pub fn is_water(x: u16, y: u16) -> bool {
    ground_height(x, y) < terrain::SEA_LEVEL
}

/// 深度带 → 材料。`depth` = 离地表的层数（0 = 地表那一层被剥掉）
pub fn material_at_depth(depth: u16) -> u8 {
    if depth <= DUG_SOIL_DEPTH {
        MAT_SOIL
    } else if depth <= DUG_STONE_DEPTH {
        MAT_STONE
    } else {
        MAT_ORE
    }
}

pub fn mat_name(i: u8) -> &'static str {
    match i {
        MAT_SOIL => "泥土",
        MAT_STONE => "石头",
        MAT_ORE => "矿",
        _ => "材料",
    }
}

/// 造一件东西要的材料（v5 起建造真花钱，钱就是挖出来的土石矿）。
///
/// 一张 16 行的表，按物品栏槽位索引。结构件吃石料、活物与花木吃泥土、
/// 灯塔要点矿石——**不是定价策略，是让"往下挖"有去处**。木料要等砍树
/// 那条记录做出来再补，这一版先把土石矿的闭环跑通。
pub fn build_cost(kind: u8) -> [u32; MAT_COUNT] {
    let mut c = [0u32; MAT_COUNT];
    match kind {
        0 => c[MAT_STONE as usize] = 1,                  // 石头
        3 | 4 => c[MAT_STONE as usize] = 1,              // 木屋 / 木桥：结构料
        5 => {
            c[MAT_STONE as usize] = 1;                   // 灯塔：石料 + 矿
            c[MAT_ORE as usize] = 1;
        }
        _ => c[MAT_SOIL as usize] = 1,                   // 树 / 花 / 篝火 / 篱笆 / 生灵
    }
    c
}

/// 世界里的一个实体（由记录重放出来的当前状态，不是独立存储）
#[derive(Debug, Clone, Serialize)]
pub struct Entity {
    pub entity_id_hex: String,
    pub name: String,
    pub x: u16,
    pub y: u16,
    pub layer: u16,
    pub kind: u8,
    pub owner: String,
    pub parent_hex: String,
    pub era: u64,
    pub created_height: u64,
    pub created_ts: u64,
    pub last_height: u64,
    pub released: bool,
}

/// 一个 chunk 的承诺根：块内实体 id 排序后拼接摘要（§9.10 chunk 粒度）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Chunk {
    pub cx: u16,
    pub cy: u16,
    pub count: usize,
    pub root_hex: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// 全部记录都通过（分链哈希链 + 签名 + 归属 + 语义）
    pub ok: bool,
    pub total: usize,
    /// 真正进入世界状态的记录数（被剔除的不算）
    pub accepted: usize,
    /// 因所在归属者链断裂而跳过的记录数
    pub skipped: usize,
    /// 首个失败记录在账本里的**行位**（1 起，0 = 无）——多方世界里各链各有各的
    /// seq，只有行位是无歧义的
    pub first_bad: u64,
    /// 断在哪把签名钥匙上（公钥摘要前缀）：多方世界里要说清是谁的链断了
    pub bad_owner: String,
    pub reason: String,
}

/// 一个归属者（按签名公钥摘要识别）自己的链条状态
#[derive(Default)]
struct OwnerChain {
    /// 已接受的条数——下一条必须是 `seq == accepted + 1`
    accepted: u64,
    /// 该归属者最后一条已接受记录的哈希；首条的 prev 是 64 个 0
    prev: [u8; 32],
    /// 这条链上最后一条 v4 记录的声明块高。v4 块高只许向前：
    /// 否则脚本可以跳回旧窗口，把同一份配额重新领一遍。
    last_v4_height: u64,
    /// 链一断就只停**这一条链**：这个公钥后续的记录全部剔除，别人的照常生效。
    /// 多方世界里，一个坏邻居不能把整个世界抹掉。
    broken: bool,
    bad_seq: u64,
    reason: String,
}

/// 某一格（含高度层）当前是否被**活着的**方块占着。
///
/// 已释放归属的实体不占格：断租不删除（记录永久可查），但那一格的空间重新
/// 有人可以造。这条规则只在重放里判定一次，第三方独立验签得出同一结论；
/// 命令层落盘前调的也是这同一个函数——绝不写两套判定。
pub(crate) fn live_at(state: &BTreeMap<String, Entity>, x: u16, y: u16, layer: u16) -> Option<&Entity> {
    state.values().find(|e| !e.released && e.x == x && e.y == y && e.layer == layer)
}

/// 一笔已确认交易的「手续费凭据」：由本机 `blocks.jsonl` 里的**交易原件**解出。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeeProof {
    /// 这笔交易被哪个块收下——记录里的 `height` 必须等于它（可复算，不能自填）
    pub height: u64,
    /// 付款人（bech32）——必须就是记录归属者本人
    pub from: String,
    /// 收款方（bech32）——必须是销毁地址
    pub to: String,
    pub amount: u64,
}

/// 链视图：已确认转账的 `digest → 凭据` 索引 + 链头高度。
///
/// 唯一数据来源是 `blocks.jsonl`（每块的原件），所以**任何第三方拿同一份链
/// 都能重建同一张表**：不需要信本机、不需要索引服务、不需要联网。实体账本
/// 因此仍然是纯离线可验证的——v3 多出来的是"这条写入的付款确实在链上"，
/// v4 多出来的是"这条写入没有预支未来的块高"。
#[derive(Default, Clone)]
pub struct ChainView {
    fees: BTreeMap<String, FeeProof>,
    /// `blocks.jsonl` 里最高的块高；空链为 0（创世块高 0 与"无块"同义，
    /// 都只允许声明块高 0 的记录——诚实钱包取 `chain.height()`）
    tip: u64,
}

#[derive(Deserialize)]
struct ViewLine {
    height: u64,
    #[serde(default)]
    wires: Vec<ViewWire>,
}

#[derive(Deserialize)]
struct ViewWire {
    envelope: crate::tx::TxEnvelope,
}

impl ChainView {
    /// 从 `blocks.jsonl` 全文建索引；只收**单笔转账**（多动作交易说不清哪一笔是手续费）
    pub fn from_blocks_text(text: &str) -> Self {
        let mut fees = BTreeMap::new();
        let mut tip = 0u64;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(vl) = serde_json::from_str::<ViewLine>(line) else { continue };
            tip = tip.max(vl.height);
            for w in &vl.wires {
                if w.envelope.actions.len() != 1 {
                    continue;
                }
                let action = &w.envelope.actions[0];
                fees.insert(
                    hex::encode(crate::tx::tx_digest(&w.envelope)),
                    FeeProof {
                        height: vl.height,
                        from: crypto::to_bech32_ql(&w.envelope.from_addr),
                        to: crypto::to_bech32_ql(&action.to),
                        amount: action.amount,
                    },
                );
            }
        }
        Self { fees, tip }
    }

    pub fn load(blocks_path: &Path) -> Self {
        match std::fs::read_to_string(blocks_path) {
            Ok(text) => Self::from_blocks_text(&text),
            Err(_) => Self::default(),
        }
    }

    pub fn fee(&self, digest_hex: &str) -> Option<&FeeProof> {
        self.fees.get(digest_hex)
    }

    /// 链头高度（`blocks.jsonl` 已落盘的最高块）
    pub fn tip(&self) -> u64 {
        self.tip
    }

    pub fn len(&self) -> usize {
        self.fees.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fees.is_empty()
    }

    /// 直接挂一条凭据（测试与上层预演用；生产路径一律走 `load`）
    pub fn insert(&mut self, digest_hex: String, p: FeeProof) {
        self.fees.insert(digest_hex, p);
    }
}

/// 重放整本实体账本，返回当前世界状态。
///
/// 与 `validate_blocks_jsonl` 同一套纪律：**逐条严格重算**，任何一条签名/归属/
/// 序号/哈希链不对就停在那里，并给出原因。返回的 Report 供 UI 直显，不静默降级。
///
/// 链条是**按归属者分开的**（识别键 = 公钥摘要）：每条链各自严格递增、各自
/// `prev_hash` 相接，`claim` 的内容寻址 id 用的就是"该归属者上一条"的哈希。
/// 单只钱包时全局链与分链数值完全重合，所以既有 v2 账本一字不改照样通过；
/// 一旦接收邻居中继来的记录（M0 联邦世界），全局下标校验就会当场判它断链。
pub fn replay(records: &[EntityRecord], view: &ChainView) -> (BTreeMap<String, Entity>, Report) {
    let w = walk_full(records, view);
    (w.state, w.report)
}

/// 重放出来的整个可见世界：实体 + 结算单 + 挖掉的土 + 每个人手里的材料。
///
/// 挖地让世界多了两个维度（地下的形状、材料的存量），但两者都是**记录集合的
/// 纯函数**——同一条账本，任何机器算出同一片地、同一本材料账。
pub struct World {
    pub entities: BTreeMap<String, Entity>,
    pub report: Report,
    /// (x, y) → 这一列已经剥掉的层数；顶面层 = `surface_layer - 已挖层数`
    pub dug: BTreeMap<(u16, u16), u32>,
    /// 地址 → 土 / 石 / 矿
    pub materials: BTreeMap<String, [u32; MAT_COUNT]>,
}

/// 整本账一条都通不过，而且它对这条链的**锚点一个都找不到** = 这本账属于另一条链
/// （链被归档重置前的旧世界）。锚点有两种：块高（记录写在链尖之上）、手续费凭据
/// （v3 记录买单的那笔交易在链上）。链一直在长，块高锚迟早失效——链长过旧记录的
/// 高度之后，凭据锚还认得出这本账：新链再长也变不出那些交易。
/// 只用来决定界面要不要给「归档旧世界账本」这个出口；它不是共识判据，真实写入
/// 仍由 `replay_world` 逐条判定。
pub fn ledger_is_foreign(
    report_ok: bool,
    heights: &[u64],
    tip: u64,
    fee_anchors_known: usize,
    fee_anchors_present: usize,
) -> bool {
    let n = heights.len();
    if report_ok || n == 0 {
        return false;
    }
    let all_above_tip = heights.iter().all(|&h| h > tip);
    // 每条记录都带 v3 手续费凭据、且没有一笔在当前链上：整本账的履历都不属于这条链
    let every_fee_absent = fee_anchors_known == n && fee_anchors_present == 0;
    all_above_tip || every_fee_absent
}

pub fn replay_world(records: &[EntityRecord], view: &ChainView) -> World {
    let w = walk_full(records, view);
    let mut materials: BTreeMap<String, [u32; MAT_COUNT]> = BTreeMap::new();
    for ((addr, mat), n) in w.mats {
        if (mat as usize) < MAT_COUNT {
            materials.entry(addr).or_insert([0; MAT_COUNT])[mat as usize] = n;
        }
    }
    World {
        entities: w.state,
        report: w.report,
        dug: w.dug,
        materials,
    }
}

/// 该列当前露在外面的那一层（地表以下已被挖空的层不算）
pub fn column_top(dug: &BTreeMap<(u16, u16), u32>, x: u16, y: u16) -> u16 {
    let removed = dug.get(&(x, y)).copied().unwrap_or(0);
    surface_layer(x, y).saturating_sub(removed.min(u16::MAX as u32) as u16)
}

/// 与 `replay` 同一份判定（绝不写两套），额外回吐"每一条是否被接受"的下标标记——
/// M0 合并邻居中继的记录时必须知道到底是哪几条进了世界，而不是只看一个总闸。
fn walk(records: &[EntityRecord], view: &ChainView) -> (BTreeMap<String, Entity>, Report, Vec<bool>) {
    let w = walk_full(records, view);
    (w.state, w.report, w.flags)
}

/// 重放的完整产物：世界状态 + 结算单 + 逐条标记 + **已花掉的手续费** +
/// **免费写入的窗口计数与链高水位**。
/// 落地一条新记录前要拿同一套判据试算，因此必须把这些"已经用掉的东西"也交出去。
struct Walked {
    state: BTreeMap<String, Entity>,
    report: Report,
    flags: Vec<bool>,
    used_fees: BTreeSet<String>,
    /// (公钥摘要, 窗口号 h/FREE_QUOTA_WINDOW) → 该窗口已接受的免费记录数
    writes: BTreeMap<(String, u64), u32>,
    /// 每个归属者最后一条免费记录的声明块高
    last_v4: BTreeMap<String, u64>,
    /// (x, y) → 已剥掉的层数
    dug: BTreeMap<(u16, u16), u32>,
    /// (地址, 材料) → 存量
    mats: BTreeMap<(String, u8), u32>,
}

fn walk_full(records: &[EntityRecord], view: &ChainView) -> Walked {
    let mut state: BTreeMap<String, Entity> = BTreeMap::new();
    let mut chains: BTreeMap<String, OwnerChain> = BTreeMap::new();
    let mut used_fees: BTreeSet<String> = BTreeSet::new();
    let mut writes: BTreeMap<(String, u64), u32> = BTreeMap::new();
    let mut last_v4: BTreeMap<String, u64> = BTreeMap::new();
    let mut dug: BTreeMap<(u16, u16), u32> = BTreeMap::new();
    let mut mats: BTreeMap<(String, u8), u32> = BTreeMap::new();
    let mut accepted = 0usize;
    let mut skipped = 0usize;
    let mut flags = vec![false; records.len()];
    let mut first: Option<(u64, String, String)> = None;
    for (i, r) in records.iter().enumerate() {
        let ch = chains.entry(r.pk_hash_hex.clone()).or_default();
        if ch.broken {
            skipped += 1;
            continue;
        }
        match apply_record(
            &mut state,
            &mut used_fees,
            &mut writes,
            &mut dug,
            &mut mats,
            ch.accepted,
            &ch.prev,
            ch.last_v4_height,
            r,
            view,
        ) {
            Ok(()) => {
                ch.prev = record_hash(r);
                ch.accepted += 1;
                if is_free_version(&r.v) {
                    *writes
                        .entry((r.pk_hash_hex.clone(), r.height / FREE_QUOTA_WINDOW))
                        .or_insert(0) += 1;
                    ch.last_v4_height = r.height;
                    last_v4.insert(r.pk_hash_hex.clone(), r.height);
                }
                accepted += 1;
                flags[i] = true;
            }
            Err(reason) => {
                ch.broken = true;
                ch.bad_seq = r.seq;
                ch.reason = reason.clone();
                if first.is_none() {
                    first = Some((i as u64 + 1, r.pk_hash_hex.clone(), reason));
                }
                skipped += 1;
            }
        }
    }
    let (first_bad, bad_owner, reason) = match first {
        Some((row, owner, why)) => (row, owner.chars().take(12).collect::<String>(), why),
        None => (0, String::new(), String::new()),
    };
    Walked {
        state,
        report: Report {
            ok: first_bad == 0,
            total: records.len(),
            accepted,
            skipped,
            first_bad,
            bad_owner,
            reason,
        },
        flags,
        used_fees,
        writes,
        last_v4,
        dug,
        mats,
    }
}

/// v4 与 v5 都走"免费 + 窗口配额"这条路；v3 走当年的手续费那条
fn is_free_version(v: &str) -> bool {
    v == RECORD_VERSION || v == RECORD_VERSION_V4
}

fn apply_record(
    state: &mut BTreeMap<String, Entity>,
    used_fees: &mut BTreeSet<String>,
    writes: &mut BTreeMap<(String, u64), u32>,
    dug: &mut BTreeMap<(u16, u16), u32>,
    mats: &mut BTreeMap<(String, u8), u32>,
    want_seq: u64,
    prev: &[u8; 32],
    last_v4_height: u64,
    r: &EntityRecord,
    view: &ChainView,
) -> Result<(), String> {
    /// 三代语义：v3 靠链上付款、v4 免费但有配额、v5 在 v4 之上加挖地与材料
    enum Ver {
        Fee,
        Free,
        World,
    }
    let ver = match r.v.as_str() {
        RECORD_VERSION => Ver::World,
        RECORD_VERSION_V4 => Ver::Free,
        RECORD_VERSION_V3 => Ver::Fee,
        other => return Err(format!("未知记录版本 {other}")),
    };
    let legacy = matches!(ver, Ver::Fee);
    if r.seq != want_seq + 1 {
        return Err(format!(
            "该归属者的序号断裂：期望 {}，实得 {}",
            want_seq + 1,
            r.seq
        ));
    }
    if hex::encode(prev) != r.prev_hash_hex {
        return Err(format!("哈希链断裂：seq {} 的 prev_hash 与其上一条不符", r.seq));
    }
    if r.op == op::DIG {
        // 挖不是造东西：不带名字、不带实体 id、不挂父实体。留出任何一个空位，
        // 账本里就会多一条"看起来像实体"的幽灵记录。
        if !matches!(ver, Ver::World) {
            return Err(format!("seq {} 是 {} 记录，那一代还没有「挖」", r.seq, r.v));
        }
        if !r.name.is_empty() || !r.entity_id_hex.is_empty() || !r.parent_hex.is_empty() {
            return Err(format!("seq {} 的挖地记录不许带名字／实体 id／父实体", r.seq));
        }
    } else if r.name.is_empty() || r.name.len() > NAME_MAX {
        return Err(format!("seq {} 名称长度为 {}，须为 1..={}", r.seq, r.name.len(), NAME_MAX));
    }
    if r.x >= WORLD_W || r.y >= WORLD_H || r.layer >= MAX_LAYER || r.kind >= SLOT_COUNT {
        return Err(format!(
            "seq {} 越界：坐标 ({},{}) 高度层 {} 或槽位 {} 非法",
            r.seq, r.x, r.y, r.layer, r.kind
        ));
    }
    if !matches!(r.op.as_str(), op::CLAIM | op::RENAME | op::MOVE | op::RELEASE | op::DIG) {
        return Err(format!("seq {} 未知操作 {}", r.seq, r.op));
    }
    // —— 密码学四件套：公钥可解 → 摘要自洽 → 签名有效 → 归属可由公钥复算 ——
    let pk = match hex::decode(&r.pk_hex) {
        Ok(b) if b.len() == crypto::PK_LEN => b,
        _ => return Err(format!("seq {} 公钥长度非法", r.seq)),
    };
    let vk = crypto::decode_pk(&pk)
        .ok_or_else(|| format!("seq {} 公钥无法解析为 ML-DSA-65 验证密钥", r.seq))?;
    let pk_hash = crypto::shake256(b"QL-PK-REG-V1", &pk, 32);
    if hex::encode(&pk_hash) != r.pk_hash_hex {
        return Err(format!("seq {} 公钥摘要与记录不符", r.seq));
    }
    let sig = match hex::decode(&r.sig_hex) {
        Ok(b) if b.len() == crypto::SIG_LEN => b,
        _ => return Err(format!("seq {} 签名长度非法", r.seq)),
    };
    let h = record_hash(r);
    if !crypto::verify(&vk, &h[..], &sig) {
        return Err(format!("seq {} 签名无效（记录被篡改或并非该公钥所签）", r.seq));
    }
    let salt = match hex::decode(&r.salt_hex) {
        Ok(b) if b.len() == 8 => [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]],
        _ => return Err(format!("seq {} 地址盐长度非法", r.seq)),
    };
    if crypto::derive_address(&vk, &salt) != r.owner {
        return Err(format!("seq {} 归属地址无法由公钥复算", r.seq));
    }

    // —— 第五件套：写入的代价 ——
    // v3：必须由一笔已确认的链上付款买单（历史口径，永远按当年规则验）。
    // v4：建造免费，但代价换成**确定性速率上限**——块高不得超过链头、
    //     只能向前、同一窗口内的记录数有硬上限。没有这三条，免费写入就是
    //     一台不需要成本的磁盘填充机。
    if legacy {
        let price = entity_fee(&r.op).ok_or_else(|| format!("seq {} 未知操作 {}", r.seq, r.op))?;
        let fee = view.fee(&r.fee_tx_hex).ok_or_else(|| {
            // 记录来自对端：截断必须按字符，不能按字节（非 ASCII 会 panic 成 DoS）
            let short: String = r.fee_tx_hex.chars().take(12).collect();
            format!(
                "seq {} 的手续费交易 {}… 不在链上（未确认／多动作／已被分叉丢弃）",
                r.seq, short
            )
        })?;
        if r.height != fee.height {
            return Err(format!(
                "seq {} 记录块高 {} ≠ 手续费所在块高 {}（块高由链上付款锚定，不能自填）",
                r.seq, r.height, fee.height
            ));
        }
        if fee.from != r.owner {
            return Err(format!("seq {} 的手续费不是归属者本人付的", r.seq));
        }
        if fee.to != fee_addr() {
            return Err(format!("seq {} 的手续费没有付进销毁地址", r.seq));
        }
        if fee.amount < price {
            return Err(format!(
                "seq {} 手续费 {} 不足造价 {}（{}）",
                r.seq, fee.amount, price, r.op
            ));
        }
        if used_fees.contains(&r.fee_tx_hex) {
            return Err(format!("seq {} 的手续费交易已被别的记录用掉", r.seq));
        }
    } else {
        if !r.fee_tx_hex.is_empty() {
            return Err(format!("seq {} 是免费记录，fee_tx_hex 必须为空", r.seq));
        }
        if r.height > view.tip() {
            return Err(format!(
                "seq {} 声明块高 {} 超过链头 {}（免费记录不能预支未来）",
                r.seq,
                r.height,
                view.tip()
            ));
        }
        if r.height < last_v4_height {
            return Err(format!(
                "seq {} 声明块高 {} 低于本链上一条免费记录的 {}（窗口不能回拨）",
                r.seq, r.height, last_v4_height
            ));
        }
        let window = r.height / FREE_QUOTA_WINDOW;
        let used = writes
            .get(&(r.pk_hash_hex.clone(), window))
            .copied()
            .unwrap_or(0);
        if used >= FREE_QUOTA_MAX {
            return Err(format!(
                "seq {} 撞到免费写入配额：块高窗口 {window} 内已写 {used}/{FREE_QUOTA_MAX} 条",
                r.seq
            ));
        }
    }

    match r.op.as_str() {
        op::DIG => {
            if is_water(r.x, r.y) {
                return Err(format!("seq {} 想挖 ({},{})：那是水面，挖不动", r.seq, r.x, r.y));
            }
            let surf = surface_layer(r.x, r.y);
            let removed = dug.get(&(r.x, r.y)).copied().unwrap_or(0);
            let top = surf.saturating_sub(removed.min(u16::MAX as u32) as u16);
            // 岩床：挖到这里就到底了，再往下按空操作处理（不撕链——只是没东西可挖）
            if top < DUG_FLOOR {
                return Ok(());
            }
            // 只许挖露在最上面的那一层。被人抢先挖掉的同一层＝空操作：
            // 记录签得没错、链也没断，只是那格土已经不在了。竞赛不该撕掉一条链。
            if r.layer != top {
                return Ok(());
            }
            let depth = surf - top;
            let want = material_at_depth(depth);
            if r.kind != want {
                return Err(format!(
                    "seq {} 挖 ({},{}) 第 {} 层（离地表 {depth} 层）应得{}，记账写的是 {}",
                    r.seq, r.x, r.y, r.layer, mat_name(want), r.kind
                ));
            }
            *dug.entry((r.x, r.y)).or_insert(0) += 1;
            *mats.entry((r.owner.clone(), want)).or_insert(0) += 1;
        }
        op::CLAIM => {
            let mut pk_hash32 = [0u8; 32];
            pk_hash32.copy_from_slice(&pk_hash);
            let want = derive_entity_id(prev, &pk_hash32, r);
            if hex::encode(want) != r.entity_id_hex {
                return Err(format!("seq {} 实体 id 与内容寻址规则不符", r.seq));
            }
            if state.contains_key(&r.entity_id_hex) {
                return Err(format!("seq {} 重复铸造同一实体", r.seq));
            }
            if !r.parent_hex.is_empty() && !state.contains_key(&r.parent_hex) {
                return Err(format!("seq {} 的父实体不存在", r.seq));
            }
            if let Some(busy) = live_at(state, r.x, r.y, r.layer) {
                return Err(format!(
                    "seq {} 想造的格子 ({},{}) 第 {} 层已经有「{}」",
                    r.seq, r.x, r.y, r.layer, busy.name
                ));
            }
            // v5：建造扣材料。土石矿都从地里挖出来，"一层地换一件东西"。
            //
            // 扣到 0 为止、**不因材料不足判非法**：材料余额取决于别人在不在同一列
            // 抢挖，把它当共识硬闸，两个人抢一格就会给其中一人的链判死刑——
            // 那比"有人无限造东西"更伤世界。硬拦放在钱包侧（`world_place`），
            // 诚实钱包在签名之前就把话说明白；共识只负责把账算准。
            if matches!(ver, Ver::World) {
                let cost = build_cost(r.kind);
                for (i, need) in cost.iter().enumerate() {
                    if *need > 0 {
                        let e = mats.entry((r.owner.clone(), i as u8)).or_insert(0);
                        *e = e.saturating_sub(*need);
                    }
                }
            }
            state.insert(
                r.entity_id_hex.clone(),
                Entity {
                    entity_id_hex: r.entity_id_hex.clone(),
                    name: r.name.clone(),
                    x: r.x,
                    y: r.y,
                    layer: r.layer,
                    kind: r.kind,
                    owner: r.owner.clone(),
                    parent_hex: r.parent_hex.clone(),
                    era: if r.height <= ERA1_END_HEIGHT { 1 } else { 2 },
                    created_height: r.height,
                    created_ts: r.ts,
                    last_height: r.height,
                    released: false,
                },
            );
        }
        op::RENAME | op::MOVE | op::RELEASE => {
            // 挪动前先记下目标格子的占用者：下面拿到 &mut e 以后就不能再读 state 了
            let occupier = if r.op == op::MOVE {
                live_at(state, r.x, r.y, r.layer).map(|o| (o.entity_id_hex.clone(), o.name.clone()))
            } else {
                None
            };
            let Some(e) = state.get_mut(&r.entity_id_hex) else {
                return Err(format!("seq {} 指向的实体不存在", r.seq));
            };
            if e.released {
                return Err(format!("seq {} 指向已释放归属的实体", r.seq));
            }
            if e.owner != r.owner {
                return Err(format!("seq {} 签名者不是该实体当前归属者", r.seq));
            }
            match r.op.as_str() {
                op::RENAME => e.name = r.name.clone(),
                op::MOVE => {
                    if let Some((id, name)) = &occupier {
                        if id != &r.entity_id_hex {
                            return Err(format!(
                                "seq {} 要挪到的格子 ({},{}) 第 {} 层已经有「{}」",
                                r.seq, r.x, r.y, r.layer, name
                            ));
                        }
                    }
                    e.x = r.x;
                    e.y = r.y;
                    e.layer = r.layer;
                }
                _ => {
                    // release：归属置空，实体永不删除（§9.9 断租不删除）
                    e.owner = String::new();
                    e.released = true;
                }
            }
            e.last_height = r.height;
        }
        other => return Err(format!("未知操作 {}", other)),
    }
    // 走到这里这条记录才算真的生效：v3 的付款同时标记为已用（一笔付款只能买一次写入）；
    // v4 的窗口计数与块高水位由 `walk_full` 在同一个 Ok 分支里推进。
    if legacy {
        used_fees.insert(r.fee_tx_hex.clone());
    }
    Ok(())
}

/// 每个非空 chunk 的承诺根（实体 id 按字典序拼接，顺序无关 → 集合可独立重算）
pub fn chunk_roots(state: &BTreeMap<String, Entity>) -> Vec<Chunk> {
    let mut groups: BTreeMap<(u16, u16), Vec<String>> = BTreeMap::new();
    for e in state.values() {
        if e.released {
            continue;
        }
        groups.entry((e.x / CHUNK, e.y / CHUNK)).or_default().push(e.entity_id_hex.clone());
    }
    groups
        .into_iter()
        .map(|((cx, cy), mut ids)| {
            ids.sort();
            let mut buf = Vec::with_capacity(4 + ids.len() * 64);
            buf.extend_from_slice(&cx.to_be_bytes());
            buf.extend_from_slice(&cy.to_be_bytes());
            for id in &ids {
                buf.extend_from_slice(id.as_bytes());
            }
            let root = crypto::shake256(CHUNK_PREFIX, &buf, 32);
            Chunk { cx, cy, count: ids.len(), root_hex: hex::encode(root) }
        })
        .collect()
}

// ================================================================ 创造权重（§9.16）
//
// 「创造没有好坏，所以不评价创造」——这条是整个设计的起点。任何评分、点赞、
// 排行榜都需要一个裁判，而裁判既是审查者又是刷分目标，并且**跨实现不可复算**，
// 因此永远进不了共识。共识只认两种量得起的事实：
//
// 1. **占了几格**：一格一主由 `live_at` 在重放里强制（`CLAIM`/`MOVE` 都要查），
//    释放（`RELEASE`）即不再计入——记录永不删除，但"你不在了就不算"；
// 2. **守了多少个块**：`tip − created_height`，只能等，不能刷。脚本可以零成本
//    写满 4096 条记录，却刷不出 64 格各自的 4000 个块——时间是唯一不可压缩的成本。
//
// 权重唯一的作用是**发行节奏**：分数越高，两次挖矿发行的最小间隔越短（50→10 块），
// 而每笔发行最多结算一个当前档位的量（`reward_at(h) × Δ`），空档不补发。于是每块
// 平均发行量 ≤ `reward_at(h)`，减半曲线与 210M 硬顶一字不改——创造换到的是"更快看到进度"
// 和更多次抢占机会，不是更多的币。这条不变量由 simchain 的性质测试钉死。

/// 一格守满这么多块记 1 分
pub const CRAFT_KEEP_UNIT: u64 = 200;
/// 单格保护时长封顶：一格吃老本不能永久躺赢
pub const CRAFT_KEEP_CAP: u64 = 4_000;
/// 最多计这么多格：写入配额是 4096 条/窗口，权重只数 64 格
pub const CRAFT_CELL_CAP: usize = 64;
/// 发行节奏阶梯（块）。下标 = 权重分（封顶 7）；0 分即 v1/v2 的 50 块冷却。
pub const MINE_FLOORS: [u64; 8] = [50, 40, 30, 25, 20, 16, 12, 10];

/// 某个地址此刻在世界里的创造权重（纯函数：世界状态 + 链头高度）
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Craft {
    /// 活着的自有格子总数（含超出计分上限的部分）
    pub cells: usize,
    /// 实际参与计分的格子数（≤ `CRAFT_CELL_CAP`）
    pub graded: usize,
    /// 这些格子累计的守护块数（单格封顶后求和）
    pub kept_blocks: u64,
    /// 权重分 = `kept_blocks / CRAFT_KEEP_UNIT`，封顶 `MINE_FLOORS.len()-1`
    pub score: u8,
    /// 由分数决定的两次挖矿发行最小间隔（块）
    pub floor: u64,
}

/// 「守了多久」的起算点是**落子那一刻**（`created_height`），不是最后一次互动：
/// 世界不需要靠心跳按钮来证明有人在，越要求互动就越奖励挂机脚本。
/// 掉下去的代价走另一条路——释放即失分，未来的租约则让"占着"本身持续花钱。
pub fn craft_of(state: &BTreeMap<String, Entity>, owner: &str, tip: u64) -> Craft {
    let mut kept: Vec<u64> = Vec::new();
    for e in state.values() {
        if e.released || e.owner != owner {
            continue;
        }
        kept.push(tip.saturating_sub(e.created_height).min(CRAFT_KEEP_CAP));
    }
    // BTreeMap 按实体 id 字典序遍历，`sort_by` 稳定 → 并列格子永远取 id 小的那批：
    // 任何一份实现、任何一次重放都选同一批，权重因此可被第三方独立复算。
    kept.sort_by(|a, b| b.cmp(a));
    let cells = kept.len();
    let graded = cells.min(CRAFT_CELL_CAP);
    let kept_blocks: u64 = kept.iter().take(graded).sum();
    // 链龄闸门：每 CRAFT_KEEP_UNIT 个块最多解锁 1 分。脚本可以在一个窗口里造满
    // 64 格，但世界的时间只有一份——新钱包无论怎么刷，都刷不出"这里已经存在了很久"。
    let raw = kept_blocks / CRAFT_KEEP_UNIT;
    let score = raw.min(tip / CRAFT_KEEP_UNIT).min((MINE_FLOORS.len() - 1) as u64) as u8;
    Craft {
        cells,
        graded,
        kept_blocks,
        score,
        floor: MINE_FLOORS[score as usize],
    }
}

/// 合并邻居中继记录的结算单（M0「彼此看见」的账面上的那一笔）
#[derive(Debug, Default, Clone, Serialize)]
pub struct MergeReport {
    /// 真正接上链、已落盘的条数
    pub accepted: usize,
    /// 本机早已见过（内容哈希相同）→ 幂等跳过
    pub known: usize,
    /// 拒收：接不上链 / 验签不过 / 格子冲突
    pub rejected: usize,
}

/// 本机这把签名钥匙的公钥摘要——实体链的识别键就是它（不是地址：地址可由它复算）
pub fn signer_pk_hash(core: &crate::keystore::WalletCore) -> String {
    hex::encode(crypto::shake256(b"QL-PK-REG-V1", &core.pk_bytes, 32))
}

// ---------------------------------------------------------------- 落盘

/// append-only 实体账本。写锁只保护"追加一行"这一步；读侧永远整本重放，无缓存可失效。
pub struct EntityLedger {
    path: PathBuf,
    lock: std::sync::Mutex<()>,
    /// 最近一次整本读入跳过的坏行数（0 = 账本逐行都能解析）
    bad: BadLines,
}

/// 整本读入的附带说明：解析不了的行**逐行跳过**并计数，绝不从那里截断。
///
/// 旧口径是遇到坏行就停：联邦世界里各归属者的记录交错在同一本文件里，
/// 中间一行坏了会把后面所有人的全部历史一起静默丢掉，账本"看起来"只是变短了。
/// 跳过后坏行只伤它自己：受影响那条链的 `prev_hash` 接不上，`walk` 当场判它断链；
/// 其余各链照常重放。判定依旧唯一，只是不再拿"解析失败"冒充"链到此为止"。
#[derive(Debug, Default, Clone, Serialize)]
pub struct LoadReport {
    /// 跳过的无法解析的行数
    pub bad_lines: usize,
    /// 第一处坏行的行号（1 起；0 = 没有坏行）
    pub first_line: usize,
    /// 第一处坏行的解析错误原文，供 UI 与日志直显，不翻译成委婉语
    pub reason: String,
}

impl LoadReport {
    pub fn is_clean(&self) -> bool {
        self.bad_lines == 0
    }
}

/// 逐行读一本 jsonl：坏行只跳过它自己并计数，**绝不**从那里截断尾部。
/// 实体账本与光尘账本共用这一份实现，两本不允许各写一套读法。
pub(crate) fn parse_lines<T: serde::de::DeserializeOwned>(text: &str) -> (Vec<T>, LoadReport) {
    let mut rep = LoadReport::default();
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<T>(line) {
            Ok(r) => out.push(r),
            Err(e) => {
                rep.bad_lines += 1;
                if rep.first_line == 0 {
                    rep.first_line = i + 1;
                    rep.reason = e.to_string();
                }
            }
        }
    }
    (out, rep)
}

/// 坏行计数的常驻监视位：账本的 `bad_lines()` 从这里读，UI 的告警条也从这里读。
#[derive(Default)]
pub(crate) struct BadLines {
    seen: std::sync::atomic::AtomicUsize,
    /// 已经就该数值告过警——轮询每次都整本重放，不控制就会刷屏
    warned: std::sync::atomic::AtomicUsize,
}

impl BadLines {
    pub fn count(&self) -> usize {
        self.seen.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn note(&self, rep: &LoadReport, what: &str, path: &Path) {
        use std::sync::atomic::Ordering;
        self.seen.store(rep.bad_lines, Ordering::Relaxed);
        let was = self.warned.swap(rep.bad_lines, Ordering::Relaxed);
        if rep.bad_lines == 0 || was == rep.bad_lines {
            return;
        }
        eprintln!(
            "[QLight] {what}有 {} 行解析失败（首处第 {} 行：{}），已逐行跳过、继续读后面的记录：{}",
            rep.bad_lines,
            rep.first_line,
            rep.reason,
            path.display()
        );
    }
}

impl EntityLedger {
    pub fn new(path: PathBuf) -> Self {
        let led = Self {
            path,
            lock: std::sync::Mutex::new(()),
            bad: BadLines::default(),
        };
        led.archive_if_legacy();
        led
    }

    /// v2 → v3 → v4 迁移：v3 之前的老记录没有手续费凭据／新语义，按新规则一条都验不过；
    /// v3 记录**保留**（老账本还能原样重放，只是新写入一律走 v4）。
    ///
    /// **不删除**：整本改名留档（谁都能拿它离线重算旧世界），世界从空开始。
    /// 这一刀是诚实的代价：写入代价的定义改过，而旧世界里的每一个方块付的是旧规则的钱。
    fn archive_if_legacy(&self) {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return };
        let legacy = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v.get("v").and_then(|x| x.as_str()).map(str::to_string))
            .any(|v| v != RECORD_VERSION && v != RECORD_VERSION_V4 && v != RECORD_VERSION_V3);
        if !legacy {
            return;
        }
        let tag = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let to = self.path.with_extension(format!("jsonl.bak-v2-{tag}"));
        if std::fs::rename(&self.path, &to).is_ok() {
            eprintln!(
                "[QLight] 实体账本升级到 {RECORD_VERSION}（建造免费 + 窗口配额）；旧账本留档：{}",
                to.display()
            );
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 整本读入；文件不存在即空世界。坏行只跳过它自己并计数（见 [`LoadReport`]）。
    pub fn load(&self) -> Vec<EntityRecord> {
        self.load_report().0
    }

    /// 最近一次整本读入跳过的坏行数。计数是"最后一次读的结果"，不是累计：
    /// 账本修好了它就回到 0，UI 上的告警也跟着消失。
    pub fn bad_lines(&self) -> usize {
        self.bad.count()
    }

    /// 整本读入 + 读到了什么不该读到的东西。上层要显式调用这一版才能拿到坏行数；
    /// `load()` 已经把同一个数存进 `bad_lines()`，快照与告警都从那里取。
    pub fn load_report(&self) -> (Vec<EntityRecord>, LoadReport) {
        let (recs, rep) = match std::fs::read_to_string(&self.path) {
            Ok(text) => parse_lines::<EntityRecord>(&text),
            Err(_) => (Vec::new(), LoadReport::default()),
        };
        self.bad.note(&rep, "实体账本", &self.path);
        (recs, rep)
    }

    /// 追加一条记录（自取写锁）
    pub fn append(&self, r: &EntityRecord) -> Result<(), String> {
        let _guard = self.lock.lock().map_err(|_| "写锁中毒".to_string())?;
        write_line(&self.path, r)
    }

    /// 某归属者（按公钥摘要识别）当前的链头：`(已接受条数, 最后一条哈希)`。
    ///
    /// 只走"链自洽的前缀"：一旦某条与它的 `prev_hash` 不符就停在那里，同主后续
    /// 一律不算数——与 `walk` 同一判据，绝不写两套。
    pub fn head_of(records: &[EntityRecord], pk_hash_hex: &str) -> (u64, [u8; 32]) {
        let mut seq = 0u64;
        let mut prev = [0u8; 32];
        for r in records.iter().filter(|r| r.pk_hash_hex == pk_hash_hex) {
            if r.seq != seq + 1 || hex::encode(prev) != r.prev_hash_hex {
                break;
            }
            prev = record_hash(r);
            seq += 1;
        }
        (seq, prev)
    }

    /// 读—签名—追加必须是一个整体：否则两个并发 claim 会拿到同一个 seq，
    /// 后写的那条直接撕断哈希链。build 收到 (seq, prev_hash)，返回完整记录。
    ///
    /// 序号与 prev 都按**本机这把公钥自己的链**取，不再是整本文件的下标：
    /// 联邦世界里别人的记录会插进同一本文件，全局下标会把所有人一起判成断链。
    pub fn commit_as(
        &self,
        pk_hash_hex: &str,
        view: &ChainView,
        build: impl FnOnce(u64, [u8; 32]) -> EntityRecord,
    ) -> Result<EntityRecord, String> {
        let _guard = self.lock.lock().map_err(|_| "写锁中毒".to_string())?;
        let recs = self.load();
        let (seq, prev) = Self::head_of(&recs, pk_hash_hex);
        let r = build(seq + 1, prev);
        // 落盘前用与重放**完全同一套判据**试算一次：绝不写一条自己都验不过的记录
        // （v4：块高超过链头、窗口回拨、配额用尽，都当场退回）
        let mut w = walk_full(&recs, view);
        let last_v4 = w.last_v4.get(pk_hash_hex).copied().unwrap_or(0);
        apply_record(
            &mut w.state,
            &mut w.used_fees,
            &mut w.writes,
            &mut w.dug,
            &mut w.mats,
            seq,
            &prev,
            last_v4,
            &r,
            view,
        )?;
        write_line(&self.path, &r)?;
        Ok(r)
    }

    /// 合并邻居中继来的实体记录：逐条按同一判据严格验签验链，**只收下真正接得上的**。
    ///
    /// 已见过的（内容哈希相同）跳过；接不上的（号不对、链不对、签名不对、格子冲突）
    /// 拒收且不落盘——本机账本永远不会因为一个坏邻居而长出一段断链。
    pub fn merge(&self, incoming: &[EntityRecord], view: &ChainView) -> Result<MergeReport, String> {
        let _guard = self.lock.lock().map_err(|_| "写锁中毒".to_string())?;
        let mut out = MergeReport::default();
        let mine = self.load();
        let mut seen: BTreeSet<String> = mine.iter().map(|r| hex::encode(record_hash(r))).collect();
        let mut fresh: Vec<&EntityRecord> = Vec::new();
        for r in incoming {
            if seen.insert(hex::encode(record_hash(r))) {
                fresh.push(r);
            } else {
                out.known += 1;
            }
        }
        if fresh.is_empty() {
            return Ok(out);
        }
        // 同一归属者必须按 seq 升序进场，否则后面那条永远接不上
        fresh.sort_by(|a, b| (a.pk_hash_hex.as_str(), a.seq).cmp(&(b.pk_hash_hex.as_str(), b.seq)));

        let mut all = mine;
        let base = all.len();
        all.extend(fresh.iter().map(|r| (*r).clone()));
        let (_, _, flags) = walk(&all, view);
        for (i, ok) in flags[base..].iter().enumerate() {
            if *ok {
                write_line(&self.path, fresh[i])?;
                out.accepted += 1;
            } else {
                out.rejected += 1;
            }
        }
        Ok(out)
    }
}

/// 单次 write_all 整行 + fsync：kill -9 不会撕行（与 append_block_header 同款）
fn write_line(path: &Path, r: &EntityRecord) -> Result<(), String> {
    use std::io::{Read, Seek, Write};
    let json = serde_json::to_string(r).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("打开实体账本失败：{e}"))?;
    // 上一笔可能被断电拦腰截断（结尾没有换行）。直接续写会把新旧两条黏成一行，
    // 于是一条坏行变成两条坏行。补一个换行，让已经坏掉的那行只坏它自己。
    let mut line = String::new();
    let len = f.metadata().map_err(|e| format!("读账本长度失败：{e}"))?.len();
    if len > 0 {
        let mut last = [0u8; 1];
        f.seek(std::io::SeekFrom::End(-1))
            .map_err(|e| format!("定位账本末尾失败：{e}"))?;
        f.read_exact(&mut last)
            .map_err(|e| format!("读账本末尾失败：{e}"))?;
        if last[0] != b'\n' {
            line.push('\n');
        }
    }
    line.push_str(&json);
    line.push('\n');
    f.write_all(line.as_bytes()).map_err(|e| format!("写入失败：{e}"))?;
    f.sync_all().ok();
    Ok(())
}

/// 铸造/操作一条记录所需的最小输入（其余字段由后端补齐并签名）
#[derive(Clone, Copy)]
pub struct Draft<'a> {
    pub op: &'a str,
    pub entity_id_hex: &'a str,
    pub parent_hex: &'a str,
    pub name: &'a str,
    pub x: u16,
    pub y: u16,
    pub layer: u16,
    pub kind: u8,
    /// 写入时取到的链头高度（v4 只用来定窗口与 era，不能超过链头）
    pub height: u64,
    pub ts: u64,
}

/// 用解锁核心签一条实体记录（与 send_qlx 同一把 ML-DSA-65 私钥，同一套确定性签名）
pub fn sign_record(
    core: &crate::keystore::WalletCore,
    seq: u64,
    prev: &[u8; 32],
    d: &Draft,
) -> EntityRecord {
    let pk_hash = crypto::shake256(b"QL-PK-REG-V1", &core.pk_bytes, 32);
    let mut pk_hash32 = [0u8; 32];
    pk_hash32.copy_from_slice(&pk_hash);

    let mut r = EntityRecord {
        v: RECORD_VERSION.into(),
        seq,
        op: d.op.into(),
        entity_id_hex: d.entity_id_hex.into(),
        parent_hex: d.parent_hex.into(),
        name: d.name.into(),
        x: d.x,
        y: d.y,
        layer: d.layer,
        kind: d.kind,
        height: d.height,
        ts: d.ts,
        owner: core.address.clone(),
        pk_hash_hex: hex::encode(pk_hash32),
        salt_hex: hex::encode(core.addr_salt),
        prev_hash_hex: hex::encode(prev),
        fee_tx_hex: String::new(),
        pk_hex: hex::encode(&core.pk_bytes),
        sig_hex: String::new(),
    };
    if d.op == op::CLAIM {
        r.entity_id_hex = hex::encode(derive_entity_id(prev, &pk_hash32, &r));
    }
    let h = record_hash(&r);
    r.sig_hex = hex::encode(crypto::sign(&core.signing_key, &h));
    r
}

/// 首条记录的 prev_hash（供测试与前端展示锚点）
pub fn genesis_prev() -> &'static str {
    ZERO_HASH
}

/// 从 `blocks.jsonl` 里按块序取出每块的 VRF（地表重画的唯一输入）
pub fn chain_vrfs(blocks_path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(blocks_path) else { return Vec::new() };
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if let Some(s) = v.get("vrf_hex").and_then(|x| x.as_str()) {
            if s.len() == 64 {
                out.push(s.to_string());
            }
        }
    }
    out
}

/// 地表种子 `words`：渲染器与矿脉层共用这一份（同一片世界）
pub fn chain_words(blocks_path: &Path) -> Vec<u32> {
    terrain::seed_words(&crate::simchain::genesis_hash_hex(), &chain_vrfs(blocks_path))
}

/// 资源层种子：**只取创世块**，不折任何块 VRF。
///
/// 采集记录（`QL-DUST-v1`）里存的是坐标和种类，重放时要用 `resource::node_at`
/// 按种子重算一遍来判定真假——所以这份种子必须永远不变，才能被任意时刻、
/// 任意邻居复现出同一个结果。`chain_words` 每出一个块就把地表湿度重画一次，
/// 拿它算资源会让手上那条记录在下一个块失效、余额凭空归零。
pub fn resource_words() -> Vec<u32> {
    terrain::seed_words(&crate::simchain::genesis_hash_hex(), &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> crate::keystore::WalletCore {
        let seed = [7u8; 32];
        crate::keystore::open_core_from_seed(&seed, &[1u8; 8]).unwrap()
    }

    fn tmp_ledger(tag: &str) -> EntityLedger {
        let mut p = std::env::temp_dir();
        p.push(format!("qlight-ent-{}-{}.jsonl", tag, std::process::id()));
        let _ = std::fs::remove_file(&p);
        EntityLedger::new(p)
    }

    #[test]
    fn resource_words_do_not_drift_when_new_blocks_land() {
        // 采集记录只存坐标和种类，真假全靠种子重算。地表种子每出一个块就重画，
        // 资源层若跟着它走，采到手的记录下个块就会被判无效、余额凭空归零。
        let mut p = std::env::temp_dir();
        p.push(format!("qlight-blocks-{}.jsonl", std::process::id()));
        std::fs::write(&p, "").unwrap();
        let stable = resource_words();
        let surface_at_genesis = chain_words(&p);
        std::fs::write(&p, format!("{{\"vrf_hex\":\"{}\"}}\n", "11".repeat(32))).unwrap();
        assert_eq!(stable, resource_words(), "资源种子不能随出块变化");
        assert_ne!(
            surface_at_genesis,
            chain_words(&p),
            "对照：地表种子确实会随出块变化，否则这条测试是空转"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// v4 的链视图：只带链头高度（免费写入不能超过它）
    fn view_at(tip: u64) -> ChainView {
        ChainView { fees: BTreeMap::new(), tip }
    }

    fn commit(
        ledger: &EntityLedger,
        view: &ChainView,
        c: &crate::keystore::WalletCore,
        d: &Draft,
    ) -> EntityRecord {
        ledger
            .commit_as(&signer_pk_hash(c), view, |seq, prev| sign_record(c, seq, &prev, d))
            .unwrap()
    }

    /// 绕过落盘前预检强写一条：模拟坏邻居 / 被手改过的旧文件
    fn force_write(
        ledger: &EntityLedger,
        c: &crate::keystore::WalletCore,
        d: &Draft,
    ) -> EntityRecord {
        let (seq, prev) = EntityLedger::head_of(&ledger.load(), &signer_pk_hash(c));
        let r = sign_record(c, seq + 1, &prev, d);
        write_line(ledger.path(), &r).unwrap();
        r
    }

    /// v3 老记录的签名器：v3 账本必须还能原样重放（新写入一律走 v4）
    fn sign_v3(
        c: &crate::keystore::WalletCore,
        seq: u64,
        prev: &[u8; 32],
        d: &Draft,
        fee_tx_hex: &str,
    ) -> EntityRecord {
        let pk_hash = crypto::shake256(b"QL-PK-REG-V1", &c.pk_bytes, 32);
        let mut pk_hash32 = [0u8; 32];
        pk_hash32.copy_from_slice(&pk_hash);
        let mut r = EntityRecord {
            v: RECORD_VERSION_V3.into(),
            seq,
            op: d.op.into(),
            entity_id_hex: d.entity_id_hex.into(),
            parent_hex: d.parent_hex.into(),
            name: d.name.into(),
            x: d.x,
            y: d.y,
            layer: d.layer,
            kind: d.kind,
            height: d.height,
            ts: d.ts,
            owner: c.address.clone(),
            pk_hash_hex: hex::encode(pk_hash32),
            salt_hex: hex::encode(c.addr_salt),
            prev_hash_hex: hex::encode(prev),
            fee_tx_hex: fee_tx_hex.to_string(),
            pk_hex: hex::encode(&c.pk_bytes),
            sig_hex: String::new(),
        };
        if d.op == op::CLAIM {
            r.entity_id_hex = hex::encode(derive_entity_id(prev, &pk_hash32, &r));
        }
        let h = record_hash(&r);
        r.sig_hex = hex::encode(crypto::sign(&c.signing_key, &h));
        r
    }

    /// 造一笔「已确认的销毁付款」凭据（只服务 v3 重放测试）
    fn pay_at(
        view: &mut ChainView,
        c: &crate::keystore::WalletCore,
        height: u64,
        amount: u64,
    ) -> String {
        let digest = hex::encode(crypto::shake256(
            b"QL-TEST-FEE",
            format!("{}-{height}-{amount}-{}", c.address, view.len()).as_bytes(),
            32,
        ));
        view.insert(
            digest.clone(),
            FeeProof { height, from: c.address.clone(), to: fee_addr(), amount },
        );
        digest
    }

    fn pay(view: &mut ChainView, c: &crate::keystore::WalletCore, height: u64) -> String {
        pay_at(view, c, height, entity_fee(op::CLAIM).unwrap())
    }

    #[test]
    fn claim_then_rename_then_release_replays_clean() {
        let c = core();
        let ledger = tmp_ledger("flow");
        let view = view_at(40);

        let r1 = commit(
            &ledger,
            &view,
            &c,
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "创世塔",
                x: 40, y: 60, layer: 0, kind: 4, height: 12, ts: 1,
            },
        );
        assert_eq!(r1.seq, 1);
        assert_eq!(r1.prev_hash_hex, genesis_prev());
        assert_eq!(r1.fee_tx_hex, "", "v4 建造免费，凭据字段必须为空");

        let r2 = commit(
            &ledger,
            &view,
            &c,
            &Draft {
                op: op::RENAME, entity_id_hex: &r1.entity_id_hex, parent_hex: "", name: "改名塔",
                x: 0, y: 0, layer: 0, kind: 0, height: 20, ts: 2,
            },
        );
        assert_eq!(r2.prev_hash_hex, hex::encode(record_hash(&r1)));

        commit(
            &ledger,
            &view,
            &c,
            &Draft {
                op: op::MOVE, entity_id_hex: &r1.entity_id_hex, parent_hex: "", name: "改名塔",
                x: 41, y: 61, layer: 0, kind: 0, height: 25, ts: 3,
            },
        );

        let (state, rep) = replay(&ledger.load(), &view);
        assert!(rep.ok, "重放应通过：{}", rep.reason);
        let e = state.get(&r1.entity_id_hex).unwrap();
        assert_eq!(e.name, "改名塔");
        assert_eq!((e.x, e.y), (41, 61));
        assert_eq!(e.era, 1);
        assert!(!e.released);

        commit(
            &ledger,
            &view,
            &c,
            &Draft {
                op: op::RELEASE, entity_id_hex: &r1.entity_id_hex, parent_hex: "", name: "改名塔",
                x: 41, y: 61, layer: 0, kind: 0, height: 30, ts: 4,
            },
        );
        let (state, rep) = replay(&ledger.load(), &view);
        assert!(rep.ok, "{}", rep.reason);
        let e = state.get(&r1.entity_id_hex).unwrap();
        // §9.9：断租不删除——实体仍在，只是没有归属
        assert!(e.released && e.owner.is_empty());
        assert_eq!(e.name, "改名塔");
        let _ = std::fs::remove_file(ledger.path());
    }

    #[test]
    fn entity_id_binds_the_creation_content() {
        let c = core();
        let ledger = tmp_ledger("tamper");
        let view = view_at(3);
        let mut r = commit(
            &ledger,
            &view,
            &c,
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "不可篡改",
                x: 5, y: 5, layer: 0, kind: 1, height: 3, ts: 1,
            },
        );
        // 改名字 → record_hash 变、签名失效。这里连"改完再自签一次"都试了：
        // id 由铸造内容派生，创建者自己也无法悄悄改写已经存在的那条历史。
        r.name = "篡改名".into();
        r.sig_hex = hex::encode(crypto::sign(&c.signing_key, &record_hash(&r)));
        let (state, rep) = replay(&[r], &view);
        assert!(!rep.ok);
        assert!(rep.reason.contains("实体 id"), "{}", rep.reason);
        assert!(state.is_empty());
        let _ = std::fs::remove_file(ledger.path());
    }

    #[test]
    fn only_the_owner_can_move_it() {
        let alice = crate::keystore::open_core_from_seed(&[1u8; 32], &[2u8; 8]).unwrap();
        let bob = crate::keystore::open_core_from_seed(&[3u8; 32], &[4u8; 8]).unwrap();
        let ledger = tmp_ledger("owner");
        let view = view_at(10);
        let r1 = commit(
            &ledger,
            &view,
            &alice,
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "Alice 的树",
                x: 9, y: 9, layer: 0, kind: 0, height: 4, ts: 1,
            },
        );
        // Bob 自己签字，想挪走 Alice 的树——强写进账本（模拟坏邻居）
        force_write(
            &ledger,
            &bob,
            &Draft {
                op: op::MOVE, entity_id_hex: &r1.entity_id_hex, parent_hex: "", name: "抢过来的树",
                x: 10, y: 10, layer: 0, kind: 0, height: 5, ts: 2,
            },
        );
        let (state, rep) = replay(&ledger.load(), &view);
        assert!(!rep.ok);
        assert!(rep.reason.contains("不是该实体当前归属者"), "{}", rep.reason);
        assert_eq!(rep.first_bad, 2);
        // 失败前的状态仍然可用：世界不会因为一条坏记录而消失
        assert_eq!(state.get(&r1.entity_id_hex).unwrap().name, "Alice 的树");
        let _ = std::fs::remove_file(ledger.path());
    }

    /// M0「彼此看见」：邻居的记录合并进来后，两条链各自成立、互不干扰；
    /// 伪造的那条被拒，已见过的重复不会长第二份。
    #[test]
    fn peer_records_merge_into_independent_chains() {
        let alice = crate::keystore::open_core_from_seed(&[11u8; 32], &[12u8; 8]).unwrap();
        let bob = crate::keystore::open_core_from_seed(&[13u8; 32], &[14u8; 8]).unwrap();
        // 各自的本机账本：两条独立的链；双方共用同一个链视图（同一个链头）
        let la = tmp_ledger("xmerge-a");
        let lb = tmp_ledger("xmerge-b");
        let view = view_at(9);
        let a1 = commit(
            &la,
            &view,
            &alice,
            &Draft { op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "A 的灯塔", x: 5, y: 5, layer: 0, kind: 0, height: 7, ts: 7 },
        );
        let _a2 = commit(
            &la,
            &view,
            &alice,
            &Draft { op: op::RENAME, entity_id_hex: &a1.entity_id_hex, parent_hex: "", name: "A 的灯塔二号", x: 5, y: 5, layer: 0, kind: 0, height: 8, ts: 8 },
        );
        let b1 = commit(
            &lb,
            &view,
            &bob,
            &Draft { op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "B 的篝火", x: 6, y: 6, layer: 0, kind: 0, height: 9, ts: 9 },
        );
        assert_eq!(b1.seq, 1, "B 的第一条必须从自己的 1 开始，不是接着 A 的 3");

        // Alice 收到 Bob 中继的记录
        let rep = la.merge(&lb.load(), &view).unwrap();
        assert_eq!((rep.accepted, rep.known, rep.rejected), (1, 0, 0), "{:?}", rep);
        let (state, report) = replay(&la.load(), &view);
        assert!(report.ok, "{}", report.reason);
        assert_eq!(report.total, 3);
        assert_eq!(state.len(), 2);
        assert_eq!(state.get(&b1.entity_id_hex).unwrap().owner, bob.address);

        // 重放同一批：幂等，不长出第二份
        let rep2 = la.merge(&lb.load(), &view).unwrap();
        assert_eq!((rep2.accepted, rep2.known), (0, 1), "{:?}", rep2);
        assert_eq!(la.load().len(), 3);

        // 伪造一条（改名字但签名不动）：必须拒收，且落盘的账本依然 ok
        let mut fake = b1.clone();
        fake.name = "偷来的篝火".into();
        let rep3 = la.merge(&[fake], &view).unwrap();
        assert_eq!((rep3.accepted, rep3.rejected), (0, 1), "{:?}", rep3);
        let (_, report3) = replay(&la.load(), &view);
        assert!(report3.ok, "伪造记录污染了账本：{}", report3.reason);

        let _ = std::fs::remove_file(la.path());
        let _ = std::fs::remove_file(lb.path());
    }

    #[test]
    fn hash_chain_gap_is_caught() {
        let c = core();
        let ledger = tmp_ledger("gap");
        let view = view_at(3);
        for i in 0..3u16 {
            commit(
                &ledger,
                &view,
                &c,
                &Draft {
                    op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "塔",
                    x: i, y: i, layer: 0, kind: 0, height: i as u64 + 1, ts: i as u64,
                },
            );
        }
        // 手工撕掉中间一条：序号与哈希链双重暴露
        let mut recs = ledger.load();
        assert_eq!(recs.len(), 3);
        recs.remove(1);
        let (_, rep) = replay(&recs, &view);
        assert!(!rep.ok);
        assert!(rep.reason.contains("序号断裂"), "{}", rep.reason);
        // first_bad 是**行位**：撕掉的就是第 2 行（多方世界里各链各有各的 seq，
        // 只有行位无歧义）
        assert_eq!(rep.first_bad, 2);
        let _ = std::fs::remove_file(ledger.path());
    }

    /// 中间一行坏了只丢它自己那一行，**不许**把后面的历史一起静默吞掉。
    ///
    /// 旧口径遇到坏行就 `break`：联邦世界里各归属者的记录交错在同一本文件里，
    /// 一次断电截断会把后面所有人的全部历史判成"从没发生过"，而账本看起来只是
    /// 短了一点——没有任何地方说它短了。现在坏行计数并原样上报。
    #[test]
    fn a_corrupt_line_in_the_middle_does_not_truncate_the_tail() {
        let c = core();
        let ledger = tmp_ledger("corrupt-middle");
        let view = view_at(3);
        for i in 0..3u16 {
            commit(
                &ledger,
                &view,
                &c,
                &Draft {
                    op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "塔",
                    x: 100 + i, y: 100, layer: 0, kind: 0, height: i as u64 + 1, ts: i as u64,
                },
            );
        }
        let good: Vec<String> = std::fs::read_to_string(ledger.path())
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(good.len(), 3);
        // 第 2 行换成半截 JSON（断电截断的形状），第 3 行原样留在它后面
        std::fs::write(
            ledger.path(),
            format!("{}\n{{\"v\":\"QL-ENTREC-v4\",\"seq\":\n{}\n", good[0], good[2]),
        )
        .unwrap();

        let (recs, rep) = ledger.load_report();
        assert_eq!(rep.bad_lines, 1, "只坏了一行");
        assert_eq!(rep.first_line, 2, "首处坏行的行号必须是真行号");
        assert!(!rep.is_clean());
        assert_eq!(ledger.bad_lines(), 1, "上层（快照/UI）读的是这个数");
        assert_eq!(recs.len(), 2, "坏行之后的历史不许跟着消失");
        assert_eq!(recs[1].seq, 3, "尾条仍是第 3 条，不是被截断后的第 1 条");

        // 反向对照：账本修好后计数回到 0，告警不会赖在界面上
        std::fs::write(ledger.path(), format!("{}\n", good.join("\n"))).unwrap();
        assert_eq!(ledger.load().len(), 3);
        assert_eq!(ledger.bad_lines(), 0);
        let _ = std::fs::remove_file(ledger.path());
    }

    #[test]
    fn a_rewritten_line_breaks_the_hash_chain() {
        let c = core();
        let ledger = tmp_ledger("rewritten");
        let view = view_at(3);
        for i in 0..2u16 {
            commit(
                &ledger,
                &view,
                &c,
                &Draft {
                    op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "塔",
                    x: i, y: i, layer: 0, kind: 0, height: i as u64 + 1, ts: i as u64,
                },
            );
        }
        // 序号仍连续，只把第二行的 prev_hash 换成创世锚 → 唯一能抓住它的是哈希链
        let mut recs = ledger.load();
        recs[1].prev_hash_hex = genesis_prev().into();
        let (_, rep) = replay(&recs, &view);
        assert!(!rep.ok);
        assert!(rep.reason.contains("哈希链断裂"), "{}", rep.reason);
        assert_eq!(rep.first_bad, 2);
        let _ = std::fs::remove_file(ledger.path());
    }

    /// Minecraft 的核心是往上叠：同一格不同高度层是**不同的位置**，
    /// 但同一格同一层只能有一个活着的方块——释放归属后那一格才重新空出来。
    #[test]
    fn blocks_stack_by_layer_and_one_voxel_holds_only_one() {
        let c = core();
        let ledger = tmp_ledger("tower");
        let view = view_at(60);
        let claim = |ledger: &EntityLedger, name: &str, layer: u16| {
            commit(
                ledger,
                &view,
                &c,
                &Draft {
                    op: op::CLAIM, entity_id_hex: "", parent_hex: "", name,
                    x: 77, y: 88, layer, kind: 2, height: 50, ts: 50,
                },
            )
        };
        let base = claim(&ledger, "塔基", 12);
        claim(&ledger, "塔身", 13);
        claim(&ledger, "塔顶", 14);
        let (state, rep) = replay(&ledger.load(), &view);
        assert!(rep.ok, "叠塔应合法：{}", rep.reason);
        let mut col: Vec<u16> = state.values().filter(|e| e.x == 77 && e.y == 88).map(|e| e.layer).collect();
        col.sort();
        assert_eq!(col, vec![12, 13, 14]);

        // 同格同层再造一枚：重放从这条起拒绝（强写：本机预检会先拦）
        force_write(
            &ledger,
            &c,
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "挤进来的",
                x: 77, y: 88, layer: 13, kind: 5, height: 51, ts: 51,
            },
        );
        let (_, rep) = replay(&ledger.load(), &view);
        assert!(!rep.ok);
        assert!(rep.reason.contains("已经有"), "{}", rep.reason);
        assert_eq!(rep.first_bad, 4);

        // 断租不删除，但那一格的空间重新可造
        commit(
            &ledger,
            &view,
            &c,
            &Draft {
                op: op::RELEASE, entity_id_hex: &base.entity_id_hex, parent_hex: "", name: "塔基",
                x: 77, y: 88, layer: 12, kind: 0, height: 52, ts: 52,
            },
        );
        let recs = ledger.load();
        let truncated = &recs[..3]; // 只重放到三次铸造，塔仍是合法的
        let (state, rep) = replay(truncated, &view);
        assert!(rep.ok, "{}", rep.reason);
        assert_eq!(state.len(), 3);

        let fresh = tmp_ledger("reuse");
        let fresh_base = claim(&fresh, "塔基", 12);
        commit(
            &fresh,
            &view,
            &c,
            &Draft {
                op: op::RELEASE, entity_id_hex: &fresh_base.entity_id_hex,
                parent_hex: "", name: "塔基", x: 77, y: 88, layer: 12, kind: 0, height: 53, ts: 53,
            },
        );
        // v4 的块高只能前进：重建也取当前高度（53），不能回拨到 50
        commit(
            &fresh,
            &view,
            &c,
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "重建塔基",
                x: 77, y: 88, layer: 12, kind: 2, height: 53, ts: 54,
            },
        );
        let (state, rep) = replay(&fresh.load(), &view);
        assert!(rep.ok, "释放后应可原地重建：{}", rep.reason);
        assert_eq!(state.values().filter(|e| !e.released).count(), 1);
        let _ = std::fs::remove_file(ledger.path());
        let _ = std::fs::remove_file(fresh.path());
    }

    /// 高度层参与 id 与哈希：改一层就等于换了一条历史
    #[test]
    fn layer_is_bound_into_the_record() {
        let c = core();
        let ledger = tmp_ledger("layer");
        let view = view_at(9);
        let mut r = commit(
            &ledger,
            &view,
            &c,
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "悬空的",
                x: 3, y: 4, layer: 20, kind: 3, height: 9, ts: 9,
            },
        );
        assert_eq!(r.layer, 20);
        r.layer = 21;
        r.sig_hex = hex::encode(crypto::sign(&c.signing_key, &record_hash(&r)));
        let (state, rep) = replay(&[r], &view);
        assert!(!rep.ok);
        assert!(rep.reason.contains("实体 id"), "{}", rep.reason);
        assert!(state.is_empty());
        let _ = std::fs::remove_file(ledger.path());
    }

    // ------------------------------------------------ v4：免费写入的三道闸

    #[test]
    fn chain_view_reads_the_tip_from_blocks_jsonl() {
        let text = "{\"height\":0,\"wires\":[]}\n{\"height\":7,\"wires\":[]}\n";
        assert_eq!(ChainView::from_blocks_text(text).tip(), 7);
    }

    /// v4 记录不许夹带付款凭据——免费就是免费，字段必须为空
    #[test]
    fn free_record_must_not_carry_a_fee() {
        let c = core();
        let view = view_at(10);
        let mut r = sign_record(
            &c,
            1,
            &[0u8; 32],
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "白送的塔",
                x: 1, y: 2, layer: 0, kind: 0, height: 5, ts: 1,
            },
        );
        r.fee_tx_hex = hex::encode([9u8; 32]);
        r.sig_hex = hex::encode(crypto::sign(&c.signing_key, &record_hash(&r)));
        let err = apply_record(
            &mut BTreeMap::new(),
            &mut BTreeSet::new(),
            &mut BTreeMap::new(),
            &mut BTreeMap::new(),
            &mut BTreeMap::new(),
            0,
            &[0u8; 32],
            0,
            &r,
            &view,
        )
        .unwrap_err();
        assert!(err.contains("必须为空"), "{err}");
    }

    /// 不能预支未来：声明块高超过链头 = 把 era 提前，全网一眼看穿
    #[test]
    fn free_record_cannot_declare_a_height_beyond_the_tip() {
        let c = core();
        let r = sign_record(
            &c,
            1,
            &[0u8; 32],
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "未来塔",
                x: 1, y: 2, layer: 0, kind: 0, height: 6, ts: 1,
            },
        );
        let err = apply_record(
            &mut BTreeMap::new(),
            &mut BTreeSet::new(),
            &mut BTreeMap::new(),
            &mut BTreeMap::new(),
            &mut BTreeMap::new(),
            0,
            &[0u8; 32],
            0,
            &r,
            &view_at(5),
        )
        .unwrap_err();
        assert!(err.contains("超过链头"), "{err}");
    }

    /// 窗口不能回拨：先写到高窗口，再把块高改回低窗口，就是想重领一份配额
    #[test]
    fn free_write_height_cannot_go_backwards() {
        let c = core();
        let view = view_at(40);
        let mut state = BTreeMap::new();
        let mut used = BTreeSet::new();
        let mut writes = BTreeMap::new();
        let r1 = sign_record(
            &c,
            1,
            &[0u8; 32],
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "先造",
                x: 1, y: 1, layer: 0, kind: 0, height: 30, ts: 1,
            },
        );
        apply_record(
            &mut state,
            &mut used,
            &mut writes,
            &mut BTreeMap::new(),
            &mut BTreeMap::new(),
            0,
            &[0u8; 32],
            0,
            &r1,
            &view,
        )
        .unwrap();
        let r2 = sign_record(
            &c,
            2,
            &record_hash(&r1),
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "回拨",
                x: 2, y: 2, layer: 0, kind: 0, height: 10, ts: 2,
            },
        );
        let err = apply_record(
            &mut state,
            &mut used,
            &mut writes,
            &mut BTreeMap::new(),
            &mut BTreeMap::new(),
            1,
            &record_hash(&r1),
            30,
            &r2,
            &view,
        )
        .unwrap_err();
        assert!(err.contains("回拨"), "{err}");
    }

    /// 一个窗口最多 4096 条：撞墙的那一条当场退回，少一条就能过
    #[test]
    fn free_writes_are_capped_per_window() {
        let c = core();
        let view = view_at(7);
        let r = sign_record(
            &c,
            1,
            &[0u8; 32],
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "配额塔",
                x: 1, y: 1, layer: 0, kind: 0, height: 7, ts: 1,
            },
        );
        let key = (signer_pk_hash(&c), 7 / FREE_QUOTA_WINDOW);
        let mut writes = BTreeMap::new();
        writes.insert(key.clone(), FREE_QUOTA_MAX);
        let err = apply_record(
            &mut BTreeMap::new(),
            &mut BTreeSet::new(),
            &mut writes,
            &mut BTreeMap::new(),
            &mut BTreeMap::new(),
            0,
            &[0u8; 32],
            0,
            &r,
            &view,
        )
        .unwrap_err();
        assert!(err.contains("配额"), "{err}");

        writes.insert(key, FREE_QUOTA_MAX - 1);
        apply_record(
            &mut BTreeMap::new(),
            &mut BTreeSet::new(),
            &mut writes,
            &mut BTreeMap::new(),
            &mut BTreeMap::new(),
            0,
            &[0u8; 32],
            0,
            &r,
            &view,
        )
        .unwrap();
    }

    // ------------------------------------------------ v5：挖地

    fn dig(ledger: &EntityLedger, view: &ChainView, c: &crate::keystore::WalletCore, x: u16, y: u16, layer: u16, kind: u8) -> EntityRecord {
        commit(
            ledger,
            view,
            c,
            &Draft {
                op: op::DIG, entity_id_hex: "", parent_hex: "", name: "",
                x, y, layer, kind, height: 50, ts: 1,
            },
        )
    }

    /// 一列从地表往下剥：每层产什么由**离地表的深度**定，挖到第 1 层为止。
    /// 这条测试把整列挖穿，确认三带的分界与层数严格对得上。
    #[test]
    fn digging_a_column_pays_soil_then_stone_then_ore() {
        let c = core();
        let ledger = tmp_ledger("dig-column");
        let view = view_at(50);
        let (x, y) = (0u16, 0u16);
        assert!(!is_water(x, y));
        let surf = surface_layer(x, y);
        assert!(surf >= 12, "测试点不够深：第 {surf} 层");

        for i in 0..surf {
            let layer = surf - i;
            dig(&ledger, &view, &c, x, y, layer, material_at_depth(i));
        }
        let w = replay_world(&ledger.load(), &view);
        assert!(w.report.ok, "{}", w.report.reason);
        assert_eq!(column_top(&w.dug, x, y), 0, "整列挖到岩床");
        let m = w.materials.get(&c.address).copied().unwrap();
        assert_eq!(m[MAT_SOIL as usize], (DUG_SOIL_DEPTH + 1) as u32, "地表 0–3 层出泥土");
        assert_eq!(
            m[MAT_STONE as usize],
            (DUG_STONE_DEPTH - DUG_SOIL_DEPTH) as u32,
            "4–9 层出石头"
        );
        assert_eq!(
            m[MAT_ORE as usize],
            (surf - 1 - DUG_STONE_DEPTH) as u32,
            "10 层以下才出矿"
        );

        // 岩床之上再挖：空操作，不撕链（链上已经有整列的历史，多一条也无妨）
        dig(&ledger, &view, &c, x, y, 1, MAT_ORE);
        let w2 = replay_world(&ledger.load(), &view);
        assert!(w2.report.ok, "{}", w2.report.reason);
        assert_eq!(column_top(&w2.dug, x, y), 0);
        assert_eq!(w2.materials.get(&c.address).unwrap()[MAT_ORE as usize], (surf - 10) as u32);
        let _ = std::fs::remove_file(ledger.path());
    }

    /// 只能剥露在最上面的那一层：跳着挖是空操作，材料与顶面都不动。
    /// （竞赛里别人抢先挖掉同一层，这一条也不该把你的链判死。）
    #[test]
    fn digging_below_the_exposed_layer_is_a_no_op_not_a_forgery() {
        let c = core();
        let ledger = tmp_ledger("dig-skip");
        let view = view_at(50);
        let (x, y) = (3u16, 3u16);
        let surf = surface_layer(x, y);
        let r = dig(&ledger, &view, &c, x, y, surf - 1, MAT_SOIL);
        let w = replay_world(&ledger.load(), &view);
        assert!(w.report.ok, "{}", w.report.reason);
        assert_eq!(column_top(&w.dug, x, y), surf, "顶面不该动");
        assert_eq!(
            w.materials.get(&c.address).copied().unwrap_or([0; MAT_COUNT])[MAT_SOIL as usize],
            0
        );
        assert_eq!(r.layer, surf - 1);
        let _ = std::fs::remove_file(ledger.path());
    }

    /// 材料与深度不符 = 伪造：重放当场拒绝。想拿矿就真挖到第 10 层以下。
    #[test]
    fn claiming_ore_from_the_surface_is_rejected() {
        let c = core();
        let ledger = tmp_ledger("dig-lie");
        let view = view_at(50);
        let (x, y) = (5u16, 5u16);
        let surf = surface_layer(x, y);
        force_write(
            &ledger,
            &c,
            &Draft {
                op: op::DIG, entity_id_hex: "", parent_hex: "", name: "",
                x, y, layer: surf, kind: MAT_ORE, height: 50, ts: 1,
            },
        );
        let (_, rep) = replay(&ledger.load(), &view);
        assert!(!rep.ok);
        assert!(rep.reason.contains("应得泥土"), "{}", rep.reason);
        let _ = std::fs::remove_file(ledger.path());
    }

    /// 水面挖不动——水不是方块，挖不出土，也不该有一条"水底记录"
    #[test]
    fn water_columns_cannot_be_dug() {
        let c = core();
        let ledger = tmp_ledger("dig-water");
        let view = view_at(50);
        // (128,250) 是窗口南缘的正经海面：36.114°N 120.665°E，黄海，大地高度 25 < 海面 86
        assert!(is_water(128, 250));
        let err = ledger
            .commit_as(&signer_pk_hash(&c), &view, |seq, prev| {
                sign_record(&c, seq, &prev, &Draft {
                    op: op::DIG, entity_id_hex: "", parent_hex: "", name: "",
                    x: 128, y: 250, layer: surface_layer(128, 250), kind: MAT_SOIL,
                    height: 50, ts: 1,
                })
            })
            .unwrap_err();
        assert!(err.contains("水面"), "{err}");
        assert!(ledger.load().is_empty(), "验不过的挖地记录一条都不许落盘");
        let _ = std::fs::remove_file(ledger.path());
    }

    /// 挖出来的材料是建造的预算：造一件就少一份，账本里同一条也可以复算。
    #[test]
    fn building_spends_the_material_you_dug() {
        let c = core();
        let ledger = tmp_ledger("dig-spend");
        let view = view_at(50);
        let (x, y) = (0u16, 0u16);
        let surf = surface_layer(x, y);

        dig(&ledger, &view, &c, x, y, surf, MAT_SOIL);
        let w = replay_world(&ledger.load(), &view);
        assert_eq!(w.materials[&c.address][MAT_SOIL as usize], 1);

        // 花丛（kind 2）造价：1 份泥土
        commit(
            &ledger,
            &view,
            &c,
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "一朵花",
                x, y, layer: surf + 1, kind: 2, height: 50, ts: 2,
            },
        );
        let w = replay_world(&ledger.load(), &view);
        assert!(w.report.ok, "{}", w.report.reason);
        assert_eq!(w.materials[&c.address][MAT_SOIL as usize], 0, "造完就花光");
        assert_eq!(build_cost(5), [0, 1, 1], "灯塔 = 1 石 + 1 矿");
        let _ = std::fs::remove_file(ledger.path());
    }

    // ------------------------------------------------ v3：老账本按当年规则重放

    /// v3 记录在自己的域里仍然严格：付款必须在链上、块高必须等于付款块高、
    /// 一笔付款只能买一条。升到 v4 不是把旧规则作废，而是让它只服务历史。
    #[test]
    fn v3_fee_records_still_replay_under_their_own_domain() {
        let c = core();
        let ledger = tmp_ledger("v3-legacy");
        let mut view = view_at(9);
        let fee = pay(&mut view, &c, 9);
        let d = Draft {
            op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "付过费的塔",
            x: 4, y: 5, layer: 0, kind: 0, height: 9, ts: 1,
        };
        let r = ledger
            .commit_as(&signer_pk_hash(&c), &view, |seq, prev| sign_v3(&c, seq, &prev, &d, &fee))
            .unwrap();
        let (state, rep) = replay(&ledger.load(), &view);
        assert!(rep.ok, "v3 老记录必须还能重放：{}", rep.reason);
        assert_eq!(state.len(), 1);
        assert_eq!(state.get(&r.entity_id_hex).unwrap().name, "付过费的塔");

        // 同一笔付款不能再买第二条
        let err = ledger
            .commit_as(&signer_pk_hash(&c), &view, |seq, prev| {
                sign_v3(&c, seq, &prev, &d, &fee)
            })
            .unwrap_err();
        assert!(err.contains("已被别的记录用掉"), "{err}");

        // 块高必须等于付款所在块高（不能自填一个更低的块高去骗 era=1）
        let fee2 = pay(&mut view, &c, 12);
        let early = Draft { height: 11, ..Draft { name: "早产塔", x: 6, y: 7, ..d } };
        let err = ledger
            .commit_as(&signer_pk_hash(&c), &view, |seq, prev| {
                sign_v3(&c, seq, &prev, &early, &fee2)
            })
            .unwrap_err();
        assert!(err.contains("手续费所在块高 12"), "{err}");

        // 钱不够：0.000000001 QLX 买不到一次铸造
        let cheap = pay_at(&mut view, &c, 9, 1);
        let err = ledger
            .commit_as(&signer_pk_hash(&c), &view, |seq, prev| {
                sign_v3(&c, seq, &prev, &d, &cheap)
            })
            .unwrap_err();
        assert!(err.contains("不足造价"), "{err}");
        assert_eq!(ledger.load().len(), 1, "验不过的记录一条都不许落盘");
        let _ = std::fs::remove_file(ledger.path());
    }

    /// v2 及更早的旧账本整本挪去留档，不删除、不假装还能验
    #[test]
    fn legacy_v2_ledger_is_archived_not_deleted() {
        let mut p = std::env::temp_dir();
        p.push(format!("qlight-ent-legacy-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        std::fs::write(&p, "{\"v\":\"QL-ENTREC-v2\",\"seq\":1}\n").unwrap();

        let ledger = EntityLedger::new(p.clone());
        assert!(!p.exists(), "旧账本应被挪走，而不是按新规则继续读");
        assert!(ledger.load().is_empty());

        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let dir = p.parent().unwrap();
        let archived: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|q| {
                q.file_name()
                    .map(|n| n.to_string_lossy().starts_with(&format!("{name}.bak-v2-")))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(archived.len(), 1, "旧账本必须留档（谁都能拿它离线重算旧世界）");
        for q in archived {
            let _ = std::fs::remove_file(q);
        }
    }

    /// v3 是上一代正式格式：升级到 v4 时原地保留，老记录照旧可重放
    #[test]
    fn v3_ledger_survives_the_v4_upgrade() {
        let mut p = std::env::temp_dir();
        p.push(format!("qlight-ent-keep-v3-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        std::fs::write(&p, "{\"v\":\"QL-ENTREC-v3\",\"seq\":1}\n").unwrap();
        let _ = EntityLedger::new(p.clone());
        assert!(p.exists(), "v3 账本必须原地保留");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn chunk_root_is_a_set_commitment() {
        let mut state = BTreeMap::new();
        for (id, x) in [("aa", 3u16), ("bb", 4), ("cc", 20)] {
            state.insert(
                id.to_string(),
                Entity {
                    entity_id_hex: id.into(), name: "n".into(), x, y: 2, layer: 0, kind: 0,
                    owner: "ql1x".into(), parent_hex: String::new(), era: 1,
                    created_height: 1, created_ts: 1, last_height: 1, released: false,
                },
            );
        }
        let chunks = chunk_roots(&state);
        assert_eq!(chunks.len(), 2); // (0,0) 两枚 + (1,0) 一枚
        assert_eq!(chunks[0].count, 2);
        assert_eq!(chunks[1].count, 1);
        // 插入顺序无关：字典序承诺
        let mut shuffled = BTreeMap::new();
        for k in ["cc", "bb", "aa"] {
            if let Some(e) = state.get(k) {
                shuffled.insert(k.to_string(), e.clone());
            }
        }
        assert_eq!(chunk_roots(&shuffled), chunks);
    }

    // ------------------------------------------------ §9.16 创造权重

    fn cell(id: &str, owner: &str, created_height: u64) -> Entity {
        Entity {
            entity_id_hex: id.into(),
            name: "格".into(),
            x: 1,
            y: 1,
            layer: 0,
            kind: 0,
            owner: owner.into(),
            parent_hex: String::new(),
            era: 1,
            created_height,
            created_ts: created_height,
            last_height: created_height,
            released: false,
        }
    }

    fn state_of(cells: &[(Entity, bool)]) -> BTreeMap<String, Entity> {
        let mut m = BTreeMap::new();
        for (e, released) in cells {
            let mut e = e.clone();
            e.released = *released;
            m.insert(e.entity_id_hex.clone(), e);
        }
        m
    }

    /// 刚落下的格子一分不值：权重买的是「这块地方归我守了多久」，不是「我按了几次建造」
    #[test]
    fn fresh_cells_weigh_nothing() {
        let st = state_of(&[(cell("a", "me", 100), false)]);
        let c = craft_of(&st, "me", 100);
        assert_eq!((c.cells, c.graded, c.kept_blocks), (1, 1, 0));
        assert_eq!(c.score, 0, "第一块也没守，凭什么加速");
        assert_eq!(c.floor, MINE_FLOORS[0], "零权重就是最慢那一档");
    }

    /// 松手 = 立刻停止计时：拆掉一格，节奏档位当场回落
    #[test]
    fn releasing_a_cell_stops_the_clock() {
        let keep = state_of(&[(cell("a", "me", 200), false), (cell("b", "me", 200), false)]);
        let at = craft_of(&keep, "me", 1000);
        assert_eq!(at.kept_blocks, 1600);
        assert_eq!(at.score, 5);

        let mut dropped = state_of(&[(cell("a", "me", 200), false), (cell("b", "me", 200), false)]);
        dropped.get_mut("b").unwrap().released = true;
        let after = craft_of(&dropped, "me", 1000);
        assert_eq!(after.cells, 1, "RELEASE 记录永久保留，但不再占格");
        assert_eq!(after.kept_blocks, 800);
        assert_eq!(after.score, 4);
        assert!(after.floor > at.floor, "拆了格子只会变慢，不会变快");
    }

    /// 链龄闸门：脚本可以在一个窗口里造满 64 格，但造不出「这里已经存在了很久」
    #[test]
    fn chain_age_caps_what_a_burst_can_buy() {
        let cells: Vec<(Entity, bool)> = (0..CRAFT_CELL_CAP as u32)
            .map(|k| (cell(&format!("{:04}", k), "me", 0), false))
            .collect();
        let st = state_of(&cells);
        let tip = 600;
        let c = craft_of(&st, "me", tip);
        assert_eq!(c.cells, CRAFT_CELL_CAP);
        assert_eq!(c.kept_blocks, 600 * CRAFT_CELL_CAP as u64);
        // 光看格子数本该给 192 分，世界的时间只给了 3 分
        assert_eq!(c.kept_blocks / CRAFT_KEEP_UNIT, 192);
        assert_eq!(c.score, 3);
        assert_eq!(c.score as u64, tip / CRAFT_KEEP_UNIT, "封顶的必须是链龄，不是产能");
        assert_eq!(c.floor, MINE_FLOORS[3]);
    }

    /// 只数自己的格；每格最多记 `CRAFT_KEEP_CAP` 块——老地标不会把阶梯焊死在最快档
    #[test]
    fn only_your_own_cells_count_and_each_one_is_capped() {
        let st = state_of(&[
            (cell("a", "me", 0), false),
            (cell("b", "别人", 0), false),
            (cell("c", "me", 0), false),
        ]);
        let tip = 100_000;
        let c = craft_of(&st, "me", tip);
        assert_eq!(c.cells, 2, "邻居的地不是我的权重");
        assert_eq!(c.kept_blocks, 2 * CRAFT_KEEP_CAP, "单格守护时长要封顶");
        assert_eq!(c.score, (MINE_FLOORS.len() - 1) as u8, "计分项也封顶在最快一档");
        assert_eq!(c.floor, *MINE_FLOORS.last().unwrap());
    }

    /// 计分不越过阶梯长度，且与状态怎么拼出来的无关：第三方独立复算得到同一个数
    #[test]
    fn weight_is_recomputable_independently_of_build_order() {
        let mut forward = BTreeMap::new();
        let mut backward = BTreeMap::new();
        for k in 0..(CRAFT_CELL_CAP + 8) {
            let id = format!("{:05}", k);
            // 故意让最老的一批格子并列，检验「取哪 64 格」不改变总分
            let e = cell(&id, "me", if k < 5 { 0 } else { (k as u64) % 37 });
            forward.insert(id.clone(), e.clone());
            backward.insert(id, e);
        }
        let a = craft_of(&forward, "me", 9_000);
        let b = craft_of(&backward, "me", 9_000);
        assert_eq!(a, b);
        assert_eq!(a.cells, CRAFT_CELL_CAP + 8);
        assert_eq!(a.graded, CRAFT_CELL_CAP, "只给最老的 64 格计分");
        assert!((a.score as usize) < MINE_FLOORS.len());
    }

    /// 「整本落在链尖之上」或「整本的手续费凭据都不在链上」才叫旧世界的账：
    /// 混着能过的记录说明是别的问题，不许归档。链长过旧记录高度之后，
    /// 凭据锚必须继续认得出它——否则等链追上，出口就消失了。
    #[test]
    fn only_a_wholly_foreign_ledger_gets_the_archive_exit() {
        assert!(ledger_is_foreign(false, &[778, 797], 80, 2, 0));
        assert!(!ledger_is_foreign(true, &[778, 797], 80, 2, 0), "能过就不是孤儿账");
        assert!(!ledger_is_foreign(false, &[], 80, 0, 0), "空账没有可归档的东西");
        assert!(!ledger_is_foreign(false, &[70, 90], 80, 2, 1), "混合账是真空错，不猜");
        assert!(!ledger_is_foreign(false, &[80], 80, 2, 1), "正好落在链尖上不算之上");
        assert!(
            ledger_is_foreign(false, &[70, 80], 80, 2, 0),
            "链已经长过旧记录高度，只要凭据一笔都不在链上，仍认得出是旧账"
        );
        assert!(
            !ledger_is_foreign(false, &[70, 90], 80, 0, 0),
            "没有凭据可对（v4 免费记录）就只剩块高口径，mixed 不猜"
        );
    }
}
