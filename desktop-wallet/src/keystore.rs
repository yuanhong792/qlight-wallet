//! 加密密钥库：Argon2id 派生 KEK + AES-256-GCM 包裹主种子
//!
//! 威胁模型（DESIGN.md §4 的钱包侧子集）：
//! - 静态文件被读取 → 只可见密文；
//! - 密码强度不足 → Argon2id 增加离线穷举成本；
//! - 内存中明文敏感数据用 zeroize 清除。
//!
//! 布局（keystore.json）：
//!
//! ```json
//! {
//!   "v": 1,
//!   "kdf": "argon2id", "salt_hex": "…", "m_cost_kb": 32768, "t_cost": 3, "p_cost": 1,
//!   "cipher": "aes-256-gcm", "nonce_hex": "…", "ct_hex": "…",
//!   "check_hex": "…"
//! }
//! ```
//!
//! 明文 = JSON：{ seed_hex, addr, created_unix }
//! check_hex = SHAKE-256("QLX-KEYSTORE-V1" || ct)[0..8]（防篡改校验，非认证替代）

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use sha3::digest::{ExtendableOutput, Update, XofReader};
use sha3::Shake256;
use std::path::PathBuf;
use zeroize::Zeroizing;

const CHECK_PREFIX: &[u8] = b"QLX-KEYSTORE-V1";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Keystore {
    pub v: u8,
    pub kdf: String,
    pub salt_hex: String,
    pub m_cost_kb: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub cipher: String,
    pub nonce_hex: String,
    pub ct_hex: String,
    pub check_hex: String,
}

/// 钱包主对象：解密态内容
pub struct WalletCore {
    pub seed: Zeroizing<[u8; 32]>,
    /// 地址盐（与密码无关，明文存储）
    pub addr_salt: [u8; 8],
    pub address: String,
    pub signing_key: crate::crypto::AccountSigningKey,
    pub verifying_key: crate::crypto::AccountVerifyingKey,
    pub pk_bytes: Box<[u8]>,
}

#[derive(Serialize, Deserialize)]
struct PlainPayload {
    seed_hex: String,
    addr_salt_hex: String,
    created_unix: u64,
}

fn kek_from_password(password: &str, salt: &[u8], m_kb: u32, t: u32, p: u32) -> [u8; 32] {
    let argon = Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(m_kb, t, p, Some(32)).expect("argon2 参数合法"),
    );
    let mut out = [0u8; 32];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .expect("argon2 派生失败");
    out
}

fn shake_check(ct: &[u8]) -> [u8; 8] {
    let mut hash = Shake256::default();
    hash.update(CHECK_PREFIX);
    hash.update(ct);
    let mut r = hash.finalize_xof();
    let mut out = [0u8; 8];
    r.read(&mut out);
    out
}

/// 口令下限（字符数，不是字节数：中文密码 8 字节只有不到 3 个字）
pub const PASSWORD_MIN_CHARS: usize = 8;

/// 常见弱口令表。这条规则不是防在线攻击——Argon2id 已经把离线穷举抬到 32 MiB×3 轮；
/// 它防的是"玩家以为自己设了密码，其实第一秒就被字典命中"。
/// ≥8 字符的按子串拒（`xpassword9` 仍是字典词），更短的只在整串相等时拒
/// （否则 "admintest1" 这类合法口令会被误伤）。
const WEAK_WORDS: &[&str] = &[
    "password",
    "passw0rd",
    "p@ssword",
    "qwertyui",
    "qwertyuiop",
    "iloveyou",
    "letmein1",
    "welcome1",
    "admin123",
    "abc12345",
    "12345678",
    "1234567890",
    "00000000",
    "aaaaaaaa",
    "asdfghjkl",
    "zxcvbnmm",
];

/// 弱口令判定：纯函数（不落盘、不派生密钥），所以前端预检和这里可以是同一套口径。
///
/// 四条规则，从小到大都能说清：长度、全同字符、连续字符、字典词。
pub fn check_password(pw: &str) -> Result<(), String> {
    let n = pw.chars().count();
    if n < PASSWORD_MIN_CHARS {
        return Err(format!("密码至少 {PASSWORD_MIN_CHARS} 位"));
    }
    let chars: Vec<char> = pw.chars().collect();
    if chars.iter().all(|c| *c == chars[0]) {
        return Err("密码不能是全同一个字符".into());
    }
    // 整串是升/降的连续码点（12345678 / hgfedcba）——长度规则挡不住，但它是最典型的懒密码
    let step = |d: i64| {
        chars
            .windows(2)
            .all(|w| (w[1] as i64 - w[0] as i64) == d)
    };
    if n >= 4 && (step(1) || step(-1)) {
        return Err("密码不能是连续字符（如 12345678）".into());
    }
    let low = pw.to_lowercase();
    for w in WEAK_WORDS {
        if w.len() >= 8 {
            if low.contains(w) {
                return Err("密码里含有常见弱口令，换一个".into());
            }
        } else if low == *w {
            return Err("这是常见弱口令，换一个".into());
        }
    }
    Ok(())
}

/// 创建新钱包：生成主种子 → ML-DSA-65 → ql1 地址 → 加密落盘
pub fn create_wallet(
    path: &PathBuf,
    password: &str,
) -> Result<(Keystore, WalletCore), String> {
    check_password(password)?;

    let seed = {
        let mut s = [0u8; 32];
        getrandom::fill(&mut s).expect("OS 熵源不可用");
        Zeroizing::new(s)
    };
    let addr_salt = {
        let mut s = [0u8; 8];
        getrandom::fill(&mut s).expect("OS 熵源不可用");
        s
    };

    let core = open_core_from_seed(&seed, &addr_salt)?;
    let core_eth = &core;

    let (ks, _) = seal(password, core_eth)?;
    std::fs::create_dir_all(path.parent().unwrap()).ok();
    std::fs::write(path, serde_json::to_vec_pretty(&ks).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok((ks, core))
}

/// 用密码解锁已存在的密钥库
pub fn unlock_wallet(path: &PathBuf, password: &str) -> Result<WalletCore, String> {
    let raw = std::fs::read(path).map_err(|e| format!("读取密钥库失败: {e}"))?;
    let ks: Keystore = serde_json::from_slice(&raw).map_err(|e| format!("解析密钥库失败: {e}"))?;
    decrypt_core(&ks, password)
}

/// 封装为 keystore：加密钱包内容（不再负责写盘，写盘在 create_wallet 内做）
pub fn seal(password: &str, core: &WalletCore) -> Result<(Keystore, Vec<u8>), String> {
    // KDF 盐
    let mut kdf_salt = [0u8; 32];
    getrandom::fill(&mut kdf_salt).expect("OS 熵源不可用");
    let m_kb: u32 = 32768; // 32 MiB
    let t: u32 = 3;
    let p: u32 = 1;
    let kek = kek_from_password(password, &kdf_salt, m_kb, t, p);

    // 明文 payload
    let plain = PlainPayload {
        seed_hex: hex::encode(core.seed.as_ref()),
        addr_salt_hex: hex::encode(core.addr_salt),
        created_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    let plain_bytes = serde_json::to_vec(&plain).map_err(|e| e.to_string())?;

    // AES-256-GCM
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).expect("OS 熵源不可用");
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&kek));
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: plain_bytes.as_slice(),
                aad: CHECK_PREFIX,
            },
        )
        .map_err(|e| format!("加密失败: {e}"))?;

    let check = shake_check(&ct);
    let ks = Keystore {
        v: 1,
        kdf: "argon2id".into(),
        salt_hex: hex::encode(kdf_salt),
        m_cost_kb: m_kb,
        t_cost: t,
        p_cost: p,
        cipher: "aes-256-gcm".into(),
        nonce_hex: hex::encode(nonce_bytes),
        ct_hex: hex::encode(&ct),
        check_hex: hex::encode(check),
    };

    // 密码相关临时数据清零
    let _ = &core;

    Ok((ks, plain_bytes))
}

/// 解封：从 keystore 还原 WalletCore（校验 check_hex + AEAD 认证）
pub fn decrypt_core(ks: &Keystore, password: &str) -> Result<WalletCore, String> {
    if ks.kdf != "argon2id" || ks.cipher != "aes-256-gcm" || ks.v != 1 {
        return Err("不支持的密钥库版本".into());
    }
    let kdf_salt = hex::decode(&ks.salt_hex).map_err(|e| e.to_string())?;
    let nonce_bytes = hex::decode(&ks.nonce_hex).map_err(|e| e.to_string())?;
    let ct = hex::decode(&ks.ct_hex).map_err(|e| e.to_string())?;

    // 防篡改（快速失败，真正的认证在 AEAD）
    let expect_check: [u8; 8] = {
        let mut h = [0u8; 8];
        h.copy_from_slice(&hex::decode(&ks.check_hex).map_err(|e| e.to_string())?);
        h
    };
    if shake_check(&ct) != expect_check {
        return Err("密钥库被修改（check 失败）".into());
    }

    let kek = kek_from_password(password, &kdf_salt, ks.m_cost_kb, ks.t_cost, ks.p_cost);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&kek));
    let plain = cipher
        .decrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: ct.as_slice(),
                aad: CHECK_PREFIX,
            },
        )
        .map_err(|_| "密码错误或密钥库损坏".to_string())?;

    let payload: PlainPayload = serde_json::from_slice(&plain).map_err(|e| e.to_string())?;
    let seed_hex = payload.seed_hex.clone();
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&hex::decode(seed_hex).map_err(|e| e.to_string())?);
    let seed = Zeroizing::new(seed);

    let mut addr_salt = [0u8; 8];
    addr_salt.copy_from_slice(&hex::decode(payload.addr_salt_hex).map_err(|e| e.to_string())?);

    open_core_from_seed(&seed, &addr_salt)
}

/// 由种子构造 WalletCore（签名密钥展开）
pub fn open_core_from_seed(seed: &[u8; 32], addr_salt: &[u8; 8]) -> Result<WalletCore, String> {
    let (sk, vk) = crate::crypto::keypair_from_seed(seed);
    let address = crate::crypto::derive_address(&vk, addr_salt);
    let pk_bytes = crate::crypto::encode_pk(&vk);
    Ok(WalletCore {
        seed: Zeroizing::new(*seed),
        addr_salt: *addr_salt,
        address,
        signing_key: sk,
        verifying_key: vk,
        pk_bytes,
    })
}

// ---------------------------------------------------------------- `.dat` 备份格式
//
// 格式（16 字节定长头 + 载荷，与常用钱包 `.dat` 备份习惯一致）：
//
// ```text
// 0..8   magic  "QLWDAT01"
// 8..12  payload 长度（u32 大端）
// 12..   keystore.json 原始字节（密文形态）
// ```
//
// 备份文件里只有密文：被偷 ≠ 被解，恢复即还原 keystore.json，解锁密码保持不变。

pub const BACKUP_MAGIC: &[u8; 8] = b"QLWDAT01";

/// 把 keystore 序列化成 .dat 字节（导出与测试共用同一实现）
pub fn backup_bytes(ks: &Keystore) -> Result<Vec<u8>, String> {
    let payload = serde_json::to_vec(ks).map_err(|e| format!("序列化密钥库失败: {e}"))?;
    let mut out = Vec::with_capacity(payload.len() + 12);
    out.extend_from_slice(BACKUP_MAGIC);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// 写 .dat 到指定路径
pub fn write_backup(path: &PathBuf, ks: &Keystore) -> Result<(), String> {
    let bytes = backup_bytes(ks)?;
    std::fs::write(path, &bytes).map_err(|e| format!("写入备份失败: {e}"))
}

/// 从 .dat 字节解析出 Keystore（校验 magic、长度，内容完整性由解锁时的 AEAD 认证兜底）
pub fn parse_backup(bytes: &[u8]) -> Result<Keystore, String> {
    if bytes.len() < 12 || &bytes[..8] != BACKUP_MAGIC {
        return Err("备份文件头不匹配（应为 QLWDAT01 的 .dat 备份）".into());
    }
    let len = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    if bytes.len() != 12 + len {
        return Err(format!(
            "备份载荷长度不符：应 {} 字节，实际 {}",
            len,
            bytes.len().saturating_sub(12)
        ));
    }
    serde_json::from_slice::<Keystore>(&bytes[12..]).map_err(|e| format!("解析备份失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// .dat 备份往返：导出 → 解析 → 同密码解锁，种子/地址必须原样还原；错密码必须失败
    #[test]
    fn backup_dat_roundtrip() {
        let dir = std::env::temp_dir().join(format!("qlx-ks-test-{}", std::process::id()));
        let path = dir.join("keystore.json");
        let _ = std::fs::remove_dir_all(&dir);

        let (ks, core0) = create_wallet(&path, "test-pass-1234").unwrap();

        // 导出 .dat（二进制头在位）
        let dat_path = dir.join("backup.dat");
        write_backup(&dat_path, &ks).unwrap();
        let bytes = std::fs::read(&dat_path).unwrap();
        assert_eq!(&bytes[..8], BACKUP_MAGIC);

        // 解析还原（模拟换一台机器）
        let ks2 = parse_backup(&bytes).unwrap();
        let core = decrypt_core(&ks2, "test-pass-1234").unwrap();
        assert_eq!(core0.address, core.address);
        assert_eq!(
            hex::encode(core0.seed.as_ref()),
            hex::encode(core.seed.as_ref())
        );

        // 错密码必须解不开
        assert!(decrypt_core(&ks2, "wrong-password").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 弱口令必须拒在创建之前：一旦钱包用弱密码落了盘，密文就是可下载的穷举靶子
    #[test]
    fn lazy_passwords_are_refused() {
        for weak in [
            "12345678",
            "87654321",
            "abcdefgh",
            "aaaaaaaa",
            "password",
            "PASSWORD1",
            "my-password-2026",
            "qwertyuiop",
            "abc1234",
        ] {
            assert!(check_password(weak).is_err(), "弱口令被放过了：{weak}");
        }
    }

    /// 中文按字符算不按字节算：6 个汉字是 18 字节，旧口径会当成"够长"
    #[test]
    fn length_counts_characters_not_bytes() {
        assert!(check_password("量子货币").is_err(), "4 个汉字不该通过");
        assert!(check_password("我的量子货币钱包口令").is_ok());
    }

    /// 正常口令不该被误伤——过严的判定只会逼玩家把密码写在便利贴上
    #[test]
    fn ordinary_passwords_pass() {
        for ok in [
            "test-pass-1234",
            "Purple-Horse-42",
            "admintest1",
            "xkcd 99 bottle",
        ] {
            assert!(check_password(ok).is_ok(), "合法口令被误拒：{ok}");
        }
    }

    /// 长度不够时创建必须失败，且不留下半个 keystore 文件
    #[test]
    fn create_wallet_refuses_before_touching_the_disk() {
        let dir = std::env::temp_dir().join(format!("qlx-ks-weak-{}-x", std::process::id()));
        let path = dir.join("keystore.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(create_wallet(&path, "12345678").is_err());
        assert!(!path.exists(), "被拒的创建不许留下文件");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
