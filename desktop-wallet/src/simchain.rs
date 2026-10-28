//! 本地真实账本（A 档：持久化 + 真验证 + 交易真入块）
//!
//! 协议口径与 TOKEN.md / DESIGN.md 一致：
//! - 出块周期 2s，初始排放 3.33 QLX/块（§12 公平挖矿——M1 简化为全量给钱包节点）；
//! - 减半：每 31,500,000 块（≈2 年 × 2s 块）；硬顶 2.1 亿 QLX（9 位小数）；
//! - VRF 存活心跳：轻节点挖矿证明（§3.3），SHAKE-256(seed ‖ height ‖ prev)；
//! - 交易 QL-TX-v1 canonical CBOR + SHAKE-256 digest + ML-DSA-65 验签。
//!
//! A 档真实化要点（区别于旧版"提交即结算"）：
//! 1. submit 只做完整协议校验（真 ML-DSA-65 验签 / nonce / 可支配余额 / 过期）并入池，不改状态；
//! 2. 打包时逐笔再验签重放，按序执行状态转移（余额、nonce、记录）；
//! 3. tx_root = SHAKE-256("QL-TXROOT-v1" ‖ concat(digests))；块哈希绑定
//!    prev‖height‖minted_after‖vrf‖tx_root——块哈希与块内交易严格绑定；
//! 4. 手续费真实结算：入块 n 笔 × FEE 归出块者；
//! 5. 块头逐块追加 blocks.jsonl，启动时重放哈希链校验，账本本地可审计；
//! 6. ledger 原子写（tmp + rename），进程崩溃不产生半截文件；
//! 7. 过期（expiry ≤ 当前块高）自动作废，不入账。

use crate::crypto;
use crate::tx::{tx_digest, TxEnvelope};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 出块周期 2 秒
pub const BLOCK_INTERVAL_SECS: u64 = 2;
/// 初始排放：3.33 QLX / 2s
pub const INIT_REWARD: u64 = 3_330_000_000;
/// 减半间隔（块）
pub const HALVING_INTERVAL: u64 = 31_500_000;
/// 硬顶 2.1 亿 QLX
pub const HARD_CAP: u64 = 210_000_000 * 1_000_000_000;
/// 手续费：0.001 QLX
pub const FEE: u64 = 1_000_000;
/// 金额小数位
pub const DECIMALS: u64 = 9;
/// 单块最多打包笔数
pub const TXS_PER_BLOCK: usize = 64;
/// 单块最多收录的光尘记录条数。
///
/// 光尘是"频繁发生的小事"（采一格、转一次），块不能因为它变胖；64 条在
/// 2 秒一块的节奏下远超手工玩法产出速度，同时又给分叉激活那一下的存量
/// 迁移留出平滑的消化速度（旧记录优先，见 `collect_dust` 的排序）。
pub const DUST_PER_BLOCK: usize = 64;
/// 交易池上限
pub const MEMPOOL_CAP: usize = 256;
/// 账本格式版本（B 档：v2 增加 miner/nonce 字段，旧档自动归档）
pub const LEDGER_V: u8 = 2;

/// 区块格式版本。
///
/// v0 = 历史块：每块无条件排放 `reward_at(h)`（旧规则，必须能原样重放）；
/// v1 = 挖矿结算：**块内必须有一张矿签才发行**，没有就只收手续费、零排放。
/// 版本进块哈希（见 `block_hash_of_ver`），所以旧块不可能被"重新解释"成 v1 来偷排放。
pub const BLOCK_V_LEGACY: u8 = 0;
/// v1 = 挖矿结算规则的起点：发行必须有矿签。v2 沿用同一套经济规则，只是多绑一份世界承诺。
pub const BLOCK_V_MINING: u8 = 1;
/// v2 = 块哈希再绑上「世界承诺」——整本实体账本的集合指纹与条数（域 `QL-BLOCK-V3`）。
pub const BLOCK_V_WORLD: u8 = 2;
/// v3 = 节奏阶梯：**发行节奏由创造挣得**。两次发行的最小间隔不再固定 50 块，而是随
/// 该地址在世界里落下的矿格数递减（50→10）；每笔只结算**最多一个当前档位** Δ 个块的量
/// （`reward_at(h) × Δ`），空档不补发，所以每块平均发行量 ≤ `reward_at(h)`：
/// 减半曲线、210M 硬顶、每块最多一张矿签全部照旧——创造换来的是节奏，不是超发。
/// （域 `QL-BLOCK-V4`；v2 块继续按 ×50 原样重放，历史不被改写。）
pub const BLOCK_V_CRAFT: u8 = 3;
/// v4 = 现行：在 v3 的经济规则上，把 `miner` 与 `ts_unix` 也绑进块哈希
/// （域 `QL-BLOCK-V5`）。没有这一步，无矿签的块可以改 `miner` 偷手续费；
/// v0 块更可以改 `miner` 偷整笔奖励。v3 及以前的块仍按旧域原样重放。
pub const BLOCK_V_MINER: u8 = 4;
/// v5 = 光尘与池子进块（域 `QL-BLOCK-V6`）。
///
/// 这是**第一阶段硬分叉**：块头多绑一份 `dust_root`（块内光尘记录的集合指纹），
/// 块行多带 `dusts` 原件；光尘余额从此由共识结算，而不再只是各机器手里的
/// 一本 gossip 账。有了它，池子才能在链上成立——光尘的稀缺性（配额、一格一次）
/// 第一次变成全网一致的状态，陌生人之间才谈得上兑换。
pub const BLOCK_V_POOL: u8 = 5;
pub const BLOCK_V: u8 = BLOCK_V_POOL;
/// v5 激活高度：此高度之后**只认 v5**。现有演示链在 141 左右，
/// 200 给两台机器留出升级窗口，之后光尘开始逐块进块。
///
/// 没有激活高度，攻击者可以在任意高度造 v0 块，绕过矿签与冷却无条件领
/// `reward_at(h)`；旧链（v0）在启动校验时会被归档重置。
pub const BLOCK_V_V5_ACTIVATION: u64 = 200;

/// 某个块高允许的最低版本：激活高度之后只认 v5，之前允许历史版本逐级升级。
pub fn version_floor_at(height: u64) -> u8 {
    if height > BLOCK_V_V5_ACTIVATION {
        BLOCK_V_POOL
    } else {
        BLOCK_V_LEGACY
    }
}
/// 本机共识规则指纹：出块域版本 ‖ 实体记录语义 ‖ 光尘语义。
///
/// 联邦公告（`NetMsg::Announce`）带上它。三项里任何一项不同，两端就互相验不过
/// 对方的块与记录——而这种分歧在链上是**静默**的：各出各的块、各自都认为自己
/// 那条才是链，连接层看不出任何异常。所以只能趁"还连得上"的时候把它说出口。
pub fn rules_tag() -> String {
    format!(
        "b{}|{}|{}",
        BLOCK_V,
        crate::world::RECORD_VERSION,
        crate::world::dust::DUST_RECORD_VERSION
    )
}
/// 两次挖矿发行之间的最小间隔（块）。2 秒一块 → 约 100 秒一回合。
///
/// 挖矿是"在世界里落一格别人可复算的坐标"，不是跑算法；这个间隔让脚本
/// 刷得再快也快不过地表自己重画的节奏，同时把单次发行放大到 50 个块的量。
pub const MINE_COOLDOWN_BLOCKS: u64 = 50;
/// 矿签目标前缀：`QLOR` ‖ x(2B) ‖ y(2B) ‖ layer(2B) ‖ 零填充。
///
/// 矿签是一笔**零金额、零手续费**的转账，收款方是这个世界坐标本身——
/// 它证明"我在这块高挖中了这一格"，奖励归付款人（= 出块者），不需要任何注册表。
pub const CLAIM_TAG: [u8; 4] = *b"QLOR";

// ---------------------------------------------------------------- 创世块（方案 A，借鉴 BTC）
//
// BTC 把创世当天报纸头版标题嵌进 coinbase 交易，于是那句话被永久锚进创世哈希——
// 改一个字，哈希就变。QLight 同理：宣言原文 → SHAKE-256 → 嵌入 tx_root → 创世哈希。
// 任何人拿宣言原文即可重算创世哈希并与本文件硬编码值比对，宣言与链的绑定自证。

/// 创世宣言（创世 2026-10-26 00:00:00 UTC）
pub const GENESIS_MANIFESTO: &str = "QLight Genesis · Oct 26 2026 · QLX quantum-safe credential chain. \
No premine, no ICO, no investors — fair launch by mining only. \
愿价值在量子时代依旧自由转移 · Value shall move freely in the quantum era.";

/// 创世时间戳：固定值，所有节点一致（不随机器时间漂移）
pub const GENESIS_TS_UNIX: u64 = 1792972800;

/// 创世块的 prev：64 个 0（链的第一个锚点，BTC 同款）
pub const GENESIS_PREV_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// 创世哈希 = SHAKE-256("QL-BLOCK-V1", 64×0 ‖ 0 ‖ 0 ‖ 0vrf ‖ tx_root(宣言摘要))
/// ——宣言在哈希里，哈希在源码里；改宣言必改此值，全网一眼识破。
pub const GENESIS_HASH_HEX: &str = "5dc2205a8f0153a05f84e26ab94086009796bb68fdfddff439f97ebfd2aa3104";

/// 创世宣言摘要——嵌进创世块 tx_root 的那一笔，宣言与链的绑定就落在这里
pub fn genesis_manifesto_digest() -> [u8; 32] {
    let d = crypto::shake256(b"QL-GENESIS-V1", GENESIS_MANIFESTO.as_bytes(), 32);
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&d);
    digest
}

/// 运行时重算创世哈希（口径与 genesis_header 完全一致，供启动断言比对）
pub fn genesis_hash() -> [u8; 32] {
    let root = tx_root_of(&[genesis_manifesto_digest()]);
    block_hash_of(GENESIS_PREV_HEX, 0, 0, &[0u8; 32], &root)
}

pub fn genesis_hash_hex() -> String {
    hex::encode(genesis_hash())
}

/// 创世块本体（blocks.jsonl 第 0 行）。
///
/// 无出块者、无排放、无手续费——它不是一次记账，是整条链的锚点；
/// 唯一内容是那笔摘要即宣言原文的"创世交易"，让宣言实打实在链上可查。
pub fn genesis_header() -> BlockHeader {
    BlockHeader {
        version: BLOCK_V_LEGACY,
        height: 0,
        ts_unix: GENESIS_TS_UNIX,
        prev_hash_hex: GENESIS_PREV_HEX.to_string(),
        block_hash_hex: genesis_hash_hex(),
        vrf_hex: hex::encode([0u8; 32]),
        minted_after: 0,
        reward_total: 0,
        miner: String::new(),
        // 创世块不承诺世界：那时还没有任何实体（v0 语义，字段恒空）
        entity_root_hex: String::new(),
        entity_count: 0,
        dust_root_hex: String::new(),
        dust_count: 0,
        txs: vec![TxAttestation {
            digest_hex: hex::encode(genesis_manifesto_digest()),
            from: String::new(),
            to: String::new(),
            amount: 0,
            fee: 0,
            nonce: 0,
        }],
    }
}

/// 启动断言：重算值必须等于锚定值，否则创世已被替换，链不再可信。
fn assert_genesis_anchored() {
    let computed = genesis_hash_hex();
    if computed != GENESIS_HASH_HEX {
        eprintln!(
            "[QLight] 致命：创世哈希与源码锚定值不符，信任根已被替换，拒绝启动。\n  \
             锚定值：{GENESIS_HASH_HEX}\n  \
             重算值：{computed}\n  \
             若确有意修改 GENESIS_MANIFESTO，须同步更新 GENESIS_HASH_HEX 并重新发布创世。"
        );
        panic!("genesis hash does not match the anchored constant");
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxRecord {
    pub hash_hex: String,
    pub height: u64,
    pub ts_unix: u64,
    pub from: String,
    pub to: String,
    pub amount: u64,
    pub fee: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ledger {
    pub v: u8,
    pub height: u64,
    pub prev_hash_hex: String,
    pub total_minted: u64,
    pub started_unix: u64,
    pub balances: HashMap<String, u64>,
    pub nonces: HashMap<String, u64>,
    pub records: Vec<TxRecord>,
    /// 最后一次**结算/发行水位**（0 = 从未）。
    ///
    /// v0 每块都发行，所以水位跟着块高走；v1+ 只在收下矿签时推进。
    /// 它既是冷却基准，也是"升级后第一笔 v4 不许把旧 v0 已发过的块再补一遍"的闸门。
    #[serde(default)]
    pub last_claim_height: u64,
    /// 最近一个已应用块的版本（0 = 创世/旧档）。版本只许升不许降，
    /// 否则攻击者可以在 v4 链后面接一个 v0 块继续免费发行。
    #[serde(default)]
    pub last_block_version: u8,
    /// 链上最近一次「世界承诺」的条数（v2 块才有；0 = 还没承诺过）。
    /// 用于单调性闸门：append-only 的世界里，倒退 = 有人想把世界清空。
    #[serde(default)]
    pub entity_count_committed: u64,
    /// v5 起：**共识结算后的光尘状态**（余额 / 本轮已采格 / 每人配额用量）。
    /// 分叉之前它恒为空——那时光尘只活在各机器手里的 gossip 账本里。
    #[serde(default)]
    pub dust: crate::world::dust::DustState,
    /// v5 起：已进块的光尘记录哈希（同一批记录不许被两块各结算一次）。
    #[serde(default)]
    pub dust_seen: std::collections::BTreeSet<String>,
    /// v5 起：每个归属者光尘链的游标（seq / prev / 最后块高）。
    #[serde(default)]
    pub dust_tips: BTreeMap<String, crate::world::dust::ChainTip>,
    /// v5 起：兑换池状态（池子 id → 两边储备 + LP 份额）。池子没有地址、没有私钥，
    /// 谁能取走什么只由恒定乘积曲线决定。
    #[serde(default)]
    pub pools: BTreeMap<String, crate::world::pool::Pool>,
    /// 全部历史 VRF（世界地表种子 = 创世哈希 ⊕ 这些值）。
    /// 不进 ledger.json：它可以从 blocks.jsonl 逐行重建，存两份只会漂移。
    #[serde(skip)]
    pub vrfs: Vec<String>,
    /// 每个地址已经落下的矿格数（v3 的节奏阶梯就看这个数）。
    /// 与 `vrfs` 同一口径：块文件里逐笔可数，绝不另存一份可能过期的副本。
    #[serde(skip)]
    pub claims_by: BTreeMap<String, u64>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            v: 2,
            height: 0,
            prev_hash_hex: genesis_hash_hex(),
            total_minted: 0,
            started_unix: GENESIS_TS_UNIX,
            balances: HashMap::new(),
            nonces: HashMap::new(),
            records: vec![],
            last_claim_height: 0,
            last_block_version: BLOCK_V_LEGACY,
            entity_count_committed: 0,
            dust: crate::world::dust::DustState::default(),
            dust_seen: std::collections::BTreeSet::new(),
            dust_tips: BTreeMap::new(),
            pools: BTreeMap::new(),
            vrfs: vec![],
            claims_by: BTreeMap::new(),
        }
    }
}

/// 区块头（blocks.jsonl 每行一条，含块内全部交易，供哈希链重放校验）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockHeader {
    /// 0 = 历史（每块固定排放）；1 = 挖矿结算（有矿签才发行）；2 = 再加世界承诺；
    /// 3 = 创造换节奏；4 = 块哈希再绑 miner/ts（现行）。旧行缺省按 0 读。
    #[serde(default)]
    pub version: u8,
    pub height: u64,
    pub ts_unix: u64,
    pub prev_hash_hex: String,
    pub block_hash_hex: String,
    pub vrf_hex: String,
    pub minted_after: u64,
    pub reward_total: u64, // 排放 + 手续费池
    /// 出块者地址（B 档联邦网络：对端要用它记账励）
    pub miner: String,
    /// v2 起：出块者此刻整本实体账本的集合指纹（`QL-ENTSET-V1`，见 `world::set_digest`）。
    /// 块哈希绑它，所以"这台机器眼里的世界是哪一份"从此写进共识、可归责、改不了。
    /// 旧行缺省按空串读（v0/v1 本来不承诺任何世界）。
    #[serde(default)]
    pub entity_root_hex: String,
    /// v2 起：该集合的去重条数。append-only 世界里它只增不减，
    /// 这条单调性本身就是"把世界清空"攻击的检测器。
    #[serde(default)]
    pub entity_count: u64,
    /// v5 起：块内光尘记录的集合指纹（`QL-DUSTROOT-V1`，与顺序无关），空块为 64 个 0。
    /// 它进了块哈希，所以块里的光尘记录从此不可重写、不可抵赖。
    #[serde(default)]
    pub dust_root_hex: String,
    /// v5 起：块内光尘记录条数（展示与对账用，根里已经覆盖）
    #[serde(default)]
    pub dust_count: u64,
    pub txs: Vec<TxAttestation>,
}

/// 块内交易凭据（重放校验 / 状态重建用）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TxAttestation {
    pub digest_hex: String,
    pub from: String,
    pub to: String,
    pub amount: u64,
    pub fee: u64,
    pub nonce: u64,
}

/// 块内交易**原件**：信封 + 签名 + 公钥 + 地址盐。
///
/// 为什么必须四件齐全：块头里的凭据（`TxAttestation`）只是"声称"，谁都能写。
/// 只有拿到原件，接收方才能独立重算 digest、由 `pk ‖ salt` 复算地址、再验 ML-DSA
/// 签名——**没有原件的整链快照等于让对端凭一面之词搬动余额**（旧实现正是如此，
/// 一条自洽的假链就能把别人的钱转走）。公钥与盐随块存放，验证因此不依赖注册表。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockWire {
    pub envelope: TxEnvelope,
    pub sig_hex: String,
    pub pk_hex: String,
    pub salt_hex: String,
}

/// blocks.jsonl 每行的完整形态：块头（参与哈希链）+ 原件（只供验签，不参与块哈希）。
///
/// `wires` 缺省即旧格式行：本机重放照旧可跑，但**不能作为快照喂给对端**——
/// `install_snapshot` 对含交易的块要求原件齐全，否则整条快照拒收。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockLine {
    #[serde(flatten)]
    pub header: BlockHeader,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wires: Vec<BlockWire>,
    /// v5 起：本块收录的光尘记录原件（逐条可独立验签验链，块不要求任何人信任）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dusts: Vec<crate::world::dust::DustRecord>,
}

impl BlockWire {
    /// 由注册表/本机钱包的原件字段构造（十六进制载体）
    pub fn new(envelope: &TxEnvelope, sig: &[u8], pk: &[u8], salt: &[u8; 8]) -> Self {
        Self {
            envelope: envelope.clone(),
            sig_hex: hex::encode(sig),
            pk_hex: hex::encode(pk),
            salt_hex: hex::encode(salt),
        }
    }

    /// 独立复验：公钥可解 → 地址可由 `pk ‖ salt` 复算 → 摘要一致 → 签名有效
    fn verify(&self) -> Result<([u8; 32], String), String> {
        if self.envelope.version != crate::tx::TX_VERSION
            || self.envelope.chain_id != crate::tx::CHAIN_ID
        {
            return Err(format!(
                "交易版本/链 id 不符：v{} chain {}",
                self.envelope.version, self.envelope.chain_id
            ));
        }
        if self.envelope.scheme_id != crypto::SCHEME_ML_DSA_65 {
            return Err(format!("未知签名方案 {}", self.envelope.scheme_id));
        }
        let pk = hex::decode(&self.pk_hex).map_err(|_| "公钥十六进制非法")?;
        if pk.len() != crypto::PK_LEN {
            return Err(format!("公钥长度 {} ≠ {}", pk.len(), crypto::PK_LEN));
        }
        let vk = crypto::decode_pk(&pk).ok_or("公钥无法解析为 ML-DSA-65 验证密钥")?;
        let salt_raw = hex::decode(&self.salt_hex).map_err(|_| "地址盐十六进制非法")?;
        if salt_raw.len() != 8 {
            return Err(format!("地址盐长度 {} ≠ 8", salt_raw.len()));
        }
        let mut salt = [0u8; 8];
        salt.copy_from_slice(&salt_raw);
        let addr = crypto::derive_address(&vk, &salt);
        if addr != crate::crypto::to_bech32_ql(&self.envelope.from_addr) {
            return Err("公钥与地址盐复算出的地址与交易 from 不符".into());
        }
        let sig = hex::decode(&self.sig_hex).map_err(|_| "签名十六进制非法")?;
        let digest = tx_digest(&self.envelope);
        if !crypto::verify(&vk, &digest, &sig) {
            return Err("ML-DSA-65 验签失败".into());
        }
        Ok((digest, addr))
    }
}

/// 交易池条目
pub struct PendingTx {
    pub envelope: TxEnvelope,
    pub sig: Vec<u8>,
    pub digest: [u8; 32],
    /// 签名公钥与地址盐原件（追账快照要逐笔独立验签，不能只靠本机注册表）
    pub pk_hex: String,
    pub salt_hex: String,
}

/// 待确认交易视图（活动页「确认中」DTO）
#[derive(Debug, Clone, Serialize)]
pub struct PendingView {
    pub hash: String,
    pub hash_short: String,
    pub to: String,
    pub amount: u64,
    pub fee: u64,
}

/// —— 哈希口径（打包/重放共用同一实现，保证账本可独立审计）——

/// tx_root = SHAKE-256("QL-TXROOT-v1" ‖ concat(digests))；空块为 32 字节全零
pub fn tx_root_of(digests: &[[u8; 32]]) -> [u8; 32] {
    if digests.is_empty() {
        return [0u8; 32];
    }
    let mut all = Vec::with_capacity(digests.len() * 32);
    for d in digests {
        all.extend_from_slice(d);
    }
    let out = crypto::shake256(b"QL-TXROOT-v1", &all, 32);
    let mut root = [0u8; 32];
    root.copy_from_slice(&out);
    root
}

/// 块哈希 = SHAKE-256("QL-BLOCK-V1", prev ‖ height ‖ minted_after ‖ vrf ‖ tx_root)
pub fn block_hash_of(
    prev_hash_hex: &str,
    height: u64,
    minted_after: u64,
    vrf: &[u8],
    root: &[u8; 32],
) -> [u8; 32] {
    let mut block_in: Vec<u8> = Vec::with_capacity(140);
    block_in.extend_from_slice(prev_hash_hex.as_bytes());
    block_in.extend_from_slice(&height.to_be_bytes());
    block_in.extend_from_slice(&minted_after.to_be_bytes());
    block_in.extend_from_slice(vrf);
    block_in.extend_from_slice(root);
    let out = crypto::shake256(b"QL-BLOCK-V1", &block_in, 32);
    let mut h = [0u8; 32];
    h.copy_from_slice(&out);
    h
}

/// v1 起块哈希把 `version` 也绑进去：同一组字段换个版本就是另一条链。
/// v0 仍走 `block_hash_of`（历史块哈希不能动），所以升级不重写任何旧块。
pub fn block_hash_of_ver(
    version: u8,
    prev_hash_hex: &str,
    height: u64,
    minted_after: u64,
    vrf: &[u8],
    root: &[u8; 32],
) -> [u8; 32] {
    if version == BLOCK_V_LEGACY {
        return block_hash_of(prev_hash_hex, height, minted_after, vrf, root);
    }
    let mut block_in: Vec<u8> = Vec::with_capacity(141);
    block_in.push(version);
    block_in.extend_from_slice(prev_hash_hex.as_bytes());
    block_in.extend_from_slice(&height.to_be_bytes());
    block_in.extend_from_slice(&minted_after.to_be_bytes());
    block_in.extend_from_slice(vrf);
    block_in.extend_from_slice(root);
    let out = crypto::shake256(b"QL-BLOCK-V2", &block_in, 32);
    let mut h = [0u8; 32];
    h.copy_from_slice(&out);
    h
}

/// v2/v3 共用的预镜像：版本 ‖ prev ‖ 高 ‖ minted ‖ vrf ‖ tx_root ‖ 世界指纹 ‖ 条数。
/// 只有哈希域随版本不同——预镜像多一个字节都算不出同一个块，历史因此无法被"重新解释"。
fn hash_world_bound(
    ver: u8,
    domain: &[u8],
    prev_hash_hex: &str,
    height: u64,
    minted_after: u64,
    vrf: &[u8],
    root: &[u8; 32],
    entity_root: &[u8; 32],
    entity_count: u64,
) -> [u8; 32] {
    let mut block_in: Vec<u8> = Vec::with_capacity(181);
    block_in.push(ver);
    block_in.extend_from_slice(prev_hash_hex.as_bytes());
    block_in.extend_from_slice(&height.to_be_bytes());
    block_in.extend_from_slice(&minted_after.to_be_bytes());
    block_in.extend_from_slice(vrf);
    block_in.extend_from_slice(root);
    block_in.extend_from_slice(entity_root);
    block_in.extend_from_slice(&entity_count.to_be_bytes());
    let out = crypto::shake256(domain, &block_in, 32);
    let mut h = [0u8; 32];
    h.copy_from_slice(&out);
    h
}

/// v2 的块哈希：预镜像在世界承诺（集合指纹 ‖ 条数）上结束，域 `QL-BLOCK-V3`。
pub fn block_hash_of_world(
    prev_hash_hex: &str,
    height: u64,
    minted_after: u64,
    vrf: &[u8],
    root: &[u8; 32],
    entity_root: &[u8; 32],
    entity_count: u64,
) -> [u8; 32] {
    hash_world_bound(
        BLOCK_V_WORLD,
        b"QL-BLOCK-V3",
        prev_hash_hex,
        height,
        minted_after,
        vrf,
        root,
        entity_root,
        entity_count,
    )
}

/// v3 的块哈希：同一份预镜像、另一个域 `QL-BLOCK-V4`（排放节奏换了规则，域必须换）。
pub fn block_hash_of_craft(
    prev_hash_hex: &str,
    height: u64,
    minted_after: u64,
    vrf: &[u8],
    root: &[u8; 32],
    entity_root: &[u8; 32],
    entity_count: u64,
) -> [u8; 32] {
    hash_world_bound(
        BLOCK_V_CRAFT,
        b"QL-BLOCK-V4",
        prev_hash_hex,
        height,
        minted_after,
        vrf,
        root,
        entity_root,
        entity_count,
    )
}

/// v4 的块哈希：在 v3 的预镜像上再加入 `ts_unix` 与 `miner`，域 `QL-BLOCK-V5`。
///
/// 为什么必须绑：块哈希原先只覆盖 prev/height/minted/vrf/tx_root/世界承诺，
/// 无矿签的块可以改 `miner` 而不改哈希——手续费（v0 还有整笔奖励）就能被偷。
/// 时间戳同理：它是块的一部分，不能留在哈希之外任人改写。
pub fn block_hash_of_miner_bound(
    prev_hash_hex: &str,
    height: u64,
    ts_unix: u64,
    minted_after: u64,
    vrf: &[u8],
    root: &[u8; 32],
    entity_root: &[u8; 32],
    entity_count: u64,
    miner: &str,
) -> [u8; 32] {
    let mut block_in: Vec<u8> = Vec::with_capacity(189 + miner.len());
    block_in.push(BLOCK_V_MINER);
    block_in.extend_from_slice(prev_hash_hex.as_bytes());
    block_in.extend_from_slice(&height.to_be_bytes());
    block_in.extend_from_slice(&ts_unix.to_be_bytes());
    block_in.extend_from_slice(&minted_after.to_be_bytes());
    block_in.extend_from_slice(vrf);
    block_in.extend_from_slice(root);
    block_in.extend_from_slice(entity_root);
    block_in.extend_from_slice(&entity_count.to_be_bytes());
    block_in.extend_from_slice(miner.as_bytes());
    let out = crypto::shake256(b"QL-BLOCK-V5", &block_in, 32);
    let mut h = [0u8; 32];
    h.copy_from_slice(&out);
    h
}

/// v5 的块哈希：在 v4 的预镜像上再绑 `dust_root`（块内光尘记录的集合指纹）
/// 与条数，域 `QL-BLOCK-V6`。
///
/// 为什么光尘要进块哈希：光尘是池子另一侧的资产，只有"哪些光尘记录被全网
/// 接受过"变成不可改写的事实，恒定乘积曲线才不会变成一句空话。
pub fn block_hash_of_dust(
    prev_hash_hex: &str,
    height: u64,
    ts_unix: u64,
    minted_after: u64,
    vrf: &[u8],
    root: &[u8; 32],
    entity_root: &[u8; 32],
    entity_count: u64,
    miner: &str,
    dust_root: &[u8; 32],
    dust_count: u64,
) -> [u8; 32] {
    let mut block_in: Vec<u8> = Vec::with_capacity(197 + miner.len());
    block_in.push(BLOCK_V_POOL);
    block_in.extend_from_slice(prev_hash_hex.as_bytes());
    block_in.extend_from_slice(&height.to_be_bytes());
    block_in.extend_from_slice(&ts_unix.to_be_bytes());
    block_in.extend_from_slice(&minted_after.to_be_bytes());
    block_in.extend_from_slice(vrf);
    block_in.extend_from_slice(root);
    block_in.extend_from_slice(entity_root);
    block_in.extend_from_slice(&entity_count.to_be_bytes());
    block_in.extend_from_slice(miner.as_bytes());
    block_in.extend_from_slice(dust_root);
    block_in.extend_from_slice(&dust_count.to_be_bytes());
    let out = crypto::shake256(b"QL-BLOCK-V6", &block_in, 32);
    let mut h = [0u8; 32];
    h.copy_from_slice(&out);
    h
}

/// 块内光尘记录的集合指纹：与顺序无关（升序哈希后取 SHAKE-256）。
/// 空集合返回全零，和 `tx_root_of` 的空块口径一致。
pub fn dust_root_of(records: &[crate::world::dust::DustRecord]) -> [u8; 32] {
    if records.is_empty() {
        return [0u8; 32];
    }
    let mut hashes: Vec<[u8; 32]> = records.iter().map(crate::world::dust::record_hash).collect();
    hashes.sort_unstable();
    hashes.dedup();
    let mut buf = Vec::with_capacity(hashes.len() * 32);
    for h in &hashes {
        buf.extend_from_slice(h);
    }
    let out = crypto::shake256(b"QL-DUSTROOT-V1", &buf, 32);
    let mut h = [0u8; 32];
    h.copy_from_slice(&out);
    h
}

/// 三个校验点（本地重放 / 联邦块 / 追账快照）共用的分域入口。
///
/// v2 起 `entity_root_hex` 必须是 64 个 hex：一个"声称自己是 v2 却没有世界承诺"的
/// 块形同作废，不该由哈希函数替它编一个域。
pub fn block_hash_of_header(
    bh: &BlockHeader,
    vrf: &[u8],
    root: &[u8; 32],
) -> Option<[u8; 32]> {
    if bh.version < BLOCK_V_WORLD {
        return Some(block_hash_of_ver(
            bh.version,
            &bh.prev_hash_hex,
            bh.height,
            bh.minted_after,
            vrf,
            root,
        ));
    }
    if !(BLOCK_V_WORLD..=BLOCK_V).contains(&bh.version) {
        return None;
    }
    let raw = hex::decode(&bh.entity_root_hex).ok()?;
    if raw.len() != 32 {
        return None;
    }
    let mut er = [0u8; 32];
    er.copy_from_slice(&raw);
    if bh.version >= BLOCK_V_POOL {
        let dr_raw = hex::decode(&bh.dust_root_hex).ok()?;
        if dr_raw.len() != 32 {
            return None;
        }
        let mut dr = [0u8; 32];
        dr.copy_from_slice(&dr_raw);
        return Some(block_hash_of_dust(
            &bh.prev_hash_hex,
            bh.height,
            bh.ts_unix,
            bh.minted_after,
            vrf,
            root,
            &er,
            bh.entity_count,
            &bh.miner,
            &dr,
            bh.dust_count,
        ));
    }
    Some(match bh.version {
        BLOCK_V_WORLD => block_hash_of_world(
            &bh.prev_hash_hex,
            bh.height,
            bh.minted_after,
            vrf,
            root,
            &er,
            bh.entity_count,
        ),
        BLOCK_V_CRAFT => block_hash_of_craft(
            &bh.prev_hash_hex,
            bh.height,
            bh.minted_after,
            vrf,
            root,
            &er,
            bh.entity_count,
        ),
        BLOCK_V_MINER => block_hash_of_miner_bound(
            &bh.prev_hash_hex,
            bh.height,
            bh.ts_unix,
            bh.minted_after,
            vrf,
            root,
            &er,
            bh.entity_count,
            &bh.miner,
        ),
        _ => return None,
    })
}

/// 矿签目标：世界坐标本身（`QLOR` ‖ x ‖ y ‖ layer）。没有私钥，也没人需要私钥。
pub fn claim_target(x: u16, y: u16, layer: u16) -> [u8; 32] {
    let mut t = [0u8; 32];
    t[..4].copy_from_slice(&CLAIM_TAG);
    t[4..6].copy_from_slice(&x.to_be_bytes());
    t[6..8].copy_from_slice(&y.to_be_bytes());
    t[8..10].copy_from_slice(&layer.to_be_bytes());
    t
}

/// 从信封里解出矿签坐标：必须是**单动作、零金额、目标带 QLOR 前缀**。
pub fn claim_cell_of(e: &TxEnvelope) -> Option<(u16, u16, u16)> {
    if e.actions.len() != 1 {
        return None;
    }
    let a = &e.actions[0];
    if a.amount != 0 || a.to[..4] != CLAIM_TAG {
        return None;
    }
    Some((
        u16::from_be_bytes([a.to[4], a.to[5]]),
        u16::from_be_bytes([a.to[6], a.to[7]]),
        u16::from_be_bytes([a.to[8], a.to[9]]),
    ))
}

/// 池子交易在凭据/历史里的收款方标签。
///
/// 池子没有地址：这串东西故意**不是**合法 bech32，所以 `claim_cell_of_attestation`
/// 不会把它误读成矿签，UI 也能一眼分辨"这不是转给谁的"。
pub fn pool_label() -> String {
    format!("qlpool:{}", crate::world::pool::POOL_ID)
}

/// 块头凭据里的矿签坐标（快照校验只拿得到凭据，拿不到原件）
pub fn claim_cell_of_attestation(t: &TxAttestation) -> Option<(u16, u16, u16)> {
    let to = bech32_decode_addr(&t.to).ok()?;
    // 远端块头是敌意输入：bech32 载荷可能不是 32 字节，切片前必须量长度
    if to.len() < 10 || to[..4] != CLAIM_TAG || t.amount != 0 {
        return None;
    }
    Some((
        u16::from_be_bytes([to[4], to[5]]),
        u16::from_be_bytes([to[6], to[7]]),
        u16::from_be_bytes([to[8], to[9]]),
    ))
}

/// 一张矿签在块高 `h` 能发行多少：把 50 个块的排放一次性结算，并按硬顶截断。
///
/// 为什么要 ×50：每 50 块才允许一次发行，若仍只付一个块的量，发行速度会掉到
/// 原来的 1/50；×50 让"挖得勤"与旧的每块排放同速，而"没人挖"就真的不发。
pub fn claim_reward(h: u64, minted_before: u64) -> u64 {
    reward_at(h)
        .saturating_mul(MINE_COOLDOWN_BLOCKS)
        .min(HARD_CAP.saturating_sub(minted_before))
}

/// v3 的发行量：只结算本次窗口（最多一个当前档位）的排放，按硬顶截断。
///
/// 为什么这样不会打乱挖币机制：相邻两笔发行覆盖的块区间互不重叠，每笔只拿自己那
/// 一个档位的量，空档不补发，所以 Σ 发行 ≤ Σ `reward_at(h)`——同一条减半曲线，
/// 同一个 210M 硬顶。节奏阶梯只能决定"多久结算一次"，决定不了"总共发多少"。
pub fn claim_reward_delta(h: u64, minted_before: u64, delta: u64) -> u64 {
    reward_at(h)
        .saturating_mul(delta)
        .min(HARD_CAP.saturating_sub(minted_before))
}

/// v3 的节奏阶梯：这个地址在世界里已经落下几格矿，下一次结算就来得多快。
/// 0 格 = 50 块（与 v1/v2 完全同口径），每多一格降一档，最快 10 块。
///
/// 反脚本的关键在**按地址计**：脚本可以开一万个钥匙，但每把新钥匙都从最慢那一档
/// 开始——分身买不到速度。速度只有同一个身份长期回到同一张世界才换得到，
/// 而每一次结算都必须真的在世界里落下一格可复算的矿（`is_ore` + 贴地 + 一格一主）。
pub fn craft_floor(prior_claims: u64) -> u64 {
    let idx = (prior_claims as usize).min(crate::world::MINE_FLOORS.len() - 1);
    crate::world::MINE_FLOORS[idx]
}

/// 这一笔矿签覆盖的窗口：返回 (当前档位, 本次实际结算的块数)；间隔未到 → `None`。
///
/// v1/v2 固定 50 块（历史口径，只为重放）；v3+ 的档位由**这个地址自己**落下过
/// 几格矿决定（50→10）。
///
/// **空块不补发**：每笔最多结清一个当前档位的窗口。没有矿签的块，其发行额度
/// 当场作废，不会累积给下一个矿工——否则「等得越久、一笔越大」，发行就不再是
/// 挖矿的回报，而是计时器的欠条。首笔同样只结真实存在的块（`min(height, 档位)`），
/// 绝不预支尚未发生的窗口；否则首笔会与下一笔重复覆盖同一段块高。
fn claim_window(ver: u8, height: u64, last_claim: u64, prior_claims: u64) -> Option<(u64, u64)> {
    let floor = if ver >= BLOCK_V_CRAFT {
        craft_floor(prior_claims)
    } else {
        MINE_COOLDOWN_BLOCKS
    };
    if last_claim == 0 {
        // 历史 v1/v2 首笔固定结清 50 块（那时还没有阶梯）；v3+ 只结真实存在的块。
        let delta = if ver >= BLOCK_V_CRAFT {
            height.min(floor)
        } else {
            floor
        };
        return Some((floor, delta));
    }
    if height < last_claim + floor {
        return None;
    }
    // v3+ 一笔最多一个档位：超过档位的空档不补发。v1/v2 的 delta 只用于历史重放
    // （`header_emission` 对 v1/v2 走固定 `claim_reward`，不读这个值）。
    let delta = if ver >= BLOCK_V_CRAFT {
        floor
    } else {
        height - last_claim
    };
    Some((floor, delta))
}

/// 当前账本的结算水位：v0 每块都发过，水位至少跟到链尖；
/// v1+ 只认最后一张矿签。升级路径和冷启动都走这一处，避免两套口径。
fn emission_watermark(l: &Ledger) -> u64 {
    if l.last_block_version == BLOCK_V_LEGACY && l.height > 0 {
        l.height.max(l.last_claim_height)
    } else {
        l.last_claim_height
    }
}

pub struct SimChain {
    pub ledger: Arc<Mutex<Ledger>>,
    mempool: Arc<Mutex<Vec<PendingTx>>>,
    /// 本机最近签发的交易原件（孤儿块交易回灌用，环形 64 条）
    sent_cache: Arc<Mutex<VecDeque<BlockWire>>>,
    ledger_path: PathBuf,
    blocks_path: PathBuf,
    /// 整本实体账本（v2 出块要向它承诺）。用 Arc&lt;RwLock&gt; 是因为 `SimChain` 会被
    /// clone 给网络线程与命令层，挂载必须对所有副本同时可见。
    world: Arc<std::sync::RwLock<Option<Arc<crate::world::EntityLedger>>>>,
    /// 本机光尘记录账本（v5 出块时从里面挑还没进块的记录）。
    /// 与 `world` 同一套挂载方式：链、网络、命令层共用同一个 Arc。
    dust_store: Arc<std::sync::RwLock<Option<Arc<crate::world::dust::DustLedger>>>>,
    /// 启动哈希链校验失败置 true，禁出新块与收交易
    pub corrupted: Arc<Mutex<bool>>,
    /// 本次启动是否归档重置过链；tag 供上层把同一代的 world 账本一起归档
    reset_tag: Mutex<Option<String>>,
}

#[derive(Serialize, Clone)]
pub struct BlockEvent {
    pub height: u64,
    pub block_hash_hex: String,
    pub vrf_hex: String,
    /// 本块总奖励（发行 + 手续费），用于余额/日志展示
    pub reward: u64,
    /// 本块真正新发行的币（不含手续费）：UI 只对 `minted > 0` 的块落"铸块砖"
    pub minted: u64,
    pub balance: u64,
    pub total_minted: u64,
    pub ts_unix: u64,
    pub txs_count: usize,
}

/// 原子写：tmp + rename，进程中断不产生半截文件；
/// rename 前 fsync——掉电时也不落半截数据（2s 出块节奏下开销可忽略）
pub(crate) fn atomic_write(path: &PathBuf, bytes: &[u8]) {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let ok = std::fs::File::create(&tmp).ok().and_then(|mut f| {
        f.write_all(bytes).ok().and_then(|_| f.sync_all().ok())
    });
    if ok.is_some() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl SimChain {
    pub fn new(ledger_path: PathBuf, blocks_path: PathBuf) -> Arc<Self> {
        // 信任根先于一切：创世哈希对不上锚定值就没有必要读任何账本
        assert_genesis_anchored();

        let ledger = match std::fs::read(&ledger_path) {
            Ok(bytes) => serde_json::from_slice::<Ledger>(&bytes)
                .ok()
                .filter(|l| l.v == LEDGER_V)
                .unwrap_or_default(),
            Err(_) => Ledger::default(),
        };

        let chain = Self {
            ledger: Arc::new(Mutex::new(ledger)),
            mempool: Arc::new(Mutex::new(Vec::new())),
            sent_cache: Arc::new(Mutex::new(VecDeque::new())),
            ledger_path,
            blocks_path,
            world: Arc::new(std::sync::RwLock::new(None)),
            dust_store: Arc::new(std::sync::RwLock::new(None)),
            corrupted: Arc::new(Mutex::new(false)),
            reset_tag: Mutex::new(None),
        };

        // 上次进程中断可能残留 .tmp 半成品（无害但清理干净）
        let _ = std::fs::remove_file(chain.ledger_path.with_extension("tmp"));
        let _ = std::fs::remove_file(chain.blocks_path.with_extension("tmp"));

        chain.ensure_genesis_line();
        if !chain.validate_blocks_jsonl() {
            // regtest 演示口径：旧格式（无 miner/nonce 字段的 v1 块）或损坏文件 →
            // 归档为 .bak-epoch 后从新创世继续；数据不丢，仅链重置。
            eprintln!(
                "[QLight] 提醒：blocks.jsonl 哈希链校验未通过（可能为旧格式或文件被改动），已归档并重启演示链"
            );
            let tag = chain.archive_and_reset();
            chain.ensure_genesis_line();
            *chain.reset_tag.lock().unwrap() = Some(tag);
        }
        chain.sync_chain_meta();
        Arc::new(chain)
    }

    /// 本次启动归档重置过链时返回归档 tag（只取一次）。
    ///
    /// 上层拿它把同一代的 world 账本（entities/dust）一起归档：链一重置，
    /// 旧世界的签名记录就指向了不再存在的块，留着只会把新世界锁死。
    pub fn take_reset_tag(&self) -> Option<String> {
        self.reset_tag.lock().ok().and_then(|mut t| t.take())
    }

    /// 从 blocks.jsonl 重建"由链本身决定"的运行态：
    /// 全部历史 VRF（地表种子）、结算水位、每个地址的矿格数、链尖版本。
    ///
    /// 这些都不进 ledger.json 的持久口径（VRF 会随链无限增长），启动时宁可多读
    /// 一遍块文件，也不让一份可能过期的副本变成第二真相。
    fn sync_chain_meta(&self) {
        let Ok(text) = std::fs::read_to_string(&self.blocks_path) else { return };
        let mut vrfs: Vec<String> = Vec::new();
        let mut last_claim = 0u64;
        let mut last_v0 = 0u64;
        let mut last_version = BLOCK_V_LEGACY;
        let mut claims_by: BTreeMap<String, u64> = BTreeMap::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(bl) = serde_json::from_str::<BlockLine>(line) else { continue };
            if bl.header.height == 0 {
                continue;
            }
            vrfs.push(bl.header.vrf_hex.clone());
            last_version = bl.header.version;
            if bl.header.version == BLOCK_V_LEGACY {
                last_v0 = bl.header.height;
            }
            if let Some(t) = bl
                .header
                .txs
                .iter()
                .find(|t| claim_cell_of_attestation(t).is_some())
            {
                last_claim = last_claim.max(bl.header.height);
                *claims_by.entry(t.from.clone()).or_default() += 1;
            }
        }
        let mut l = self.ledger.lock().unwrap();
        l.vrfs = vrfs;
        // v0 每块都发过：水位至少跟到最后一条 v0 块，避免升级后重复补发。
        l.last_claim_height = last_claim.max(last_v0);
        l.last_block_version = last_version;
        l.claims_by = claims_by;
    }

    /// 保证 blocks.jsonl 第 0 行是创世块。
    ///
    /// 新档直接写入；旧档（首行即 #1 且 prev 正是创世哈希）一次性前置补写，
    /// 让"创世是否启动"在文件里可查。其余形态一律不动，交给重放校验裁决。
    fn ensure_genesis_line(&self) {
        let Ok(line) = serde_json::to_string(&genesis_header()) else {
            return;
        };
        let content = std::fs::read_to_string(&self.blocks_path).unwrap_or_default();
        match content.lines().find(|l| !l.trim().is_empty()) {
            None => atomic_write(&self.blocks_path, format!("{line}\n").as_bytes()),
            Some(first) => {
                if let Ok(h) = serde_json::from_str::<BlockHeader>(first) {
                    if h.height == 1 && h.prev_hash_hex == genesis_hash_hex() {
                        atomic_write(
                            &self.blocks_path,
                            format!("{line}\n{content}").as_bytes(),
                        );
                    }
                }
            }
        }
    }

    /// 校验失败的账本文件归档（ledger/blocks → *.bak-epoch），并从新创世继续。
    /// 返回归档 tag，调用方用它给 world 账本打同一枚戳。
    fn archive_and_reset(&self) -> String {
        let tag = now_unix().to_string();
        let _ = std::fs::rename(
            &self.blocks_path,
            self.blocks_path.with_extension(format!("jsonl.bak-{tag}")),
        );
        let _ = std::fs::rename(
            &self.ledger_path,
            self.ledger_path.with_extension(format!("json.bak-{tag}")),
        );
        *self.ledger.lock().unwrap() = Ledger::default();
        *self.corrupted.lock().unwrap() = false;
        tag
    }

    /// 重放校验：逐行读块头，重算块哈希并与记录比对；无 blocks 文件视为通过
    fn validate_blocks_jsonl(&self) -> bool {
        let content = match std::fs::read_to_string(&self.blocks_path) {
            Ok(c) => c,
            Err(_) => return true,
        };
        let mut headers: Vec<BlockHeader> = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<BlockHeader>(line) {
                Ok(b) => headers.push(b),
                Err(_) => return false,
            }
        }
        if !Self::verify_header_slice(&headers) {
            return false;
        }
        let tail = headers
            .last()
            .map(|h| h.block_hash_hex.clone())
            .unwrap_or_else(genesis_hash_hex);
        // 尾块哈希必须等于账本当前 prev_hash
        let ledger = self.ledger.lock().unwrap();
        ledger.prev_hash_hex == tail
    }

    pub fn height(&self) -> u64 {
        self.ledger.lock().unwrap().height
    }

    pub fn balance_of(&self, addr: &str) -> u64 {
        self.ledger
            .lock()
            .unwrap()
            .balances
            .get(addr)
            .copied()
            .unwrap_or(0)
    }

    pub fn records(&self) -> Vec<TxRecord> {
        let l = self.ledger.lock().unwrap();
        l.records.iter().rev().take(200).cloned().collect()
    }

    /// 池子储备（未激活 / 从未做市时是空池，不是"不存在"）
    pub fn pool_pub(&self) -> crate::world::pool::Pool {
        self.ledger
            .lock()
            .unwrap()
            .pools
            .get(crate::world::pool::POOL_ID)
            .cloned()
            .unwrap_or_default()
    }

    /// 共识结算后的光尘余额：v5 之前恒为 0（那时光尘只活在 gossip 账本里）
    pub fn dust_balance_pub(&self, addr: &str) -> u64 {
        self.ledger.lock().unwrap().dust.balance_of(addr)
    }

    /// v5 是否已经生效（激活高度之后出过块）。界面据此区分"已确认/待确认"光尘。
    pub fn dust_fork_active(&self) -> bool {
        self.ledger.lock().unwrap().last_block_version >= BLOCK_V_POOL
    }

    /// 内存池中某地址的待确认交易（活动页「确认中」条目；出块后自动消失）
    pub fn pending_of(&self, addr_bech32: &str) -> Vec<PendingView> {
        let want = addr_hex_of(addr_bech32);
        let mp = self.mempool.lock().unwrap();
        mp.iter()
            .filter(|p| hex::encode(&p.envelope.from_addr) == want)
            .map(|p| {
                let h = hex::encode(&p.digest);
                let (to, amount) = p
                    .envelope
                    .actions
                    .first()
                    .map(|a| (crypto::to_bech32_ql(&a.to), a.amount))
                    .unwrap_or((String::new(), 0));
                PendingView {
                    hash_short: format!("{}…{}", &h[..10], &h[54..]),
                    hash: h,
                    to,
                    amount,
                    fee: FEE,
                }
            })
            .collect()
    }

    pub fn mempool_len(&self) -> usize {
        self.mempool.lock().unwrap().len()
    }

    /// 已应用 nonce + 该地址池中未入块笔数（连续发送不重 nonce）
    pub fn pending_nonce(&self, addr: &str) -> u64 {
        let ledger = self.ledger.lock().unwrap();
        let applied = ledger.nonces.get(addr).copied().unwrap_or(0);
        drop(ledger);
        let mp = self.mempool.lock().unwrap();
        let queued = mp
            .iter()
            .filter(|p| hex::encode(&p.envelope.from_addr) == addr_hex_of(addr))
            .count() as u64;
        applied + queued
    }

    /// 提交交易：完整协议校验（真验签/nonce/可支配余额/过期），入池不改状态
    pub fn submit(
        &self,
        core: &crate::keystore::WalletCore,
        envelope: TxEnvelope,
        sig: Vec<u8>,
    ) -> Result<[u8; 32], String> {
        if *self.corrupted.lock().unwrap() {
            return Err("账本完整性校验未通过，已停止收单（详见日志）".into());
        }
        let pool_op = envelope.pool.clone();
        if pool_op.is_some() {
            if !envelope.actions.is_empty() {
                return Err("池子操作不能夹带转账".into());
            }
        } else if envelope.actions.is_empty() {
            return Err("交易不包含任何转账".into());
        }
        // 零金额只允许一种形态：矿签（单动作、目标是 QLOR 世界坐标）。
        // 它不付手续费——挖矿必须能从"余额为 0 的新钱包"开始，否则发行就被锁死。
        let claim = claim_cell_of(&envelope);
        if claim.is_none()
            && pool_op.is_none()
            && envelope.actions.iter().any(|a| a.amount == 0)
        {
            return Err("转账金额必须大于 0".into());
        }
        let tx_fee = if claim.is_some() || pool_op.is_some() { 0 } else { FEE };

        // —— 真 ML-DSA-65 验签：对 SHAKE-256 digest ——
        let digest = tx_digest(&envelope);
        if !crypto::verify(&core.verifying_key, &digest, &sig) {
            return Err("签名校验失败（ML-DSA-65 verify）".into());
        }

        let from = crate::crypto::to_bech32_ql(&envelope.from_addr);
        if from != core.address {
            return Err("from_addr 与当前账户不符".into());
        }
        let from_hex = hex::encode(&envelope.from_addr);

        {
            let ledger = self.ledger.lock().unwrap();
            let mp = self.mempool.lock().unwrap();

            // nonce：必须 = 已应用 nonce + 本地址池中未入块笔数
            let queued: u64 = mp
                .iter()
                .filter(|p| hex::encode(&p.envelope.from_addr) == from_hex)
                .count() as u64;
            let expect = ledger.nonces.get(&from).copied().unwrap_or(0) + queued;
            if envelope.nonce != expect {
                return Err(format!(
                    "nonce 不匹配：期望 {expect}，收到 {}",
                    envelope.nonce
                ));
            }
            if envelope.expiry <= ledger.height {
                return Err("交易已过期（expiry 块高）".into());
            }
            // 池子：先确认下一块真的是 v5，再在副本上试算一遍（磁盘上的账本不许动）
            if let Some(op) = &pool_op {
                if version_floor_at(ledger.height + 1) < BLOCK_V_POOL {
                    return Err(format!(
                        "兑换池要等硬分叉 v5（块高 {} 之后）才开放，当前 #{}",
                        BLOCK_V_V5_ACTIVATION + 1,
                        ledger.height
                    ));
                }
                let mut trial = ledger.clone();
                Self::apply_pool_op(&mut trial, &from, op)
                    .map_err(|e| format!("池子操作不成立：{e}"))?;
            }

            // 可支配余额 = 余额 − 池中本地址未入块总支出
            let projected_spend: u64 = mp
                .iter()
                .filter(|p| hex::encode(&p.envelope.from_addr) == from_hex)
                .map(|p| {
                    p.envelope.actions.iter().map(|a| a.amount).sum::<u64>()
                        + if claim_cell_of(&p.envelope).is_some() { 0 } else { FEE }
                })
                .sum();
            let need: u64 = envelope.actions.iter().map(|a| a.amount).sum::<u64>() + tx_fee;
            let bal = ledger.balances.get(&from).copied().unwrap_or(0);
            let spendable = bal.saturating_sub(projected_spend);
            if spendable < need {
                return Err(format!(
                    "余额不足：可支配 {}（已扣除未入块支出），本次需要 {}（含手续费 0.001 QLX），当前余额 {}",
                    spendable, need, bal
                ));
            }
            // 矿签在此刻（= 下一个块的 h−1）必须是矿，且落在贴地层
            if let Some((x, y, layer)) = claim {
                let words = crate::world::terrain::seed_words(GENESIS_HASH_HEX, &ledger.vrfs);
                if !crate::world::ore::is_ore(x as u32, y as u32, &words, ledger.height)
                    || crate::world::ore::ore_layer(x as u32, y as u32, &words) != layer
                {
                    return Err(format!("({x},{y}) 第 {layer} 层此刻不是矿——地表已经重画"));
                }
            }
            if mp.len() >= MEMPOOL_CAP {
                return Err("交易池已满，等待下一块打包".into());
            }
        }

        let wire = BlockWire::new(&envelope, &sig, &core.pk_bytes, &core.addr_salt);
        self.mempool.lock().unwrap().push(PendingTx {
            envelope,
            sig,
            digest,
            pk_hex: wire.pk_hex.clone(),
            salt_hex: wire.salt_hex.clone(),
        });
        // 进签发缓存（分叉切换时孤儿交易回灌用；只缓存本机签发的交易）
        {
            let mut cache = self.sent_cache.lock().unwrap();
            cache.push_back(wire);
            while cache.len() > 64 {
                cache.pop_front();
            }
        }
        Ok(digest)
    }

    /// 提交一张矿签：零金额、零手续费，收款方是矿格坐标本身。
    /// 返回 (原件, 摘要)——原件要立刻广播，让赢家出块时能收下。
    pub fn submit_claim(
        &self,
        core: &crate::keystore::WalletCore,
        x: u16,
        y: u16,
        layer: u16,
    ) -> Result<(BlockWire, [u8; 32]), String> {
        if x as u32 >= crate::world::WORLD_W as u32
            || y as u32 >= crate::world::WORLD_H as u32
            || layer >= crate::world::MAX_LAYER
        {
            return Err(format!("矿格 ({x},{y}) 第 {layer} 层越界"));
        }
        let from = bech32_decode_addr(&core.address)?;
        let expiry = self.height() + MINE_COOLDOWN_BLOCKS + 10;
        let envelope =
            TxEnvelope::new(from, self.pending_nonce(&core.address), expiry)
                .transfer(claim_target(x, y, layer), 0);
        let digest = tx_digest(&envelope);
        let sig = crypto::sign(&core.signing_key, &digest);
        self.submit(core, envelope.clone(), sig.clone())?;
        let wire = BlockWire::new(&envelope, &sig, &core.pk_bytes, &core.addr_salt);
        Ok((wire, digest))
    }

    /// 上一次结算/发行水位所在块高（0 = 从未）
    pub fn last_claim_height(&self) -> u64 {
        emission_watermark(&self.ledger.lock().unwrap())
    }

    /// 下一笔挖矿最早能结算的块高：按**这把钥匙自己**的节奏阶梯（一格矿都没落过 = 50 块）。
    pub fn next_claim_height(&self, miner: &str) -> u64 {
        let l = self.ledger.lock().unwrap();
        let floor = craft_floor(l.claims_by.get(miner).copied().unwrap_or(0));
        let last = emission_watermark(&l);
        if last == 0 {
            l.height + 1
        } else {
            (last + floor).max(l.height + 1)
        }
    }

    /// 这把钥匙的挖矿节奏：(已落下的矿格数, 当前的最小间隔)。
    /// 只有这两个数能告诉用户"再落一格，结算就快到 X 块"——它买的是节奏，不是更多的币。
    pub fn claim_pace(&self, miner: &str) -> (u64, u64) {
        let l = self.ledger.lock().unwrap();
        let prior = l.claims_by.get(miner).copied().unwrap_or(0);
        (prior, craft_floor(prior))
    }

    /// 下一次发行的预览：(最早结算块高, 那一笔的量)。
    /// 量按「最多一个当前档位」算——阶梯越低单笔越小、结算越勤，总速度因此一分不变；
    /// 空档不补发，所以预览值就是到账值（除非硬顶先到）。
    pub fn claim_preview(&self, miner: &str) -> (u64, u64) {
        let l = self.ledger.lock().unwrap();
        let prior = l.claims_by.get(miner).copied().unwrap_or(0);
        let floor = craft_floor(prior);
        let last = emission_watermark(&l);
        let next_h = if last == 0 {
            l.height + 1
        } else {
            (last + floor).max(l.height + 1)
        };
        let delta = claim_window(BLOCK_V, next_h, last, prior)
            .map(|(_, d)| d)
            .unwrap_or(floor);
        (next_h, claim_reward_delta(next_h, l.total_minted, delta))
    }

    /// 出块（由 net 网络循环每 2s 调用）：池内逐笔真验签 → 状态转移 → 块哈希绑定交易 → 原子持久化。
    /// 返回 (块事件, 块头, 块内交易原件) —— 块头+交易原件供联邦网络广播给对端复验。
    ///
    /// v1 规则：**只有收下了一张本机矿签，这个块才发行**；否则块照出、手续费照收，
    /// 但 `reward_total` 里没有任何新币。币因此不是计时器印的，是世界里多了一格
    /// 别人可复算的坐标换来的（§9.13）。
    pub fn mine_block(
        &self,
        core: &crate::keystore::WalletCore,
    ) -> Option<(BlockEvent, BlockHeader, Vec<BlockWire>, Vec<crate::world::dust::DustRecord>)> {
        if *self.corrupted.lock().unwrap() {
            return None;
        }
        let mut ledger = self.ledger.lock().unwrap();
        let height_next = ledger.height + 1;
        let prev_hash_before = ledger.prev_hash_hex.clone();
        // 分叉窗口：激活高度之前照旧出 v4（还没升级的机器也认），之后只出 v5
        let version = if height_next > BLOCK_V_V5_ACTIVATION {
            BLOCK_V_POOL
        } else {
            BLOCK_V_MINER
        };

        // —— VRF 存活心跳（轻节点证明；输入绑定钱包种子/高度/前块哈希）——
        let mut vrf_in: Vec<u8> = Vec::with_capacity(80);
        vrf_in.extend_from_slice(core.seed.as_ref());
        vrf_in.extend_from_slice(&height_next.to_be_bytes());
        vrf_in.extend_from_slice(prev_hash_before.as_bytes());
        let vrf = crypto::shake256(b"QL-VRF-v1", &vrf_in, 32);

        // —— 先挑本机矿签：必须在这一档节奏之外、且此刻（h−1 的地表）仍是矿 ——
        let words = crate::world::terrain::seed_words(GENESIS_HASH_HEX, &ledger.vrfs);
        let prior_mine = ledger
            .claims_by
            .get(&core.address)
            .copied()
            .unwrap_or(0);
        // 旧 v0 链每块都发过：水位必须跟到链尖，否则升级后的第一笔 v4
        // 会把 v0 已经发过的 50 个块再补一遍（双重发行）。
        let last_claim_before = emission_watermark(&ledger);
        if last_claim_before != ledger.last_claim_height {
            ledger.last_claim_height = last_claim_before;
        }
        let window = claim_window(version, height_next, last_claim_before, prior_mine);
        let can_claim = window.is_some();
        let mut claim_tx: Option<PendingTx> = None;
        let mut taken: Vec<PendingTx> = Vec::with_capacity(TXS_PER_BLOCK);
        {
            let mut mp = self.mempool.lock().unwrap();
            if can_claim {
                if let Some(pos) = mp.iter().position(|p| {
                    let from = crate::crypto::to_bech32_ql(&p.envelope.from_addr);
                    from == core.address
                        && p.envelope.expiry > ledger.height
                        && claim_cell_of(&p.envelope).is_some_and(|(x, y, l)| {
                            crate::world::ore::is_ore(x as u32, y as u32, &words, ledger.height)
                                && crate::world::ore::ore_layer(x as u32, y as u32, &words) == l
                        })
                }) {
                    claim_tx = Some(mp.remove(pos));
                }
            }
            let reserve = usize::from(claim_tx.is_some());
            let mut i = 0;
            while i < mp.len() && taken.len() + reserve < TXS_PER_BLOCK {
                if claim_cell_of(&mp[i].envelope).is_some() {
                    i += 1; // 别人的矿签留在池里等自己的回合
                    continue;
                }
                taken.push(mp.remove(i));
            }
        }

        let mut included: Vec<TxAttestation> = Vec::with_capacity(taken.len() + 1);
        let mut digests: Vec<[u8; 32]> = Vec::with_capacity(taken.len() + 1);
        let mut wires: Vec<BlockWire> = Vec::with_capacity(taken.len() + 1);
        let mut fee_pool: u64 = 0;
        let ts = now_unix();
        let miner = core.address.clone();

        // 矿签先执行：同一地址的普通交易 nonce 紧随其后
        if let Some(p) = claim_tx {
            let wire = BlockWire {
                envelope: p.envelope.clone(),
                sig_hex: hex::encode(&p.sig),
                pk_hex: p.pk_hex.clone(),
                salt_hex: p.salt_hex.clone(),
            };
            match Self::apply_wire(&mut ledger, &wire, height_next, ts, &miner, version) {
                Ok((att, digest, fee)) => {
                    included.push(att);
                    digests.push(digest);
                    wires.push(wire);
                    fee_pool += fee;
                }
                Err(e) => eprintln!("[QLight] 池中矿签未能结算，留待下回合：{e}"),
            }
        }
        for p in taken {
            // 打包时按**这笔交易自己的公钥**复验原件：过去这里用本机 core 的公钥验，
            // 于是邻居的交易永远验不过、被静默丢弃——等于内置审查。
            let wire = BlockWire {
                envelope: p.envelope.clone(),
                sig_hex: hex::encode(&p.sig),
                pk_hex: p.pk_hex.clone(),
                salt_hex: p.salt_hex.clone(),
            };
            match Self::apply_wire(&mut ledger, &wire, height_next, ts, &miner, version) {
                Ok((att, digest, fee)) => {
                    included.push(att);
                    digests.push(digest);
                    wires.push(wire);
                    fee_pool += fee;
                }
                Err(_) => continue, // 过期/nonce 断档/余额不足：作废
            }
        }
        if ledger.records.len() > 500 {
            let cut = ledger.records.len() - 500;
            ledger.records.drain(0..cut);
        }

        // —— 排放：有矿签才发，且最多结清一个当前档位的窗口（减半 + 硬顶同一口径）——
        // v3 的阶梯快，只是因为结算得更勤；每块平均发行量仍不超过 `reward_at(h)`。
        // 没有矿签的块零发行，空档不补发（否则一笔就能把整段空档印出来）。
        let has_claim = included
            .iter()
            .any(|t| claim_cell_of_attestation(t).is_some());
        let delta = window.map(|(_, d)| d).unwrap_or(0);
        let reward = if has_claim {
            claim_reward_delta(height_next, ledger.total_minted, delta)
        } else {
            0
        };
        ledger.total_minted += reward;
        *ledger.balances.entry(miner.clone()).or_insert(0) += reward + fee_pool;
        ledger.vrfs.push(hex::encode(&vrf));

        // —— 块哈希绑定块内全部交易、版本与「世界承诺」——
        // 承诺 = 本机此刻整本实体账本的集合指纹（`QL-ENTSET-V1`）。它进了块哈希，
        // 于是"这台机器认的世界是哪一份"写死在共识里：离线换掉 entities.jsonl
        // 会对不上任何一块 v2 块的承诺，而谁在哪个高度承诺了什么，永久可归责。
        let root = tx_root_of(&digests);
        let (entity_count, entity_root_hex) = self.world_head();
        let mut entity_root = [0u8; 32];
        if let Ok(b) = hex::decode(&entity_root_hex) {
            if b.len() == 32 {
                entity_root.copy_from_slice(&b);
            }
        }
        // —— 光尘进块（v5）：从本机 gossip 账本里挑还没被任何块结算过的记录 ——
        // 激活之前一块不带（v4 块头没有这个字段），激活之后逐块带货。
        let dusts = if version >= BLOCK_V_POOL {
            self.collect_dust(&mut ledger, height_next, DUST_PER_BLOCK)
        } else {
            Vec::new()
        };
        let dust_root = dust_root_of(&dusts);
        let dust_count = dusts.len() as u64;
        let block_hash = if version >= BLOCK_V_POOL {
            block_hash_of_dust(
                &prev_hash_before,
                height_next,
                ts,
                ledger.total_minted,
                &vrf,
                &root,
                &entity_root,
                entity_count,
                &miner,
                &dust_root,
                dust_count,
            )
        } else {
            block_hash_of_miner_bound(
                &prev_hash_before,
                height_next,
                ts,
                ledger.total_minted,
                &vrf,
                &root,
                &entity_root,
                entity_count,
                &miner,
            )
        };

        ledger.height = height_next;
        ledger.prev_hash_hex = hex::encode(&block_hash);
        ledger.last_block_version = version;
        ledger.entity_count_committed = entity_count;

        // —— 原子持久化 + 追加可审计块头 ——
        if let Ok(bytes) = serde_json::to_vec(&*ledger) {
            atomic_write(&self.ledger_path, &bytes);
        }
        let header = BlockHeader {
            version,
            height: height_next,
            ts_unix: ts,
            prev_hash_hex: prev_hash_before,
            block_hash_hex: hex::encode(&block_hash),
            vrf_hex: hex::encode(&vrf),
            minted_after: ledger.total_minted,
            reward_total: reward + fee_pool,
            miner: miner.clone(),
            entity_root_hex,
            entity_count,
            dust_root_hex: if version >= BLOCK_V_POOL {
                hex::encode(dust_root)
            } else {
                String::new()
            },
            dust_count,
            txs: included,
        };
        drop(ledger);
        append_block_line(&self.blocks_path, &header, &wires, &dusts);

        let balance = self.balance_of(&miner);
        Some((
            BlockEvent {
                height: height_next,
                block_hash_hex: hex::encode(&block_hash[..16]),
                vrf_hex: hex::encode(&vrf[..12]),
                reward: reward + fee_pool,
                minted: reward,
                balance,
                total_minted: self.total_minted(),
                ts_unix: header.ts_unix,
                txs_count: digests.len(),
            },
            header.clone(),
            wires,
            dusts,
        ))
    }

    /// 池子操作的状态转移：所有校验都在动钱之前完成（失败不留半截账）。
    ///
    /// QLX 走 `balances`，光尘走 `dust.balances`，储备走 `pools`——三条数据
    /// 在同一个账本里，所以"给出资产"和"记下储备"要么一起成立，要么都不成立。
    fn apply_pool_op(
        l: &mut Ledger,
        from: &str,
        op: &crate::tx::PoolOp,
    ) -> Result<(), String> {
        use crate::tx::PoolOp;
        use crate::world::pool::{Dir, POOL_ID};
        let mut pool = l.pools.get(POOL_ID).cloned().unwrap_or_default();
        let qlx_bal = l.balances.get(from).copied().unwrap_or(0);
        let dust_bal = l.dust.balance_of(from);
        match op {
            PoolOp::AddLiquidity { qlx, dust } => {
                if qlx_bal < *qlx {
                    return Err(format!("QLX 不够做市：有 {qlx_bal}，要 {qlx}"));
                }
                if dust_bal < *dust {
                    return Err(format!("光尘不够做市：有 {dust_bal}，要 {dust}"));
                }
                pool.add(from, *qlx, *dust)?;
                *l.balances.get_mut(from).expect("余额已确认存在") -= *qlx;
                *l.dust.balances.get_mut(from).expect("余额已确认存在") -= *dust;
            }
            PoolOp::RemoveLiquidity { lp } => {
                let (q, d) = pool.remove(from, *lp)?;
                *l.balances.entry(from.to_string()).or_insert(0) += q;
                *l.dust.balances.entry(from.to_string()).or_insert(0) += d;
            }
            PoolOp::SwapQlxForDust { qlx_in, min_out } => {
                if qlx_bal < *qlx_in {
                    return Err(format!("QLX 不够：有 {qlx_bal}，要换 {qlx_in}"));
                }
                let out = pool.swap(Dir::QlxIn, *qlx_in, *min_out)?;
                *l.balances.get_mut(from).expect("余额已确认存在") -= *qlx_in;
                *l.dust.balances.entry(from.to_string()).or_insert(0) += out;
            }
            PoolOp::SwapDustForQlx { dust_in, min_out } => {
                if dust_bal < *dust_in {
                    return Err(format!("光尘不够：有 {dust_bal}，要换 {dust_in}"));
                }
                let out = pool.swap(Dir::DustIn, *dust_in, *min_out)?;
                *l.dust.balances.get_mut(from).expect("余额已确认存在") -= *dust_in;
                *l.balances.entry(from.to_string()).or_insert(0) += out;
            }
        }
        l.pools.insert(POOL_ID.to_string(), pool);
        Ok(())
    }

    /// 单笔交易的状态转移（本地出块 / 远端块 / 快照重建三处共用，绝不写三套）。
    ///
    /// 矿签与普通转账的差别只有三处：金额 0、手续费 0、必须由出块者本人提交，
    /// 且坐标要在 `height−1` 的矿脉里。其余（验签、nonce、余额）一模一样。
    fn apply_wire(
        l: &mut Ledger,
        w: &BlockWire,
        height: u64,
        ts: u64,
        miner: &str,
        version: u8,
    ) -> Result<(TxAttestation, [u8; 32], u64), String> {
        let (digest, from) = w.verify()?;
        let e = &w.envelope;
        let pool_op = e.pool.clone();
        if pool_op.is_some() && !e.actions.is_empty() {
            return Err("池子操作不能夹带转账".into());
        }
        if pool_op.is_some() && version < BLOCK_V_POOL {
            return Err("池子操作只允许出现在 v5 块里".into());
        }
        if pool_op.is_none() && e.actions.is_empty() {
            return Err("交易不包含任何转账".into());
        }
        let claim = claim_cell_of(e);
        if claim.is_some() && version == BLOCK_V_LEGACY {
            return Err("历史版本块不允许矿签".into());
        }
        if claim.is_none()
            && pool_op.is_none()
            && e.actions.iter().any(|a| a.amount == 0)
        {
            return Err("零金额交易只允许矿签".into());
        }
        if e.expiry <= height {
            return Err("交易已过期".into());
        }
        let nonce_now = l.nonces.get(&from).copied().unwrap_or(0);
        if e.nonce != nonce_now {
            return Err(format!("nonce 断档（期望 {nonce_now}，实得 {}）", e.nonce));
        }
        let fee = if claim.is_some() || pool_op.is_some() { 0 } else { FEE };
        let need: u64 = if pool_op.is_some() {
            0
        } else {
            e.actions.iter().map(|a| a.amount).sum::<u64>() + fee
        };
        // 池子交易：两边余额都由这一处结算，链上没有第二个人能动池子。
        if let Some(op) = &pool_op {
            Self::apply_pool_op(l, &from, op)?;
            l.nonces.insert(from.clone(), e.nonce + 1);
            let label = pool_label();
            l.records.push(TxRecord {
                hash_hex: hex::encode(digest),
                height,
                ts_unix: ts,
                from: from.clone(),
                to: label.clone(),
                amount: 0,
                fee: 0,
            });
            return Ok((
                TxAttestation {
                    digest_hex: hex::encode(digest),
                    from,
                    to: label,
                    amount: 0,
                    fee: 0,
                    nonce: e.nonce,
                },
                digest,
                0,
            ));
        }
        if let Some((x, y, layer)) = claim {
            if from != miner {
                return Err("矿签必须由出块者本人提交（奖励不能被代领）".into());
            }
            let prior = l.claims_by.get(&from).copied().unwrap_or(0);
            Self::claim_allowed(l, height, x, y, layer, version, prior)?;
            l.last_claim_height = height;
            // 节奏阶梯的账就在这里记：一格矿 = 下一档更快的结算，只认这把钥匙自己落过的
            *l.claims_by.entry(from.clone()).or_default() += 1;
        }
        // 余额为 0 的新钱包也能从矿签起步：entry 先建账，再扣（矿签 need = 0）
        {
            let bal = l.balances.entry(from.clone()).or_insert(0);
            if *bal < need {
                return Err(format!("余额不足（{} < {need}）", *bal));
            }
            *bal -= need;
        }
        for a in &e.actions {
            let to = crate::crypto::to_bech32_ql(&a.to);
            *l.balances.entry(to).or_insert(0) += a.amount;
        }
        l.nonces.insert(from.clone(), e.nonce + 1);
        let to_first = crate::crypto::to_bech32_ql(&e.actions[0].to);
        l.records.push(TxRecord {
            hash_hex: hex::encode(digest),
            height,
            ts_unix: ts,
            from: from.clone(),
            to: to_first.clone(),
            amount: e.actions[0].amount,
            fee,
        });
        Ok((
            TxAttestation {
                digest_hex: hex::encode(digest),
                from,
                to: to_first,
                amount: e.actions[0].amount,
                fee,
                nonce: e.nonce,
            },
            digest,
            fee,
        ))
    }

    /// 矿签的额外四问：冷却到了吗（v3 起=这个地址挣来的节奏）、在 `h−1` 的矿脉里吗、
    /// 落在贴地层吗、坐标是不是自己的（一格一主由实体账本重放判，这里不重复记账）
    fn claim_allowed(
        l: &Ledger,
        height: u64,
        x: u16,
        y: u16,
        layer: u16,
        version: u8,
        prior_claims: u64,
    ) -> Result<(), String> {
        let last = emission_watermark(l);
        if claim_window(version, height, last, prior_claims).is_none() {
            let floor = if version >= BLOCK_V_CRAFT {
                craft_floor(prior_claims)
            } else {
                MINE_COOLDOWN_BLOCKS
            };
            let earliest = if last == 0 { floor } else { last + floor };
            return Err(format!(
                "冷却未到：上次结算在块高 {last}，这一档节奏要求每 {floor} 块一次，本回合最早 {earliest}（已落矿格 {prior_claims} 格）"
            ));
        }
        let words = crate::world::terrain::seed_words(GENESIS_HASH_HEX, &l.vrfs);
        let (x32, y32) = (x as u32, y as u32);
        if !crate::world::ore::is_ore(x32, y32, &words, height - 1) {
            return Err(format!("({x},{y}) 在块高 {} 的地表上不是矿", height - 1));
        }
        if crate::world::ore::ore_layer(x32, y32, &words) != layer {
            return Err(format!("矿格 ({x},{y}) 必须贴地（layer {}）", crate::world::ore::ore_layer(x32, y32, &words)));
        }
        Ok(())
    }

    pub fn total_minted(&self) -> u64 {
        self.ledger.lock().unwrap().total_minted
    }

    pub fn ledger_started(&self) -> u64 {
        self.ledger.lock().unwrap().started_unix
    }

    // ================================================================ B 档联邦接口
    //
    // 口径：对端推来的每一份交易/块，都走与本地完全相同的验证规则——
    // 验签（ML-DSA-65，公钥来自注册表）、nonce、余额、过期、tx_root、块哈希链。
    // 通不过的一律拒绝。验证逻辑共享 = 各节点对同一账本算出同一条链。

    /// 单块排放口径（头校验共用）：返回 (应发行量, 本块后的 last_claim)。
    ///
    /// 这里只看块头凭据，不看矿格坐标——坐标是否真是矿必须拿原件重放（`apply_wire`），
    /// 块头里的 `TxAttestation` 只是声称。但"发不发、发多少、谁领"能在这里独立判死：
    /// - v0（历史块）：无条件按减半曲线发行，出现矿签即非法；
    /// - v1/v2：最多一张矿签，必须由出块者本人提交、金额/手续费为 0，冷却固定 50 块，
    ///   一次结清 50 个块的量；没有矿签 → 本块发行量为 0（币不靠计时器印）。
    /// - v3+：同上，但节奏档由 `prior_claims`（该地址已经落下几格矿）决定，
    ///   且每笔最多结算一个当前档位、空档不补发 → 每块平均发行量 ≤ `reward_at(h)`。
    fn header_emission(
        bh: &BlockHeader,
        minted_before: u64,
        last_claim_before: u64,
        prior_claims: u64,
    ) -> Option<(u64, u64)> {
        // 未来版本无从解释排放口径，不猜（调用点的版本闸门之外也要自守住）
        if bh.version > BLOCK_V {
            return None;
        }
        if bh.version == BLOCK_V_LEGACY {
            if bh
                .txs
                .iter()
                .any(|t| claim_cell_of_attestation(t).is_some())
            {
                return None;
            }
            return Some((
                reward_at(bh.height).min(HARD_CAP.saturating_sub(minted_before)),
                last_claim_before,
            ));
        }
        let claims: Vec<&TxAttestation> = bh
            .txs
            .iter()
            .filter(|t| claim_cell_of_attestation(t).is_some())
            .collect();
        if claims.len() > 1 {
            return None; // 每块最多一张矿签
        }
        let Some(c) = claims.first() else {
            return Some((0, last_claim_before));
        };
        if c.from != bh.miner || c.amount != 0 || c.fee != 0 {
            return None;
        }
        if bh.version >= BLOCK_V_CRAFT {
            let (_, delta) = claim_window(bh.version, bh.height, last_claim_before, prior_claims)?;
            Some((claim_reward_delta(bh.height, minted_before, delta), bh.height))
        } else {
            if last_claim_before != 0 && bh.height < last_claim_before + MINE_COOLDOWN_BLOCKS {
                return None;
            }
            Some((claim_reward(bh.height, minted_before), bh.height))
        }
    }

    /// 头部序列校验（接受远端块 / 追账快照共用）：
    /// 前向哈希连续 + 逐块重算 tx_root 与块哈希 + 版本化排放口径一致
    fn verify_header_slice(headers: &[BlockHeader]) -> bool {
        let mut expect_prev = genesis_hash_hex();
        let mut minted_running: u64 = 0;
        let mut last_claim: u64 = 0;
        // 版本只许升不许降：v4 链后面接 v0 = 绕过矿签免费发行，直接拒。
        let mut prev_version = BLOCK_V_LEGACY;
        // 最近一次「世界承诺」的条数（只在 v2+ 块之间比较；v0/v1 没承诺过世界）
        let mut prev_entity_count: u64 = 0;
        // 每个地址已经落下的矿格数：v3 的节奏阶梯只读这个纯块头可复算的量
        let mut claims_by: BTreeMap<String, u64> = BTreeMap::new();
        for bh in headers {
            // 创世行只做锚定：与源码常量逐字相等；它没有出块者，也没有排放
            if bh.height == 0 {
                if bh != &genesis_header() {
                    return false;
                }
                expect_prev = bh.block_hash_hex.clone();
                continue;
            }
            if bh.miner.is_empty() {
                return false;
            }
            if bh.version > BLOCK_V {
                return false; // 未来版本无从解释，不猜
            }
            // 版本单调 + 激活高度：旧版本可以在历史区间重放，但不能在 v4 之后回退；
            // 超过激活高度还拿 v0/v1/v2/v3 来出块，同样拒。
            if bh.version < prev_version || bh.version < version_floor_at(bh.height) {
                return false;
            }
            // v2 起必须真的承诺一份世界：64 个 hex，且条数不减（append-only 世界里
            // 诚实出块人只会知道更多记录；倒退 = 想把世界清空）
            let world_ok = bh.version < BLOCK_V_WORLD
                || (bh.entity_root_hex.len() == 64
                    && bh.entity_count >= prev_entity_count);
            if !world_ok {
                return false;
            }
            if bh.version >= BLOCK_V_WORLD {
                prev_entity_count = bh.entity_count;
            }
            if bh.prev_hash_hex != expect_prev {
                return false;
            }
            let mut digests = Vec::with_capacity(bh.txs.len());
            for t in &bh.txs {
                let mut d = [0u8; 32];
                if hex::decode_to_slice(&t.digest_hex, &mut d).is_err() {
                    return false;
                }
                digests.push(d);
            }
            let root = tx_root_of(&digests);
            let mut vrf = [0u8; 32];
            if hex::decode_to_slice(&bh.vrf_hex, &mut vrf).is_err() {
                return false;
            }
            let Some(expect) = block_hash_of_header(bh, &vrf, &root) else {
                return false;
            };
            if hex::encode(expect) != bh.block_hash_hex {
                return false;
            }
            let prior = claims_by.get(&bh.miner).copied().unwrap_or(0);
            let Some((reward, next_claim)) =
                Self::header_emission(bh, minted_running, last_claim, prior)
            else {
                return false;
            };
            let fee_sum: u64 = bh.txs.iter().map(|t| t.fee).sum();
            if bh.reward_total != reward + fee_sum {
                return false;
            }
            if bh.minted_after != minted_running + reward {
                return false;
            }
            if bh
                .txs
                .iter()
                .any(|t| claim_cell_of_attestation(t).is_some())
            {
                *claims_by.entry(bh.miner.clone()).or_default() += 1;
            }
            minted_running = bh.minted_after;
            // v0 每块都发过，水位跟着块高走；这样 v0→v4 升级后的第一笔 v4
            // 只结算升级之后的新块，不会把 v0 已发过的 50 块再补一遍。
            last_claim = if bh.version == BLOCK_V_LEGACY {
                bh.height
            } else {
                next_claim
            };
            prev_version = bh.version;
            expect_prev = bh.block_hash_hex.clone();
        }
        true
    }

    /// 块内光尘记录：形状（集合指纹/条数）+ 逐条验签验链 + 语义结算。
    ///
    /// 任何一条不过就整块作废——记录已经被块哈希绑住，矿工没有"夹带一条坏账"
    /// 的余地；这也正是光尘余额能被陌生人当作兑换凭据的原因。
    fn apply_dust_block(
        l: &mut Ledger,
        h: &BlockHeader,
        dusts: &[crate::world::dust::DustRecord],
    ) -> Result<(), String> {
        use crate::world::dust;
        if h.version < BLOCK_V_POOL {
            if !dusts.is_empty() {
                return Err("旧版本块不应携带光尘记录".into());
            }
            return Ok(());
        }
        if h.dust_root_hex.len() != 64 {
            return Err("v5 块缺光尘集合指纹".into());
        }
        if h.dust_count != dusts.len() as u64 {
            return Err(format!(
                "光尘条数不符：头 {} ≠ 原件 {}",
                h.dust_count,
                dusts.len()
            ));
        }
        if hex::encode(dust_root_of(dusts)) != h.dust_root_hex {
            return Err("光尘集合指纹与原件不一致".into());
        }
        for r in dusts {
            let hash = dust::record_hash(r);
            let key = hex::encode(hash);
            if l.dust_seen.contains(&key) {
                return Err(format!("光尘记录 {} 已被前块结算（一笔不能记两次）", &key[..12]));
            }
            let tip = l.dust_tips.get(&r.pk_hash_hex).cloned().unwrap_or_default();
            dust::check_record(r, &tip, h.height)?;
            dust::apply_record(&mut l.dust, r, h.height)?;
            l.dust_tips
                .entry(r.pk_hash_hex.clone())
                .or_default()
                .advance(hash, r.height);
            l.dust_seen.insert(key);
        }
        Ok(())
    }

    /// 追账唯一的状态重建：**从交易原件**逐笔验签、复算凭据、推进余额，
    /// 再把复算出的凭据与块头逐字对照。
    ///
    /// 为什么不能像以前那样直接照块头记账：块头里的 `TxAttestation` 只是"声称"，
    /// 而块哈希由声称者自己算——一条自洽的假链就能凭伪造凭据搬走任何人的余额。
    /// 只有原件（envelope ‖ sig ‖ pk ‖ salt）能被第三方独立复验：
    /// 摘要重算、地址由 `pk ‖ salt` 复算、ML-DSA 验签，任何一环不符就整条快照拒收。
    fn apply_block_line(l: &mut Ledger, bl: &BlockLine) -> Result<(), String> {
        let h = &bl.header;
        // 创世块不改变状态：它只把链钉在宣言上（无铸造、无转账、不入交易历史）
        if h.height == 0 {
            if !bl.wires.is_empty() || !bl.dusts.is_empty() {
                return Err("创世块不应携带交易/光尘原件 → 快照拒收".into());
            }
            l.height = 0;
            l.prev_hash_hex = h.block_hash_hex.clone();
            l.last_block_version = h.version;
            return Ok(());
        }
        if h.version > BLOCK_V {
            return Err(format!("块 #{} 版本号未知 → 快照拒收", h.height));
        }
        if h.version < l.last_block_version || h.version < version_floor_at(h.height) {
            return Err(format!(
                "块 #{} 版本回退/过旧（块 v{}，前一版本 v{}，此高度最低 v{}）→ 快照拒收",
                h.height,
                h.version,
                l.last_block_version,
                version_floor_at(h.height)
            ));
        }
        if h.version >= BLOCK_V_WORLD {
            if h.entity_root_hex.len() != 64 {
                return Err(format!("块 #{} 声称 v2+ 却没有世界承诺 → 快照拒收", h.height));
            }
            if h.entity_count < l.entity_count_committed {
                return Err(format!(
                    "块 #{} 的世界条数倒退（链上已承诺 {}，这块只认 {}）→ 想把世界清空？快照拒收",
                    h.height, l.entity_count_committed, h.entity_count
                ));
            }
            l.entity_count_committed = h.entity_count;
        }
        if bl.wires.len() != h.txs.len() {
            return Err(format!(
                "块 #{} 交易原件不全（凭据 {} 笔，原件 {} 笔）→ 无从验签，快照拒收",
                h.height,
                h.txs.len(),
                bl.wires.len()
            ));
        }

        // 逐笔原件走与本地出块完全相同的状态转移（矿签的坐标/冷却也在这里判）
        let last_claim_before = l.last_claim_height;
        let prior_before = l.claims_by.get(&h.miner).copied().unwrap_or(0);
        let mut rebuilt: Vec<TxAttestation> = Vec::with_capacity(bl.wires.len());
        let mut fee_sum: u64 = 0;
        for w in &bl.wires {
            let (att, _digest, fee) =
                Self::apply_wire(l, w, h.height, h.ts_unix, &h.miner, h.version)
                    .map_err(|e| format!("块 #{} 交易原件复验失败 → 快照拒收：{e}", h.height))?;
            fee_sum += fee;
            rebuilt.push(att);
        }
        // 凭据逐字对照：块哈希绑的是 tx_root（由这些凭据里的摘要构成），
        // 复算凭据与块头任何一处不符，就意味着要么签名要么金额被动过。
        if rebuilt != h.txs {
            return Err(format!("块 #{} 重放凭据与块头不一致 → 快照拒收", h.height));
        }
        Self::apply_dust_block(l, h, &bl.dusts)
            .map_err(|e| format!("块 #{} 光尘记录复验失败 → 快照拒收：{e}", h.height))?;
        if l.records.len() > 500 {
            let cut = l.records.len() - 500;
            l.records.drain(0..cut);
        }
        // 排放口径与出块时一致：v0 无条件发行；v1/v2 有矿签就结清 50 个块的量；
        // v3+ 有矿签最多结清一个当前档位（空档不补发，总量一分不多）
        let reward = match h.version {
            BLOCK_V_LEGACY => reward_at(h.height).min(HARD_CAP.saturating_sub(l.total_minted)),
            _ => {
                if !rebuilt.iter().any(|t| claim_cell_of_attestation(t).is_some()) {
                    0
                } else if h.version >= BLOCK_V_CRAFT {
                    let (_, delta) = claim_window(h.version, h.height, last_claim_before, prior_before)
                        .ok_or_else(|| format!("块 #{} 的矿签间隔不合法 → 快照拒收", h.height))?;
                    claim_reward_delta(h.height, l.total_minted, delta)
                } else {
                    claim_reward(h.height, l.total_minted)
                }
            }
        };
        l.total_minted += reward;
        *l.balances.entry(h.miner.clone()).or_insert(0) += reward + fee_sum;
        l.vrfs.push(h.vrf_hex.clone());
        // v0 每块都发过：水位跟到块高；v1+ 由 apply_wire 在矿签结算时推进。
        if h.version == BLOCK_V_LEGACY {
            l.last_claim_height = h.height;
        }
        l.height = h.height;
        l.prev_hash_hex = h.block_hash_hex.clone();
        l.last_block_version = h.version;
        Ok(())
    }

    /// 联邦交易入池：对端送来的是**完整原件**（信封 ‖ 签名 ‖ 公钥 ‖ 地址盐），
    /// 这里独立复验（验签、地址绑定、nonce、可支配余额、过期、池容量）。
    /// 验证不再依赖注册表——「对方还没广播注册」从此不再是丢弃交易的理由。
    pub fn accept_remote_tx(&self, wire: BlockWire) -> Result<[u8; 32], String> {
        if *self.corrupted.lock().unwrap() {
            return Err("账本完整性校验未通过，拒绝入网交易".into());
        }
        let pool_op = wire.envelope.pool.clone();
        if pool_op.is_some() && !wire.envelope.actions.is_empty() {
            return Err("池子操作不能夹带转账".into());
        }
        if pool_op.is_none() && wire.envelope.actions.is_empty() {
            return Err("交易不包含任何转账".into());
        }
        let claim = claim_cell_of(&wire.envelope);
        if claim.is_none()
            && pool_op.is_none()
            && wire
                .envelope
                .actions
                .iter()
                .any(|a| a.amount == 0)
        {
            return Err("转账金额必须大于 0".into());
        }
        let tx_fee = if claim.is_some() || pool_op.is_some() { 0 } else { FEE };

        let (digest, from) = wire.verify()?;
        let from_hex = hex::encode(&wire.envelope.from_addr);
        {
            let ledger = self.ledger.lock().unwrap();
            let mp = self.mempool.lock().unwrap();
            let queued: u64 = mp
                .iter()
                .filter(|p| hex::encode(&p.envelope.from_addr) == from_hex)
                .count() as u64;
            let expect = ledger.nonces.get(&from).copied().unwrap_or(0) + queued;
            if wire.envelope.nonce != expect {
                return Err(format!("nonce 不匹配：期望 {expect}，收到 {}", wire.envelope.nonce));
            }
            if wire.envelope.expiry <= ledger.height {
                return Err("交易已过期（expiry 块高）".into());
            }
            if let Some(op) = &pool_op {
                if version_floor_at(ledger.height + 1) < BLOCK_V_POOL {
                    return Err("兑换池还没开放（等 v5 分叉）".into());
                }
                let mut trial = ledger.clone();
                Self::apply_pool_op(&mut trial, &from, op)
                    .map_err(|e| format!("池子操作不成立：{e}"))?;
            }
            let projected_spend: u64 = mp
                .iter()
                .filter(|p| hex::encode(&p.envelope.from_addr) == from_hex)
                .map(|p| {
                    p.envelope.actions.iter().map(|a| a.amount).sum::<u64>()
                        + if claim_cell_of(&p.envelope).is_some() { 0 } else { FEE }
                })
                .sum();
            let need: u64 = wire.envelope.actions.iter().map(|a| a.amount).sum::<u64>() + tx_fee;
            let bal = ledger.balances.get(&from).copied().unwrap_or(0);
            if bal.saturating_sub(projected_spend) < need {
                return Err("余额不足（已扣除该地址未入块支出）".into());
            }
            // 矿签的坐标必须在此刻（= 下一块的 h−1）仍是矿；换块了就重新找
            if let Some((x, y, layer)) = claim {
                let words = crate::world::terrain::seed_words(GENESIS_HASH_HEX, &ledger.vrfs);
                if !crate::world::ore::is_ore(x as u32, y as u32, &words, ledger.height)
                    || crate::world::ore::ore_layer(x as u32, y as u32, &words) != layer
                {
                    return Err(format!("({x},{y}) 第 {layer} 层此刻不是矿——地表已经重画"));
                }
            }
            if mp.len() >= MEMPOOL_CAP {
                return Err("交易池已满".into());
            }
        }

        let BlockWire {
            envelope,
            sig_hex,
            pk_hex,
            salt_hex,
        } = wire;
        let sig = hex::decode(&sig_hex).map_err(|_| "签名十六进制非法")?;
        self.mempool.lock().unwrap().push(PendingTx {
            envelope,
            sig,
            digest,
            pk_hex,
            salt_hex,
        });
        Ok(digest)
    }

    /// 接受联邦对端出的块：整块真验（逐笔**原件**重验签重放 + tx_root/块哈希重算 + 排放口径），
    /// staging 全过且与块头完全一致才一次成型入账（失败不落任何状态）。
    pub fn apply_remote_block(
        &self,
        header: &BlockHeader,
        wires: Vec<BlockWire>,
        dusts: Vec<crate::world::dust::DustRecord>,
    ) -> Result<BlockEvent, String> {
        if *self.corrupted.lock().unwrap() {
            return Err("账本完整性校验未通过，拒绝外部块".into());
        }
        let mut ledger = self.ledger.lock().unwrap();
        let height_next = ledger.height + 1;
        if header.height != height_next || header.prev_hash_hex != ledger.prev_hash_hex {
            return Err(format!(
                "块高/前向不衔接：本地 #{}（prev {}…），来块 #{}",
                ledger.height,
                &ledger.prev_hash_hex[..16.min(ledger.prev_hash_hex.len())],
                header.height
            ));
        }
        if header.miner.is_empty() {
            return Err("块头缺 miner".into());
        }
        if header.version > BLOCK_V {
            return Err("块版本未知 → 整块拒绝".into());
        }
        if header.version < ledger.last_block_version
            || header.version < version_floor_at(height_next)
        {
            return Err(format!(
                "块版本回退/过旧（来块 v{}，前一版本 v{}，此高度最低 v{}）→ 整块拒绝",
                header.version,
                ledger.last_block_version,
                version_floor_at(height_next)
            ));
        }
        if header.version >= BLOCK_V_WORLD {
            if header.entity_root_hex.len() != 64 {
                return Err("v2+ 块缺世界承诺 → 整块拒绝".into());
            }
            if header.entity_count < ledger.entity_count_committed {
                return Err(format!(
                    "世界条数倒退（链上已承诺 {}，来块只认 {}）→ 整块拒绝",
                    ledger.entity_count_committed, header.entity_count
                ));
            }
        }

        // —— staging 复本上逐笔重验+重放（任何一笔不过 → 整块拒绝）——
        // 与本地出块共用 `apply_wire`：矿签的坐标、冷却、出块者身份都在同一处判。
        let last_claim_before = emission_watermark(&ledger);
        let prior_before = ledger
            .claims_by
            .get(&header.miner)
            .copied()
            .unwrap_or(0);
        let mut staged = ledger.clone();
        let mut included: Vec<TxAttestation> = Vec::with_capacity(wires.len());
        let mut digests: Vec<[u8; 32]> = Vec::with_capacity(wires.len());
        let mut fee_pool: u64 = 0;

        for (i, wire) in wires.iter().enumerate() {
            let (att, digest, fee) = Self::apply_wire(
                &mut staged,
                wire,
                height_next,
                header.ts_unix,
                &header.miner,
                header.version,
            )
            .map_err(|e| format!("块内第 {} 笔交易原件复验失败 → 整块拒绝：{e}", i + 1))?;
            fee_pool += fee;
            included.push(att);
            digests.push(digest);
        }
        if staged.records.len() > 500 {
            let cut = staged.records.len() - 500;
            staged.records.drain(0..cut);
        }

        // —— 与块头逐一对照（凭据、序、量）——
        if included != header.txs {
            return Err("重放凭据与块头不一致 → 整块拒绝".into());
        }
        // —— v5：光尘记录同样逐条验签验链，任何一条不过整块拒绝 ——
        Self::apply_dust_block(&mut staged, header, &dusts)
            .map_err(|e| format!("块内光尘记录复验失败 → 整块拒绝：{e}"))?;

        // —— 排放（版本化口径与本地出块一致）——
        let claim_count = included
            .iter()
            .filter(|t| claim_cell_of_attestation(t).is_some())
            .count();
        let reward = match header.version {
            BLOCK_V_LEGACY => {
                if claim_count > 0 {
                    return Err("历史版本块含矿签 → 整块拒绝".into());
                }
                reward_at(height_next).min(HARD_CAP.saturating_sub(staged.total_minted))
            }
            _ => {
                if claim_count > 0 {
                    let c = included
                        .iter()
                        .find(|t| claim_cell_of_attestation(t).is_some())
                        .expect("claim_count > 0");
                    if c.from != header.miner {
                        return Err("矿签不是出块者本人 → 整块拒绝".into());
                    }
                    if header.version >= BLOCK_V_CRAFT {
                        // v3+：一笔最多结清一个当前档位，空档不补发；间隔不合法 = 节奏没挣到 → 整块拒绝
                        let (_, delta) = claim_window(
                            header.version,
                            height_next,
                            last_claim_before,
                            prior_before,
                        )
                        .ok_or("矿签间隔未到这一档节奏 → 整块拒绝")?;
                        claim_reward_delta(height_next, staged.total_minted, delta)
                    } else {
                        claim_reward(height_next, staged.total_minted)
                    }
                } else {
                    0
                }
            }
        };
        staged.total_minted += reward;
        *staged.balances.entry(header.miner.clone()).or_insert(0) += reward + fee_pool;
        staged.vrfs.push(header.vrf_hex.clone());

        // —— 块哈希/断面一致 ——
        if header.minted_after != staged.total_minted {
            return Err(format!(
                "排放不匹配：头 {} ≠ 重放 {}",
                header.minted_after, staged.total_minted
            ));
        }
        if header.reward_total != reward + fee_pool {
            return Err(format!(
                "奖励口径不符：头 {} ≠ 重放 {}",
                header.reward_total,
                reward + fee_pool
            ));
        }
        let root = tx_root_of(&digests);
        let mut vrf_bytes = [0u8; 32];
        if hex::decode_to_slice(&header.vrf_hex, &mut vrf_bytes).is_err() {
            return Err("块头 vrf 非法".into());
        }
        let Some(expect) = block_hash_of_header(header, &vrf_bytes, &root) else {
            return Err("v2 块的世界承诺形态非法（须 64 hex）→ 整块拒绝".into());
        };
        if hex::encode(expect) != header.block_hash_hex {
            return Err("块哈希重算不符 → 整块拒绝".into());
        }

        // —— 一次成型入账 ——
        staged.height = height_next;
        staged.prev_hash_hex = header.block_hash_hex.clone();
        staged.last_block_version = header.version;
        if header.version == BLOCK_V_LEGACY {
            // v0 每块都发过：水位跟到块高，防止升级后的第一笔 v4 重复补发。
            staged.last_claim_height = height_next;
        }
        if header.version >= BLOCK_V_WORLD {
            staged.entity_count_committed = header.entity_count;
        }
        *ledger = staged;

        if let Ok(bytes) = serde_json::to_vec(&*ledger) {
            atomic_write(&self.ledger_path, &bytes);
        }
        drop(ledger);
        append_block_line(&self.blocks_path, header, &wires, &dusts);

        // 已入账的 from 地址，其池内挂着的后续交易 nonce 已可能过期——先撤下等重发
        let mut froms: Vec<String> = header.txs.iter().map(|t| t.from.clone()).collect();
        froms.sort();
        froms.dedup();
        self.drop_pending_for(&froms);

        Ok(BlockEvent {
            height: header.height,
            block_hash_hex: header.block_hash_hex[..16].to_string(),
            vrf_hex: header.vrf_hex[..12].to_string(),
            reward: header.reward_total,
            minted: reward,
            balance: 0, // 由 net 层以本机钱包补算
            total_minted: header.minted_after,
            ts_unix: header.ts_unix,
            txs_count: header.txs.len(),
        })
    }

    /// 追账快照：对端链更长时整链采用。
    ///
    /// 两道关，缺一不可：
    /// ① `verify_header_slice`——哈希链连续、逐块 tx_root 与块哈希重算、排放口径一致；
    /// ② `apply_block_line`——**逐笔交易原件**独立验签并从零重建余额。
    /// 只过①是不够的：自洽的假链谁都能造。②把"对端说什么就是什么"变成"对端必须拿原件来"。
    pub fn install_snapshot(&self, lines: &str) -> Result<u64, String> {
        // 旧链交易清单（孤儿交易回收用）
        let old_hashes: Vec<String> = self
            .ledger
            .lock()
            .unwrap()
            .records
            .iter()
            .map(|r| r.hash_hex.clone())
            .collect();
        let mut block_lines: Vec<BlockLine> = Vec::new();
        for line in lines.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let b: BlockLine = serde_json::from_str(line).map_err(|e| format!("快照解析失败: {e}"))?;
            block_lines.push(b);
        }
        if block_lines.is_empty() {
            return Err("空快照".into());
        }
        let headers: Vec<BlockHeader> = block_lines.iter().map(|l| l.header.clone()).collect();
        if !Self::verify_header_slice(&headers) {
            return Err("快照哈希链校验失败 → 拒绝采用".into());
        }
        // ② 逐块从**原件**验签重建：任何一笔复验不过，整条快照作废、本机状态不动
        let mut staged = Ledger {
            v: LEDGER_V,
            height: 0,
            prev_hash_hex: genesis_hash_hex(),
            total_minted: 0,
            started_unix: GENESIS_TS_UNIX,
            balances: HashMap::new(),
            nonces: HashMap::new(),
            records: vec![],
            last_claim_height: 0,
            last_block_version: BLOCK_V_LEGACY,
            entity_count_committed: 0,
            dust: crate::world::dust::DustState::default(),
            dust_seen: std::collections::BTreeSet::new(),
            dust_tips: BTreeMap::new(),
            pools: BTreeMap::new(),
            vrfs: vec![],
            claims_by: BTreeMap::new(),
        };
        for bl in &block_lines {
            Self::apply_block_line(&mut staged, bl)?;
        }
        let tip_height = staged.height;

        // 孤儿交易回收清单：旧链有、新链无的交易（随被打败的分叉一起被丢弃的）
        let new_set: std::collections::HashSet<&str> = staged
            .records
            .iter()
            .map(|r| r.hash_hex.as_str())
            .collect();
        let missing: Vec<String> = old_hashes
            .into_iter()
            .filter(|h| !new_set.contains(h.as_str()))
            .collect();

        // 采用前把本机现有账本归档（分叉切换留下可审计痕迹）
        if self.blocks_path.exists() || self.ledger_path.exists() {
            let tag = now_unix();
            let _ = std::fs::rename(
                &self.blocks_path,
                self.blocks_path.with_extension(format!("jsonl.bak-fork-{tag}")),
            );
            let _ = std::fs::rename(
                &self.ledger_path,
                self.ledger_path.with_extension(format!("json.bak-fork-{tag}")),
            );
        }

        *self.ledger.lock().unwrap() = staged;
        self.mempool.lock().unwrap().clear();

        // —— 孤儿交易回灌：从签发缓存找回原件，按新链口径重验后重新入池 ——
        // （下一拍重新打包上链；周期公告会把它们再广播给全网。真实区块链同款行为）
        let mut reinjected = 0usize;
        if !missing.is_empty() {
            let cache = self.sent_cache.lock().unwrap();
            for wire in cache.iter() {
                let d = hex::encode(tx_digest(&wire.envelope));
                if missing.iter().any(|m| m == &d) && self.requeue_orphan(wire) {
                    reinjected += 1;
                }
            }
            if reinjected > 0 {
                eprintln!("[QLight] 分叉切换：{reinjected} 笔孤儿交易已回灌内存池（下块重新打包上链）");
            }
        }

        if let Ok(bytes) = serde_json::to_vec(&*self.ledger.lock().unwrap()) {
            atomic_write(&self.ledger_path, &bytes);
        }
        // 快照整链重写必须原子——这里曾是裸 fs::write，强杀进程会撕出半截 jsonl
        atomic_write(&self.blocks_path, lines.as_bytes());
        Ok(tip_height)
    }

    /// 孤儿交易回灌（原件自带公钥与地址盐，重验一次签名；再按新链口径查 nonce/过期/余额/池容量）
    fn requeue_orphan(&self, wire: &BlockWire) -> bool {
        let Ok((digest, from)) = wire.verify() else { return false };
        let ledger = self.ledger.lock().unwrap();
        let mut mp = self.mempool.lock().unwrap();
        if mp.len() >= MEMPOOL_CAP {
            return false;
        }
        let from_hex = hex::encode(&wire.envelope.from_addr);
        let queued: u64 = mp
            .iter()
            .filter(|p| hex::encode(&p.envelope.from_addr) == from_hex)
            .count() as u64;
        let expect = ledger.nonces.get(&from).copied().unwrap_or(0) + queued;
        if wire.envelope.nonce != expect {
            return false; // nonce 已被新链消耗 → 交易作废（正确行为）
        }
        if wire.envelope.expiry <= ledger.height {
            return false;
        }
        let projected_spend: u64 = mp
            .iter()
            .filter(|p| hex::encode(&p.envelope.from_addr) == from_hex)
            .map(|p| p.envelope.actions.iter().map(|a| a.amount).sum::<u64>() + FEE)
            .sum();
        let need: u64 = wire.envelope.actions.iter().map(|a| a.amount).sum::<u64>() + FEE;
        let bal = ledger.balances.get(&from).copied().unwrap_or(0);
        if bal.saturating_sub(projected_spend) < need {
            return false;
        }
        let Ok(sig) = hex::decode(&wire.sig_hex) else { return false };
        mp.push(PendingTx {
            envelope: wire.envelope.clone(),
            sig,
            digest,
            pk_hex: wire.pk_hex.clone(),
            salt_hex: wire.salt_hex.clone(),
        });
        true
    }

    /// 撤下指定地址（bech32）挂在池里的所有交易（它们的 nonce 已被新块消耗）
    pub fn drop_pending_for(&self, addrs: &[String]) {
        let mut mp = self.mempool.lock().unwrap();
        mp.retain(|p| {
            let from: String = crate::crypto::to_bech32_ql(&p.envelope.from_addr);
            !addrs.contains(&from)
        });
    }

    // ---------------- 联邦只读暴露（net 层使用）----------------

    /// 当前 tip = 下一个块要续接的 prev 哈希（ledger.prev_hash_hex）
    pub fn tip_hash_hex_pub(&self) -> String {
        self.ledger.lock().unwrap().prev_hash_hex.clone()
    }

    /// blocks 文件路径（联邦读出整链做追账快照）
    pub fn blocks_path_pub(&self) -> PathBuf {
        self.blocks_path.clone()
    }

    /// 挂载整本实体账本：v2 起出块要向它承诺世界。钱包启动时调用一次，
    /// 网络线程与命令层拿到的都是同一个 Arc，因此承诺对所有副本一致。
    pub fn attach_world(&self, ledger: Arc<crate::world::EntityLedger>) {
        *self.world.write().unwrap() = Some(ledger);
    }

    /// 挂载本机光尘账本：v5 起出块要从这里取还没进块的记录。
    pub fn attach_dust(&self, store: Arc<crate::world::dust::DustLedger>) {
        *self.dust_store.write().unwrap() = Some(store);
    }

    /// 挑一批还没被任何块结算过的光尘记录，接到账本上并返回（v5 出块用）。
    ///
    /// 顺序 `(声明块高, 归属, seq)`：同一条归属链的 seq 必须从小到大，
    /// 否则 `check_record` 会当场拒绝。语义不过（余额不足、配额用完）的记录
    /// 只跳过、不进块——一块不能因为一条坏记录而整块作废，但错误也绝不吞进共识。
    fn collect_dust(
        &self,
        ledger: &mut Ledger,
        height_next: u64,
        limit: usize,
    ) -> Vec<crate::world::dust::DustRecord> {
        use crate::world::dust::record_hash;
        let Some(store) = self.dust_store.read().unwrap().clone() else {
            return Vec::new();
        };
        let mut pending: Vec<crate::world::dust::DustRecord> = store
            .load()
            .into_iter()
            .filter(|r| !ledger.dust_seen.contains(&hex::encode(record_hash(r))))
            .collect();
        pending.sort_by(|a, b| {
            a.height
                .cmp(&b.height)
                .then_with(|| a.owner.cmp(&b.owner))
                .then_with(|| a.seq.cmp(&b.seq))
        });

        let mut tips = ledger.dust_tips.clone();
        let mut state = ledger.dust.clone();
        let mut out: Vec<crate::world::dust::DustRecord> = Vec::new();
        for r in pending {
            if out.len() >= limit {
                break;
            }
            let h = record_hash(&r);
            let tip = tips.get(&r.pk_hash_hex).cloned().unwrap_or_default();
            if crate::world::dust::check_record(&r, &tip, height_next).is_err() {
                continue;
            }
            let mut trial = state.clone();
            if crate::world::dust::apply_record(&mut trial, &r, height_next).is_err() {
                continue;
            }
            state = trial;
            tips.entry(r.pk_hash_hex.clone()).or_default().advance(h, r.height);
            ledger.dust_seen.insert(hex::encode(h));
            out.push(r);
        }
        ledger.dust = state;
        ledger.dust_tips = tips;
        out
    }

    /// 本机此刻的世界承诺：`(去重条数, 集合指纹)`。没挂世界就是诚实的空集合，
    /// 而不是"不承诺"——v2 块永远带一个可被别人复算的指纹。
    pub fn world_head(&self) -> (u64, String) {
        let slot = self.world.read().unwrap();
        match slot.as_ref() {
            Some(led) => crate::world::set_digest(&led.load()),
            None => crate::world::set_digest(&[]),
        }
    }

    /// 池内全部交易原件（周期性再广播，让各家节点 mempool 对齐）
    pub fn mempool_wires_pub(&self) -> Vec<BlockWire> {
        self.mempool
            .lock()
            .unwrap()
            .iter()
            .map(|p| BlockWire {
                envelope: p.envelope.clone(),
                sig_hex: hex::encode(&p.sig),
                pk_hex: p.pk_hex.clone(),
                salt_hex: p.salt_hex.clone(),
            })
            .collect()
    }
}

fn addr_hex_of(bech32_addr: &str) -> String {
    // pending_nonce 直接以 bech32 串比对：把池中 envelope 的 from 八进制转 bech32 太贵，
    // 这里退化为 hex 比对（addr 参数本应是 bech32；hex 是 unwrap 失败时的兜底）
    bech32_decode_addr(bech32_addr)
        .map(|b| hex::encode(b))
        .unwrap_or_else(|_| bech32_addr.to_string())
}

/// 块行落盘：块头 + 交易原件（`wires` 为空时与旧行逐字相同，故本机重放照旧可读）
fn append_block_line(
    path: &PathBuf,
    h: &BlockHeader,
    wires: &[BlockWire],
    dusts: &[crate::world::dust::DustRecord],
) {
    use std::io::Write;
    let line = BlockLine {
        header: h.clone(),
        wires: wires.to_vec(),
        dusts: dusts.to_vec(),
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        if let Ok(json) = serde_json::to_string(&line) {
            // 单次 write_all 整行落盘 + fsync：kill -9 不会撕行，掉电也不丢尾块
            let mut buf = json.into_bytes();
            buf.push(b'\n');
            let _ = f.write_all(&buf);
            let _ = f.sync_all();
        }
    }
}

/// 校验 ql1 地址字符串（bech32 反解码 → 32 字节）
pub fn bech32_decode_addr(addr: &str) -> Result<[u8; 32], String> {
    use bech32::{primitives::decode::CheckedHrpstring, Bech32m};
    let parsed =
        CheckedHrpstring::new::<Bech32m>(addr).map_err(|e| format!("bech32 解码失败: {e}"))?;
    let bytes = parsed.byte_iter().collect::<Vec<u8>>();
    if bytes.len() != 32 {
        return Err(format!("地址数据长度应是 32 字节，实际 {}", bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// 排放函数（独立成函数以便重放校验复用）
pub fn reward_at(height: u64) -> u64 {
    let halvings = height / HALVING_INTERVAL;
    if halvings >= 30 {
        return 0;
    }
    INIT_REWARD >> halvings
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tx_root：空块为全零；交易集合/顺序变化 → root 变化 → 块哈希变化（块哈希与块内交易严格绑定）
    #[test]
    fn tx_root_and_block_hash_binding() {
        assert_eq!(tx_root_of(&[]), [0u8; 32]);

        let d1 = [1u8; 32];
        let d2 = [2u8; 32];
        assert_ne!(tx_root_of(&[d1]), tx_root_of(&[d2]));
        assert_ne!(tx_root_of(&[d1, d2]), tx_root_of(&[d2, d1])); // 顺序敏感

        let vrf = vec![9u8; 32];
        let h1 = block_hash_of("genesis", 1, 0, &vrf, &tx_root_of(&[d1]));
        let h2 = block_hash_of("genesis", 1, 0, &vrf, &tx_root_of(&[d2]));
        let h3 = block_hash_of("genesis", 1, 0, &vrf, &tx_root_of(&[]));
        assert_ne!(h1, h2);
        assert_ne!(h1, h3);
        // 同参数确定性
        assert_eq!(block_hash_of("genesis", 1, 0, &vrf, &tx_root_of(&[d1])), h1);
    }

    #[test]
    fn genesis_is_anchored() {
        // 创世哈希确定性：宣言原文不变 → 哈希不变
        let g = genesis_hash();
        assert_eq!(hex::encode(g), genesis_hash_hex());
        // 宣言被篡改 → 哈希必变（宣言与链的绑定自证，BTC 同款）
        let d_tampered = crypto::shake256(b"QL-GENESIS-V1", b"tampered manifesto", 32);
        let mut dig = [0u8; 32];
        dig.copy_from_slice(&d_tampered);
        let zero_vrf = [0u8; 32];
        let tampered = block_hash_of(GENESIS_PREV_HEX, 0, 0, &zero_vrf, &tx_root_of(&[dig]));
        assert_ne!(g, tampered);
        // 创世 prev = 64 个 0（链的第一个锚点）
        assert_eq!(GENESIS_PREV_HEX.len(), 64);
        assert!(GENESIS_PREV_HEX.chars().all(|c| c == '0'));
        // 创世时间戳固定（2026-10-26 00:00:00 UTC）
        assert_eq!(GENESIS_TS_UNIX, 1792972800);
    }

    /// 信任根回归闸：改宣言不必报警，改宣言而不改锚定值必须让测试与启动同时失败
    #[test]
    fn genesis_hash_equals_anchored_constant() {
        assert_eq!(genesis_hash_hex(), GENESIS_HASH_HEX);
    }

    /// 创世块自身自洽：块哈希由自己的字段算出，且不是一次记账（无出块者/无排放）
    #[test]
    fn genesis_header_is_self_consistent() {
        let g = genesis_header();
        assert_eq!(g.height, 0);
        assert_eq!(g.prev_hash_hex, GENESIS_PREV_HEX);
        assert_eq!(g.minted_after, 0);
        assert_eq!(g.reward_total, 0);
        assert!(g.miner.is_empty());
        assert_eq!(g.block_hash_hex, genesis_hash_hex());

        let vrf = hex::decode(&g.vrf_hex).unwrap();
        let digests: Vec<[u8; 32]> = g
            .txs
            .iter()
            .map(|t| {
                let mut d = [0u8; 32];
                hex::decode_to_slice(&t.digest_hex, &mut d).unwrap();
                d
            })
            .collect();
        let recomputed =
            block_hash_of(&g.prev_hash_hex, g.height, g.minted_after, &vrf, &tx_root_of(&digests));
        assert_eq!(hex::encode(recomputed), g.block_hash_hex);
        // 宣言真的在创世块里（txs 唯一那笔的摘要即宣言摘要）
        assert_eq!(g.txs[0].digest_hex, hex::encode(genesis_manifesto_digest()));
    }

    /// 创世行 + 首个真实块整链可重放；篡改创世行的非哈希字段必须被拒
    #[test]
    fn genesis_line_replays_and_tampering_is_rejected() {
        let g = genesis_header();
        let vrf_hex = hex::encode([7u8; 32]);
        let mut one = BlockHeader {
            version: BLOCK_V_LEGACY,
            height: 1,
            ts_unix: GENESIS_TS_UNIX + 2,
            prev_hash_hex: g.block_hash_hex.clone(),
            block_hash_hex: String::new(),
            vrf_hex: vrf_hex.clone(),
            minted_after: reward_at(1),
            reward_total: reward_at(1),
            miner: "ql1qqqminer".to_string(),
            entity_root_hex: String::new(),
            entity_count: 0,
            dust_root_hex: String::new(),
            dust_count: 0,
            txs: vec![],
        };
        one.block_hash_hex = hex::encode(block_hash_of(
            &one.prev_hash_hex,
            one.height,
            one.minted_after,
            &hex::decode(&vrf_hex).unwrap(),
            &tx_root_of(&[]),
        ));
        assert!(SimChain::verify_header_slice(&[g.clone(), one.clone()]));

        // reward_total / miner 不进块哈希，只能靠"逐字等于源码常量"这道闸拦住
        let mut greedy = g.clone();
        greedy.reward_total = 1_000_000_000_000;
        greedy.miner = "ql1qqqattacker".to_string();
        assert!(!SimChain::verify_header_slice(&[greedy, one.clone()]));
    }

    /// 链被归档重置时把 tag 交给上层（同一枚戳给 world 账本归档用），且只交一次
    #[test]
    fn a_reset_chain_hands_out_its_archive_tag_once() {
        let (dir, chain) = temp_chain("reset-tag");
        assert!(chain.take_reset_tag().is_none(), "新链没有重置过");
        drop(chain);

        // 把链文件改成对不上的内容：启动自检必然失败 → 归档重置
        std::fs::write(dir.join("blocks.jsonl"), "{\"height\":0,\"hash\":\"00\"}\n").unwrap();
        std::fs::write(
            dir.join("ledger.json"),
            serde_json::to_string(&Ledger::default()).unwrap(),
        )
        .unwrap();
        let chain = SimChain::new(dir.join("ledger.json"), dir.join("blocks.jsonl"));
        let tag = chain
            .take_reset_tag()
            .expect("校验失败必须给出归档 tag");
        assert!(dir.join(format!("blocks.jsonl.bak-{tag}")).exists());
        assert!(dir.join(format!("ledger.json.bak-{tag}")).exists());
        assert!(chain.take_reset_tag().is_none(), "tag 只能取一次");
        assert_eq!(chain.height(), 0, "重置后从新创世继续");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 新建链的 blocks.jsonl 第 0 行就是创世块（"创世是否启动"落盘可查）
    #[test]
    fn new_chain_materializes_genesis_line() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("qlight-genesis-test-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let blocks = dir.join("blocks.jsonl");

        let chain = SimChain::new(dir.join("ledger.json"), blocks.clone());
        let first_line = std::fs::read_to_string(&blocks).unwrap();
        let first_line = first_line.lines().next().unwrap();
        assert_eq!(
            serde_json::from_str::<BlockHeader>(first_line).unwrap(),
            genesis_header()
        );
        assert_eq!(chain.height(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------ P0-①：追账快照逐笔验签

    fn temp_chain(tag: &str) -> (PathBuf, Arc<SimChain>) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("qlight-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let chain = SimChain::new(dir.join("ledger.json"), dir.join("blocks.jsonl"));
        (dir, chain)
    }

    fn test_core(byte: u8) -> crate::keystore::WalletCore {
        let seed = [byte; 32];
        let (sk, vk) = crypto::keypair_from_seed(&seed);
        let salt = [byte; 8];
        let address = crypto::derive_address(&vk, &salt);
        let pk_bytes = crypto::encode_pk(&vk);
        crate::keystore::WalletCore {
            seed: zeroize::Zeroizing::new(seed),
            addr_salt: salt,
            address,
            signing_key: sk,
            verifying_key: vk,
            pk_bytes,
        }
    }

    /// 在本节点当前视图里找一格此刻可挖的矿（测试专用；真实客户端走 ore::scan）。
    ///
    /// 注意：矿脉是概率性的——高块高时配额收窄，某一回合**真的可能一格都没有**。
    /// 这不是错误，而是协议本身的性质：这一回合挖不到，就等下一个块的 VRF 重画地表。
    fn try_find_ore(chain: &SimChain) -> Option<(u16, u16, u16)> {
        let vrfs = chain.ledger.lock().unwrap().vrfs.clone();
        let words = crate::world::terrain::seed_words(GENESIS_HASH_HEX, &vrfs);
        let h = chain.height();
        for y in 0..crate::world::WORLD_H as u32 {
            for x in 0..crate::world::WORLD_W as u32 {
                if crate::world::ore::is_ore(x, y, &words, h) {
                    return Some((
                        x as u16,
                        y as u16,
                        crate::world::ore::ore_layer(x, y, &words),
                    ));
                }
            }
        }
        None
    }

    fn find_ore(chain: &SimChain) -> (u16, u16, u16) {
        try_find_ore(chain).expect("创世纪元格 + 配额下竟无矿可挖")
    }

    /// 出第一块：提交一张真矿签，让出块者拿到发行（v1 只有收下矿签才发币）
    fn mine_first_block_with_claim(chain: &SimChain, miner: &crate::keystore::WalletCore) {
        let (x, y, layer) = find_ore(chain);
        chain
            .submit_claim(miner, x, y, layer)
            .expect("矿签应入池");
        chain.mine_block(miner).expect("第 1 块应出块");
    }

    // ------------------------------------------------ §9.16 v3：节奏换收益，曲线不动

    /// 「一等到就落签」的最快手速出块，返回每次结算的 (块高, 该笔新增发行)。
    ///
    /// 某一回合地表恰好无矿时只能先出空块——这是矿脉概率性的正常结果，
    /// 不是节奏失效；下一块 VRF 换了地表再试。
    fn strike_fast(chain: &SimChain, miner: &crate::keystore::WalletCore, target: u64) -> Vec<(u64, u64)> {
        let mut strikes = Vec::new();
        let mut minted_before = chain.total_minted();
        while chain.height() < target {
            if chain.height() + 1 >= chain.next_claim_height(&miner.address) {
                if let Some((x, y, layer)) = try_find_ore(chain) {
                    chain.submit_claim(miner, x, y, layer).expect("节奏到了，矿签应入池");
                }
            }
            let (_, head, _, _) = chain.mine_block(miner).expect("应出块");
            if head.minted_after > minted_before {
                strikes.push((head.height, head.minted_after - minted_before));
                minted_before = head.minted_after;
            }
        }
        strikes
    }

    /// 减半曲线到块高 `h` 为止的**应发行总量**——发行永远不许越过它
    fn curve_to(h: u64) -> u64 {
        (1..=h).map(reward_at).fold(0u64, |acc, r| acc.saturating_add(r))
    }

    /// 阶梯与结算窗口的口径只有一句话：**一笔最多结清一个当前档位，空档不补发**。
    /// 首笔也只结真实存在的块（`min(height, 档位)`），绝不预支尚未发生的窗口。
    #[test]
    fn v3_window_pays_the_covered_interval_and_nothing_more() {
        assert_eq!(craft_floor(0), crate::world::MINE_FLOORS[0], "零矿格 = 最慢一档");
        assert_eq!(craft_floor(1), 40);
        assert_eq!(craft_floor(7), 10);
        assert_eq!(craft_floor(9_999), 10, "阶梯到底就是底，永不进 10 以下");

        // 首笔（v3+）：只结真实存在的块，不预支未来窗口
        assert_eq!(claim_window(BLOCK_V_CRAFT, 1, 0, 0), Some((50, 1)));
        assert_eq!(claim_window(BLOCK_V_CRAFT, 10, 0, 0), Some((50, 10)));
        assert_eq!(claim_window(BLOCK_V_CRAFT, 50, 0, 0), Some((50, 50)));
        assert_eq!(claim_window(BLOCK_V_CRAFT, 1_000, 0, 0), Some((50, 50)));
        // 首笔（v1/v2 历史口径）：当年就是固定结一个整档，只为重放
        assert_eq!(claim_window(BLOCK_V_WORLD, 1, 0, 0), Some((50, 50)));
        // 第二笔起：档由**这个地址自己**落过几格决定，早一块都不给
        assert_eq!(claim_window(BLOCK_V_CRAFT, 40, 1, 0), None, "50 档还差一块");
        assert_eq!(claim_window(BLOCK_V_CRAFT, 41, 1, 0), None);
        assert_eq!(claim_window(BLOCK_V_CRAFT, 51, 1, 0), Some((50, 50)));
        assert_eq!(claim_window(BLOCK_V_CRAFT, 41, 1, 1), Some((40, 40)));
        // 空档不补发：隔了 20 块也只结当前 10 档，多出来的 10 块作废
        assert_eq!(claim_window(BLOCK_V_CRAFT, 21, 1, 7), Some((10, 10)));
        assert_eq!(claim_window(BLOCK_V_CRAFT, 91, 1, 7), Some((10, 10)));
        // 老版本没有阶梯：v2 及以前永远 50 块一刀
        assert_eq!(claim_window(BLOCK_V_WORLD, 41, 1, 7), None);
        assert_eq!(claim_window(BLOCK_V_WORLD, 51, 1, 7), Some((50, 50)));
        // 干等不会把钱等到更大：Δ 由窗口决定，封顶仍在硬顶
        assert_eq!(claim_reward_delta(1, 0, 3), reward_at(1) * 3);
        assert_eq!(claim_reward_delta(1, HARD_CAP, 3), 0, "挖完就是挖完了");
    }

    /// 回归：空块（没有矿签的块）永远不补发。隔得再久，一笔也只结一个档位。
    #[test]
    fn empty_blocks_are_never_backfilled() {
        // 首笔在高度 1 只拿 1 块，而不是把 50 块预支掉
        let (_, first) = claim_window(BLOCK_V_CRAFT, 1, 0, 0).unwrap();
        assert_eq!(first, 1);

        // 中间空 200 块后落一笔：仍是当前档位，不是 200
        let (floor, delta) = claim_window(BLOCK_V_CRAFT, 201, 1, 0).unwrap();
        assert_eq!((floor, delta), (50, 50));
        let (floor, delta) = claim_window(BLOCK_V_CRAFT, 201, 1, 7).unwrap();
        assert_eq!((floor, delta), (10, 10), "档位再快也不许把空档一次性印出来");

        // 空档累计起来也追不上减半曲线
        let mut minted = 0u64;
        let mut last = 0u64;
        let mut prior = 0u64;
        for h in [1u64, 7, 40, 400, 401, 900] {
            if let Some((_, d)) = claim_window(BLOCK_V, h, last, prior) {
                minted = minted.saturating_add(claim_reward_delta(h, minted, d));
                last = h;
                prior += 1;
            }
        }
        assert!(
            minted <= curve_to(900),
            "空档补发让发行 {} 越过了曲线 {}",
            minted,
            curve_to(900)
        );
    }

    /// 造一个不承诺世界的 v4 空块（测试版本闸门/水位用）
    fn v4_header(prev: &str, height: u64, minted: u64, ts: u64, miner: &str) -> BlockHeader {
        let vrf = [9u8; 32];
        let root = tx_root_of(&[]);
        let er = [0u8; 32];
        let hash = block_hash_of_miner_bound(
            prev,
            height,
            ts,
            minted,
            &vrf,
            &root,
            &er,
            0,
            miner,
        );
        BlockHeader {
            version: BLOCK_V_MINER,
            height,
            ts_unix: ts,
            prev_hash_hex: prev.to_string(),
            block_hash_hex: hex::encode(hash),
            vrf_hex: hex::encode(vrf),
            minted_after: minted,
            reward_total: 0,
            miner: miner.to_string(),
            entity_root_hex: hex::encode(er),
            entity_count: 0,
            dust_root_hex: String::new(),
            dust_count: 0,
            txs: vec![],
        }
    }

    /// v4 之后不许回退到 v0；激活高度之后旧版本一律拒。
    #[test]
    fn version_cannot_downgrade_after_v4() {
        let g = genesis_header();
        let v4 = v4_header(
            &g.block_hash_hex,
            1,
            0,
            GENESIS_TS_UNIX + 2,
            "ql1v4miner",
        );
        assert!(SimChain::verify_header_slice(&[g.clone(), v4.clone()]));

        let v0 = BlockHeader {
            version: BLOCK_V_LEGACY,
            height: 2,
            ts_unix: GENESIS_TS_UNIX + 4,
            prev_hash_hex: v4.block_hash_hex.clone(),
            block_hash_hex: String::new(),
            vrf_hex: hex::encode([5u8; 32]),
            minted_after: reward_at(2),
            reward_total: reward_at(2),
            miner: "ql1v0miner".to_string(),
            entity_root_hex: String::new(),
            entity_count: 0,
            dust_root_hex: String::new(),
            dust_count: 0,
            txs: vec![],
        };
        let mut v0 = v0;
        v0.block_hash_hex = hex::encode(block_hash_of(
            &v0.prev_hash_hex,
            v0.height,
            v0.minted_after,
            &hex::decode(&v0.vrf_hex).unwrap(),
            &tx_root_of(&[]),
        ));
        assert!(
            !SimChain::verify_header_slice(&[g, v4, v0]),
            "v4 链后面接 v0 = 绕过矿签免费发行，必须拒绝"
        );

        assert_eq!(version_floor_at(BLOCK_V_V5_ACTIVATION), BLOCK_V_LEGACY);
        assert_eq!(version_floor_at(BLOCK_V_V5_ACTIVATION + 1), BLOCK_V);
    }

    /// v0→v4 升级：水位跟到最后一条 v0，第一笔 v4 只结算升级后的新块。
    #[test]
    fn v0_to_v4_upgrade_sets_the_emission_watermark() {
        let (dir, chain) = temp_chain("v0-upgrade");
        let g = genesis_header();
        let v0 = BlockHeader {
            version: BLOCK_V_LEGACY,
            height: 1,
            ts_unix: GENESIS_TS_UNIX + 2,
            prev_hash_hex: g.block_hash_hex.clone(),
            block_hash_hex: String::new(),
            vrf_hex: hex::encode([5u8; 32]),
            minted_after: reward_at(1),
            reward_total: reward_at(1),
            miner: "ql1v0miner".to_string(),
            entity_root_hex: String::new(),
            entity_count: 0,
            dust_root_hex: String::new(),
            dust_count: 0,
            txs: vec![],
        };
        let mut v0 = v0;
        v0.block_hash_hex = hex::encode(block_hash_of(
            &v0.prev_hash_hex,
            v0.height,
            v0.minted_after,
            &hex::decode(&v0.vrf_hex).unwrap(),
            &tx_root_of(&[]),
        ));
        let v4 = v4_header(
            &v0.block_hash_hex,
            2,
            reward_at(1),
            GENESIS_TS_UNIX + 4,
            "ql1v4miner",
        );
        let snap = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&g).unwrap(),
            serde_json::to_string(&v0).unwrap(),
            serde_json::to_string(&v4).unwrap()
        );
        chain.install_snapshot(&snap).expect("v0→v4 升级链应通过");
        assert_eq!(chain.height(), 2);
        assert_eq!(chain.last_claim_height(), 1, "水位必须停在最后一条 v0");
        assert_eq!(
            chain.next_claim_height("ql1v4miner"),
            51,
            "v0 已发过块 1，v4 第一笔要从 51 起算，不能把 2..51 再补一遍"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 性质测试：最快手速连挖，发行总量也追不上减半曲线——
    /// 阶梯把「多久能领」从 50 块压到 10 块，一分钱额外发行都没造出来。
    #[test]
    fn max_cadence_never_outruns_the_emission_curve() {
        let (dir, chain) = temp_chain("v3-curve");
        let miner = test_core(41);
        let target = 260;
        let strikes = strike_fast(&chain, &miner, target);
        assert!(strikes.len() > 10, "阶梯越爬越快，260 块里该落十几格：{}", strikes.len());
        assert_eq!(chain.height(), target);

        // 逐笔核账：每笔最多结清一个当前档位，空档不补发
        let mut prior = 0u64;
        let mut last_claim = 0u64;
        for (h, gain) in &strikes {
            let (floor, delta) = claim_window(BLOCK_V, *h, last_claim, prior).expect("结算窗口存在");
            assert_eq!(*gain, reward_at(*h) * delta, "块高 {h} 多印了：不是按窗口结算");
            assert_eq!(
                delta,
                if last_claim == 0 { (*h).min(floor) } else { floor },
                "块高 {h} 结了超过一个档位的量"
            );
            prior += 1;
            last_claim = *h;
        }

        // 总量闸门：每一笔都落在减半曲线的积分之内，一分不多、不需要任何豁免。
        let covered = strikes.last().unwrap().0;
        let settled: u64 = strikes.iter().map(|(_, g)| g).sum();
        assert!(
            settled <= curve_to(covered),
            "已结算 {settled}，而 {covered} 块的曲线只给了 {}",
            curve_to(covered)
        );
        assert!(
            chain.total_minted() <= curve_to(target),
            "发行 {} 越过了曲线总量 {}",
            chain.total_minted(),
            curve_to(target)
        );
        // 节奏确实变快了：同样的块数里，落下的矿格比 50 块一刀时多
        assert!(
            strikes.len() as u64 > (target / MINE_COOLDOWN_BLOCKS),
            "阶梯没起作用：{} 次结算 / {} 块",
            strikes.len(),
            target
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 换钥匙 = 从零开始爬阶梯：新钥匙的第一笔只能按最慢那一档结，且一笔最多一个档位。
    /// 空档不补发之后，"同一个人隔 50 块再落" 反而会把自己 40 档之外的 10 块作废——
    /// 开小号既买不到更快的档位，也换不到越过曲线的发行。
    #[test]
    fn a_second_key_adds_nothing_a_gap_was_already_worth() {
        let (dir_a, solo) = temp_chain("v3-solo");
        let (dir_b, alt) = temp_chain("v3-syb");
        let key_a = test_core(42);
        let key_b = test_core(43);

        for (chain, striker) in [(&solo, &key_a), (&alt, &key_b)] {
            strike_fast(chain, &key_a, 1);
            while chain.height() < 51 {
                let next = chain.height() + 1;
                if next == 51 {
                    // 矿签必须由出块者本人提交：谁落这一格，谁就得出这一块的块
                    let (x, y, layer) = find_ore(chain);
                    chain
                        .submit_claim(striker, x, y, layer)
                        .expect("第 51 块该轮到这一格矿");
                }
                let producer = if next == 51 { striker } else { &key_a };
                chain.mine_block(producer).expect("出块");
            }
        }

        assert_eq!(solo.height(), 51);
        assert_eq!(alt.height(), 51);
        // 曲线是硬上界：无论几把钥匙，发行都不许越过它
        assert!(
            alt.total_minted() <= curve_to(51),
            "两把钥匙也在曲线之内：{}",
            alt.total_minted()
        );
        assert!(
            solo.total_minted() <= curve_to(51),
            "单钥匙也在曲线之内：{}",
            solo.total_minted()
        );
        // 新钥匙的第一笔在块高 51 落下，只结 min(51, 50) = 50 块——正好是那段空档本身，
        // 一分不多。原钥匙为了等同一高度，把自己 40 档之外的 10 块作废了（空档不补发的
        // 代价），所以这里 solo 反而更少；这不是小号多拿，两把钥匙都仍在曲线之内。
        assert_eq!(
            alt.total_minted(),
            curve_to(51),
            "新钥匙只结清空档本身，不多一分"
        );
        assert!(
            solo.total_minted() < alt.total_minted(),
            "超出自有档位的等待会被作废：solo={} alt={}",
            solo.total_minted(),
            alt.total_minted()
        );
        // 全新钥匙的第一步只能是最慢那一档：阶梯不看钥匙多，只看这把钥匙落下过几格
        assert_eq!(alt.claim_pace(&key_b.address).0, 1);
        assert_eq!(alt.claim_pace(&key_b.address).1, 40, "落过 1 格 = 40 块档");
        assert_eq!(solo.claim_pace(&key_a.address).0, 2, "同一个人已落 2 格");
        // 真正的反脚本性质：新钥匙的档位永远不比老钥匙快，开小号买不到节奏
        assert!(
            alt.claim_pace(&key_b.address).1 >= solo.claim_pace(&key_a.address).1,
            "开小号换到了更快的档位：alt={} solo={}",
            alt.claim_pace(&key_b.address).1,
            solo.claim_pace(&key_a.address).1
        );

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    /// 快照重建走同一套账：v3 块头序列被独立节点逐块复算通过，
    /// 且节奏阶梯从 `blocks.jsonl` 当场重建（`claims_by` 不落盘，避免第二本账）
    #[test]
    fn v3_chain_rebuilds_its_ladder_from_block_headers_alone() {
        let (dir_src, src) = temp_chain("v3-snap-src");
        let (dir_dst, dst) = temp_chain("v3-snap-dst");
        let miner = test_core(44);
        let strikes = strike_fast(&src, &miner, 130);
        assert!(strikes.len() >= 3);

        let snap = std::fs::read_to_string(dir_src.join("blocks.jsonl")).unwrap();
        let tip = dst.install_snapshot(&snap).expect("v3 自证快照应被接受");
        assert_eq!(tip, 130);
        assert_eq!(dst.total_minted(), src.total_minted());
        assert_eq!(dst.height(), src.height());
        // 阶梯跟着账本一起过来：对端算出的下一档 = 源头算出的下一档
        assert_eq!(dst.claim_pace(&miner.address), src.claim_pace(&miner.address));
        assert_eq!(
            dst.next_claim_height(&miner.address),
            src.next_claim_height(&miner.address)
        );
        let (prior, floor) = src.claim_pace(&miner.address);
        assert_eq!(prior, strikes.len() as u64, "已落矿格数只从块头复算");
        assert!(floor < crate::world::MINE_FLOORS[0], "落过几格就该比起步档快");

        let _ = std::fs::remove_dir_all(&dir_src);
        let _ = std::fs::remove_dir_all(&dir_dst);
    }

    /// 自证快照能整链重建：真出两块（含一笔转账），对端装进去后高度/余额逐字一致
    #[test]
    fn snapshot_roundtrip_rebuilds_the_same_ledger() {
        let (dir_a, src) = temp_chain("snap-src");
        let (dir_b, dst) = temp_chain("snap-dst");
        let miner = test_core(11);
        let payee = test_core(22);

        mine_first_block_with_claim(&src, &miner);
        assert!(src.balance_of(&miner.address) > 0, "挖到矿才发币");
        let env = TxEnvelope::new(
            bech32_decode_addr(&miner.address).unwrap(),
            src.pending_nonce(&miner.address),
            50,
        )
        .transfer(bech32_decode_addr(&payee.address).unwrap(), 1_000_000_000);
        let sig = crypto::sign(&miner.signing_key, &tx_digest(&env));
        src.submit(&miner, env, sig).expect("转账应入池");
        src.mine_block(&miner).expect("第 2 块应出块");

        let snap = std::fs::read_to_string(dir_a.join("blocks.jsonl")).unwrap();
        let tip = dst.install_snapshot(&snap).expect("自证快照应被接受");
        assert_eq!(tip, 2);
        assert_eq!(dst.height(), src.height());
        assert_eq!(dst.balance_of(&miner.address), src.balance_of(&miner.address));
        assert_eq!(dst.balance_of(&payee.address), 1_000_000_000);

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    /// 只有「声称」没有原件 → 拒收。旧实现照单全收，一条自洽的假链就能搬走余额。
    #[test]
    fn snapshot_without_tx_originals_is_rejected() {
        let (dir_a, src) = temp_chain("snap-strip-src");
        let (dir_b, dst) = temp_chain("snap-strip-dst");
        let miner = test_core(12);
        let payee = test_core(23);

        mine_first_block_with_claim(&src, &miner);
        let env = TxEnvelope::new(
            bech32_decode_addr(&miner.address).unwrap(),
            src.pending_nonce(&miner.address),
            50,
        )
        .transfer(bech32_decode_addr(&payee.address).unwrap(), 1_000_000_000);
        let sig = crypto::sign(&miner.signing_key, &tx_digest(&env));
        src.submit(&miner, env, sig).expect("转账应入池");
        src.mine_block(&miner).expect("第 2 块应出块");

        let snap = std::fs::read_to_string(dir_a.join("blocks.jsonl")).unwrap();
        let stripped: Vec<String> = snap
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let mut bl: BlockLine = serde_json::from_str(l).unwrap();
                bl.wires.clear();
                serde_json::to_string(&bl).unwrap()
            })
            .collect();

        let err = dst.install_snapshot(&stripped.join("\n")).unwrap_err();
        assert!(err.contains("原件不全"), "应因缺原件拒收，实得：{err}");
        assert_eq!(dst.height(), 0, "拒收后本机状态不许动");
        assert_eq!(dst.balance_of(&payee.address), 0);

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    /// 自洽的假账本 + 真签名原件也搬不走钱：
    /// 攻击者把受害者**真签**的 0.1 QLX 转账塞进块里，块头却声称「他付了 3 QLX」。
    /// 哈希链 / tx_root / 排放全都能重算通过（第一道闸拦不住），
    /// 但原件复算出的凭据与块头声称不符 → 整条快照拒收，余额一分不动。
    #[test]
    fn forged_attestation_with_genuine_wire_is_rejected() {
        let (dir_b, dst) = temp_chain("snap-forge-dst");
        let victim = test_core(33);
        let attacker = test_core(44);

        // 真块 1（历史 v0 口径）：victim 出块拿到 3.33 QLX——让「余额不足」不成为拒绝的理由
        let g = genesis_header();
        let mut b1 = BlockHeader {
            version: BLOCK_V_LEGACY,
            height: 1,
            ts_unix: GENESIS_TS_UNIX + 2,
            prev_hash_hex: g.block_hash_hex.clone(),
            block_hash_hex: String::new(),
            vrf_hex: hex::encode([5u8; 32]),
            minted_after: reward_at(1),
            reward_total: reward_at(1),
            miner: victim.address.clone(),
            entity_root_hex: String::new(),
            entity_count: 0,
            dust_root_hex: String::new(),
            dust_count: 0,
            txs: vec![],
        };
        b1.block_hash_hex = hex::encode(block_hash_of(
            &b1.prev_hash_hex,
            b1.height,
            b1.minted_after,
            &hex::decode(&b1.vrf_hex).unwrap(),
            &tx_root_of(&[]),
        ));

        // victim 真签 0.1 QLX 给 attacker
        let env = TxEnvelope::new(bech32_decode_addr(&victim.address).unwrap(), 0, 50)
            .transfer(bech32_decode_addr(&attacker.address).unwrap(), 100_000_000);
        let sig = crypto::sign(&victim.signing_key, &tx_digest(&env));
        let wire = BlockWire::new(&env, &sig, &victim.pk_bytes, &victim.addr_salt);
        let digest = tx_digest(&env);

        // 伪造块 2：同一个 digest / from / nonce，金额吹成 3 QLX
        let vrf = [7u8; 32];
        let mut b2 = BlockHeader {
            version: BLOCK_V_LEGACY,
            height: 2,
            ts_unix: GENESIS_TS_UNIX + 4,
            prev_hash_hex: b1.block_hash_hex.clone(),
            block_hash_hex: String::new(),
            vrf_hex: hex::encode(vrf),
            minted_after: reward_at(1) + reward_at(2),
            reward_total: reward_at(2) + FEE,
            miner: attacker.address.clone(),
            entity_root_hex: String::new(),
            entity_count: 0,
            dust_root_hex: String::new(),
            dust_count: 0,
            txs: vec![TxAttestation {
                digest_hex: hex::encode(digest),
                from: victim.address.clone(),
                to: attacker.address.clone(),
                amount: 3_000_000_000,
                fee: FEE,
                nonce: 0,
            }],
        };
        b2.block_hash_hex = hex::encode(block_hash_of(
            &b2.prev_hash_hex,
            b2.height,
            b2.minted_after,
            &vrf,
            &tx_root_of(&[digest]),
        ));
        assert!(
            SimChain::verify_header_slice(&[g.clone(), b1.clone(), b2.clone()]),
            "伪造块本身是自洽的（这正是第一道闸拦不住的那类攻击）"
        );

        let snap = vec![
            serde_json::to_string(&g).unwrap(),
            serde_json::to_string(&BlockLine {
                header: b1,
                wires: vec![],
                dusts: vec![],
            })
            .unwrap(),
            serde_json::to_string(&BlockLine {
                header: b2,
                wires: vec![wire],
                dusts: vec![],
            })
            .unwrap(),
        ]
        .join("\n");

        let err = dst.install_snapshot(&snap).unwrap_err();
        assert!(
            err.contains("重放凭据与块头不一致"),
            "应因凭据不符拒收，实得：{err}"
        );
        assert_eq!(dst.height(), 0, "拒收后本机状态不许动");
        assert_eq!(dst.balance_of(&attacker.address), 0, "假账本的付款不许落地");
        assert_eq!(dst.balance_of(&victim.address), 0);

        let _ = std::fs::remove_dir_all(&dir_b);
    }

    // ------------------------------------------------ v5：池子与光尘的共识结算

    /// 池子操作在同一本账里同时动 QLX、光尘与储备：任何一步不成立，三条都不动。
    #[test]
    fn pool_ops_move_both_ledgers_and_cannot_overdraw() {
        use crate::tx::PoolOp;
        use crate::world::pool::POOL_ID;

        let chain = temp_chain("pool-op").1;
        let owner = test_core(5).address;
        let mut l = chain.ledger.lock().unwrap().clone();
        let qlx0 = 1_000_000_000_000_000u64;
        let dust0 = 1_000_000u64;
        l.balances.insert(owner.clone(), qlx0);
        l.dust.balances.insert(owner.clone(), dust0);

        // 首次做市：两边同时扣、储备同时记上，LP 份额里留一份永久锁死
        SimChain::apply_pool_op(
            &mut l,
            &owner,
            &PoolOp::AddLiquidity {
                qlx: qlx0,
                dust: dust0,
            },
        )
        .unwrap();
        let p = l.pools.get(POOL_ID).cloned().unwrap();
        assert_eq!((p.qlx, p.dust), (qlx0, dust0));
        assert_eq!(l.balances[&owner], 0);
        assert_eq!(l.dust.balance_of(&owner), 0);
        assert!(
            p.lp_total > p.shares_of(&owner),
            "首次做市必须锁死一份份额"
        );

        // 余额不够的做市整笔拒绝：储备 / QLX 余额 / 光尘余额一个字都不许动
        l.balances.insert(owner.clone(), 1); // QLX 够 1、光尘为 0 → 卡在光尘那一步
        let before = (l.pools.clone(), l.balances.clone(), l.dust.clone());
        let err = SimChain::apply_pool_op(
            &mut l,
            &owner,
            &PoolOp::AddLiquidity { qlx: 1, dust: 1 },
        )
        .unwrap_err();
        assert!(err.contains("光尘不够做市"), "实得：{err}");
        assert_eq!(l.pools, before.0);
        assert_eq!(l.balances, before.1);
        assert_eq!(l.dust, before.2);

        // QLX 换光尘：付出扣掉、收入加上、k 只增不减（手续费留在池里）
        l.balances.insert(owner.clone(), qlx0 / 2);
        let k0 = l.pools[POOL_ID].k();
        let dust_before = l.dust.balance_of(&owner);
        let qlx_in = 1_000_000_000_000u64;
        SimChain::apply_pool_op(
            &mut l,
            &owner,
            &PoolOp::SwapQlxForDust {
                qlx_in,
                min_out: 0,
            },
        )
        .unwrap();
        let got = l.dust.balance_of(&owner) - dust_before;
        assert!(got > 0);
        assert_eq!(l.balances[&owner], qlx0 / 2 - qlx_in);
        assert!(l.pools[POOL_ID].k() >= k0);

        // 滑点保护：算出来多少要多少 +1 就必须拒绝，且状态原样
        let before = (l.pools.clone(), l.balances.clone(), l.dust.clone());
        assert!(SimChain::apply_pool_op(
            &mut l,
            &owner,
            &PoolOp::SwapQlxForDust {
                qlx_in,
                min_out: got + 1,
            },
        )
        .is_err());
        assert_eq!(l.pools, before.0);
        assert_eq!(l.balances, before.1);
        assert_eq!(l.dust, before.2);

        // 反方向：光尘换 QLX，两边余额一起变动
        let dust_in = got / 2;
        let qlx_before = l.balances[&owner];
        SimChain::apply_pool_op(
            &mut l,
            &owner,
            &PoolOp::SwapDustForQlx {
                dust_in,
                min_out: 0,
            },
        )
        .unwrap_or_else(|e| panic!("光尘换 QLX 应成功：{e}"));
        let back = l.balances[&owner] - qlx_before;
        assert!(back > 0);
        assert_eq!(l.dust.balance_of(&owner), got - dust_in);

        // 撤资：份额变回两边资产，锁死的那份永远拿不回来
        let shares = l.pools[POOL_ID].shares_of(&owner);
        SimChain::apply_pool_op(&mut l, &owner, &PoolOp::RemoveLiquidity { lp: shares }).unwrap();
        assert!(l.balances[&owner] > qlx_before);
        assert!(l.dust.balance_of(&owner) > 0);
        assert_eq!(l.pools[POOL_ID].shares_of(&owner), 0);
        assert!(
            l.pools[POOL_ID].ready(),
            "锁死的份额让池子不会被自己掏空"
        );
    }

    /// 分叉前的链拒收池子交易（升级窗口内不许旧规则下动新资产）
    #[test]
    fn pool_tx_is_refused_before_v5() {
        use crate::tx::PoolOp;

        let chain = temp_chain("pool-gate").1;
        let core = test_core(6);
        let env = TxEnvelope::new(bech32_decode_addr(&core.address).unwrap(), 0, 50).with_pool(
            PoolOp::AddLiquidity { qlx: 1, dust: 1 },
        );
        let sig = crypto::sign(&core.signing_key, &tx_digest(&env));
        let err = chain.submit(&core, env, sig).unwrap_err();
        assert!(err.contains("v5"), "应提示等硬分叉 v5，实得：{err}");
    }

    /// v5 块：光尘记录进块 → 集合指纹进块哈希 → 快照逐块重放出同一本光尘账
    #[test]
    fn v5_block_binds_dust_and_replays_from_snapshot() {
        use crate::world::dust::{self, DustDraft, DustView};

        let (dir, chain) = temp_chain("v5-dust");
        let miner = test_core(7);
        while chain.height() < BLOCK_V_V5_ACTIVATION {
            chain.mine_block(&miner).expect("空块也应出块");
        }
        assert_eq!(chain.height(), BLOCK_V_V5_ACTIVATION);

        // 在当前链头采一格资源，落一条真签（真实客户端走 world_dust_harvest）
        let words = crate::world::resource_words();
        let hrec = chain.height();
        let (x, y, k) = (0..64u32)
            .flat_map(|y| (0..64u32).map(move |x| (x, y)))
            .filter_map(|(x, y)| {
                crate::world::resource::node_at(x, y, &words).map(|k| (x, y, k))
            })
            .find(|&(_, _, k)| crate::world::resource::reward_at(k, hrec) > 0)
            .expect("64×64 里没有资源点");
        let reward = crate::world::resource::reward_at(k, hrec);

        let store = Arc::new(dust::DustLedger::new(dir.join("dust.jsonl")));
        chain.attach_dust(store.clone());
        let pk_hash = hex::encode(crypto::shake256(b"QL-PK-REG-V1", &miner.pk_bytes, 32));
        store
            .commit_as(&pk_hash, &DustView { tip: hrec }, |seq, prev| {
                dust::sign_record(
                    &miner,
                    seq,
                    &prev,
                    &DustDraft {
                        op: dust::op::HARVEST,
                        epoch: crate::world::resource::epoch_of(hrec),
                        x: x as u16,
                        y: y as u16,
                        kind: k,
                        amount: reward,
                        to: "",
                        height: hrec,
                        ts: now_unix(),
                    },
                )
            })
            .expect("真签应落盘");

        // 下一块自动带上这条记录，并把它的集合指纹写进块哈希
        let (_, head, _, carried) = chain.mine_block(&miner).expect("第 201 块应出块");
        assert_eq!(head.version, BLOCK_V_POOL);
        assert_eq!((head.dust_count, carried.len()), (1, 1));
        assert_eq!(head.dust_root_hex, hex::encode(dust_root_of(&carried)));
        assert_eq!(
            chain.ledger.lock().unwrap().dust.balance_of(&miner.address),
            reward
        );

        // 同一份 blocks.jsonl 在空机器上重放：光尘余额与出块机一致
        let lines = std::fs::read_to_string(dir.join("blocks.jsonl")).unwrap();
        let (dir2, dst) = temp_chain("v5-replay");
        assert_eq!(
            dst.install_snapshot(&lines).unwrap(),
            BLOCK_V_V5_ACTIVATION + 1
        );
        assert_eq!(
            dst.ledger.lock().unwrap().dust.balance_of(&miner.address),
            reward,
            "重放必须得到同一本光尘账"
        );

        // 篡改一条已进块的光尘记录：原件与块头指纹对不上，整条快照拒收
        let mut bl: BlockLine = serde_json::from_str(lines.lines().last().unwrap()).unwrap();
        bl.dusts[0].amount += 1;
        let mut rows: Vec<String> = lines.lines().map(str::to_string).collect();
        *rows.last_mut().unwrap() = serde_json::to_string(&bl).unwrap();
        let err = dst.install_snapshot(&rows.join("\n")).unwrap_err();
        assert!(err.contains("指纹"), "应因指纹不符拒收，实得：{err}");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// v5 上的池子交易：真出块 → 账本动账 → 快照重放出同一份储备
    #[test]
    fn pool_tx_applies_in_v5_blocks_and_replays() {
        use crate::tx::PoolOp;
        use crate::world::dust::{self, DustDraft, DustView};
        use crate::world::pool::POOL_ID;

        let (dir, chain) = temp_chain("v5-pool");
        let miner = test_core(8);
        // 正常挖到激活高度：QLX 全部来自矿签发行（没有预铸，池子也不能凭空有钱）
        strike_fast(&chain, &miner, BLOCK_V_V5_ACTIVATION);
        let qlx_in = 1_000_000_000u64;
        let qlx_start = chain.balance_of(&miner.address);
        assert!(qlx_start > qlx_in, "矿签发行应足够做市，实得 {qlx_start}");

        // 光尘先真采一笔，让它随 201 块进共识
        let words = crate::world::resource_words();
        let hrec = chain.height();
        let (x, y, k) = (0..64u32)
            .flat_map(|y| (0..64u32).map(move |x| (x, y)))
            .filter_map(|(x, y)| {
                crate::world::resource::node_at(x, y, &words).map(|k| (x, y, k))
            })
            .find(|&(_, _, k)| crate::world::resource::reward_at(k, hrec) > 0)
            .expect("64×64 里没有资源点");
        let dust_in = crate::world::resource::reward_at(k, hrec);
        let store = Arc::new(dust::DustLedger::new(dir.join("dust.jsonl")));
        chain.attach_dust(store.clone());
        let pk_hash = hex::encode(crypto::shake256(b"QL-PK-REG-V1", &miner.pk_bytes, 32));
        store
            .commit_as(&pk_hash, &DustView { tip: hrec }, |seq, prev| {
                dust::sign_record(
                    &miner,
                    seq,
                    &prev,
                    &DustDraft {
                        op: dust::op::HARVEST,
                        epoch: crate::world::resource::epoch_of(hrec),
                        x: x as u16,
                        y: y as u16,
                        kind: k,
                        amount: dust_in,
                        to: "",
                        height: hrec,
                        ts: now_unix(),
                    },
                )
            })
            .expect("真签应落盘");
        chain.mine_block(&miner).expect("第 201 块应出块");
        assert_eq!(chain.ledger.lock().unwrap().dust.balance_of(&miner.address), dust_in);

        // 做市交易进池 → 下一块结算落账
        let nonce = chain.pending_nonce(&miner.address);
        let env = TxEnvelope::new(
            bech32_decode_addr(&miner.address).unwrap(),
            nonce,
            400,
        )
        .with_pool(PoolOp::AddLiquidity {
            qlx: qlx_in,
            dust: dust_in,
        });
        let sig = crypto::sign(&miner.signing_key, &tx_digest(&env));
        chain.submit(&miner, env, sig).expect("池子交易应入池");
        let (_, head, _, _) = chain.mine_block(&miner).expect("第 202 块应出块");
        assert!(head
            .txs
            .iter()
            .any(|t| t.to == pool_label() && t.amount == 0));

        {
            let l = chain.ledger.lock().unwrap();
            let p = l.pools.get(POOL_ID).cloned().expect("池子应存在");
            assert_eq!((p.qlx, p.dust), (qlx_in, dust_in));
            assert_eq!(p.qlx + l.balances[&miner.address], qlx_start);
            assert_eq!(l.dust.balance_of(&miner.address), 0);
        }

        // 同一份块文件在空机器上重放：储备、余额、光尘全部一致
        let lines = std::fs::read_to_string(dir.join("blocks.jsonl")).unwrap();
        let (dir2, dst) = temp_chain("v5-pool-replay");
        assert_eq!(dst.install_snapshot(&lines).unwrap(), BLOCK_V_V5_ACTIVATION + 2);
        {
            let l = dst.ledger.lock().unwrap();
            assert_eq!(l.pools.get(POOL_ID), chain.ledger.lock().unwrap().pools.get(POOL_ID));
            assert_eq!(l.balances.get(&miner.address), Some(&(qlx_start - qlx_in)));
            assert_eq!(l.dust.balance_of(&miner.address), 0);
        }

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}

/// 出块循环（兼容入口：交给 mine_block 新签名，网络模式已由 net.rs 接管出块节奏）
pub fn spawn_mining_thread(
    chain: Arc<SimChain>,
    core: Arc<crate::keystore::WalletCore>,
    app: tauri::AppHandle,
    stop: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        use tauri::Emitter;
        let mut next_block_unix = now_unix() + BLOCK_INTERVAL_SECS;
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if now_unix() >= next_block_unix {
                if let Some((event, ..)) = chain.mine_block(&core) {
                    let _ = app.emit("chain_block", &event);
                }
                next_block_unix += BLOCK_INTERVAL_SECS;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
}
