//! 端到端：真链 → 真 ChainView → v4 免费实体记录落账。
//!
//! 走的是客户端命令背后的同一组函数——`SimChain`、`world::ChainView::load`、
//! `EntityLedger::commit_as`——而不是世界模块里手搓的假视图。它回答两个问题：
//! **建造是不是真的不花币**，以及**免费写下的世界能不能被任何人从块链独立重放**。

use qlight_wallet_lib::{keystore, simchain, world};
use std::path::PathBuf;

fn tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("qlight-e2e-{tag}-{}-{nanos}", std::process::id()));
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

/// 造一块石碑：链不付钱、余额不变、记录可被链视图重放
#[test]
fn free_world_record_lands_without_spending_a_cent() {
    let dir = tmp("free");
    let chain = simchain::SimChain::new(dir.join("ledger.json"), dir.join("blocks.jsonl"));
    let core = keystore::open_core_from_seed(&[7u8; 32], &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();

    for _ in 0..3 {
        assert!(chain.mine_block(&core).is_some(), "出块失败");
    }
    let before = chain.balance_of(&core.address);

    let view = world::ChainView::load(&chain.blocks_path_pub());
    let ledger = world::EntityLedger::new(dir.join("entities.jsonl"));
    let pk_hash = world::signer_pk_hash(&core);
    let d = draft("端到端石碑", 12, 34, chain.height());
    let rec = ledger
        .commit_as(&pk_hash, &view, |seq, prev| {
            world::sign_record(&core, seq, &prev, &d)
        })
        .expect("免费记录应能落账");

    assert_eq!(rec.height, chain.height(), "记录块高取自链头");
    assert!(rec.fee_tx_hex.is_empty(), "v4 记录不带任何付款凭据");
    assert_eq!(chain.balance_of(&core.address), before, "建造不该花币");

    // 同一用户再放一件：免费写入不是一次性
    let d2 = draft("第二块石头", 20, 34, chain.height());
    ledger
        .commit_as(&pk_hash, &view, |seq, prev| {
            world::sign_record(&core, seq, &prev, &d2)
        })
        .expect("第二条免费记录也应落账");

    // 独立重放：世界只由账本 + 链视图解出
    let (map, report) = world::replay(&ledger.load(), &view);
    assert!(report.ok, "重放失败：{}", report.reason);
    assert_eq!(map.len(), 2);
    assert_eq!(map[&rec.entity_id_hex].name, "端到端石碑");

    // 前端读的字段名必须与后端序列化一致（Rust 改名而 ui/app.js 没跟上时拦在这里）
    let mr = qlight_wallet_lib::commands::MineResult {
        record: rec.clone(),
        claim_tx_hex: "ab".repeat(32),
        x: rec.x,
        y: rec.y,
        layer: rec.layer,
        claim_height: chain.height() + 1,
        reward: 1,
        pace_floor: simchain::craft_floor(0),
        pace_next_floor: simchain::craft_floor(1),
    };
    let v = serde_json::to_value(&mr).unwrap();
    for k in [
        "record",
        "claim_tx_hex",
        "x",
        "y",
        "layer",
        "claim_height",
        "reward",
        "pace_floor",
        "pace_next_floor",
    ] {
        assert!(v.get(k).is_some(), "ui/app.js 读的字段 {k} 不在 MineResult 里");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 免费记录不能预支未来：声明块高超过链头 → 拒收且不落盘
#[test]
fn free_record_cannot_claim_a_future_height() {
    let dir = tmp("future");
    let chain = simchain::SimChain::new(dir.join("ledger.json"), dir.join("blocks.jsonl"));
    let core = keystore::open_core_from_seed(&[9u8; 32], &[9, 8, 7, 6, 5, 4, 3, 2]).unwrap();
    chain.mine_block(&core);

    let view = world::ChainView::load(&chain.blocks_path_pub());
    let ledger = world::EntityLedger::new(dir.join("entities.jsonl"));
    let d = draft("未来石碑", 1, 1, chain.height() + 1);
    let out = ledger.commit_as(&world::signer_pk_hash(&core), &view, |seq, prev| {
        world::sign_record(&core, seq, &prev, &d)
    });
    assert!(out.is_err(), "超过链头的免费记录竟被接受");
    assert!(ledger.load().is_empty(), "被拒的记录不能落盘");

    let _ = std::fs::remove_dir_all(&dir);
}
