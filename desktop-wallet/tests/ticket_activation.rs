//! v6 激活闸门（DESIGN.md §9.33）：真的把块高挖过激活高度，证明——
//! ① 出块人自己产出的块**带着**一张可被别人独立复验的门票（票原件进块、指纹进块哈希）；
//! ② 空机器整链重放时逐块复验这张票，摘掉票的快照整条拒收；
//! ③ 门票指纹改一个字节，块哈希就重算不回来（票不是可以事后贴上去的标签）。
//!
//! 这里没有绕过任何判据：挖矿走的是 `mine_block`（net.rs 的出块循环同一个入口），
//! 重放走的是 `install_snapshot`（联邦追账同一个入口）。
//!
//! 证不到的一半也写在这：本文件是**一个节点**出块、另一个节点重放；
//! 两机在同一回合里抢票（谁票小谁出块）要跑真网络，由 `relay_node.rs` 与手工双机彩排覆盖。

use qlight_wallet_lib::simchain::{self, BlockLine};
use std::path::PathBuf;

fn tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let d = std::env::temp_dir().join(format!(
        "qlight-v6act-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn mining_past_activation_emits_blocks_with_a_verifiable_ticket() {
    let dir = tmp("miner");
    let keystore = dir.join("ledger.json");
    let blocks = dir.join("blocks.jsonl");
    let chain = simchain::SimChain::new(keystore, blocks.clone());
    let core = {
        // 一个真钱包：门票必须由真私钥签出，测试不能拿假公钥糊过去
        let (_, c) = qlight_wallet_lib::keystore::create_wallet(&dir.join("keystore.json"), "v6激活彩排-2026")
            .expect("建钱包");
        c
    };

    // 挖过激活高度：空块也要顶过去（没有矿签就没有发行，判据不看发行）
    let t0 = std::time::Instant::now();
    while chain.height() <= simchain::BLOCK_V_V6_ACTIVATION {
        chain.mine_block(&core).expect("空块也应出块");
    }
    println!(
        "顶到 {} 块用了 {:.1}s",
        chain.height(),
        t0.elapsed().as_secs_f64()
    );

    let (_, head, _, _) = chain.mine_block(&core).expect("激活后的块应出块");
    assert_eq!(head.version, simchain::BLOCK_V_TICKET, "过了激活高度就该出 v6 块");
    assert_eq!(head.ticket_vrf_hex.len(), 64, "v6 块必须声明门票指纹");

    // ① 块文件里那一行该带着票原件，而且第三方能独立复验
    let raw = std::fs::read_to_string(&blocks).unwrap();
    let last: BlockLine = serde_json::from_str(raw.lines().last().unwrap()).unwrap();
    let proof = last.ticket.clone().expect("v6 块行必须带门票原件");
    simchain::check_ticket_binding(
        head.height,
        &head.prev_hash_hex,
        &head.miner,
        &head.ticket_vrf_hex,
        &proof,
    )
    .expect("本机的块应当被本机的判据收下");

    // ③ 门票指纹是块哈希的输入：把它改一个 hex，第三方重算就对不回来
    assert!(head.txs.is_empty(), "本彩排没提交过交易，块内应为空");
    let mut vrf_bytes = [0u8; 32];
    vrf_bytes.copy_from_slice(&hex::decode(&head.vrf_hex).unwrap());
    let mut moved = last.header.clone();
    let mut fpr = moved.ticket_vrf_hex.clone();
    fpr.replace_range(0..1, if &fpr[0..1] == "0" { "1" } else { "0" });
    moved.ticket_vrf_hex = fpr;
    let recomputed = simchain::block_hash_of_header(&moved, &vrf_bytes, &simchain::tx_root_of(&[]))
        .expect("改后的指纹仍是 64 hex，形态合法");
    assert_ne!(
        hex::encode(recomputed),
        head.block_hash_hex,
        "改了门票指纹却还能算出同一个块哈希，说明票根本没进哈希"
    );

    // ② 空机器整链重放：逐块复验门票，账对得上
    let dst_dir = tmp("replay");
    let dst = simchain::SimChain::new(dst_dir.join("ledger.json"), dst_dir.join("blocks.jsonl"));
    let h = dst.install_snapshot(&raw).expect("带票的快照应整链采用");
    assert_eq!(h, chain.height(), "重放不该少吃一块");
    assert_eq!(dst.tip_hash_hex_pub(), chain.tip_hash_hex_pub(), "两机同一条链");
    assert_eq!(
        dst.balance_of(&core.address),
        chain.balance_of(&core.address),
        "重放出来的余额一致"
    );

    // 摘掉最后一块的票：块头一个字没改，快照仍然必须整条拒收。
    // 每次都用一台空机器：免得"快照不比本机长"这种无关理由替判据背下断言。
    let mut rows: Vec<String> = raw.lines().map(str::to_string).collect();
    let mut stripped = last.clone();
    stripped.ticket = None;
    *rows.last_mut().unwrap() = serde_json::to_string(&stripped).unwrap();
    let bare_dir = tmp("replay-stripped");
    let bare = simchain::SimChain::new(bare_dir.join("ledger.json"), bare_dir.join("blocks.jsonl"));
    let err = bare
        .install_snapshot(&rows.join("\n"))
        .err()
        .expect("摘掉门票的快照必须被拒");
    assert!(err.contains("门票"), "拒收理由该指向门票，实得：{err}");
    assert_eq!(bare.height(), 0, "整条快照拒收 = 一块都不落");

    // 把最后一块的门票指纹改一个 hex（票原件不动）：块哈希重算不回来，整条拒收
    let mut moved_line = last.clone();
    moved_line.header = moved;
    *rows.last_mut().unwrap() = serde_json::to_string(&moved_line).unwrap();
    let fpr_dir = tmp("replay-fpr");
    let fpr_chain = simchain::SimChain::new(fpr_dir.join("ledger.json"), fpr_dir.join("blocks.jsonl"));
    let err = fpr_chain
        .install_snapshot(&rows.join("\n"))
        .err()
        .expect("改过门票指纹的快照必须被拒");
    assert!(err.contains("哈希"), "拒收理由该指向块哈希，实得：{err}");

    for d in [dir, dst_dir, bare_dir, fpr_dir] {
        let _ = std::fs::remove_dir_all(&d);
    }
}
