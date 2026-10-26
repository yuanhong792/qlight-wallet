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
use std::collections::{HashMap, VecDeque};
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
pub const BLOCK_V: u8 = 1;
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
    /// 本链已挖矿发行的最后一笔所在块高（0 = 从未）——两次发行至少隔 50 块
    #[serde(default)]
    pub last_claim_height: u64,
    /// 全部历史 VRF（世界地表种子 = 创世哈希 ⊕ 这些值）。
    /// 不进 ledger.json：它可以从 blocks.jsonl 逐行重建，存两份只会漂移。
    #[serde(skip)]
    pub vrfs: Vec<String>,
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
            vrfs: vec![],
        }
    }
}

/// 区块头（blocks.jsonl 每行一条，含块内全部交易，供哈希链重放校验）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockHeader {
    /// 0 = 历史（每块固定排放）；1 = 挖矿结算（有矿签才发行）。旧行缺省按 0 读
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

pub struct SimChain {
    pub ledger: Arc<Mutex<Ledger>>,
    mempool: Arc<Mutex<Vec<PendingTx>>>,
    /// 本机最近签发的交易原件（孤儿块交易回灌用，环形 64 条）
    sent_cache: Arc<Mutex<VecDeque<BlockWire>>>,
    ledger_path: PathBuf,
    blocks_path: PathBuf,
    /// 启动哈希链校验失败置 true，禁出新块与收交易
    pub corrupted: Arc<Mutex<bool>>,
}

#[derive(Serialize, Clone)]
pub struct BlockEvent {
    pub height: u64,
    pub block_hash_hex: String,
    pub vrf_hex: String,
    pub reward: u64,
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
            corrupted: Arc::new(Mutex::new(false)),
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
            chain.archive_and_reset();
            chain.ensure_genesis_line();
        }
        chain.sync_chain_meta();
        Arc::new(chain)
    }

    /// 从 blocks.jsonl 重建两项"由链本身决定"的运行态：
    /// 全部历史 VRF（地表种子）与上一笔挖矿发行的块高。
    ///
    /// 两者都不进 ledger.json 的持久口径（VRF 会随链无限增长），启动时宁可多读
    /// 一遍块文件，也不让一份可能过期的副本变成第二真相。
    fn sync_chain_meta(&self) {
        let Ok(text) = std::fs::read_to_string(&self.blocks_path) else { return };
        let mut vrfs: Vec<String> = Vec::new();
        let mut last_claim = 0u64;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(bl) = serde_json::from_str::<BlockLine>(line) else { continue };
            if bl.header.height == 0 {
                continue;
            }
            vrfs.push(bl.header.vrf_hex.clone());
            if bl.header.txs.iter().any(|t| claim_cell_of_attestation(t).is_some()) {
                last_claim = last_claim.max(bl.header.height);
            }
        }
        let mut l = self.ledger.lock().unwrap();
        l.vrfs = vrfs;
        l.last_claim_height = last_claim;
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

    /// 校验失败的账本文件归档（ledger/blocks → *.bak-epoch），并从新创世继续
    fn archive_and_reset(&self) {
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
        if envelope.actions.is_empty() {
            return Err("交易不包含任何转账".into());
        }
        // 零金额只允许一种形态：矿签（单动作、目标是 QLOR 世界坐标）。
        // 它不付手续费——挖矿必须能从"余额为 0 的新钱包"开始，否则发行就被锁死。
        let claim = claim_cell_of(&envelope);
        if claim.is_none() && envelope.actions.iter().any(|a| a.amount == 0) {
            return Err("转账金额必须大于 0".into());
        }
        let tx_fee = if claim.is_some() { 0 } else { FEE };

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

    /// 上一笔挖矿发行所在块高（0 = 从未）
    pub fn last_claim_height(&self) -> u64 {
        self.ledger.lock().unwrap().last_claim_height
    }

    /// 下一笔挖矿最早能结算的块高：从未发行就是下一个块；否则距离上次满 50 块。
    pub fn next_claim_height(&self) -> u64 {
        let l = self.ledger.lock().unwrap();
        if l.last_claim_height == 0 {
            l.height + 1
        } else {
            (l.last_claim_height + MINE_COOLDOWN_BLOCKS).max(l.height + 1)
        }
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
    ) -> Option<(BlockEvent, BlockHeader, Vec<BlockWire>)> {
        if *self.corrupted.lock().unwrap() {
            return None;
        }
        let mut ledger = self.ledger.lock().unwrap();
        let height_next = ledger.height + 1;
        let prev_hash_before = ledger.prev_hash_hex.clone();

        // —— VRF 存活心跳（轻节点证明；输入绑定钱包种子/高度/前块哈希）——
        let mut vrf_in: Vec<u8> = Vec::with_capacity(80);
        vrf_in.extend_from_slice(core.seed.as_ref());
        vrf_in.extend_from_slice(&height_next.to_be_bytes());
        vrf_in.extend_from_slice(prev_hash_before.as_bytes());
        let vrf = crypto::shake256(b"QL-VRF-v1", &vrf_in, 32);

        // —— 先挑本机矿签：必须在冷却之外、且此刻（h−1 的地表）仍是矿 ——
        let words = crate::world::terrain::seed_words(GENESIS_HASH_HEX, &ledger.vrfs);
        let can_claim = ledger.last_claim_height == 0
            || height_next >= ledger.last_claim_height + MINE_COOLDOWN_BLOCKS;
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
            match Self::apply_wire(&mut ledger, &wire, height_next, ts, &miner, BLOCK_V) {
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
            match Self::apply_wire(&mut ledger, &wire, height_next, ts, &miner, BLOCK_V) {
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

        // —— 排放：有矿签才发，且把 50 个块的量一次结清（减半 + 硬顶同一口径）——
        let has_claim = included
            .iter()
            .any(|t| claim_cell_of_attestation(t).is_some());
        let reward = if has_claim {
            claim_reward(height_next, ledger.total_minted)
        } else {
            0
        };
        ledger.total_minted += reward;
        *ledger.balances.entry(miner.clone()).or_insert(0) += reward + fee_pool;
        ledger.vrfs.push(hex::encode(&vrf));

        // —— 块哈希绑定块内全部交易与版本 ——
        let root = tx_root_of(&digests);
        let block_hash = block_hash_of_ver(
            BLOCK_V,
            &prev_hash_before,
            height_next,
            ledger.total_minted,
            &vrf,
            &root,
        );

        ledger.height = height_next;
        ledger.prev_hash_hex = hex::encode(&block_hash);

        // —— 原子持久化 + 追加可审计块头 ——
        if let Ok(bytes) = serde_json::to_vec(&*ledger) {
            atomic_write(&self.ledger_path, &bytes);
        }
        let header = BlockHeader {
            version: BLOCK_V,
            height: height_next,
            ts_unix: ts,
            prev_hash_hex: prev_hash_before,
            block_hash_hex: hex::encode(&block_hash),
            vrf_hex: hex::encode(&vrf),
            minted_after: ledger.total_minted,
            reward_total: reward + fee_pool,
            miner: miner.clone(),
            txs: included,
        };
        drop(ledger);
        append_block_line(&self.blocks_path, &header, &wires);

        let balance = self.balance_of(&miner);
        Some((
            BlockEvent {
                height: height_next,
                block_hash_hex: hex::encode(&block_hash[..16]),
                vrf_hex: hex::encode(&vrf[..12]),
                reward: reward + fee_pool,
                balance,
                total_minted: self.total_minted(),
                ts_unix: header.ts_unix,
                txs_count: digests.len(),
            },
            header.clone(),
            wires,
        ))
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
        if e.actions.is_empty() {
            return Err("交易不包含任何转账".into());
        }
        let claim = claim_cell_of(e);
        if claim.is_some() && version == BLOCK_V_LEGACY {
            return Err("历史版本块不允许矿签".into());
        }
        if claim.is_none() && e.actions.iter().any(|a| a.amount == 0) {
            return Err("零金额交易只允许矿签".into());
        }
        if e.expiry <= height {
            return Err("交易已过期".into());
        }
        let nonce_now = l.nonces.get(&from).copied().unwrap_or(0);
        if e.nonce != nonce_now {
            return Err(format!("nonce 断档（期望 {nonce_now}，实得 {}）", e.nonce));
        }
        let fee = if claim.is_some() { 0 } else { FEE };
        let need: u64 = e.actions.iter().map(|a| a.amount).sum::<u64>() + fee;
        if let Some((x, y, layer)) = claim {
            if from != miner {
                return Err("矿签必须由出块者本人提交（奖励不能被代领）".into());
            }
            Self::claim_allowed(l, height, x, y, layer)?;
            l.last_claim_height = height;
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

    /// 矿签的额外三问：冷却到了吗、在 `h−1` 的矿脉里吗、落在贴地层吗
    fn claim_allowed(
        l: &Ledger,
        height: u64,
        x: u16,
        y: u16,
        layer: u16,
    ) -> Result<(), String> {
        if l.last_claim_height != 0 && height < l.last_claim_height + MINE_COOLDOWN_BLOCKS {
            return Err(format!(
                "冷却未到：上次发行在块高 {}，本回合最早 {}",
                l.last_claim_height,
                l.last_claim_height + MINE_COOLDOWN_BLOCKS
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
    /// - v1：最多一张矿签，必须由出块者本人提交、金额/手续费为 0，且冷却已到；
    ///   没有矿签 → 本块发行量为 0（币不靠计时器印）。
    fn header_emission(
        bh: &BlockHeader,
        minted_before: u64,
        last_claim_before: u64,
    ) -> Option<(u64, u64)> {
        match bh.version {
            BLOCK_V_LEGACY => {
                if bh
                    .txs
                    .iter()
                    .any(|t| claim_cell_of_attestation(t).is_some())
                {
                    return None;
                }
                Some((
                    reward_at(bh.height).min(HARD_CAP.saturating_sub(minted_before)),
                    last_claim_before,
                ))
            }
            BLOCK_V => {
                let claims: Vec<&TxAttestation> = bh
                    .txs
                    .iter()
                    .filter(|t| claim_cell_of_attestation(t).is_some())
                    .collect();
                if claims.len() > 1 {
                    return None; // 每块最多一张矿签
                }
                match claims.first() {
                    Some(c) => {
                        if c.from != bh.miner || c.amount != 0 || c.fee != 0 {
                            return None;
                        }
                        if last_claim_before != 0
                            && bh.height < last_claim_before + MINE_COOLDOWN_BLOCKS
                        {
                            return None;
                        }
                        Some((claim_reward(bh.height, minted_before), bh.height))
                    }
                    None => Some((0, last_claim_before)),
                }
            }
            _ => None,
        }
    }

    /// 头部序列校验（接受远端块 / 追账快照共用）：
    /// 前向哈希连续 + 逐块重算 tx_root 与块哈希 + 版本化排放口径一致
    fn verify_header_slice(headers: &[BlockHeader]) -> bool {
        let mut expect_prev = genesis_hash_hex();
        let mut minted_running: u64 = 0;
        let mut last_claim: u64 = 0;
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
            if bh.version != BLOCK_V_LEGACY && bh.version != BLOCK_V {
                return false;
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
            let expect = block_hash_of_ver(
                bh.version,
                &bh.prev_hash_hex,
                bh.height,
                bh.minted_after,
                &vrf,
                &root,
            );
            if hex::encode(expect) != bh.block_hash_hex {
                return false;
            }
            let Some((reward, next_claim)) = Self::header_emission(bh, minted_running, last_claim)
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
            minted_running = bh.minted_after;
            last_claim = next_claim;
            expect_prev = bh.block_hash_hex.clone();
        }
        true
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
            if !bl.wires.is_empty() {
                return Err("创世块不应携带交易原件 → 快照拒收".into());
            }
            l.height = 0;
            l.prev_hash_hex = h.block_hash_hex.clone();
            return Ok(());
        }
        if h.version != BLOCK_V_LEGACY && h.version != BLOCK_V {
            return Err(format!("块 #{} 版本号未知 → 快照拒收", h.height));
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
        if l.records.len() > 500 {
            let cut = l.records.len() - 500;
            l.records.drain(0..cut);
        }
        // 排放口径与出块时一致：v0 无条件发行；v1 只有收下矿签才发行
        let reward = match h.version {
            BLOCK_V_LEGACY => reward_at(h.height).min(HARD_CAP.saturating_sub(l.total_minted)),
            _ => {
                if rebuilt.iter().any(|t| claim_cell_of_attestation(t).is_some()) {
                    claim_reward(h.height, l.total_minted)
                } else {
                    0
                }
            }
        };
        l.total_minted += reward;
        *l.balances.entry(h.miner.clone()).or_insert(0) += reward + fee_sum;
        l.vrfs.push(h.vrf_hex.clone());
        l.height = h.height;
        l.prev_hash_hex = h.block_hash_hex.clone();
        Ok(())
    }

    /// 联邦交易入池：对端送来的是**完整原件**（信封 ‖ 签名 ‖ 公钥 ‖ 地址盐），
    /// 这里独立复验（验签、地址绑定、nonce、可支配余额、过期、池容量）。
    /// 验证不再依赖注册表——「对方还没广播注册」从此不再是丢弃交易的理由。
    pub fn accept_remote_tx(&self, wire: BlockWire) -> Result<[u8; 32], String> {
        if *self.corrupted.lock().unwrap() {
            return Err("账本完整性校验未通过，拒绝入网交易".into());
        }
        if wire.envelope.actions.is_empty() {
            return Err("交易不包含任何转账".into());
        }
        let claim = claim_cell_of(&wire.envelope);
        if claim.is_none()
            && wire
                .envelope
                .actions
                .iter()
                .any(|a| a.amount == 0)
        {
            return Err("转账金额必须大于 0".into());
        }
        let tx_fee = if claim.is_some() { 0 } else { FEE };

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
        if header.version != BLOCK_V_LEGACY && header.version != BLOCK_V {
            return Err("块版本未知 → 整块拒绝".into());
        }

        // —— staging 复本上逐笔重验+重放（任何一笔不过 → 整块拒绝）——
        // 与本地出块共用 `apply_wire`：矿签的坐标、冷却、出块者身份都在同一处判。
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
                    claim_reward(height_next, staged.total_minted)
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
        let expect = block_hash_of_ver(
            header.version,
            &header.prev_hash_hex,
            header.height,
            header.minted_after,
            &vrf_bytes,
            &root,
        );
        if hex::encode(expect) != header.block_hash_hex {
            return Err("块哈希重算不符 → 整块拒绝".into());
        }

        // —— 一次成型入账 ——
        staged.height = height_next;
        staged.prev_hash_hex = header.block_hash_hex.clone();
        *ledger = staged;

        if let Ok(bytes) = serde_json::to_vec(&*ledger) {
            atomic_write(&self.ledger_path, &bytes);
        }
        drop(ledger);
        append_block_line(&self.blocks_path, header, &wires);

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
            vrfs: vec![],
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
fn append_block_line(path: &PathBuf, h: &BlockHeader, wires: &[BlockWire]) {
    use std::io::Write;
    let line = BlockLine {
        header: h.clone(),
        wires: wires.to_vec(),
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

    /// 在本节点当前视图里找一格此刻可挖的矿（测试专用；真实客户端走 ore::scan）
    fn find_ore(chain: &SimChain) -> (u16, u16, u16) {
        let vrfs = chain.ledger.lock().unwrap().vrfs.clone();
        let words = crate::world::terrain::seed_words(GENESIS_HASH_HEX, &vrfs);
        let h = chain.height();
        for y in 0..crate::world::WORLD_H as u32 {
            for x in 0..crate::world::WORLD_W as u32 {
                if crate::world::ore::is_ore(x, y, &words, h) {
                    return (
                        x as u16,
                        y as u16,
                        crate::world::ore::ore_layer(x, y, &words),
                    );
                }
            }
        }
        panic!("创世纪元格 + 配额下竟无矿可挖");
    }

    /// 出第一块：提交一张真矿签，让出块者拿到发行（v1 只有收下矿签才发币）
    fn mine_first_block_with_claim(chain: &SimChain, miner: &crate::keystore::WalletCore) {
        let (x, y, layer) = find_ore(chain);
        chain
            .submit_claim(miner, x, y, layer)
            .expect("矿签应入池");
        chain.mine_block(miner).expect("第 1 块应出块");
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
            })
            .unwrap(),
            serde_json::to_string(&BlockLine {
                header: b2,
                wires: vec![wire],
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
                if let Some((event, _, _)) = chain.mine_block(&core) {
                    let _ = app.emit("chain_block", &event);
                }
                next_block_unix += BLOCK_INTERVAL_SECS;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
}
