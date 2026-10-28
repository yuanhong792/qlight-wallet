//! 光尘账本 `QL-DUST-v1`：世界里唯一的软币。
//!
//! 两层货币的分工（DESIGN §9.17）：
//! - **QLX**：总量 2.1 亿封顶，只能挖矿产出，是硬资产；
//! - **光尘**：无上限，采出来的，只进世界消费，越玩越多越不值钱。
//!
//! 为什么单独一本账：`QL-TX-v1` 的顶层是定长 9 元素 CBOR，多一个字段就会改掉
//! 每一笔历史交易的 digest（`blocks.jsonl` 存的就是它），链当场重放不过去。
//! 所以光尘和实体一样走**独立账本 + 独立前缀**，将来要进块再挂新的 `dust_root`。
//!
//! 可验证性口径与实体账本完全一致，没有任何服务器：
//! - 记录哈希 `SHAKE-256("QL-DUSTREC-H1", canon)`
//! - 签名     `ML-DSA-65(sk, record_hash)`，与转账、实体同一把钥匙
//! - 归属     `owner = bech32(derive_address(pk, salt))`，由记录自带公钥复算
//! - 顺序     每个归属者一条 append-only 哈希链（`seq` + `prev_hash`）
//!
//! 两种操作：
//! - `harvest`：采一格资源，金额由 `(kind, height)` 唯一决定，坐标/种类必须与
//!   `resource::node_at` 一致；同一格同一轮只能被采一次，每人每轮有配额。
//! - `send`：把光尘转给别人。发送者链的顺序就是防双花的全部依据。
//!
//! 余额按 `(height, record_hash)` 全局排序后折叠，跨节点可复算；由于只增不减的
//! 入账不会让一笔已经合法的支出变成非法，历史不会被后来的记录追溯推翻。

use crate::crypto;
use crate::keystore::WalletCore;
use crate::world::resource;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const DUST_RECORD_VERSION: &str = "QL-DUST-v1";
const REC_PREFIX: &[u8] = b"QL-DUSTREC-v1";
const REC_HASH_PREFIX: &[u8] = b"QL-DUSTREC-H1";
const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub mod op {
    pub const HARVEST: &str = "harvest";
    pub const SEND: &str = "send";
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DustRecord {
    pub v: String,
    /// 从 1 开始严格递增（每个归属者各自一条链）
    pub seq: u64,
    pub op: String,
    /// 采集所在轮次；send 恒为 0
    pub epoch: u64,
    pub x: u16,
    pub y: u16,
    /// 被采的资源种类（`resource::NODE_*`）
    pub kind: u8,
    /// 最小单位的光尘数量
    pub amount: u64,
    /// send 的收款地址；harvest 为空
    pub to: String,
    /// 声明块高：决定夜昼产量与全局结算顺序，不得超过链头
    pub height: u64,
    pub ts: u64,
    pub owner: String,
    pub pk_hash_hex: String,
    pub salt_hex: String,
    pub prev_hash_hex: String,
    pub pk_hex: String,
    pub sig_hex: String,
}

fn canon(r: &DustRecord) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(128);
    fn u64b(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_be_bytes());
    }
    fn bytes(out: &mut Vec<u8>, b: &[u8]) {
        out.extend_from_slice(&(b.len() as u16).to_be_bytes());
        out.extend_from_slice(b);
    }
    out.extend_from_slice(REC_PREFIX);
    u64b(&mut out, r.seq);
    bytes(&mut out, r.op.as_bytes());
    u64b(&mut out, r.epoch);
    out.extend_from_slice(&r.x.to_be_bytes());
    out.extend_from_slice(&r.y.to_be_bytes());
    out.push(r.kind);
    u64b(&mut out, r.amount);
    bytes(&mut out, r.to.as_bytes());
    u64b(&mut out, r.height);
    u64b(&mut out, r.ts);
    bytes(&mut out, &hex256(&r.pk_hash_hex));
    bytes(&mut out, &hex::decode(&r.salt_hex).unwrap_or_default());
    bytes(&mut out, &hex256(&r.prev_hash_hex));
    out
}

fn hex256(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    if let Ok(b) = hex::decode(s) {
        let n = b.len().min(32);
        out[..n].copy_from_slice(&b[..n]);
    }
    out
}

pub fn record_hash(r: &DustRecord) -> [u8; 32] {
    let d = crypto::shake256(REC_HASH_PREFIX, &canon(r), 32);
    let mut out = [0u8; 32];
    out.copy_from_slice(&d);
    out
}

/// 光尘「集合指纹」：`(条数, SHAKE-256(域 ‖ 升序 record_hash))`，与落盘顺序无关。
///
/// 与实体账本同一套缺口检测口径：推送会漏，公告指纹让漏的一方当场发现自己漏了。
pub fn set_digest(records: &[DustRecord]) -> (u64, String) {
    let mut hashes: Vec<[u8; 32]> = records.iter().map(record_hash).collect();
    hashes.sort_unstable();
    hashes.dedup();
    let mut buf = Vec::with_capacity(hashes.len() * 32);
    for h in &hashes {
        buf.extend_from_slice(h);
    }
    (
        hashes.len() as u64,
        hex::encode(crypto::shake256(b"QL-DUSTSET-V1", &buf, 32)),
    )
}

/// 重放所需的外部输入：链头高度。
///
/// 世界种子**故意不作为入参**：采集记录只存坐标与种类，真假要靠 `node_at`
/// 按种子重算。种子一旦能由调用方传入，就可能混进随出块漂移的 `chain_words`，
/// 于是手上的记录在下个块被判无效、余额凭空归零。这里统一取
/// [`crate::world::resource_words`]（只由创世块决定，永不变化）。
pub struct DustView {
    pub tip: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct DustState {
    /// 地址 → 光尘余额
    pub balances: BTreeMap<String, u64>,
    /// 本轮已被采走的资源点 `(x, y, epoch)`
    pub harvested: BTreeSet<(u16, u16, u64)>,
    /// 每归属者每轮已采数量
    pub epoch_counts: BTreeMap<(String, u64), u32>,
    /// 累计铸造量（只增，用于展示与对账）
    pub minted_total: u64,
}

impl DustState {
    pub fn balance_of(&self, addr: &str) -> u64 {
        self.balances.get(addr).copied().unwrap_or(0)
    }

    pub fn harvested_count(&self, owner: &str, epoch: u64) -> u32 {
        self.epoch_counts
            .get(&(owner.to_string(), epoch))
            .copied()
            .unwrap_or(0)
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct DustReport {
    pub ok: bool,
    pub total: usize,
    pub accepted: usize,
    pub skipped: usize,
    pub first_bad: u64,
    pub bad_owner: String,
    pub reason: String,
}

#[derive(Default)]
struct OwnerChain {
    accepted: u64,
    prev: [u8; 32],
    last_height: u64,
    broken: bool,
}

/// 整本重放。逐归属者验链验签，再按 `(height, record_hash)` 全局排序结算余额。
pub fn replay(records: &[DustRecord], view: &DustView) -> (DustState, DustReport) {
    let (state, rep, _) = walk(records, view);
    (state, rep)
}

/// 与 `replay` 同一份判定，额外回吐逐条结果（用于落盘前试算：必须知道
/// 到底是"新加的这条"被拒，还是历史里本来就有坏记录）。
fn walk(
    records: &[DustRecord],
    view: &DustView,
) -> (DustState, DustReport, Vec<Result<(), String>>) {
    let words = crate::world::resource_words();
    let mut chains: BTreeMap<String, OwnerChain> = BTreeMap::new();
    let mut results: Vec<Result<(), String>> = vec![Ok(()); records.len()];
    let mut order: Vec<usize> = Vec::new();
    let mut first: Option<(u64, String, String)> = None;
    let mut skipped = 0usize;

    for (i, r) in records.iter().enumerate() {
        let ch = chains.entry(r.pk_hash_hex.clone()).or_default();
        if ch.broken {
            results[i] = Err("上一条记录已断链".into());
            skipped += 1;
            continue;
        }
        match check_chain(r, ch, view) {
            Ok(h) => {
                ch.prev = h;
                ch.accepted += 1;
                ch.last_height = r.height;
                order.push(i);
            }
            Err(why) => {
                ch.broken = true;
                results[i] = Err(why.clone());
                skipped += 1;
                if first.is_none() {
                    first = Some((i as u64 + 1, r.pk_hash_hex.clone(), why));
                }
            }
        }
    }

    // 全局结算顺序必须与文件顺序无关：块高升序，同高按记录哈希。
    order.sort_by(|&a, &b| {
        let ra = &records[a];
        let rb = &records[b];
        ra.height
            .cmp(&rb.height)
            .then_with(|| hex::encode(record_hash(ra)).cmp(&hex::encode(record_hash(rb))))
    });

    let mut state = DustState::default();
    let mut applied = 0usize;
    for i in order {
        match apply_semantics(&mut state, &records[i], &words, view.tip) {
            Ok(()) => applied += 1,
            Err(why) => {
                results[i] = Err(why);
                skipped += 1;
            }
        }
    }

    let (first_bad, bad_owner, reason) = match first {
        Some((row, owner, why)) => (row, owner.chars().take(12).collect::<String>(), why),
        None => (0, String::new(), String::new()),
    };
    (
        state,
        DustReport {
            // 语义层拒绝（余额不足、配额用完）不算坏账本，只有链断才算
            ok: first_bad == 0,
            total: records.len(),
            accepted: applied,
            skipped,
            first_bad,
            bad_owner,
            reason,
        },
        results,
    )
}

/// 某一个归属者光尘链的游标（v5 起随账本持久化，进块即共识状态）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainTip {
    /// 已接上的条数（下一条的 seq 必须是它 + 1）
    pub seq: u64,
    /// 已接上的最后一条记录哈希（第一条必须是全零）
    pub prev_hex: String,
    /// 已接上的最后一条声明块高（同一条链的时间不能倒流）
    pub last_height: u64,
}

impl Default for ChainTip {
    /// 空游标的 prev 是**全零哈希**，不是空串：`check_record` 按 64 位十六进制比对，
    /// 写错一个字符就会把每个新归属者的第一条记录判成断链。
    fn default() -> Self {
        Self {
            seq: 0,
            prev_hex: genesis_prev().to_string(),
            last_height: 0,
        }
    }
}

fn check_chain(r: &DustRecord, ch: &OwnerChain, view: &DustView) -> Result<[u8; 32], String> {
    check_record(
        r,
        &ChainTip {
            seq: ch.accepted,
            prev_hex: hex::encode(ch.prev),
            last_height: ch.last_height,
        },
        view.tip,
    )
}

/// 单条记录的链 + 密码学校验（整本重放与块内应用共用同一份判定）。
///
/// `chain_tip` 是这条记录**被应用时**的链头高度：记录自报的高度不得超过它，
/// 否则就是"还没到那个高度就先把那笔账写了"。
pub fn check_record(
    r: &DustRecord,
    tip: &ChainTip,
    chain_tip: u64,
) -> Result<[u8; 32], String> {
    if r.v != DUST_RECORD_VERSION {
        return Err(format!("未知光尘记录版本 {}", r.v));
    }
    if r.seq != tip.seq + 1 {
        return Err(format!(
            "该归属者的序号断裂：期望 {}，实得 {}",
            tip.seq + 1,
            r.seq
        ));
    }
    if tip.prev_hex != r.prev_hash_hex {
        return Err(format!("哈希链断裂：seq {} 的 prev_hash 与上一条不符", r.seq));
    }
    if r.height > chain_tip {
        return Err(format!(
            "seq {} 声明块高 {} 超过链头 {}",
            r.seq,
            r.height,
            chain_tip
        ));
    }
    if r.seq > 1 && r.height < tip.last_height {
        return Err(format!(
            "seq {} 声明块高 {} 低于本链上一条的 {}（时间不能倒流）",
            r.seq, r.height, tip.last_height
        ));
    }

    let pk = match hex::decode(&r.pk_hex) {
        Ok(b) if b.len() == crypto::PK_LEN => b,
        _ => return Err(format!("seq {} 公钥长度非法", r.seq)),
    };
    let vk = crypto::decode_pk(&pk)
        .ok_or_else(|| format!("seq {} 公钥无法解析为 ML-DSA-65 验证密钥", r.seq))?;
    if hex::encode(crypto::shake256(b"QL-PK-REG-V1", &pk, 32)) != r.pk_hash_hex {
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
    Ok(h)
}

impl ChainTip {
    /// 接上一条记录：游标前移一格
    pub fn advance(&mut self, hash: [u8; 32], height: u64) {
        self.seq += 1;
        self.prev_hex = hex::encode(hash);
        self.last_height = height;
    }
}

/// 语义层单条入口。`now` 是判定这条记录时**链头的块高**：块内应用即出块块高，
/// 本机预览即当前链高。轮次与产出都按它算，绝不按记录自报的 `height` 算——
/// 那是客户端字段：一把新钥匙可以把历史上每个轮次各刷 60 个配额，还能在"声明的
/// 旧轮次"里重采别人本轮已经采走的格。时间只能往前走：采集只对本轮有效。
pub fn apply_record(state: &mut DustState, r: &DustRecord, now: u64) -> Result<(), String> {
    apply_semantics(state, r, &crate::world::resource_words(), now)
}

/// 语义层：这条记录对世界做了什么。失败只丢这一条，不断链。
fn apply_semantics(
    state: &mut DustState,
    r: &DustRecord,
    words: &[u32],
    now: u64,
) -> Result<(), String> {
    match r.op.as_str() {
        op::HARVEST => {
            if r.x as usize >= super::WORLD_W as usize || r.y as usize >= super::WORLD_H as usize {
                return Err("采集坐标越界".into());
            }
            let round = resource::epoch_of(now);
            if r.epoch != round {
                return Err(format!(
                    "采集轮次 {} 与本轮 {} 不符：轮次由进块的块高决定，历史轮次不许回拨",
                    r.epoch, round
                ));
            }
            let want = resource::node_at(r.x as u32, r.y as u32, words)
                .ok_or_else(|| "这一格没有资源".to_string())?;
            if want != r.kind {
                return Err("采集的资源种类与这一格不符".into());
            }
            // 产出同样按"当下"算：自报一个本轮里白天的块高来躲夜间减半，也是回拨
            let reward = resource::reward_at(r.kind, now);
            if reward == 0 || r.amount != reward {
                return Err(format!(
                    "产出 {} 与规则算出的 {} 不符",
                    r.amount, reward
                ));
            }
            let key = (r.owner.clone(), round);
            let used = state.epoch_counts.get(&key).copied().unwrap_or(0);
            if used >= resource::HARVEST_QUOTA {
                return Err(format!(
                    "本轮采集配额已用完（{used}/{}）",
                    resource::HARVEST_QUOTA
                ));
            }
            if state.harvested.contains(&(r.x, r.y, round)) {
                return Err("这一格本轮已经被采过".into());
            }
            state.harvested.insert((r.x, r.y, round));
            *state.epoch_counts.entry(key).or_insert(0) += 1;
            *state.balances.entry(r.owner.clone()).or_insert(0) += r.amount;
            state.minted_total += r.amount;
            Ok(())
        }
        op::SEND => {
            if r.epoch != 0 || r.x != 0 || r.y != 0 || r.kind != 0 {
                return Err("转账记录不应带采集字段".into());
            }
            if r.amount == 0 {
                return Err("转账金额不能为 0".into());
            }
            if r.to == r.owner {
                return Err("不能转给自己".into());
            }
            if crate::simchain::bech32_decode_addr(&r.to).is_err() {
                return Err("收款地址不是合法的 ql1 地址".into());
            }
            let from = state.balances.get(&r.owner).copied().unwrap_or(0);
            if from < r.amount {
                return Err(format!("余额不足：有 {from}，要转 {}", r.amount));
            }
            *state.balances.entry(r.owner.clone()).or_insert(0) = from - r.amount;
            *state.balances.entry(r.to.clone()).or_insert(0) += r.amount;
            Ok(())
        }
        other => Err(format!("未知光尘操作 {other}")),
    }
}

// ---------------------------------------------------------------- 落盘

#[derive(Debug, Default, Clone, Serialize)]
pub struct MergeReport {
    pub accepted: usize,
    pub known: usize,
    pub rejected: usize,
}

pub struct DustLedger {
    path: PathBuf,
    lock: std::sync::Mutex<()>,
    /// 最近一次整本读入跳过的坏行数（与 `EntityLedger` 同一口径、同一实现）
    bad: super::BadLines,
}

impl DustLedger {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: std::sync::Mutex::new(()),
            bad: super::BadLines::default(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Vec<DustRecord> {
        self.load_report().0
    }

    /// 整本读入 + 坏行说明。光尘的集合指纹要进块哈希，所以"少读了几行"绝不能只是
    /// 静默少几行：那会让本机的 `dust_root` 永远对不上任何人，而账面看起来一切正常。
    pub fn load_report(&self) -> (Vec<DustRecord>, super::LoadReport) {
        let (recs, rep) = match std::fs::read_to_string(&self.path) {
            Ok(text) => super::parse_lines::<DustRecord>(&text),
            Err(_) => (Vec::new(), super::LoadReport::default()),
        };
        self.bad.note(&rep, "光尘账本", &self.path);
        (recs, rep)
    }

    /// 最近一次整本读入跳过的坏行数（账本修好后回到 0）
    pub fn bad_lines(&self) -> usize {
        self.bad.count()
    }

    pub fn head_of(records: &[DustRecord], pk_hash_hex: &str) -> (u64, [u8; 32]) {
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

    /// 读—签名—追加必须是一个整体，否则并发写会拿到同一个 seq 撕断链。
    pub fn commit_as(
        &self,
        pk_hash_hex: &str,
        view: &DustView,
        build: impl FnOnce(u64, [u8; 32]) -> DustRecord,
    ) -> Result<DustRecord, String> {
        let _guard = self.lock.lock().map_err(|_| "写锁中毒".to_string())?;
        let recs = self.load();
        let (seq, prev) = Self::head_of(&recs, pk_hash_hex);
        let r = build(seq + 1, prev);
        // 落盘前用与重放完全同一套判据试算：绝不写一条自己都验不过的记录
        let mut trial = recs;
        trial.push(r.clone());
        let (_, _, results) = walk(&trial, view);
        if let Some(Err(why)) = results.last() {
            return Err(why.clone());
        }
        write_line(&self.path, &r)?;
        Ok(r)
    }

    /// 合并邻居中继来的记录：逐条验签验链，只收真正接得上的。
    pub fn merge(&self, incoming: &[DustRecord], view: &DustView) -> Result<MergeReport, String> {
        let _guard = self.lock.lock().map_err(|_| "写锁中毒".to_string())?;
        let mut out = MergeReport::default();
        let mut recs = self.load();
        let mut seen: BTreeSet<String> = recs.iter().map(|r| hex::encode(record_hash(r))).collect();
        for r in incoming {
            let h = hex::encode(record_hash(r));
            if seen.contains(&h) {
                out.known += 1;
                continue;
            }
            let (seq, prev) = Self::head_of(&recs, &r.pk_hash_hex);
            if r.seq != seq + 1 || r.prev_hash_hex != hex::encode(prev) {
                out.rejected += 1;
                continue;
            }
            let mut trial = recs.clone();
            trial.push(r.clone());
            let (_, _, results) = walk(&trial, view);
            if let Some(Err(_)) = results.last() {
                out.rejected += 1;
                continue;
            }
            if write_line(&self.path, r).is_err() {
                out.rejected += 1;
                continue;
            }
            recs.push(r.clone());
            seen.insert(h);
            out.accepted += 1;
        }
        Ok(out)
    }
}

fn write_line(path: &Path, r: &DustRecord) -> Result<(), String> {
    use std::io::{Read, Seek, Write};
    let json = serde_json::to_string(r).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("打开光尘账本失败：{e}"))?;
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
    f.flush().map_err(|e| format!("落盘失败：{e}"))?;
    Ok(())
}

/// 用解锁核心签一条光尘记录
pub fn sign_record(
    core: &WalletCore,
    seq: u64,
    prev: &[u8; 32],
    d: &DustDraft,
) -> DustRecord {
    let pk_hash = crypto::shake256(b"QL-PK-REG-V1", &core.pk_bytes, 32);
    let mut r = DustRecord {
        v: DUST_RECORD_VERSION.into(),
        seq,
        op: d.op.into(),
        epoch: d.epoch,
        x: d.x,
        y: d.y,
        kind: d.kind,
        amount: d.amount,
        to: d.to.into(),
        height: d.height,
        ts: d.ts,
        owner: core.address.clone(),
        pk_hash_hex: hex::encode(pk_hash),
        salt_hex: hex::encode(core.addr_salt),
        prev_hash_hex: hex::encode(prev),
        pk_hex: hex::encode(&core.pk_bytes),
        sig_hex: String::new(),
    };
    let h = record_hash(&r);
    r.sig_hex = hex::encode(crypto::sign(&core.signing_key, &h));
    r
}

pub struct DustDraft<'a> {
    pub op: &'a str,
    pub epoch: u64,
    pub x: u16,
    pub y: u16,
    pub kind: u8,
    pub amount: u64,
    pub to: &'a str,
    pub height: u64,
    pub ts: u64,
}

pub fn genesis_prev() -> &'static str {
    ZERO_HASH
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simchain::GENESIS_HASH_HEX;

    fn core() -> WalletCore {
        crate::keystore::open_core_from_seed(&[9u8; 32], &[3u8; 8]).unwrap()
    }

    fn words() -> Vec<u32> {
        crate::world::terrain::seed_words(GENESIS_HASH_HEX, &[])
    }

    /// 找一格白天能采的资源（测试用，扫前 64×64）
    fn find_node(w: &[u32], height: u64) -> (u16, u16, u8) {
        for y in 0..64u32 {
            for x in 0..64u32 {
                if let Some(k) = resource::node_at(x, y, w) {
                    if resource::reward_at(k, height) > 0 {
                        return (x as u16, y as u16, k);
                    }
                }
            }
        }
        panic!("64×64 里没有资源点");
    }

    fn harvest(core: &WalletCore, seq: u64, prev: [u8; 32], x: u16, y: u16, k: u8, h: u64) -> DustRecord {
        sign_record(
            core,
            seq,
            &prev,
            &DustDraft {
                op: op::HARVEST,
                epoch: resource::epoch_of(h),
                x,
                y,
                kind: k,
                amount: resource::reward_at(k, h),
                to: "",
                height: h,
                ts: h,
            },
        )
    }

    fn view(_w: &[u32], tip: u64) -> DustView {
        DustView { tip }
    }

    #[test]
    fn harvest_credits_balance_and_marks_the_node() {
        let c = core();
        let w = words();
        let (x, y, k) = find_node(&w, 10);
        let r = harvest(&c, 1, [0u8; 32], x, y, k, 10);
        let (st, rep) = replay(&[r], &view(&w, 10));
        assert!(rep.ok, "{}", rep.reason);
        assert_eq!(rep.accepted, 1);
        assert_eq!(st.balance_of(&c.address), resource::reward_at(k, 10));
        assert!(st.harvested.contains(&(x, y, 0)));
    }

    fn tmp_dust(tag: &str) -> DustLedger {
        let mut p = std::env::temp_dir();
        p.push(format!("qlight-dust-{}-{}.jsonl", tag, std::process::id()));
        let _ = std::fs::remove_file(&p);
        DustLedger::new(p)
    }

    /// 64×64 里的第二个资源点（同一轮里采两格要用不同坐标）
    fn second_node(w: &[u32], skip: (u16, u16), height: u64) -> (u16, u16, u8) {
        for y in 0..64u32 {
            for x in 0..64u32 {
                let at = (x as u16, y as u16);
                if at == skip {
                    continue;
                }
                if let Some(k) = resource::node_at(x, y, w) {
                    if resource::reward_at(k, height) > 0 {
                        return (at.0, at.1, k);
                    }
                }
            }
        }
        panic!("64×64 里找不到第二个资源点");
    }

    /// 坏行只丢它自己并计数：光尘的集合指纹要进块哈希，"静默少读几行"等于
    /// 悄悄改掉本机承诺的到底是哪一本账。
    #[test]
    fn a_corrupt_dust_line_is_counted_not_swallowed() {
        let c = core();
        let w = words();
        let led = tmp_dust("corrupt");
        let (x1, y1, k1) = find_node(&w, 10);
        let r1 = harvest(&c, 1, [0u8; 32], x1, y1, k1, 10);
        let (x2, y2, k2) = second_node(&w, (x1, y1), 11);
        let r2 = harvest(&c, 2, record_hash(&r1), x2, y2, k2, 11);
        let text = format!(
            "{}\n{{\"v\":\"QL-DUST-v1\",\"seq\":\n{}\n",
            serde_json::to_string(&r1).unwrap(),
            serde_json::to_string(&r2).unwrap()
        );
        std::fs::write(led.path(), text).unwrap();

        let (recs, rep) = led.load_report();
        assert_eq!(recs.len(), 2, "坏行之后的记录不许跟着消失");
        assert_eq!(rep.bad_lines, 1);
        assert_eq!(rep.first_line, 2);
        assert_eq!(led.bad_lines(), 1, "快照与 UI 读的是这个数");

        // 反向对照：坏行夹在中间，两条记录仍各自接得上自己的链
        let (st, st_rep) = replay(&recs, &DustView { tip: 11 });
        assert!(st_rep.ok, "一行坏了不该判断整条链：{}", st_rep.reason);
        assert_eq!(
            st.balance_of(&c.address),
            resource::reward_at(k1, 10) + resource::reward_at(k2, 11)
        );

        // 修好后计数归零，告警不会赖在界面上
        std::fs::write(
            led.path(),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&r1).unwrap(),
                serde_json::to_string(&r2).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(led.load().len(), 2, "整本重读：两条都在");
        assert_eq!(led.bad_lines(), 0);
        let _ = std::fs::remove_file(led.path());
    }

    #[test]
    fn same_node_twice_in_one_epoch_is_refused() {
        let a = core();
        let b = crate::keystore::open_core_from_seed(&[8u8; 32], &[4u8; 8]).unwrap();
        let w = words();
        let (x, y, k) = find_node(&w, 10);
        let ra = harvest(&a, 1, [0u8; 32], x, y, k, 10);
        let rb = harvest(&b, 1, [0u8; 32], x, y, k, 10);
        let (st, rep) = replay(&[ra, rb], &view(&w, 10));
        assert_eq!(rep.accepted, 1, "同一格被采了两次");
        assert_eq!(st.balance_of(&a.address), resource::reward_at(k, 10));
        assert_eq!(st.balance_of(&b.address), 0);
    }

    /// 回拨轮次换配额：一把钥匙把**历史上每一轮**各写一条采集，一条都不许进账。
    ///
    /// 这一条钉的是"绝对公平"的地基：轮次一旦由客户端自报的块高决定，脚本就能在
    /// 链头 6000 时把 0..6000 的十个轮次一次刷完（每轮 60 格），还能在"声明的旧轮次"
    /// 里重采别人本轮已经采走的格。现在轮次只由判定它的那一块决定。
    #[test]
    fn backdated_epochs_buy_nothing() {
        let c = core();
        let w = words();
        let tip = 5 * resource::EPOCH_BLOCKS + 10;
        let (x, y, k) = find_node(&w, tip);
        let mut forged = Vec::new();
        let mut prev = [0u8; 32];
        for e in 0..5u64 {
            let h = e * resource::EPOCH_BLOCKS + 10;
            let r = harvest(&c, e + 1, prev, x, y, k, h);
            prev = record_hash(&r);
            forged.push(r);
        }
        let (st, rep) = replay(&forged, &view(&w, tip));
        assert_eq!(
            st.balance_of(&c.address),
            0,
            "回拨的采集进了账：{}",
            rep.reason
        );

        // 反向对照：同一个签名者按**当下**写，正常进账——上面那条不是把门焊死
        let now = harvest(&c, 1, [0u8; 32], x, y, k, tip);
        let (st2, rep2) = replay(&[now], &view(&w, tip));
        assert!(rep2.ok, "当下的采集被误杀：{}", rep2.reason);
        assert_eq!(st2.balance_of(&c.address), resource::reward_at(k, tip));
    }

    /// 旧轮次不解锁资源点：别人本轮采走的格，不能靠声明"上一轮"再采一遍。
    #[test]
    fn claiming_an_old_epoch_does_not_unharvest_a_cell() {
        let a = core();
        let b = crate::keystore::open_core_from_seed(&[9u8; 32], &[2u8, 3, 5, 7, 11, 13, 17, 19]).unwrap();
        let w = words();
        let tip = 4 * resource::EPOCH_BLOCKS + 300;
        let (x, y, k) = find_node(&w, tip);
        let ra = harvest(&a, 1, [0u8; 32], x, y, k, tip);
        // b 挑上一轮同一点：形状自洽（轮次=那一轮的块高），但判定它的块在本次
        let rb = harvest(&b, 1, [0u8; 32], x, y, k, tip - resource::EPOCH_BLOCKS);
        let (st, _) = replay(&[ra, rb], &view(&w, tip));
        assert_eq!(st.balance_of(&a.address), resource::reward_at(k, tip));
        assert_eq!(st.balance_of(&b.address), 0, "回拨轮次绕过了「本轮已采」");
    }

    /// 产出也按当下算：本轮里挑一个白天的块高来躲夜间减半，同样是回拨。
    #[test]
    fn night_rate_follows_the_block_that_judges_it() {
        let c = core();
        let w = words();
        let day = 10 * resource::EPOCH_BLOCKS + 100;
        let night = 10 * resource::EPOCH_BLOCKS + 450;
        assert!(resource::is_night(night) && !resource::is_night(day));
        let (x, y, k) = find_node(&w, night);

        let stale = harvest(&c, 1, [0u8; 32], x, y, k, day);
        let (st, _) = replay(&[stale], &view(&w, night));
        assert_eq!(st.balance_of(&c.address), 0, "自报白天块高就拿到白天价");

        let fresh = harvest(&c, 1, [0u8; 32], x, y, k, night);
        let (st2, rep2) = replay(&[fresh], &view(&w, night));
        assert!(rep2.ok, "夜间当下的采集被误杀：{}", rep2.reason);
        assert_eq!(st2.balance_of(&c.address), resource::reward_at(k, night));
        assert_eq!(
            resource::reward_at(k, night),
            resource::base_reward(k) / 2,
            "夜间就该是半价"
        );
    }

    #[test]
    fn reward_must_match_the_rule() {
        let c = core();
        let w = words();
        let (x, y, k) = find_node(&w, 10);
        let mut r = harvest(&c, 1, [0u8; 32], x, y, k, 10);
        r.amount += 1;
        let h = record_hash(&r);
        r.sig_hex = hex::encode(crypto::sign(&c.signing_key, &h));
        let (st, _) = replay(&[r], &view(&w, 10));
        assert_eq!(st.balance_of(&c.address), 0);
    }

    #[test]
    fn send_moves_balance_and_blocks_overdraft() {
        let a = core();
        let w = words();
        let (x, y, k) = find_node(&w, 10);
        let h1 = harvest(&a, 1, [0u8; 32], x, y, k, 10);
        let prev = record_hash(&h1);
        let reward = resource::reward_at(k, 10);
        let to = crate::crypto::to_bech32_ql(&[7u8; 32]);
        let send = sign_record(
            &a,
            2,
            &prev,
            &DustDraft {
                op: op::SEND,
                epoch: 0,
                x: 0,
                y: 0,
                kind: 0,
                amount: reward,
                to: &to,
                height: 11,
                ts: 11,
            },
        );
        let (st, rep) = replay(&[h1.clone(), send.clone()], &view(&w, 11));
        assert!(rep.ok, "{}", rep.reason);
        assert_eq!(st.balance_of(&a.address), 0);
        assert_eq!(st.balance_of(&to), reward);

        // 再转一次同样的金额：链合法，但余额不够，必须被语义层拒绝
        let prev2 = record_hash(&send);
        let again = sign_record(
            &a,
            3,
            &prev2,
            &DustDraft {
                op: op::SEND,
                epoch: 0,
                x: 0,
                y: 0,
                kind: 0,
                amount: 1,
                to: &to,
                height: 12,
                ts: 12,
            },
        );
        let (st2, _) = replay(&[h1, send, again], &view(&w, 12));
        assert_eq!(st2.balance_of(&a.address), 0);
        assert_eq!(st2.balance_of(&to), reward);
    }

    #[test]
    fn broken_chain_stops_at_the_break() {
        let c = core();
        let w = words();
        let (x, y, k) = find_node(&w, 10);
        let r1 = harvest(&c, 1, [0u8; 32], x, y, k, 10);
        let mut r2 = harvest(&c, 2, record_hash(&r1), x + 1, y, k, 10);
        r2.prev_hash_hex = ZERO_HASH.into();
        let (_, rep) = replay(&[r1, r2], &view(&w, 10));
        assert_eq!(rep.first_bad, 2);
        assert_eq!(rep.accepted, 1);
    }

    #[test]
    fn night_pays_half_and_is_still_valid() {
        let c = core();
        let w = words();
        let h = 450; // 夜里
        let (x, y, k) = find_node(&w, h);
        let r = harvest(&c, 1, [0u8; 32], x, y, k, h);
        let (st, rep) = replay(&[r], &view(&w, h));
        assert!(rep.ok, "{}", rep.reason);
        assert_eq!(st.balance_of(&c.address), resource::reward_at(k, h));
    }
}
