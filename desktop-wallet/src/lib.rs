//! QLight Wallet · 量子闪电桌面钱包（M2.1）
//!
//! 架构：Tauri 2 + Rust 内核（纯 Rust 后量子栈）
//! - 签名：ML-DSA-65（RustCrypto ml-dsa，FIPS 204）
//! - 哈希：SHAKE-256（FIPS 202）
//! - 地址：ql1（bech32，DESIGN.md §6.1）
//! - 交易：QL-TX-v1 canonical CBOR（§14.2）
//! - 链：内置持久化账本 regtest（A 档）+ libp2p 联邦网络（B 档：mDNS 发现 +
//!   gossipsub 中继交易/块 + 门票选举出块人 + 整链对账）

pub mod commands;
pub mod crypto;
pub mod keystore;
pub mod net;
pub mod simchain;
pub mod tx;
pub mod world;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tauri::Manager;

pub fn run() {
    // 诊断日志：RUST_LOG=debug 时启用（默认关闭，普通用户无感；libp2p 走 tracing）
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    tauri::Builder::default()
        .setup(|app| {
            let base_dir = app
                .path()
                .app_data_dir()
                .expect("获取应用数据目录失败");
            std::fs::create_dir_all(&base_dir).ok();

            // B 档本地联邦演示：同机多开钱包实例时，第二/第三个实例自动落到
            // node-1/node-2 子目录（文件锁判互斥），账本与密钥互不掺和。
            let data_dir = claim_instance_dir(&base_dir);
            eprintln!("[QLight] 本实例数据目录：{}", data_dir.display());

            let keystore_path = data_dir.join("keystore.json");
            let ledger_path = data_dir.join("ledger.json");
            let blocks_path = data_dir.join("blocks.jsonl");
            let entities_path = data_dir.join("entities.jsonl");
            let chain =
                simchain::SimChain::new(ledger_path.clone(), blocks_path.clone());

            // 解锁核心槽：AppState 与联邦网络共用同一个 Arc（锁定/解锁一处写两侧同步）
            let core_slot: Arc<RwLock<Option<Arc<keystore::WalletCore>>>> =
                Arc::new(RwLock::new(None));
            // 引导注册表放公共根目录：所有实例（根目录 + node-N）都读写同一份，
            // 互相读到对方的「节点号 + 监听地址」就 TCP 直连（macOS 同机组播不通的替代正路）
            let entities = Arc::new(world::EntityLedger::new(entities_path));
            let dust_path = data_dir.join("dust.jsonl");
            let dust = Arc::new(world::dust::DustLedger::new(dust_path));
            // v2 出块要向整本实体账本承诺世界（块哈希绑 `entity_root`），
            // 所以链必须在网络与命令层拿到副本之前就先看到它
            chain.attach_world(entities.clone());
            // v5 起出块要把光尘记录带给全网：链必须看得到同一个光尘账本
            chain.attach_dust(dust.clone());
            let net = net::Net::start(
                app.handle().clone(),
                chain.clone(),
                entities.clone(),
                dust.clone(),
                core_slot.clone(),
                base_dir.join("peers-registry.json"),
            );

            app.manage(commands::AppState {
                data_dir,
                keystore_path,
                ledger_path,
                blocks_path,
                entities: entities.clone(),
                dust,
                chain: chain.clone(),
                core: core_slot,
                net,
            });
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::wallet_status,
            commands::chain_state,
            commands::tx_history,
            commands::tx_pending,
            commands::create_wallet,
            commands::unlock_wallet,
            commands::lock_wallet,
            commands::send_qlx,
            commands::export_pk_hash,
            commands::receive_qr,
            commands::export_backup,
            commands::import_backup,
            commands::net_status,
            commands::app_version,
            commands::world_snapshot,
            commands::world_place,
            commands::world_rename,
            commands::world_move,
            commands::world_release,
            commands::world_mine_once,
            commands::world_proof,
            commands::world_dust_state,
            commands::world_resources,
            commands::world_harvest,
            commands::world_dust_send,
            commands::world_trader_state,
            commands::world_trader_buy,
            commands::pool_state,
            commands::pool_add,
            commands::pool_remove,
            commands::pool_swap,
        ])
        .run(tauri::generate_context!())
        .expect("QLight Wallet 启动失败");
}

/// 多实例数据目录判定：谁先锁住 base_dir 根目录谁用根目录；
/// 抢不到锁（已有实例在跑）的自动落到 node-N 子目录，各自独立账本/密钥库。
fn claim_instance_dir(base: &PathBuf) -> PathBuf {
    use fs2::FileExt;

    fn try_lock(dir: &PathBuf) -> bool {
        std::fs::create_dir_all(dir).ok();
        match std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(dir.join("node.lock"))
        {
            Ok(f) => match f.try_lock_exclusive() {
                Ok(()) => {
                    // 故意泄漏文件句柄：进程存活期间一直持锁
                    std::mem::forget(f);
                    true
                }
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    if try_lock(base) {
        return base.clone();
    }
    for k in 1..=8 {
        let d = base.join(format!("node-{k}"));
        if try_lock(&d) {
            eprintln!("[QLight] 检测到已有钱包实例在运行，本实例数据目录：{}", d.display());
            return d;
        }
    }
    // 兜底：带时间戳的独立目录（理论上到不了这里）
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let d = base.join(format!("node-{tag}"));
    std::fs::create_dir_all(&d).ok();
    d
}
