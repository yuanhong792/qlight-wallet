//! 交易信封：QL-TX-v1 canonical CBOR + SHAKE-256 digest
//!
//! 对应 DESIGN.md §14.2：
//!
//! ```c
//! struct TxEnvelope {
//!   uint8    version;          // 1
//!   uint64   chain_id;
//!   uint8    scheme_id;        // 1=ml_dsa_65
//!   bytes32  from_addr;
//!   bytes32  pk_hash;          // 首笔绑定交易时出现
//!   uint64   nonce;
//!   uint64   expiry;           // 块高
//!   uint64   feeCap;
//!   Action[] actions;          // Transfer / Call / Stream / Bridge
//!   bytes    session_nonce;
//! }
//! digest = SHAKE-256("QL-TX-v1" || canonical_cbor(TxEnvelope))[0..32]
//! ```
//!
//! M1 PoC：Action 仅实现 Transfer；pk_hash 支持首笔账户绑定；session_nonce 为空。
//!
//! canonical 规则（M1 子集）：
//! - 顶层为定长数组（9 元素），字段顺序 = 结构体定义顺序；
//! - 无符号整数一律最短编码（RFC 8949 §4.2.1）；
//! - 字节串 length 亦最短编码；bytes32 直接作为内容写入；
//! - 治理字段（pk_hash）None → CBOR null，Some → 定长数组包裹。
//!
//! v5 起追加第 10 个元素承载池子操作，**只在 `pool` 存在时出现**：
//! 无条件加字段会让整条历史链的 digest 全部改变（blocks.jsonl 每行存的
//! 就是 digest_hex），所以没有池子操作的交易仍然编成 9 元素，digest 与
//! v4 时期逐字节相同。

use crate::crypto;

/// 模拟链 chain_id（"QL" 的 ASCII，作为 regtest 标识）
pub const CHAIN_ID: u64 = 0x0051_4C;

/// 交易信封版本（canonical CBOR 顶层第一字段）
pub const TX_VERSION: u8 = 1;

const MAJOR_UINT: u8 = 0x00;
const MAJOR_BYTES: u8 = 0x40;
const MAJOR_ARRAY: u8 = 0x80;
const NULL: u8 = 0xF6;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TransferAction {
    /// 目标地址（ql1... 的 32 字节 commitment 原样）
    pub to: [u8; 32],
    /// 金额（QLX 最小单位，9 位小数）
    pub amount: u64,
}

/// 池子操作：光尘与 QLX 之间的四种状态迁移（v5 起随交易进块）。
///
/// 池子没有地址、没有私钥，储备直接记在 `Ledger.pools` 里。任何一笔
/// 操作都由签名者本人发起：加/撤流动性动的是他自己的两种余额，
/// 兑换按恒定乘积曲线由共识复算，谁也拿不走别人的份额。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PoolOp {
    /// 按现有比例注入两种资产（首笔注入定价）
    AddLiquidity { qlx: u64, dust: u64 },
    /// 赎回 LP 份额（按储备比例回吐两种资产）
    RemoveLiquidity { lp: u64 },
    /// QLX 换光尘，`min_out` 是滑点保护
    SwapQlxForDust { qlx_in: u64, min_out: u64 },
    /// 光尘换 QLX
    SwapDustForQlx { dust_in: u64, min_out: u64 },
}

impl PoolOp {
    /// CBOR 标签（数组第一元素；改了就改 digest，勿动）
    pub fn tag(&self) -> u64 {
        match self {
            PoolOp::AddLiquidity { .. } => 0,
            PoolOp::RemoveLiquidity { .. } => 1,
            PoolOp::SwapQlxForDust { .. } => 2,
            PoolOp::SwapDustForQlx { .. } => 3,
        }
    }

    /// 编码成定长 `[tag, a, b]`：撤流动性没有第二个数，补 0 保持结构一致
    fn operands(&self) -> (u64, u64) {
        match *self {
            PoolOp::AddLiquidity { qlx, dust } => (qlx, dust),
            PoolOp::RemoveLiquidity { lp } => (lp, 0),
            PoolOp::SwapQlxForDust { qlx_in, min_out } => (qlx_in, min_out),
            PoolOp::SwapDustForQlx { dust_in, min_out } => (dust_in, min_out),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TxEnvelope {
    pub version: u8,
    pub chain_id: u64,
    pub scheme_id: u8,
    pub from_addr: [u8; 32],
    /// 首笔账户绑定（pk_hash = SHA-256 of ML-DSA-65 公钥）；后续交易为空
    pub pk_hash: Option<[u8; 32]>,
    pub nonce: u64,
    /// 过期块高（模拟链：当前高度 + 50）
    pub expiry: u64,
    pub fee_cap: u64,
    pub actions: Vec<TransferAction>,
    /// v5：池子操作（与 actions 互斥；旧块里缺省为 None）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<PoolOp>,
}

impl TxEnvelope {
    pub fn new(from_addr: [u8; 32], nonce: u64, expiry: u64) -> Self {
        Self {
            version: 1,
            chain_id: CHAIN_ID,
            scheme_id: crypto::SCHEME_ML_DSA_65,
            from_addr,
            pk_hash: None,
            nonce,
            expiry,
            fee_cap: 0,
            actions: Vec::new(),
            pool: None,
        }
    }

    pub fn transfer(mut self, to: [u8; 32], amount: u64) -> Self {
        self.actions.push(TransferAction { to, amount });
        self
    }

    pub fn with_pool(mut self, op: PoolOp) -> Self {
        self.pool = Some(op);
        self
    }
}

// ---------------------------------------------------------------- canonical CBOR

/// 最短长度整数头（RFC 8949 确定性要求）
fn cbor_uint(out: &mut Vec<u8>, value: u64) {
    if value < 24 {
        out.push(MAJOR_UINT | (value as u8));
    } else if value <= 0xFF {
        out.push(MAJOR_UINT | 24);
        out.extend_from_slice(&(value as u8).to_be_bytes());
    } else if value <= 0xFFFF {
        out.push(MAJOR_UINT | 25);
        out.extend_from_slice(&(value as u16).to_be_bytes());
    } else if value <= 0xFFFF_FFFF {
        out.push(MAJOR_UINT | 26);
        out.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        out.push(MAJOR_UINT | 27);
        out.extend_from_slice(&value.to_be_bytes());
    }
}

fn cbor_bytes(out: &mut Vec<u8>, b: &[u8]) {
    if b.len() < 24 {
        out.push(MAJOR_BYTES | (b.len() as u8));
    } else if b.len() <= 0xFF {
        out.push(MAJOR_BYTES | 24);
        out.push(b.len() as u8);
    } else {
        out.push(MAJOR_BYTES | 25);
        out.extend_from_slice(&(b.len() as u16).to_be_bytes());
    }
    out.extend_from_slice(b);
}

fn cbor_array_header(out: &mut Vec<u8>, len: usize) {
    assert!(len <= 23, "M1 消费范围不含长数组");
    out.push(MAJOR_ARRAY | (len as u8));
}

/// pk_hash：None → null；Some → 单元素数组包裹（与“未来扩展字段”兼容）
fn cbor_optional_pk_hash(out: &mut Vec<u8>, v: &Option<[u8; 32]>) {
    match v {
        None => out.push(NULL),
        Some(h) => {
            cbor_array_header(out, 1);
            cbor_bytes(out, h);
        }
    }
}

/// 对信封做 canonical CBOR 编码
pub fn encode_tx(tx: &TxEnvelope) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    // 9 个顶层字段：version, chain_id, scheme_id, from, pk_hash, nonce, expiry, fee_cap, actions
    // 有池子操作时才是 10 个——见模块头：保持一致是 digest 兼容的全部前提
    cbor_array_header(&mut out, if tx.pool.is_some() { 10 } else { 9 });
    cbor_uint(&mut out, tx.version as u64);
    cbor_uint(&mut out, tx.chain_id);
    cbor_uint(&mut out, tx.scheme_id as u64);
    cbor_bytes(&mut out, &tx.from_addr);
    cbor_optional_pk_hash(&mut out, &tx.pk_hash);
    cbor_uint(&mut out, tx.nonce);
    cbor_uint(&mut out, tx.expiry);
    cbor_uint(&mut out, tx.fee_cap);
    // actions 数组
    assert!(tx.actions.len() <= 23, "M1 消费范围不含长 action 数组");
    cbor_array_header(&mut out, tx.actions.len());
    for a in &tx.actions {
        cbor_array_header(&mut out, 2);
        cbor_bytes(&mut out, &a.to);
        cbor_uint(&mut out, a.amount);
    }
    if let Some(op) = &tx.pool {
        cbor_array_header(&mut out, 3);
        cbor_uint(&mut out, op.tag());
        let (a, b) = op.operands();
        cbor_uint(&mut out, a);
        cbor_uint(&mut out, b);
    }
    out
}

/// QL-TX-v1 digest：SHAKE-256("QL-TX-v1" || canonical_cbor))[0..32]
pub fn tx_digest(tx: &TxEnvelope) -> [u8; 32] {
    let cbor = encode_tx(tx);
    let d = crypto::shake256(b"QL-TX-v1", &cbor, 32);
    let mut out = [0u8; 32];
    out.copy_from_slice(&d);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_cbor_golden() {
        let mut tx = TxEnvelope::new([7u8; 32], 0, 50);
        tx = tx.transfer([9u8; 32], 1_234_567_890);
        let cbor = encode_tx(&tx);
        // 顶层 9 字段数组（0x89），version=1（0x01），chain_id=0x514C 走 u16 最短编码
        assert_eq!(&cbor[0..5], &[0x89, 0x01, 0x19, 0x51, 0x4C]);
        // digest 稳定性
        assert_eq!(tx_digest(&tx), tx_digest(&tx));
    }

    #[test]
    fn legacy_digest_survives_json_roundtrip_without_pool_field() {
        let tx = TxEnvelope::new([7u8; 32], 3, 50).transfer([9u8; 32], 42);
        // 旧块里的 JSON 没有 pool 字段：反序列化必须是 None，编码必须逐字节不变
        let json = serde_json::to_string(&tx).unwrap();
        assert!(!json.contains("pool"));
        let back: TxEnvelope = serde_json::from_str(&json).unwrap();
        assert!(back.pool.is_none());
        assert_eq!(encode_tx(&tx), encode_tx(&back));
        assert_eq!(tx_digest(&tx), tx_digest(&back));
        assert_eq!(encode_tx(&back)[0], 0x89);
    }

    #[test]
    fn pool_ops_extend_the_array_and_change_the_digest() {
        let base = TxEnvelope::new([7u8; 32], 0, 50);
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(hex::encode(tx_digest(&base)));
        let cases: [(PoolOp, &[u8]); 4] = [
            (
                PoolOp::AddLiquidity { qlx: 1_000_000, dust: 10 },
                // [ 数组3, tag0, 1_000_000(u32), 10 ]
                &[0x83, 0x00, 0x1A, 0x00, 0x0F, 0x42, 0x40, 0x0A],
            ),
            (
                PoolOp::RemoveLiquidity { lp: 500 },
                // 撤流动性补 0，保持三元结构
                &[0x83, 0x01, 0x19, 0x01, 0xF4, 0x00],
            ),
            (
                PoolOp::SwapQlxForDust { qlx_in: 2_000, min_out: 1 },
                &[0x83, 0x02, 0x19, 0x07, 0xD0, 0x01],
            ),
            (
                PoolOp::SwapDustForQlx { dust_in: 3, min_out: 100 },
                &[0x83, 0x03, 0x03, 0x18, 0x64],
            ),
        ];
        for (op, suffix) in cases.iter() {
            let tx = base.clone().with_pool(op.clone());
            let cbor = encode_tx(&tx);
            assert_eq!(cbor[0], 0x8A, "有池子操作时顶层是 10 元素");
            assert!(cbor.ends_with(suffix), "尾元素编码不符：{}", hex::encode(&cbor));
            let back: TxEnvelope =
                serde_json::from_str(&serde_json::to_string(&tx).unwrap()).unwrap();
            assert_eq!(back.pool.as_ref(), Some(op));
            assert_eq!(encode_tx(&back), cbor);
            assert!(
                seen.insert(hex::encode(tx_digest(&tx))),
                "四种池子操作不能撞 digest"
            );
        }
    }
}
