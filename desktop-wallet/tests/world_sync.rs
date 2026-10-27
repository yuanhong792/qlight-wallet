//! 端到端：掉线期间错过的世界，靠「指纹公告 → 索要 → 逐条验后合并」补回来。
//!
//! 补的是推送模型的洞：邻居的新记录只在它签下的那一刻广播一次（`NetMsg::Entity`），
//! 本机当时不在，那块方块就永久丢了，而且**谁都不知道丢了**。这里验证的是发现机制
//! （集合指纹）与搬运机制（整本快照），裁判仍然是逐条验签验链，不是快照本身。

use qlight_wallet_lib::{keystore, net, simchain, world};
use std::path::PathBuf;

fn tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("qlight-sync-{tag}-{}-{nanos}", std::process::id()));
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

/// 造 n 个方块，返回整本记录（每台机器一份独立账本文件）
fn build(
    dir: &PathBuf,
    file: &str,
    core: &keystore::WalletCore,
    view: &world::ChainView,
    height: u64,
    n: u32,
) -> world::EntityLedger {
    let ledger = world::EntityLedger::new(dir.join(file));
    let pk_hash = world::signer_pk_hash(core);
    for k in 0..n {
        let name = format!("追赶方块 {k}");
        let d = draft(&name, 10 + k as u16, 20, height);
        ledger
            .commit_as(&pk_hash, view, |seq, prev| {
                world::sign_record(core, seq, &prev, &d)
            })
            .unwrap_or_else(|e| panic!("第 {k} 条免费记录应能落账：{e}"));
    }
    ledger
}

/// 掉线节点发现自己缺记录，并在合并后与对方指纹完全一致
#[test]
fn offline_node_discovers_the_gap_and_catches_up() {
    let dir = tmp("catchup");
    let chain = simchain::SimChain::new(dir.join("ledger.json"), dir.join("blocks.jsonl"));
    let core_a = keystore::open_core_from_seed(&[11u8; 32], &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
    for _ in 0..3 {
        assert!(chain.mine_block(&core_a).is_some(), "出块失败");
    }
    let view = world::ChainView::load(&chain.blocks_path_pub());

    // A 造了 3 个方块；B 全程掉线，一条推送都没收到
    let ledger_a = build(&dir, "entities-a.jsonl", &core_a, &view, chain.height(), 3);
    let ledger_b = world::EntityLedger::new(dir.join("entities-b.jsonl"));

    let head_a = world::set_digest(&ledger_a.load());
    let head_b = world::set_digest(&ledger_b.load());
    assert_eq!(head_a.0, 3, "A 的世界应有 3 条");
    assert_eq!(head_b.0, 0, "B 错过了一切");
    // 缺口检测：更完整的一方胜，落后方据此发起索要
    assert!(net::ent_head_wins(head_a.clone(), &head_b), "B 应当判定自己落后");
    assert!(
        !net::ent_head_wins(head_b.clone(), &head_a),
        "落后的一方不能被当成更完整，否则两边互刷整本账本"
    );

    // 搬运：整本实体账本压缩上网 → 解出记录列表 → 逐条验后合并
    let raw = std::fs::read(ledger_a.path()).unwrap();
    let snap = net::snapshot_pack(&raw).expect("快照打包失败");
    let list = net::ent_snap_records(&snap).expect("快照解析失败");
    assert_eq!(list.len(), 3, "快照应原样带出 3 条");

    let rep = ledger_b.merge(&list, &view).expect("合并失败");
    assert_eq!(rep.accepted, 3, "补账应收下全部 3 条");
    assert_eq!(rep.rejected, 0);

    // 对齐之后：指纹一致 → 双方都不"胜"，不会再发起第二_round 索要（防追赶风暴）
    let head_b2 = world::set_digest(&ledger_b.load());
    assert_eq!(head_b2, head_a, "追赶后两台机器的世界指纹必须一致");
    assert!(!net::ent_head_wins(head_b2.clone(), &head_a));
    assert!(!net::ent_head_wins(head_a.clone(), &head_b2));

    // 补回来的世界真的能重放（不是只多了几行字）
    let (map, report) = world::replay(&ledger_b.load(), &view);
    assert!(report.ok, "补账后的重放失败：{}", report.reason);
    assert_eq!(map.len(), 3);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 快照只是搬运工：里面混一条验不过的记录，本机账本一条都不长
#[test]
fn poisoned_snapshot_cannot_grow_the_ledger() {
    let dir = tmp("poison");
    let chain = simchain::SimChain::new(dir.join("ledger.json"), dir.join("blocks.jsonl"));
    let core = keystore::open_core_from_seed(&[12u8; 32], &[2, 2, 2, 2, 2, 2, 2, 2]).unwrap();
    for _ in 0..2 {
        chain.mine_block(&core);
    }
    let view = world::ChainView::load(&chain.blocks_path_pub());
    let ledger = build(&dir, "entities-src.jsonl", &core, &view, chain.height(), 2);

    let mut list = ledger.load();
    // 篡改最后一条的签名（结构合法、内容被改）
    let last = list.len() - 1;
    let sig = &mut list[last].sig_hex;
    let tail = sig.len() - 1;
    let c = sig.as_bytes()[tail];
    list[last].sig_hex = format!("{}{}", &sig[..tail], if c == b'0' { "1" } else { "0" });

    let before = world::set_digest(&list[..1]);
    let target = world::EntityLedger::new(dir.join("entities-dst.jsonl"));
    let rep = target.merge(&list, &view).expect("合并不应整体报错");
    assert_eq!(rep.accepted, 1, "只应收下那条真正验得过的");
    assert_eq!(rep.rejected, 1, "被篡改的那条必须拒收");
    assert_eq!(
        world::set_digest(&target.load()),
        before,
        "落盘的应当恰好是没被改的那一条"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 坏行整份丢弃：收下"能解析的一部分"会让本机指纹永远对不上，追赶变成风暴
#[test]
fn snapshot_with_a_bad_line_is_dropped_whole() {
    let dir = tmp("badline");
    let chain = simchain::SimChain::new(dir.join("ledger.json"), dir.join("blocks.jsonl"));
    let core = keystore::open_core_from_seed(&[13u8; 32], &[3, 3, 3, 3, 3, 3, 3, 3]).unwrap();
    chain.mine_block(&core);
    let view = world::ChainView::load(&chain.blocks_path_pub());
    let ledger = build(&dir, "entities-x.jsonl", &core, &view, chain.height(), 2);

    let mut raw = std::fs::read(ledger.path()).unwrap();
    raw.extend_from_slice(b"{ this is not json\n");
    let snap = net::snapshot_pack(&raw).unwrap();
    assert!(
        net::ent_snap_records(&snap).is_none(),
        "含坏行的快照必须整份拒绝，不能部分收下"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 指纹只认「记录集合」：落盘顺序不同、重复同一条，都不改变指纹
#[test]
fn digest_is_a_property_of_the_set_not_the_file_order() {
    let dir = tmp("digest");
    let chain = simchain::SimChain::new(dir.join("ledger.json"), dir.join("blocks.jsonl"));
    let core = keystore::open_core_from_seed(&[14u8; 32], &[4, 4, 4, 4, 4, 4, 4, 4]).unwrap();
    chain.mine_block(&core);
    let view = world::ChainView::load(&chain.blocks_path_pub());
    let ledger = build(&dir, "entities-d.jsonl", &core, &view, chain.height(), 4);

    let list = ledger.load();
    let base = world::set_digest(&list);
    let mut shuffled = list.clone();
    shuffled.reverse();
    assert_eq!(world::set_digest(&shuffled), base, "同一集合换个顺序，指纹不该变");

    let mut dup = list.clone();
    dup.push(list[1].clone());
    assert_eq!(
        world::set_digest(&dup),
        base,
        "同一条记录落两遍不该被数成两条，否则永远追不平"
    );

    let mut missing = list.clone();
    missing.pop();
    assert_ne!(world::set_digest(&missing), base, "少一条必须让指纹变化（缺口检测的依据）");
    assert!(net::ent_head_wins(base.clone(), &world::set_digest(&missing)));

    let _ = std::fs::remove_dir_all(&dir);
}
