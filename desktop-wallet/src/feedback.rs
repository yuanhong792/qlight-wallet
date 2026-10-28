//! 反馈包：给玩家一个"把问题说清楚"的出口，同时把"默认不带走敏感物"钉成代码。
//!
//! 三条硬规矩，都由这个模块自己保证（不是靠调用方自觉）：
//! 1. **字段白名单**：产出的 JSON 只有下面列出的那几个键，多一个都算 bug，测试会红。
//! 2. **不读账本原文**：这里只收聚合数字（条数、坏行数、块高），所以密钥库、
//!    实体账本、光尘账本、`peers-registry.json` 的内容永远不会出现在包里。
//! 3. **地址默认缺席**：`ql1…` 是玩家的收款地址，属于个人标识；要带必须显式勾选。
//!
//! 另外说清一件 presently 做不到但容易被误会的事：**本版没有任何自动上传通道**。
//! 反馈包只写进玩家自己选的本地文件，网络层（`net.rs`）里也没有能收这个的端点。

use serde::{Deserialize, Serialize};

/// 反馈包格式标识：将来加字段要靠它区分，而不是靠猜
pub const FEEDBACK_KIND: &str = "QL-FEEDBACK-v1";
/// 问题描述的字数上限：超长截断，因为反馈包不该变成日志容器
pub const TEXT_MAX: usize = 2000;

/// 聚合诊断数字。全部来自内存与已加载的账本统计，**没有一个字段是文件内容**。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Diag {
    pub app_version: String,
    /// 规则指纹（块域 | 实体记录 | 光尘记录）—— 分叉排查第一手
    pub rules: String,
    pub os: String,
    pub arch: String,
    pub height: u64,
    pub peers: usize,
    /// 在场邻居数（在场不进共识，但"连上了几个人"对反馈有用）
    pub present: usize,
    pub entity_lines: usize,
    pub dust_lines: usize,
    pub entity_bad_lines: usize,
    pub dust_bad_lines: usize,
    /// 反馈那一刻钱包是否解锁：只影响能不能复现某些操作，不指向任何密钥
    pub unlocked: bool,
    /// 数据目录是否在同机多开实例里（node-N）：只报这个布尔，不报路径
    pub multi_instance: bool,
}

/// 产出反馈包 JSON。`include_addr` 为假时 `addr` 一律丢弃，不靠调用方传空串。
pub fn build(d: &Diag, text: &str, has_shot: bool, include_addr: bool, addr: &str, ts: u64) -> serde_json::Value {
    let clean: String = text.chars().take(TEXT_MAX).collect();
    serde_json::json!({
        "kind": FEEDBACK_KIND,
        "ts_unix": ts,
        "problem": clean,
        "screenshot_attached": has_shot,
        "address": if include_addr { addr } else { "" },
        "address_included": include_addr,
        "app_version": d.app_version,
        "rules": d.rules,
        "os": d.os,
        "arch": d.arch,
        "height": d.height,
        "peers": d.peers,
        "present": d.present,
        "entity_lines": d.entity_lines,
        "dust_lines": d.dust_lines,
        "entity_bad_lines": d.entity_bad_lines,
        "dust_bad_lines": d.dust_bad_lines,
        "unlocked": d.unlocked,
        "multi_instance": d.multi_instance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag() -> Diag {
        Diag {
            app_version: "0.5.0".into(),
            rules: "b5|QL-ENTREC-v4|QL-DUST-v1".into(),
            os: "macos".into(),
            arch: "aarch64".into(),
            height: 141,
            peers: 2,
            present: 1,
            entity_lines: 37,
            dust_lines: 12,
            entity_bad_lines: 0,
            dust_bad_lines: 0,
            unlocked: true,
            multi_instance: false,
        }
    }

    /// 白名单：多一个键都要让测试红，因为"顺手塞点东西进去"就是这样开始的
    #[test]
    fn the_bundle_carries_exactly_the_allowlist() {
        let v = build(&diag(), "地面卡住了", false, false, "ql1abcdefghijklmnopqrstuvwxyz", 1_700_000_000);
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "address",
                "address_included",
                "app_version",
                "arch",
                "dust_bad_lines",
                "dust_lines",
                "entity_bad_lines",
                "entity_lines",
                "height",
                "kind",
                "multi_instance",
                "os",
                "peers",
                "present",
                "problem",
                "rules",
                "screenshot_attached",
                "ts_unix",
                "unlocked",
            ]
        );
    }

    /// 最要紧的一条：包里不许出现任何密钥材料或账本原文的形状。
    /// 这里逐个点名密钥库与账本记录里的字段名，防止"改了结构顺手带出去"。
    #[test]
    fn the_bundle_says_nothing_about_keys_or_ledgers() {
        let v = build(&diag(), "挖矿按钮点了没反应", true, true, "ql1example", 1_700_000_000);
        let s = v.to_string();
        for forbidden in [
            "ct_hex",
            "nonce_hex",
            "salt_hex",
            "check_hex",
            "seed",
            "mnemonic",
            "keystore",
            "signing",
            "sk",
            "prev_hash",
            "entities.jsonl",
            "dust.jsonl",
            "blocks.jsonl",
        ] {
            assert!(!s.contains(forbidden), "反馈包里出现了不该有的字段：{forbidden}");
        }
    }

    /// 地址是个人的：不勾就不带，传进来也没用
    #[test]
    fn an_address_is_carried_only_when_explicitly_asked() {
        let addr = "ql1abcdefghijklmnopqrstuvwxyz";
        let off = build(&diag(), "x", false, false, addr, 1);
        assert_eq!(off["address"], "");
        assert_eq!(off["address_included"], false);
        let on = build(&diag(), "x", false, true, addr, 1);
        assert_eq!(on["address"], addr);
    }

    /// 网络层里没有任何能收这个包的端点，所以"提交"只能是本地导出。
    /// 这条测试存在的意义是：真加了上传通道时，它会把文档口径逼着一起改。
    #[test]
    fn a_long_problem_is_cut_not_dropped() {
        let long = "啊".repeat(TEXT_MAX + 500);
        let v = build(&diag(), &long, false, false, "", 1);
        let got = v["problem"].as_str().unwrap();
        assert_eq!(got.chars().count(), TEXT_MAX, "超长要截断，不能整段丢");
        assert!(got.starts_with("啊啊啊"));
    }
}
