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
pub fn genesis_hash() -> [u8; 32] {
    let d = crypto::shake256(b"QL-GENESIS-V1", GENESIS_MANIFESTO.as_bytes(), 32);
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&d);
    let root = tx_root_of(&[digest]);
    let zero_vrf = [0u8; 32];
    block_hash_of(GENESIS_PREV_HEX, 0, 0, &zero_vrf, &root)
}

pub fn genesis_hash_hex() -> String {
    hex::encode(genesis_hash())
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
        }
    }
}

/// 区块头（blocks.jsonl 每行一条，含块内全部交易，供哈希链重放校验）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
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

/// 交易池条目
pub struct PendingTx {
    pub envelope: TxEnvelope,
    pub sig: Vec<u8>,
    pub digest: [u8; 32],
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

pub struct SimChain {
    pub ledger: Arc<Mutex<Ledger>>,
    mempool: Arc<Mutex<Vec<PendingTx>>>,
    /// 本机最近签发的交易原件（孤儿块交易回灌用，环形 64 条）
    sent_cache: Arc<Mutex<VecDeque<([u8; 32], TxEnvelope, Vec<u8>)>>>,
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
fn atomic_write(path: &PathBuf, bytes: &[u8]) {
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

        if !chain.validate_blocks_jsonl() {
            // regtest 演示口径：旧格式（无 miner/nonce 字段的 v1 块）或损坏文件 →
            // 归档为 .bak-epoch 后从新创世继续；数据不丢，仅链重置。
            eprintln!(
                "[QLight] 提醒：blocks.jsonl 哈希链校验未通过（可能为旧格式或文件被改动），已归档并重启演示链"
            );
            chain.archive_and_reset();
        }
        Arc::new(chain)
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
        let mut expect_prev = genesis_hash_hex();
        // 重放时的 minted_before 用于复刻硬顶减发口径（与打包时完全一致）
        let mut minted_running: u64 = 0;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let bh: BlockHeader = match serde_json::from_str(line) {
                Ok(b) => b,
                Err(_) => return false,
            };
            // 1) 前向哈希连续
            if bh.prev_hash_hex != expect_prev {
                return false;
            }
            // 2) 重算 tx_root 与块哈希
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
            let expect = block_hash_of(&bh.prev_hash_hex, bh.height, bh.minted_after, &vrf, &root);
            if hex::encode(expect) != bh.block_hash_hex {
                return false;
            }
            // 3) 奖励口径：reward_total == min(基准排放, HARD_CAP − 此前已排放) + Σ块内手续费
            let base = reward_at(bh.height);
            let reward = base.min(HARD_CAP.saturating_sub(minted_running));
            let fee_sum: u64 = bh.txs.iter().map(|t| t.fee).sum();
            if bh.reward_total != reward + fee_sum {
                return false;
            }
            minted_running = bh.minted_after;
            expect_prev = bh.block_hash_hex;
        }
        // 尾块哈希必须等于账本当前 prev_hash
        let ledger = self.ledger.lock().unwrap();
        ledger.prev_hash_hex == expect_prev
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
        for a in &envelope.actions {
            if a.amount == 0 {
                return Err("转账金额必须大于 0".into());
            }
        }

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
                .map(|p| p.envelope.actions.iter().map(|a| a.amount).sum::<u64>() + FEE)
                .sum();
            let need: u64 = envelope.actions.iter().map(|a| a.amount).sum::<u64>() + FEE;
            let bal = ledger.balances.get(&from).copied().unwrap_or(0);
            let spendable = bal.saturating_sub(projected_spend);
            if spendable < need {
                return Err(format!(
                    "余额不足：可支配 {}（已扣除未入块支出），本次需要 {}（含手续费 0.001 QLX），当前余额 {}",
                    spendable, need, bal
                ));
            }
            if mp.len() >= MEMPOOL_CAP {
                return Err("交易池已满，等待下一块打包".into());
            }
        }

        self.mempool.lock().unwrap().push(PendingTx {
            envelope: envelope.clone(),
            sig: sig.clone(),
            digest,
        });
        // 进签发缓存（分叉切换时孤儿交易回灌用；只缓存本机签发的交易）
        {
            let mut cache = self.sent_cache.lock().unwrap();
            cache.push_back((digest, envelope, sig));
            while cache.len() > 64 {
                cache.pop_front();
            }
        }
        Ok(digest)
    }

    /// 出块（由 net 网络循环每 2s 调用）：池内逐笔真验签 → 状态转移 → 块哈希绑定交易 → 原子持久化。
    /// 返回 (块事件, 块头, 块内交易原件) —— 块头+交易原件供联邦网络广播给对端复验。
    pub fn mine_block(
        &self,
        core: &crate::keystore::WalletCore,
    ) -> Option<(BlockEvent, BlockHeader, Vec<(TxEnvelope, Vec<u8>)>)> {
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

        // —— 打包交易池（FIFO）——
        let taken: Vec<PendingTx> = {
            let mut mp = self.mempool.lock().unwrap();
            let take = mp.len().min(TXS_PER_BLOCK);
            mp.drain(0..take).collect()
        };

        let mut included: Vec<TxAttestation> = Vec::with_capacity(taken.len());
        let mut digests: Vec<[u8; 32]> = Vec::with_capacity(taken.len());
        let mut wires: Vec<(TxEnvelope, Vec<u8>)> = Vec::with_capacity(taken.len());
        let mut fee_pool: u64 = 0;

        for p in taken {
            // 打包时再真验签一次
            if !crypto::verify(&core.verifying_key, &p.digest, &p.sig) {
                continue;
            }
            // 过期作废
            if p.envelope.expiry <= ledger.height {
                continue;
            }
            let from = crate::crypto::to_bech32_ql(&p.envelope.from_addr);
            let need: u64 = p.envelope.actions.iter().map(|a| a.amount).sum::<u64>() + FEE;
            let nonce_now = ledger.nonces.get(&from).copied().unwrap_or(0);
            if p.envelope.nonce != nonce_now {
                continue; // nonce 不连续（重复提交）作废
            }
            let bal = ledger.balances.get(&from).copied().unwrap_or(0);
            if bal < need {
                continue; // 余额不足作废
            }

            // —— 状态转移 ——
            *ledger.balances.get_mut(&from).unwrap() -= need;
            for a in &p.envelope.actions {
                let to = crate::crypto::to_bech32_ql(&a.to);
                *ledger.balances.entry(to.clone()).or_insert(0) += a.amount;
            }
            ledger.nonces.insert(from.clone(), p.envelope.nonce + 1);
            fee_pool += FEE;

            // 块内交易原件收进 wires（网络广播给对端复验）
            wires.push((p.envelope.clone(), p.sig.clone()));

            let to_first = crate::crypto::to_bech32_ql(&p.envelope.actions[0].to);
            let record = TxRecord {
                hash_hex: hex::encode(p.digest),
                height: height_next,
                ts_unix: now_unix(),
                from,
                to: to_first,
                amount: p.envelope.actions[0].amount,
                fee: FEE,
            };
            included.push(TxAttestation {
                digest_hex: hex::encode(p.digest),
                from: record.from.clone(),
                to: record.to.clone(),
                amount: record.amount,
                fee: FEE,
                nonce: p.envelope.nonce,
            });
            digests.push(p.digest);
            ledger.records.push(record);
        }
        if ledger.records.len() > 500 {
            let cut = ledger.records.len() - 500;
            ledger.records.drain(0..cut);
        }

        // —— 排放（减半 + 硬顶）——
        let mut reward = reward_at(height_next);
        let remaining = HARD_CAP - ledger.total_minted;
        if reward > remaining {
            reward = remaining;
        }
        ledger.total_minted += reward;

        let miner = core.address.clone();
        *ledger.balances.entry(miner.clone()).or_insert(0) += reward + fee_pool;

        // —— 块哈希绑定块内全部交易 ——
        let root = tx_root_of(&digests);
        let block_hash = block_hash_of(
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
            height: height_next,
            ts_unix: now_unix(),
            prev_hash_hex: prev_hash_before,
            block_hash_hex: hex::encode(&block_hash),
            vrf_hex: hex::encode(&vrf),
            minted_after: ledger.total_minted,
            reward_total: reward + fee_pool,
            miner: miner.clone(),
            txs: included,
        };
        drop(ledger);
        append_block_header(&self.blocks_path, &header);

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

    /// 头部序列校验（接受远端块 / 追账快照共用）：
    /// 前向哈希连续 + 逐块重算 tx_root 与块哈希 + 排放/手续费口径一致
    fn verify_header_slice(headers: &[BlockHeader]) -> bool {
        let mut expect_prev = genesis_hash_hex();
        let mut minted_running: u64 = 0;
        for bh in headers {
            if bh.miner.is_empty() {
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
            let expect = block_hash_of(&bh.prev_hash_hex, bh.height, bh.minted_after, &vrf, &root);
            if hex::encode(expect) != bh.block_hash_hex {
                return false;
            }
            let fee_sum: u64 = bh.txs.iter().map(|t| t.fee).sum();
            let reward = reward_at(bh.height).min(HARD_CAP.saturating_sub(minted_running));
            if bh.reward_total != reward + fee_sum {
                return false;
            }
            minted_running = bh.minted_after;
            expect_prev = bh.block_hash_hex.clone();
        }
        true
    }

    /// 共享状态转移：按块头凭据推进 balances/nonce/records，封顶排放收归出块者。
    /// （本地打包 / 远端块采用 / 追账重建 三个口径完全一致）
    fn apply_header_to_ledger(l: &mut Ledger, h: &BlockHeader) -> Result<(), String> {
        let fee_pool: u64 = h.txs.iter().map(|t| t.fee).sum();
        for t in &h.txs {
            let bal = l.balances.get(&t.from).copied().unwrap_or(0);
            let need = t.amount.saturating_add(t.fee);
            if bal < need {
                return Err(format!(
                    "块 #{} 状态重建失败：{} 余额不足（{} < {}）",
                    h.height, t.from, bal, need
                ));
            }
            *l.balances.get_mut(&t.from).unwrap() -= need;
            *l.balances.entry(t.to.clone()).or_insert(0) += t.amount;
            l.nonces.insert(t.from.clone(), t.nonce + 1);
            l.records.push(TxRecord {
                hash_hex: t.digest_hex.clone(),
                height: h.height,
                ts_unix: h.ts_unix,
                from: t.from.clone(),
                to: t.to.clone(),
                amount: t.amount,
                fee: t.fee,
            });
        }
        if l.records.len() > 500 {
            let cut = l.records.len() - 500;
            l.records.drain(0..cut);
        }
        let reward = reward_at(h.height).min(HARD_CAP.saturating_sub(l.total_minted));
        l.total_minted += reward;
        *l.balances.entry(h.miner.clone()).or_insert(0) += reward + fee_pool;
        l.height = h.height;
        l.prev_hash_hex = h.block_hash_hex.clone();
        Ok(())
    }

    /// 联邦交易入池：net 层已完成注册表解析（携带公钥），此处重走完整轻单校验
    /// （真验签 / nonce / 可支配余额 / 过期 / 池容量），通过后与其他交易统一公平打包。
    pub fn accept_remote_tx(
        &self,
        envelope: TxEnvelope,
        sig: Vec<u8>,
        pk_bytes: &[u8],
    ) -> Result<[u8; 32], String> {
        if *self.corrupted.lock().unwrap() {
            return Err("账本完整性校验未通过，拒绝入网交易".into());
        }
        if envelope.actions.is_empty() {
            return Err("交易不包含任何转账".into());
        }
        for a in &envelope.actions {
            if a.amount == 0 {
                return Err("转账金额必须大于 0".into());
            }
        }

        let digest = tx_digest(&envelope);
        let vk = crate::crypto::decode_pk(pk_bytes).ok_or("公钥无法解析（1952 字节要求）")?;
        if !crypto::verify(&vk, &digest, &sig) {
            return Err("签名校验失败（ML-DSA-65 verify）".into());
        }

        let from = crate::crypto::to_bech32_ql(&envelope.from_addr);
        let from_hex = hex::encode(&envelope.from_addr);
        {
            let ledger = self.ledger.lock().unwrap();
            let mp = self.mempool.lock().unwrap();
            let queued: u64 = mp
                .iter()
                .filter(|p| hex::encode(&p.envelope.from_addr) == from_hex)
                .count() as u64;
            let expect = ledger.nonces.get(&from).copied().unwrap_or(0) + queued;
            if envelope.nonce != expect {
                return Err(format!("nonce 不匹配：期望 {expect}，收到 {}", envelope.nonce));
            }
            if envelope.expiry <= ledger.height {
                return Err("交易已过期（expiry 块高）".into());
            }
            let projected_spend: u64 = mp
                .iter()
                .filter(|p| hex::encode(&p.envelope.from_addr) == from_hex)
                .map(|p| p.envelope.actions.iter().map(|a| a.amount).sum::<u64>() + FEE)
                .sum();
            let need: u64 = envelope.actions.iter().map(|a| a.amount).sum::<u64>() + FEE;
            let bal = ledger.balances.get(&from).copied().unwrap_or(0);
            if bal.saturating_sub(projected_spend) < need {
                return Err("余额不足（已扣除该地址未入块支出）".into());
            }
            if mp.len() >= MEMPOOL_CAP {
                return Err("交易池已满".into());
            }
        }

        self.mempool.lock().unwrap().push(PendingTx {
            envelope,
            sig,
            digest,
        });
        Ok(digest)
    }

    /// 接受联邦对端出的块：整块真验（逐笔重验签重放 + tx_root/块哈希重算 + 排放口径），
    /// staging 全过且与块头完全一致才一次成型入账（失败不落任何状态）。
    pub fn apply_remote_block(
        &self,
        header: &BlockHeader,
        wires: Vec<(TxEnvelope, Vec<u8>, Vec<u8>)>,
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

        // —— staging 复本上逐笔重验+重放（任何一笔不过 → 整块拒绝）——
        let mut staged = ledger.clone();
        let mut included: Vec<TxAttestation> = Vec::with_capacity(wires.len());
        let mut digests: Vec<[u8; 32]> = Vec::with_capacity(wires.len());
        let mut fee_pool: u64 = 0;

        for (envelope, sig, pk) in &wires {
            let digest = tx_digest(envelope);
            let vk = crate::crypto::decode_pk(pk).ok_or("公钥无法解析")?;
            if !crypto::verify(&vk, &digest, sig) {
                return Err("块内交易验签失败 → 整块拒绝（验签一次都不能虚）".into());
            }
            let from = crate::crypto::to_bech32_ql(&envelope.from_addr);
            if envelope.actions.is_empty() {
                return Err("块内存在无转账交易 → 整块拒绝".into());
            }
            if envelope.actions.iter().any(|a| a.amount == 0) {
                return Err("块内存在零金额交易 → 整块拒绝".into());
            }
            if envelope.expiry <= ledger.height {
                return Err("块内存在过期交易 → 整块拒绝".into());
            }
            let nonce_now = staged.nonces.get(&from).copied().unwrap_or(0);
            if envelope.nonce != nonce_now {
                return Err(format!(
                    "块内 nonce 断档 {}≠{} → 整块拒绝",
                    envelope.nonce, nonce_now
                ));
            }
            let bal = staged.balances.get(&from).copied().unwrap_or(0);
            let need: u64 = envelope.actions.iter().map(|a| a.amount).sum::<u64>() + FEE;
            if bal < need {
                return Err("块内余额不足 → 整块拒绝".into());
            }

            // staging 状态转移
            *staged.balances.get_mut(&from).unwrap() -= need;
            for a in &envelope.actions {
                let to = crate::crypto::to_bech32_ql(&a.to);
                *staged.balances.entry(to.clone()).or_insert(0) += a.amount;
            }
            staged.nonces.insert(from.clone(), envelope.nonce + 1);
            fee_pool += FEE;

            let record = TxRecord {
                hash_hex: hex::encode(digest),
                height: height_next,
                ts_unix: header.ts_unix,
                from,
                to: crate::crypto::to_bech32_ql(&envelope.actions[0].to),
                amount: envelope.actions[0].amount,
                fee: FEE,
            };
            included.push(TxAttestation {
                digest_hex: hex::encode(digest),
                from: record.from.clone(),
                to: record.to.clone(),
                amount: record.amount,
                fee: FEE,
                nonce: envelope.nonce,
            });
            digests.push(digest);
            staged.records.push(record);
        }
        if staged.records.len() > 500 {
            let cut = staged.records.len() - 500;
            staged.records.drain(0..cut);
        }

        // —— 与块头逐一对照（凭据、序、量）——
        if included != header.txs {
            return Err("重放凭据与块头不一致 → 整块拒绝".into());
        }

        // —— 排放（封顶口径与本地一致）——
        let mut reward = reward_at(height_next);
        reward = reward.min(HARD_CAP.saturating_sub(staged.total_minted));
        staged.total_minted += reward;
        *staged.balances.entry(header.miner.clone()).or_insert(0) += reward + fee_pool;

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
        let expect = block_hash_of(
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
        append_block_header(&self.blocks_path, header);

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

    /// 追账快照：对端链更长时整链采用（逐块验证靠 verify_header_slice + 状态重建靠 apply_header_to_ledger）
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
        let mut headers = Vec::new();
        for line in lines.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let h: BlockHeader = serde_json::from_str(line).map_err(|e| format!("快照解析失败: {e}"))?;
            headers.push(h);
        }
        if headers.is_empty() {
            return Err("空快照".into());
        }
        if !Self::verify_header_slice(&headers) {
            return Err("快照哈希链校验失败 → 拒绝采用".into());
        }
        let mut staged = Ledger {
            v: LEDGER_V,
            height: 0,
            prev_hash_hex: genesis_hash_hex(),
            total_minted: 0,
            started_unix: headers[0].ts_unix,
            balances: HashMap::new(),
            nonces: HashMap::new(),
            records: vec![],
        };
        for h in &headers {
            Self::apply_header_to_ledger(&mut staged, h)?;
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
            for (d, env, sig) in cache.iter() {
                if missing.iter().any(|m| m == &hex::encode(d)) && self.requeue_orphan(env, sig, *d) {
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

    /// 孤儿交易回灌（签名在签发时已验过；此处只按新链口径查 nonce/过期/余额/池容量）
    fn requeue_orphan(&self, envelope: &TxEnvelope, sig: &Vec<u8>, digest: [u8; 32]) -> bool {
        let ledger = self.ledger.lock().unwrap();
        let mut mp = self.mempool.lock().unwrap();
        if mp.len() >= MEMPOOL_CAP {
            return false;
        }
        let from = crate::crypto::to_bech32_ql(&envelope.from_addr);
        let from_hex = hex::encode(&envelope.from_addr);
        let queued: u64 = mp
            .iter()
            .filter(|p| hex::encode(&p.envelope.from_addr) == from_hex)
            .count() as u64;
        let expect = ledger.nonces.get(&from).copied().unwrap_or(0) + queued;
        if envelope.nonce != expect {
            return false; // nonce 已被新链消耗 → 交易作废（正确行为）
        }
        if envelope.expiry <= ledger.height {
            return false;
        }
        let projected_spend: u64 = mp
            .iter()
            .filter(|p| hex::encode(&p.envelope.from_addr) == from_hex)
            .map(|p| p.envelope.actions.iter().map(|a| a.amount).sum::<u64>() + FEE)
            .sum();
        let need: u64 = envelope.actions.iter().map(|a| a.amount).sum::<u64>() + FEE;
        let bal = ledger.balances.get(&from).copied().unwrap_or(0);
        if bal.saturating_sub(projected_spend) < need {
            return false;
        }
        mp.push(PendingTx {
            envelope: envelope.clone(),
            sig: sig.clone(),
            digest,
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
    pub fn mempool_wires_pub(&self) -> Vec<(TxEnvelope, Vec<u8>)> {
        self.mempool
            .lock()
            .unwrap()
            .iter()
            .map(|p| (p.envelope.clone(), p.sig.clone()))
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

fn append_block_header(path: &PathBuf, h: &BlockHeader) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        if let Ok(line) = serde_json::to_string(h) {
            // 单次 write_all 整行落盘 + fsync：kill -9 不会撕行，掉电也不丢尾块
            let mut buf = line.into_bytes();
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
