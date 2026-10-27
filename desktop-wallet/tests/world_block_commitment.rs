//! `entity_root` 进块（块格式 v2）：整本实体账本从此被共识点名。
//!
//! 补的是"币即世界"最大的那堵墙：记录逐条验签是真的，但整份 `entities.jsonl`
//! 可以被离线换成另一本而链看不出来。v2 起，出块人把自己此刻的世界集合指纹
//! （`QL-ENTSET-V1`）与条数绑进块哈希（域 `QL-BLOCK-V3`）——从此"谁在哪个高度认
//! 哪一份世界"永久可归责，条数倒退当场就是"想把世界清空"。
//!
//! 另一头同样重要：**旧块不重写**。v0 仍在 `QL-BLOCK-V1`、v1 仍在 `QL-BLOCK-V2`
//! 域下重放，这条由测试里那条 v1 块钉住。

use qlight_wallet_lib::{keystore, simchain, world};
use std::path::PathBuf;
use std::sync::Arc;

fn tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("qlight-commit-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn draft<'a>(name: &'a str, x: u16, y: u16, height: u64) -> world::Draft<'a> {
    world::Draft {
        op: world::op::CLAIM,
        entity_id_hex: "",
        parent_hex: "",
        name,
        x,
        y,
        layer: 2,
        kind: 0,
        height,
        ts: 1,
    }
}

/// 64 hex → 32 字节（测试里反复要手工还原承诺/指纹）
fn h32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&hex::decode(s).unwrap());
    out
}

/// 块头行（blocks.jsonl 的一行 = 头 + 可选原件）
fn line(h: &simchain::BlockHeader) -> String {
    let l = simchain::BlockLine {
        header: h.clone(),
        wires: vec![],
    };
    format!("{}\n", serde_json::to_string(&l).unwrap())
}

/// 造一个只承诺"空世界"的 v1 块（老格式，域 `QL-BLOCK-V2`）：证明 v2 不重写历史
fn v1_block(prev_hex: &str, height: u64) -> simchain::BlockHeader {
    let vrf = [5u8; 32];
    let root = simchain::tx_root_of(&[]);
    let hash = simchain::block_hash_of_ver(simchain::BLOCK_V_MINING, prev_hex, height, 0, &vrf, &root);
    simchain::BlockHeader {
        version: simchain::BLOCK_V_MINING,
        height,
        ts_unix: simchain::GENESIS_TS_UNIX + height * 2,
        prev_hash_hex: prev_hex.to_string(),
        block_hash_hex: hex::encode(hash),
        vrf_hex: hex::encode(vrf),
        minted_after: 0,
        reward_total: 0,
        miner: "ql1v1miner".to_string(),
        entity_root_hex: String::new(),
        entity_count: 0,
        txs: vec![],
    }
}

/// 造一个承诺指定世界的 v2 块（域 `QL-BLOCK-V3`）
fn v2_block(
    prev_hex: &str,
    height: u64,
    entity_root_hex: &str,
    entity_count: u64,
) -> simchain::BlockHeader {
    let vrf = [6u8; 32];
    let root = simchain::tx_root_of(&[]);
    let mut er = [0u8; 32];
    er.copy_from_slice(&hex::decode(entity_root_hex).unwrap());
    let hash = simchain::block_hash_of_world(
        prev_hex,
        height,
        0,
        &vrf,
        &root,
        &er,
        entity_count,
    );
    simchain::BlockHeader {
        version: simchain::BLOCK_V,
        height,
        ts_unix: simchain::GENESIS_TS_UNIX + height * 2,
        prev_hash_hex: prev_hex.to_string(),
        block_hash_hex: hex::encode(hash),
        vrf_hex: hex::encode(vrf),
        minted_after: 0,
        reward_total: 0,
        miner: "ql1v2miner".to_string(),
        entity_root_hex: entity_root_hex.to_string(),
        entity_count,
        txs: vec![],
    }
}

/// 出块人把自己眼里的世界写进块，并且第三方能独立复算这块哈希
#[test]
fn the_world_the_producer_sees_is_written_into_the_block() {
    let dir = tmp("mine");
    let chain = simchain::SimChain::new(dir.join("ledger.json"), dir.join("blocks.jsonl"));
    let core = keystore::open_core_from_seed(&[21u8; 32], &[1, 1, 2, 3, 5, 8, 13, 21]).unwrap();
    let entities = Arc::new(world::EntityLedger::new(dir.join("entities.jsonl")));
    chain.attach_world(entities.clone());

    chain.mine_block(&core).expect("第 1 块应出块");
    let view = world::ChainView::load(&chain.blocks_path_pub());
    let pk_hash = world::signer_pk_hash(&core);
    for k in 0..2u16 {
        let name = format!("承诺方块 {k}");
        let d = draft(&name, 30 + k, 40, chain.height());
        entities
            .commit_as(&pk_hash, &view, |seq, prev| {
                world::sign_record(&core, seq, &prev, &d)
            })
            .expect("免费记录应能落账");
    }

    let (_, head, _) = chain.mine_block(&core).expect("第 2 块应出块");
    let (count, digest) = world::set_digest(&entities.load());
    assert_eq!(head.version, simchain::BLOCK_V, "现行出块必须是 v2");
    assert_eq!(head.entity_count, count, "块头条数 = 出块人账本条数");
    assert_eq!(head.entity_root_hex, digest, "块头指纹 = 出块人账本指纹");

    // 独立复算：只靠块头字段就能重算出同一个块哈希（换世界 = 换块，第三方一算就知道）
    let mut vrf = [0u8; 32];
    vrf.copy_from_slice(&hex::decode(&head.vrf_hex).unwrap());
    let again = simchain::block_hash_of_world(
        &head.prev_hash_hex,
        head.height,
        head.minted_after,
        &vrf,
        &simchain::tx_root_of(&[]),
        &h32(&head.entity_root_hex),
        head.entity_count,
    );
    assert_eq!(hex::encode(again), head.block_hash_hex);

    // 反向对照：同一个块，只把世界指纹换一个字节，块哈希就变了 ——
    // 说明"离线换掉 entities.jsonl"这件事从此在块上留痕
    let mut alt = vrf;
    alt[0] ^= 1;
    let shifted = simchain::block_hash_of_world(
        &head.prev_hash_hex,
        head.height,
        head.minted_after,
        &alt,
        &simchain::tx_root_of(&[]),
        &h32(&head.entity_root_hex),
        head.entity_count,
    );
    assert_ne!(hex::encode(shifted), head.block_hash_hex);

    // 换一份世界再出块：块哈希跟着变（世界承诺不是装饰品）
    let other = world::EntityLedger::new(dir.join("entities-other.jsonl"));
    let (other_count, other_digest) = world::set_digest(&other.load());
    assert_eq!(other_count, 0);
    assert_ne!(other_digest, digest, "空世界与两格世界必须是两个承诺");

    let _ = std::fs::remove_dir_all(&dir);
}

/// v0 创世 + v1 旧块 + v2 新块混在同一条链里，逐块按各自的域重放通过
#[test]
fn v1_blocks_still_replay_under_their_own_domain() {
    let dir = tmp("mixed");
    let chain = simchain::SimChain::new(dir.join("ledger.json"), dir.join("blocks.jsonl"));
    let core = keystore::open_core_from_seed(&[22u8; 32], &[2, 3, 5, 7, 11, 13, 17, 19]).unwrap();
    chain.mine_block(&core).expect("本机先出一块 v2");

    let g = simchain::genesis_header();
    let b1 = v1_block(&g.block_hash_hex, 1);
    let (count, digest) = chain.world_head();
    let b2 = v2_block(&b1.block_hash_hex, 2, &digest, count);
    let text = format!("{}{}{}", line(&g), line(&b1), line(&b2));

    let h = chain
        .install_snapshot(&text)
        .expect("混版本链应逐块重放通过（旧块不重写）");
    assert_eq!(h, 2);
    // v1 那条的哈希仍在 `QL-BLOCK-V2` 域：换域就算不出来
    let wrong = simchain::block_hash_of_world(
        &b1.prev_hash_hex,
        b1.height,
        b1.minted_after,
        &hex::decode(&b1.vrf_hex).unwrap()[..],
        &simchain::tx_root_of(&[]),
        &[0u8; 32],
        0,
    );
    assert_ne!(hex::encode(wrong), b1.block_hash_hex, "v1 块不许被重新解释成 v2");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 篡改与倒退：改一个 hex 就整份快照拒收；条数倒退 = 想把世界清空，同样拒收
#[test]
fn tampered_root_and_count_regression_are_refused() {
    let dir = tmp("refuse");
    let chain = simchain::SimChain::new(dir.join("ledger.json"), dir.join("blocks.jsonl"));

    let g = simchain::genesis_header();
    let b1 = v1_block(&g.block_hash_hex, 1);
    let full = world::set_digest(&[]).1; // 空世界指纹（64 hex，形态合法）
    let b2 = v2_block(&b1.block_hash_hex, 2, &full, 7);
    let good = format!("{}{}{}", line(&g), line(&b1), line(&b2));
    chain
        .install_snapshot(&good)
        .expect("正常混版本快照应先通过（否则下面的拒收没有对照意义）");

    // 篡改：把块 2 的世界指纹改掉，块哈希就对不上 → 整份拒收
    let tampered = good.replace(&format!("\"entity_root_hex\":\"{full}\""), "\"entity_root_hex\":\"0000000000000000000000000000000000000000000000000000000000000000\"");
    assert_ne!(tampered, good, "篡改构造失败：没找到待替换的指纹字段");
    let dir2 = tmp("refuse2");
    let chain2 = simchain::SimChain::new(dir2.join("ledger.json"), dir2.join("blocks.jsonl"));
    assert!(
        chain2.install_snapshot(&tampered).is_err(),
        "改了世界指纹的 v2 块必须被拒收"
    );

    // 倒退：块 2 承诺 7 条，块 3 只认 6 条 → append-only 世界做不到，拒收
    let b3 = v2_block(&b2.block_hash_hex, 3, &full, 6);
    let back = format!("{}{}{}{}", line(&g), line(&b1), line(&b2), line(&b3));
    let dir3 = tmp("refuse3");
    let chain3 = simchain::SimChain::new(dir3.join("ledger.json"), dir3.join("blocks.jsonl"));
    assert!(
        chain3.install_snapshot(&back).is_err(),
        "条数倒退的 v2 链必须被拒收"
    );
    // 对照组：除了条数改成只增不减，其余一字不动 —— 必须通过。
    // 这一对才真正说明上面拒收的原因是"倒退"，不是别的。
    let b3grow = v2_block(&b2.block_hash_hex, 3, &full, 9);
    let grown = format!("{}{}{}{}", line(&g), line(&b1), line(&b2), line(&b3grow));
    let dir5 = tmp("refuse5");
    let chain5 = simchain::SimChain::new(dir5.join("ledger.json"), dir5.join("blocks.jsonl"));
    assert_eq!(
        chain5.install_snapshot(&grown).expect("条数只增的 v2 链应当通过"),
        3
    );

    // 形态：声称 v2 却没有 64 hex 的承诺 → 拒收
    let mut empty = v2_block(&b1.block_hash_hex, 2, &full, 1);
    empty.entity_root_hex = String::new();
    let shape = format!("{}{}{}", line(&g), line(&b1), line(&empty));
    let dir4 = tmp("refuse4");
    let chain4 = simchain::SimChain::new(dir4.join("ledger.json"), dir4.join("blocks.jsonl"));
    assert!(
        chain4.install_snapshot(&shape).is_err(),
        "v2 块不许不承诺世界"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
    let _ = std::fs::remove_dir_all(&dir3);
    let _ = std::fs::remove_dir_all(&dir4);
    let _ = std::fs::remove_dir_all(&dir5);
}

/// 离线把整本 entities.jsonl 换掉：链上那块块的承诺当场对不上（可检测、可归责）
#[test]
fn an_offline_ledger_swap_breaks_against_the_committed_root() {
    let dir = tmp("swap");
    let chain = simchain::SimChain::new(dir.join("ledger.json"), dir.join("blocks.jsonl"));
    let core = keystore::open_core_from_seed(&[23u8; 32], &[3, 1, 4, 1, 5, 9, 2, 6]).unwrap();
    let entities = Arc::new(world::EntityLedger::new(dir.join("entities.jsonl")));
    chain.attach_world(entities.clone());

    chain.mine_block(&core).expect("出块");
    let view = world::ChainView::load(&chain.blocks_path_pub());
    let pk_hash = world::signer_pk_hash(&core);
    for k in 0..3u16 {
        let name = format!("石碑 {k}");
        let d = draft(&name, 50 + k, 60, chain.height());
        entities
            .commit_as(&pk_hash, &view, |seq, prev| {
                world::sign_record(&core, seq, &prev, &d)
            })
            .expect("落账");
    }
    let (_, head, _) = chain.mine_block(&core).expect("出块");
    assert_eq!(head.entity_count, 3);

    // 有人离线把账本截短成 1 条（每一行都仍然自洽，只是少了两条）
    let kept = entities.load();
    let one = format!("{}\n", serde_json::to_string(&kept[0]).unwrap());
    std::fs::write(entities.path(), one).expect("替换账本");

    let (count, digest) = chain.world_head();
    assert_eq!(count, 1, "本机世界确实被换小了");
    assert_ne!(digest, head.entity_root_hex, "块上的承诺当场对不上：换档不是无痕的");
    assert_eq!(
        chain.height(),
        head.height,
        "承诺不参与本机重放：链本身不因此停摆（liveness 不被世界追赶绑架）"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
