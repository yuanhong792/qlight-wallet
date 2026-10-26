//! 密码学核心：ML-DSA-65 账户签名 + ql1 地址派生
//!
//! 对应 DESIGN.md：
//! - §5.1 账户签名 = ML-DSA-65（FIPS 204，RustCrypto ml-dsa 0.1.x 纯 Rust 实现）
//! - §5.1 链上哈希 = SHAKE-256（FIPS 202）
//! - §6.1 地址 = ql1<bech32 32字节 commitment>，
//!   commitment[0..3] = version + scheme_id，[4..31] = SHAKE-256(pk || salt) 截断
//!
//! 私钥持久化口径：FIPS 204 推荐 32 字节种子（seed），
//! 展开/验签密钥均由种子确定性导出——密钥库只需保护 32 字节。

use ml_dsa::{
    MlDsa65, Signature, SignatureEncoding, SigningKey as MlDsaSigningKey,
    VerifyingKey as MlDsaVerifyingKey,
};
use ml_dsa::{KeyExport as MlDsaKeyExport, KeyInit as MlDsaKeyInit};
use sha3::digest::{ExtendableOutput, Update, XofReader};
use sha3::{Shake256, Shake256Reader};
use zeroize::Zeroize;

pub type Seed = [u8; 32];
pub type AccountSigningKey = MlDsaSigningKey<MlDsa65>;
pub type AccountVerifyingKey = MlDsaVerifyingKey<MlDsa65>;
pub type AccountSignature = Signature<MlDsa65>;

pub const SK_SEED_LEN: usize = 32;
pub const PK_LEN: usize = 1952;
pub const SIG_LEN: usize = 3309;

/// scheme_id，与 DESIGN.md §14.2 一致：1=ml_dsa_65
pub const SCHEME_ML_DSA_65: u8 = 1;
/// 地址版本字节
pub const ADDR_VERSION: u8 = 0;

/// 从主种子（32B）确定性生成 ML-DSA-65 密钥对
pub fn keypair_from_seed(seed: &Seed) -> (AccountSigningKey, AccountVerifyingKey) {
    use ml_dsa::Keypair;
    let sk = AccountSigningKey::new_from_slice(seed.as_ref())
        .expect("32 字节种子对 ML-DSA-65 合法");
    let vk = sk.verifying_key();
    (sk, vk)
}

/// 真随机生成主种子
pub fn random_seed() -> Seed {
    let mut s = [0u8; 32];
    getrandom::fill(&mut s).expect("OS 熵源不可用");
    s
}

/// SHAKE-256 可变长度输出
pub fn shake256(prefix: &[u8], data: &[u8], out_len: usize) -> Vec<u8> {
    let mut hash = Shake256::default();
    hash.update(prefix);
    hash.update(data);
    let mut reader: Shake256Reader = hash.finalize_xof();
    let mut out = vec![0u8; out_len];
    reader.read(&mut out);
    out
}

/// 公钥编码（1952 字节，用于地址派生与展示）
pub fn encode_pk(vk: &AccountVerifyingKey) -> Box<[u8]> {
    vk.to_bytes().as_slice().to_vec().into_boxed_slice()
}

/// 公钥解码（从 1952 字节）
pub fn decode_pk(bytes: &[u8]) -> Option<AccountVerifyingKey> {
    AccountVerifyingKey::new_from_slice(bytes).ok()
}

/// ql1 地址派生（DESIGN.md §6.1）
///
/// commitment[0] = version(0x00)
/// commitment[1] = scheme_id(0x01 ml_dsa_65)
/// commitment[2] = 保留（0x00）
/// commitment[3..32] = SHAKE-256(b"ql-addr-v1" || pk || salt)[0..29]
///
/// bech32 hrp = "ql"，编码结果形如 ql1q…（bech32m 校验）
pub fn derive_address(vk: &AccountVerifyingKey, salt: &[u8; 8]) -> String {
    let pk = encode_pk(vk);
    let mut data = Vec::with_capacity(PK_LEN + 8);
    data.extend_from_slice(&pk);
    data.extend_from_slice(salt);
    let hash = shake256(b"ql-addr-v1", &data, 29);

    let mut commitment = [0u8; 32];
    commitment[0] = ADDR_VERSION;
    commitment[1] = SCHEME_ML_DSA_65;
    commitment[2] = 0;
    commitment[3..].copy_from_slice(&hash);

    to_bech32_ql(&commitment)
}

/// 32 字节 commitment → bech32（hrp "ql"，bech32m 校验）
pub fn to_bech32_ql(commitment: &[u8; 32]) -> String {
    use bech32::{Bech32m, Hrp};

    let hrp = Hrp::parse("ql").expect("hrp 合法");
    bech32::encode::<Bech32m>(hrp, commitment).expect("bech32 编码不会失败")
}

/// 对消息签名（msg = QL-TX-v1 digest）
///
/// M1 使用 FIPS 204 可选的确定性签名变体（ml-dsa Signer 默认，空上下文）。
pub fn sign(sk: &AccountSigningKey, msg: &[u8]) -> Vec<u8> {
    use ml_dsa::Signer;
    sk.sign(msg).to_vec()
}

/// 验签
pub fn verify(vk: &AccountVerifyingKey, msg: &[u8], sig: &[u8]) -> bool {
    use ml_dsa::Verifier;
    if sig.len() != SIG_LEN {
        return false;
    }
    match AccountSignature::try_from(sig) {
        Ok(s) => vk.verify(msg, &s).is_ok(),
        Err(_) => false,
    }
}

/// 清零敏感缓冲
pub fn zeroize_buf(buf: &mut [u8]) {
    buf.zeroize();
}

#[cfg(test)]
mod real_crypto_tests {
    use super::*;

    /// 证明用的是真算法：密钥/签名长度必须精确等于 FIPS 204 规定值
    #[test]
    fn mldsa65_spec_lengths() {
        assert_eq!(PK_LEN, 1952, "FIPS 204规定 ML-DSA-65 公钥 = 1952 字节");
        assert_eq!(SIG_LEN, 3309, "FIPS 204规定 ML-DSA-65 签名 = 3309 字节");

        let seed = [7u8; 32];
        let (sk, vk) = keypair_from_seed(&Seed::from(seed));
        let pk = encode_pk(&vk);
        assert_eq!(pk.len(), 1952, "实际实现的公钥确实是 1952 字节");

        let sig = sign(&sk, b"QLight real-crypto proof");
        assert_eq!(sig.len(), 3309, "实际实现的签名确实是 3309 字节");
    }

    /// 证明签名真的在算：同消息可验 / 篡改一字节即失败 / 换消息即失败 / 不同种子密钥不同
    #[test]
    fn mldsa65_sign_verify_tamper() {
        let (sk1, vk1) = keypair_from_seed(&[1u8; 32]);
        let (sk2, vk2) = keypair_from_seed(&[2u8; 32]);
        let msg = b"QL-TX payload";

        let sig = sign(&sk1, msg);
        assert!(verify(&vk1, msg, &sig), "正确签名必须验证通过");
        assert!(!verify(&vk1, b"other", &sig), "换消息必须失败");

        let mut bad = sig.clone();
        bad[100] ^= 0x01;               // 篡改签名任意一字节
        assert!(!verify(&vk1, msg, &bad), "篡改签名必须失败");

        let sig2 = sign(&sk2, msg);
        assert!(verify(&vk2, msg, &sig2), "另一把钥匙的签名必须失败前先保证自己可验");
        assert!(!verify(&vk1, msg, &sig2), "另一把钥匙的签名必须失败");
    }

    /// 从 1952 字节公钥还原验证密钥并验签（对应网络传输路径）
    #[test]
    fn mldsa65_pubkey_roundtrip() {
        let (sk, vk) = keypair_from_seed(&[9u8; 32]);
        let pk = encode_pk(&vk);
        let vk2 = decode_pk(&pk).expect("1952B 公钥应可还原");
        let sig = sign(&sk, b"roundtrip");
        assert!(verify(&vk2, b"roundtrip", &sig));
    }
}
