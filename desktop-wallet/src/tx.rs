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

use crate::crypto;

/// 模拟链 chain_id（"QL" 的 ASCII，作为 regtest 标识）
pub const CHAIN_ID: u64 = 0x0051_4C;

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
        }
    }

    pub fn transfer(mut self, to: [u8; 32], amount: u64) -> Self {
        self.actions.push(TransferAction { to, amount });
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
    cbor_array_header(&mut out, 9);
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
}
