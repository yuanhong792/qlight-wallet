//! 「挖第一个币」彩排：一台电脑、一个全新目录，从 0 走到余额大于 0，再走一次两机互转。
//!
//! 这里刻意只用 UI 走的那批**公开入口**（`keystore::create_wallet` → `SimChain::new` →
//! `submit_claim` → `mine_block` → `apply_remote_block`），不碰测试专用的内部把手——
//! 证的不是"某个函数返回了什么"，而是"业主照 README 点下去，第一个币真的会出来"。
//!
//! 证不到的一半也写在这：真机上 GUI 那几下点击、以及 mDNS 在真实 Wi-Fi 里的发现，
//! 都不在这个文件的能力范围内（出块循环住在 `net.rs` 的 swarm tick 里，要 `AppHandle`）。
//! 这个闸管的是判据与账本：干净环境能不能凭空长出第一个币、换一台机器能不能原样重放、
//! 改文件能不能多拿币。

use qlight_wallet_lib::{crypto, keystore, node, simchain, tx, world};
use std::path::PathBuf;

const PW: &str = "第一个币-2026-QLight";

fn tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let d = std::env::temp_dir().join(format!(
        "qlight-firstcoin-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 客户端怎么找矿，这里就怎么写：同一份 `chain_vrfs` + `seed_words` + `ore::scan`
fn pick_ore(chain: &simchain::SimChain, blocks_path: &PathBuf) -> (u16, u16, u16) {
    let vrfs = world::chain_vrfs(blocks_path);
    let words = world::terrain::seed_words(&simchain::genesis_hash_hex(), &vrfs);
    let taken = world::ore::Taken::new();
    let h = chain.height();
    *world::ore::scan(&words, h, &taken, 8)
        .first()
        .unwrap_or_else(|| panic!("块高 {h} 本回合无矿可挖（矿脉是概率性的，等下一块）"))
}

/// 公平启动：一台什么都没有的机器，密码建钱包 → 第一块 → 第一个币
#[test]
fn first_coin_from_a_clean_machine() {
    let dir = tmp("solo");
    let blocks = dir.join("blocks.jsonl");
    let (ks, miner) = keystore::create_wallet(&dir.join("keystore.json"), PW).expect("建钱包");
    assert!(dir.join("keystore.json").exists(), "密钥库要落盘");
    assert_eq!(
        (ks.kdf.as_str(), ks.cipher.as_str()),
        ("argon2id", "aes-256-gcm"),
        "私钥是 KDF + AEAD 加密落盘的，不是明文"
    );

    assert_eq!(
        keystore::unlock_wallet(&dir.join("keystore.json"), PW)
            .expect("同一个密码要能重开")
            .address,
        miner.address,
        "重开拿到的还是同一把钥匙——第一个币不会认别人"
    );
    assert!(keystore::unlock_wallet(&dir.join("keystore.json"), "错一个字符-2026-QLight").is_err(),
        "错密码必须解不开");

    let chain = simchain::SimChain::new(dir.join("ledger.json"), blocks.clone());
    assert_eq!(chain.height(), 0, "新目录只有创世那一行");
    assert_eq!(chain.total_minted(), 0, "公平启动：创世不预挖一个币");
    assert_eq!(chain.balance_of(&miner.address), 0, "新钱包余额为 0");
    assert_eq!(
        chain.tip_hash_hex_pub(),
        simchain::genesis_hash_hex(),
        "链尖就是锚死的创世哈希"
    );

    // ① 先交发行凭据（矿签），② 再出块——顺序反了就没有"这一笔发行"
    let (x, y, layer) = pick_ore(&chain, &blocks);
    let (_wire, digest) = chain
        .submit_claim(&miner, x, y, layer)
        .expect("零余额的新钱包也必须交得起矿签（矿签不付手续费）");
    assert_eq!(chain.mempool_len(), 1, "矿签在池里等进块");

    let (_event, header, wires, _dusts) = chain.mine_block(&miner).expect("第 1 块应出块");
    assert_eq!(header.height, 1);
    assert_eq!(wires.len(), 1, "矿签应随块一起带走");
    assert_eq!(chain.height(), 1);
    let minted = chain.total_minted();
    assert!(minted > 0, "第一个币要真的被发行出来");
    assert_eq!(
        minted, simchain::INIT_REWARD,
        "块高 1 的第一笔只结清一个块的排放：{} 微 = 初始排放档",
        minted
    );
    assert_eq!(
        chain.balance_of(&miner.address),
        minted,
        "发行只进挖矿者，且此刻全网只有他有钱"
    );
    // 钱位只有 `DECIMALS=9` 这一个口径（界面与命令层的除数都是 1e9）：
    // 这里曾拿 1e6 念数，于是文档把 3.33 QLX 写成了 3,330 QLX。
    println!(
        "第一个币：矿格 ({x},{y}) 第 {layer} 层 → 块高 1 发行 {minted} 微 QLX（{:.2} QLX），矿签 {}",
        minted as f64 / 1e9,
        &hex::encode(digest)[..16]
    );

    // ③ 换一台机器：同一份文件重放出同一条链（这才叫"任何机器同一片地"）
    let again = simchain::SimChain::new(dir.join("ledger.json"), blocks.clone());
    assert_eq!(again.height(), 1, "重放不新增也不少吃块");
    assert_eq!(again.total_minted(), minted, "重放不改变发行量");
    assert_eq!(again.balance_of(&miner.address), minted, "重放不改变余额");
    assert_eq!(again.tip_hash_hex_pub(), chain.tip_hash_hex_pub(), "链尖逐位相同");

    // ④ 反向对照：手改文件里那一块的发行量，不能凭空多拿币
    let forged = tmp("forged");
    let raw = std::fs::read_to_string(&blocks).unwrap();
    let bumped: String = raw
        .lines()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v.get("height").and_then(|h| h.as_u64()) == Some(1) {
                let mut v = v;
                v["minted_after"] = serde_json::json!(minted + 1_000_000_000);
                v["reward_total"] = serde_json::json!(minted + 1_000_000_000);
                return serde_json::to_string(&v).unwrap();
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(forged.join("blocks.jsonl"), bumped).unwrap();
    std::fs::copy(dir.join("ledger.json"), forged.join("ledger.json")).unwrap();
    let tampered = simchain::SimChain::new(forged.join("ledger.json"), forged.join("blocks.jsonl"));
    assert!(
        tampered.balance_of(&miner.address) <= minted,
        "改文件把发行量写大就得多拿币——这道闸必须挡住（实测余额 {} > {minted}）",
        tampered.balance_of(&miner.address)
    );
    assert_ne!(
        tampered.tip_hash_hex_pub(),
        chain.tip_hash_hex_pub(),
        "被改过的链不可能还有同一个链尖；相等说明这条文件根本没被读"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&forged);
}

/// 两台电脑：A 出块、B 逐条验后收块，再转一笔过去——B 不靠任何注册表也能独立验签
#[test]
fn two_machines_mine_and_transfer() {
    let dir_a = tmp("node-a");
    let dir_b = tmp("node-b");
    let blocks_a = dir_a.join("blocks.jsonl");

    let (_, a) = keystore::create_wallet(&dir_a.join("keystore.json"), PW).expect("A 建钱包");
    let (_, b) = keystore::create_wallet(&dir_b.join("keystore.json"), PW).expect("B 建钱包");
    let chain_a = simchain::SimChain::new(dir_a.join("ledger.json"), blocks_a.clone());
    let chain_b = simchain::SimChain::new(dir_b.join("ledger.json"), dir_b.join("blocks.jsonl"));
    assert_ne!(a.address, b.address, "两台机器两把钥匙");

    // A 单机出第一块（B 不在，B 之后靠对账补）
    let (x, y, layer) = pick_ore(&chain_a, &blocks_a);
    chain_a.submit_claim(&a, x, y, layer).expect("矿签入池");
    let (_e, h1, w1, d1) = chain_a.mine_block(&a).expect("A 出块 1");

    // B 补账：整链对账走的就是 apply_remote_block，逐条验签验链，不认快照本身。
    // 块 1 在 v6 激活高度（2000）之前：那一代的票既不在块里、也不是判据，传 None 是如实描述。
    chain_b
        .apply_remote_block(&h1, w1, d1, None, None)
        .expect("B 应收下 A 的块 1");
    assert_eq!(chain_b.height(), 1);
    assert_eq!(chain_b.tip_hash_hex_pub(), chain_a.tip_hash_hex_pub(), "两机链尖一致");
    assert_eq!(chain_b.balance_of(&a.address), chain_a.balance_of(&a.address));
    assert_eq!(chain_b.balance_of(&b.address), 0, "B 没挖过就该是 0");

    // A → B 转一笔（走 send_qlx 同一套：envelope → 摘要 → ML-DSA-65 签名 → submit）
    let amount = 25_000_000; // 25 QLX（微 QLX 计价）
    let from = simchain::bech32_decode_addr(&a.address).unwrap();
    let to = simchain::bech32_decode_addr(&b.address).unwrap();
    let envelope = tx::TxEnvelope::new(from, chain_a.pending_nonce(&a.address), chain_a.height() + 50)
        .transfer(to, amount);
    let digest = tx::tx_digest(&envelope);
    let sig = crypto::sign(&a.signing_key, &digest);
    chain_a.submit(&a, envelope.clone(), sig.clone()).expect("转账入池");
    // 联邦中继：交易原件（含公钥与地址盐）寄给对端——对端不靠注册表也能独立验签入池
    let wire = simchain::BlockWire::new(&envelope, &sig, &a.pk_bytes, &a.addr_salt);
    let relayed = chain_b
        .accept_remote_tx(wire)
        .expect("B 独立验签收下这笔它从没见过地址的交易");
    assert_eq!(relayed, digest, "两机算出的交易摘要一致");
    assert_eq!(chain_b.mempool_len(), 1, "B 的池里也有这一笔");

    // 块 2 交给 B 出：联邦里出块人由门票选举，不必是交易发起方——B 池里有那笔中继来的
    // 交易，它自己就能打包，A 反过来收 B 的块。这样手续费的去向也看得见。
    let (minted_before, bal_a_before) = (chain_a.total_minted(), chain_a.balance_of(&a.address));
    let (_e2, h2, w2, d2) = chain_b.mine_block(&b).expect("B 出块 2（含 A 那笔转账）");
    assert_eq!(chain_b.total_minted(), minted_before, "转账本身不发行");
    assert_eq!(w2.len(), 1, "块 2 带走那笔转账");
    // 反向对照先做：把块 2 的发行量改大寄给 A，A 必须拒收。放在收块之前是有讲究的——
    // 此刻 A 停在块 1，块高与前向都对得上，能拒它的只剩"发行与曲线不符/块哈希对不上"。
    let mut greedy = h2.clone();
    greedy.minted_after += 1_000_000;
    greedy.reward_total += 1_000_000;
    let refused = chain_a
        .apply_remote_block(&greedy, w2.clone(), vec![], None, None)
        .err()
        .expect("改过发行的块头不可能过验");
    println!("伪造块被拒的理由：{refused}");
    assert!(
        !(refused.contains("块高") || refused.contains("不衔接")),
        "拒的理由必须是发行/哈希，不是块高对不上：{refused}"
    );

    chain_a
        .apply_remote_block(&h2, w2, d2, None, None)
        .expect("A 收下 B 出的块 2");

    assert_eq!(chain_a.tip_hash_hex_pub(), chain_b.tip_hash_hex_pub(), "转账后两机同一条链");
    assert_eq!(
        chain_a.balance_of(&b.address),
        amount + simchain::FEE,
        "B 收到转账额，外加它作为出块人拿到的手续费"
    );
    assert_eq!(
        chain_b.balance_of(&b.address),
        chain_a.balance_of(&b.address),
        "同一地址在两本账上余额一致"
    );
    assert_eq!(
        chain_a.balance_of(&a.address) + amount + simchain::FEE,
        bal_a_before,
        "A 的减少 = 转账额 + 手续费（一分不蒸发，也不凭空多出来）"
    );

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// 在飞的矿签只许一张（§9.34 三进程真链实测暴露的脏池）
///
/// 实测里：75 个块、24 次落格、进块的只有 2 张——剩下的 22 张注定进不了块（矿签的
/// 地表判据每个块重画一次），却躺在池里被 5 秒一次的再广播原样推给全网，邻居们
/// 每张都真验一次签、再拒一次，三个进程一共刷出 328 条「拒收联邦交易」。
/// 这个闸把那条路径堵在源头：多签的那一张根本不该存在。
#[test]
fn only_one_claim_is_in_flight() {
    let dir = tmp("inflight");
    let blocks = dir.join("blocks.jsonl");
    let (_ks, wallet) = keystore::create_wallet(&dir.join("keystore.json"), PW).expect("建钱包");
    let core = std::sync::Arc::new(wallet);
    let chain = simchain::SimChain::new(dir.join("ledger.json"), blocks.clone());
    let entities = std::sync::Arc::new(world::EntityLedger::new(dir.join("entities.jsonl")));
    chain.attach_world(entities.clone());

    // ① 连按两次按钮：第二次必须被认出来是重复签
    let hop = node::mine_one_hop(&chain, &entities, &blocks, &core, 0, |_| {})
        .expect("干净创世的第一跳要交出凭据");
    assert_eq!(chain.pending_claims_of(&core.address), 1, "一张凭据在飞");
    assert!(chain.is_pooled(&hop.claim_digest), "凭据的原件就在自家池里");
    let again = node::mine_one_hop(&chain, &entities, &blocks, &core, 0, |_| {});
    let err = match again {
        Ok(_hop) => panic!("第二次按必须被拒，不该又签出一张凭据"),
        Err(e) => e,
    };
    assert!(err.starts_with("上一张矿签"), "第二次按必须被拒：{err}");
    assert_eq!(chain.pending_claims_of(&core.address), 1, "拒了就不许多签一张");

    // ② 出块收下它：池子当场干净，此后没有任何垃圾可广播
    let (event, _header, wires, _dusts) = chain.mine_block(&core).expect("第 1 块应出块");
    assert_eq!(wires.len(), 1, "块 1 带走了那张凭据");
    assert_eq!(event.minted, simchain::INIT_REWARD, "第一笔结清一个块的排放");
    assert!(!chain.is_pooled(&hop.claim_digest), "已入账的凭据不许还躺在池里");
    assert_eq!(chain.pending_claims_of(&core.address), 0, "在飞数归零");
    assert_eq!(chain.mempool_len(), 0, "池子空了");

    // ③ 节奏没到的那几个块：一个字节都不该进池
    for h in 2..=6u64 {
        chain.mine_block(&core).expect("空块照出");
        assert_eq!(chain.height(), h);
        assert_eq!(chain.mempool_len(), 0, "块高 {h} 之后池里不该有残留");
    }
    println!(
        "在飞凭据闸门：块高 {} 时池子 0 笔，第一笔发行 {} 微 QLX",
        chain.height(),
        chain.total_minted()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
