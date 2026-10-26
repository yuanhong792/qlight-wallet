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
//! - 顺序     每条带 `prev_hash`，整本 append-only 哈希链，撕一条必改后面全部
//! - 内容寻址 `entity_id = SHAKE-256("QL-ENT-V1", prev_hash ‖ pk_hash ‖ seq)`
//!   id 由铸造内容（名称/坐标/槽位/块高/父实体）一并派生：没有注册机构、没有
//!   命名空间仲裁、没有白名单，谁先写谁得；创建者也无法事后自签改写这条历史。
//!
//! Minecraft 借来的三件事：格子坐标（点击即放置）、物品栏（kind 8 种方块/生灵）、
//! chunk 16×16（每个 chunk 一个承诺根，实体"都可查"但不是每条都单独上链）。

use crate::crypto;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 世界尺寸（与 tools/world 参考实现同一口径：256×256 格）
pub const WORLD_W: u16 = 256;
pub const WORLD_H: u16 = 256;
/// Minecraft 口径：chunk 16×16 是加载/网络/承诺单位
pub const CHUNK: u16 = 16;
/// 创世纪元截止块高：此前诞生的实体永久标 era=1（§9.9 稀缺来自时间）
pub const ERA1_END_HEIGHT: u64 = 100_000;
/// 物品栏槽位数（kind ∈ 0..SLOT_COUNT）
pub const SLOT_COUNT: u8 = 8;
pub const NAME_MAX: usize = 64;

const REC_PREFIX: &[u8] = b"QL-ENTREC-v1";
const REC_HASH_PREFIX: &[u8] = b"QL-ENTREC-H1";
const ID_PREFIX: &[u8] = b"QL-ENT-V1";
const CHUNK_PREFIX: &[u8] = b"QL-CHUNK-V1";
const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// 操作类型（对应 §9.11 的四件事：铸、改名、挪动、释放归属）
pub mod op {
    pub const CLAIM: &str = "claim";
    pub const RENAME: &str = "rename";
    pub const MOVE: &str = "move";
    pub const RELEASE: &str = "release";
}

/// 一条实体记录（entities.jsonl 每行一条，字段全部参与哈希链，无冗余可篡改位）
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
    pub kind: u8,
    /// 写入时的块高——决定 era，也决定"谁先看见"
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
    /// 1952 字节 ML-DSA-65 公钥（独立验签所需，不信任任何索引）
    pub pk_hex: String,
    /// ML-DSA-65 对 record_hash 的签名
    pub sig_hex: String,
}

/// 参与签名的字段 → 确定性字节串（长度前缀，杜绝字段边界歧义）
fn canon(r: &EntityRecord) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(256);
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
    bytes(&mut out, &hex256(&r.entity_id_hex));
    bytes(&mut out, &hex256(&r.parent_hex));
    bytes(&mut out, r.name.as_bytes());
    out.extend_from_slice(&r.x.to_be_bytes());
    out.extend_from_slice(&r.y.to_be_bytes());
    out.push(r.kind);
    u64b(&mut out, r.height);
    u64b(&mut out, r.ts);
    bytes(&mut out, &hex256(&r.pk_hash_hex));
    bytes(&mut out, &hex::decode(&r.salt_hex).unwrap_or_default());
    bytes(&mut out, &hex256(&r.prev_hash_hex));
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

/// 内容寻址的实体 id：由「上一条哈希 ‖ 创建者公钥摘要 ‖ 序号 ‖ 铸造内容」算出，
/// 不需要任何注册机构批准，也不可被抢注（同一 seq 位置只能有一条合法记录）。
///
/// 内容（名称/坐标/槽位/块高）也进 id：否则事后改一条 claim 再自签一次，
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
    buf.push(r.kind);
    buf.extend_from_slice(&r.height.to_be_bytes());
    let d = crypto::shake256(ID_PREFIX, &buf, 32);
    let mut out = [0u8; 32];
    out.copy_from_slice(&d);
    out
}

/// 世界里的一个实体（由记录重放出来的当前状态，不是独立存储）
#[derive(Debug, Clone, Serialize)]
pub struct Entity {
    pub entity_id_hex: String,
    pub name: String,
    pub x: u16,
    pub y: u16,
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
    /// 全部记录都通过（哈希链 + 签名 + 归属 + 语义）
    pub ok: bool,
    pub total: usize,
    /// 首个失败记录的 seq（0 = 无）
    pub first_bad: u64,
    pub reason: String,
}

/// 重放整本实体账本，返回当前世界状态。
///
/// 与 `validate_blocks_jsonl` 同一套纪律：**逐条严格重算**，任何一条签名/归属/
/// 序号/哈希链不对就停在那里，并给出原因。返回的 Report 供 UI 直显，不静默降级。
/// 失败时仍交出"坏记录之前"的状态——一条坏记录不该让整个世界消失。
pub fn replay(records: &[EntityRecord]) -> (BTreeMap<String, Entity>, Report) {
    let mut state: BTreeMap<String, Entity> = BTreeMap::new();
    let mut prev = [0u8; 32];
    for (i, r) in records.iter().enumerate() {
        if let Err(reason) = apply_record(&mut state, &prev, i, r) {
            return (state, Report { ok: false, total: records.len(), first_bad: r.seq, reason });
        }
        prev = record_hash(r);
    }
    (state, Report { ok: true, total: records.len(), first_bad: 0, reason: String::new() })
}

fn apply_record(
    state: &mut BTreeMap<String, Entity>,
    prev: &[u8; 32],
    i: usize,
    r: &EntityRecord,
) -> Result<(), String> {
    if r.v != "QL-ENTREC-v1" {
        return Err(format!("未知记录版本 {}", r.v));
    }
    if r.seq != (i as u64) + 1 {
        return Err(format!("序号断裂：期望 {}，实得 {}", i + 1, r.seq));
    }
    if hex::encode(prev) != r.prev_hash_hex {
        return Err(format!("哈希链断裂：seq {} 的 prev_hash 与上一条不符", r.seq));
    }
    if r.name.is_empty() || r.name.len() > NAME_MAX {
        return Err(format!("seq {} 名称长度为 {}，须为 1..={}", r.seq, r.name.len(), NAME_MAX));
    }
    if r.x >= WORLD_W || r.y >= WORLD_H || r.kind >= SLOT_COUNT {
        return Err(format!("seq {} 越界：坐标 ({},{}) 或槽位 {} 非法", r.seq, r.x, r.y, r.kind));
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

    match r.op.as_str() {
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
            state.insert(
                r.entity_id_hex.clone(),
                Entity {
                    entity_id_hex: r.entity_id_hex.clone(),
                    name: r.name.clone(),
                    x: r.x,
                    y: r.y,
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
                    e.x = r.x;
                    e.y = r.y;
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

// ---------------------------------------------------------------- 落盘

/// append-only 实体账本。写锁只保护"追加一行"这一步；读侧永远整本重放，无缓存可失效。
pub struct EntityLedger {
    path: PathBuf,
    lock: std::sync::Mutex<()>,
}

impl EntityLedger {
    pub fn new(path: PathBuf) -> Self {
        Self { path, lock: std::sync::Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 整本读入；文件不存在即空世界。坏行按"截断"处理并停止（与块账本同口径）
    pub fn load(&self) -> Vec<EntityRecord> {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return Vec::new() };
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<EntityRecord>(line) {
                Ok(r) => out.push(r),
                Err(_) => break,
            }
        }
        out
    }

    /// 追加一条记录（自取写锁）
    pub fn append(&self, r: &EntityRecord) -> Result<(), String> {
        let _guard = self.lock.lock().map_err(|_| "写锁中毒".to_string())?;
        write_line(&self.path, r)
    }

    /// 读—签名—追加必须是一个整体：否则两个并发 claim 会拿到同一个 seq，
    /// 后写的那条直接撕断哈希链。build 收到 (seq, prev_hash)，返回完整记录。
    pub fn commit(
        &self,
        build: impl FnOnce(u64, [u8; 32]) -> EntityRecord,
    ) -> Result<EntityRecord, String> {
        let _guard = self.lock.lock().map_err(|_| "写锁中毒".to_string())?;
        let recs = self.load();
        let seq = recs.len() as u64 + 1;
        let prev = recs.last().map(record_hash).unwrap_or([0u8; 32]);
        let r = build(seq, prev);
        write_line(&self.path, &r)?;
        Ok(r)
    }
}

/// 单次 write_all 整行 + fsync：kill -9 不会撕行（与 append_block_header 同款）
fn write_line(path: &Path, r: &EntityRecord) -> Result<(), String> {
    use std::io::Write;
    let mut line = serde_json::to_string(r).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("打开实体账本失败：{e}"))?;
    f.write_all(line.as_bytes()).map_err(|e| format!("写入失败：{e}"))?;
    f.sync_all().ok();
    Ok(())
}

/// 铸造/操作一条记录所需的最小输入（其余字段由后端补齐并签名）
pub struct Draft<'a> {
    pub op: &'a str,
    pub entity_id_hex: &'a str,
    pub parent_hex: &'a str,
    pub name: &'a str,
    pub x: u16,
    pub y: u16,
    pub kind: u8,
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
        v: "QL-ENTREC-v1".into(),
        seq,
        op: d.op.into(),
        entity_id_hex: d.entity_id_hex.into(),
        parent_hex: d.parent_hex.into(),
        name: d.name.into(),
        x: d.x,
        y: d.y,
        kind: d.kind,
        height: d.height,
        ts: d.ts,
        owner: core.address.clone(),
        pk_hash_hex: hex::encode(pk_hash32),
        salt_hex: hex::encode(core.addr_salt),
        prev_hash_hex: hex::encode(prev),
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

    fn commit(ledger: &EntityLedger, c: &crate::keystore::WalletCore, d: &Draft) -> EntityRecord {
        ledger.commit(|seq, prev| sign_record(c, seq, &prev, d)).unwrap()
    }

    #[test]
    fn claim_then_rename_then_release_replays_clean() {
        let c = core();
        let ledger = tmp_ledger("flow");

        let r1 = commit(
            &ledger,
            &c,
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "创世塔",
                x: 40, y: 60, kind: 4, height: 12, ts: 1,
            },
        );
        assert_eq!(r1.seq, 1);
        assert_eq!(r1.prev_hash_hex, genesis_prev());

        let r2 = commit(
            &ledger,
            &c,
            &Draft {
                op: op::RENAME, entity_id_hex: &r1.entity_id_hex, parent_hex: "", name: "改名塔",
                x: 0, y: 0, kind: 0, height: 20, ts: 2,
            },
        );
        assert_eq!(r2.prev_hash_hex, hex::encode(record_hash(&r1)));

        commit(
            &ledger,
            &c,
            &Draft {
                op: op::MOVE, entity_id_hex: &r1.entity_id_hex, parent_hex: "", name: "改名塔",
                x: 41, y: 61, kind: 0, height: 25, ts: 3,
            },
        );

        let (state, rep) = replay(&ledger.load());
        assert!(rep.ok, "重放应通过：{}", rep.reason);
        let e = state.get(&r1.entity_id_hex).unwrap();
        assert_eq!(e.name, "改名塔");
        assert_eq!((e.x, e.y), (41, 61));
        assert_eq!(e.era, 1);
        assert!(!e.released);

        commit(
            &ledger,
            &c,
            &Draft {
                op: op::RELEASE, entity_id_hex: &r1.entity_id_hex, parent_hex: "", name: "改名塔",
                x: 41, y: 61, kind: 0, height: 30, ts: 4,
            },
        );
        let (state, rep) = replay(&ledger.load());
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
        let mut r = commit(
            &ledger,
            &c,
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "不可篡改",
                x: 5, y: 5, kind: 1, height: 3, ts: 1,
            },
        );
        // 改名字 → record_hash 变、签名失效。这里连"改完再自签一次"都试了：
        // id 由铸造内容派生，创建者自己也无法悄悄改写已经存在的那条历史。
        r.name = "篡改名".into();
        r.sig_hex = hex::encode(crypto::sign(&c.signing_key, &record_hash(&r)));
        let (state, rep) = replay(&[r]);
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
        let r1 = commit(
            &ledger,
            &alice,
            &Draft {
                op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "Alice 的树",
                x: 9, y: 9, kind: 0, height: 4, ts: 1,
            },
        );
        commit(
            &ledger,
            &bob,
            &Draft {
                op: op::MOVE, entity_id_hex: &r1.entity_id_hex, parent_hex: "", name: "抢过来的树",
                x: 10, y: 10, kind: 0, height: 5, ts: 2,
            },
        );
        let (state, rep) = replay(&ledger.load());
        assert!(!rep.ok);
        assert!(rep.reason.contains("不是该实体当前归属者"), "{}", rep.reason);
        assert_eq!(rep.first_bad, 2);
        // 失败前的状态仍然可用：世界不会因为一条坏记录而消失
        assert_eq!(state.get(&r1.entity_id_hex).unwrap().name, "Alice 的树");
        let _ = std::fs::remove_file(ledger.path());
    }

    #[test]
    fn hash_chain_gap_is_caught() {
        let c = core();
        let ledger = tmp_ledger("gap");
        for i in 0..3u16 {
            commit(
                &ledger,
                &c,
                &Draft {
                    op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "塔",
                    x: i, y: i, kind: 0, height: i as u64 + 1, ts: i as u64,
                },
            );
        }
        // 手工撕掉中间一条：序号与哈希链双重暴露
        let mut recs = ledger.load();
        assert_eq!(recs.len(), 3);
        recs.remove(1);
        let (_, rep) = replay(&recs);
        assert!(!rep.ok);
        assert!(rep.reason.contains("序号断裂"), "{}", rep.reason);
        assert_eq!(rep.first_bad, 3);
        let _ = std::fs::remove_file(ledger.path());
    }

    #[test]
    fn a_rewritten_line_breaks_the_hash_chain() {
        let c = core();
        let ledger = tmp_ledger("rewritten");
        for i in 0..2u16 {
            commit(
                &ledger,
                &c,
                &Draft {
                    op: op::CLAIM, entity_id_hex: "", parent_hex: "", name: "塔",
                    x: i, y: i, kind: 0, height: i as u64 + 1, ts: i as u64,
                },
            );
        }
        // 序号仍连续，只把第二行的 prev_hash 换成创世锚 → 唯一能抓住它的是哈希链
        let mut recs = ledger.load();
        recs[1].prev_hash_hex = genesis_prev().into();
        let (_, rep) = replay(&recs);
        assert!(!rep.ok);
        assert!(rep.reason.contains("哈希链断裂"), "{}", rep.reason);
        assert_eq!(rep.first_bad, 2);
        let _ = std::fs::remove_file(ledger.path());
    }

    #[test]
    fn chunk_root_is_a_set_commitment() {
        let mut state = BTreeMap::new();
        for (id, x) in [("aa", 3u16), ("bb", 4), ("cc", 20)] {
            state.insert(
                id.to_string(),
                Entity {
                    entity_id_hex: id.into(), name: "n".into(), x, y: 2, kind: 0,
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
}
