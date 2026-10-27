//! Tauri 命令层：钱包生命周期 + 链状态 + 转账 + 挖矿心跳
//!
//! 对应 DESIGN.md §14.1 的钱包侧口径（本地闭环版）：
//! - getBalance  → chain_state
//! - sendRawTx   → send_qlx（构造信封 → 签名 → submit → 结算）

use crate::crypto;
use crate::keystore::{self, Keystore, WalletCore};
use crate::net;
use crate::simchain::{self, bech32_decode_addr, BlockEvent, SimChain};
use crate::tx;
use crate::world;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::{DialogExt, FilePath};

/// 应用共享状态
pub struct AppState {
    pub data_dir: PathBuf,
    pub keystore_path: PathBuf,
    pub ledger_path: PathBuf,
    pub blocks_path: PathBuf,
    /// 世界实体账本（entities.jsonl，append-only 哈希链；DESIGN.md §9.11）
    pub entities: Arc<world::EntityLedger>,
    pub chain: Arc<SimChain>,
    /// 解锁核心槽（与 net 层共用同一个 Arc<RwLock>：锁定/解锁两侧自动同步）
    pub core: Arc<RwLock<Option<Arc<WalletCore>>>>,
    /// 联邦网络句柄
    pub net: Arc<net::Net>,
}

type AppCtx<'a> = State<'a, AppState>;

// ---------------------------------------------------------------- 状态查询

#[derive(serde::Serialize)]
pub struct WalletStatus {
    pub has_keystore: bool,
    pub unlocked: bool,
}

#[tauri::command]
pub fn wallet_status(state: AppCtx) -> WalletStatus {
    WalletStatus {
        has_keystore: state.keystore_path.exists(),
        unlocked: state.core.read().unwrap().is_some(),
    }
}

#[derive(serde::Serialize)]
pub struct ChainState {
    pub unlocked: bool,
    pub address: Option<String>,
    pub balance: u64,
    pub height: u64,
    pub total_minted: u64,
    pub reward_per_block: u64,
    /// 本机下一次挖矿实际会发行多少（按这把钥匙自己的节奏窗口结算，已含硬顶截断）
    pub claim_reward: u64,
    /// 本机下一笔挖矿最早可结算的块高
    pub next_claim_height: u64,
    /// 本机当前节奏档（两次发行至少间隔的块数）
    pub claim_floor: u64,
    pub block_interval_secs: u64,
    pub decimals: u64,
    pub fee: u64,
    pub hard_cap: u64,
    pub started_unix: u64,
}

#[tauri::command]
pub fn chain_state(state: AppCtx) -> ChainState {
    let core = state.core.read().unwrap().clone();
    let address = core.as_ref().map(|c| c.address.clone());
    let balance = address
        .as_ref()
        .map(|a| state.chain.balance_of(a))
        .unwrap_or(0);
    let height = state.chain.height();
    let miner_addr = address.clone().unwrap_or_default();
    let (_, claim_reward) = state.chain.claim_preview(&miner_addr);
    let next_claim_height = state.chain.next_claim_height(&miner_addr);
    let (_, claim_floor) = state.chain.claim_pace(&miner_addr);
    ChainState {
        unlocked: core.is_some(),
        address,
        balance,
        height,
        total_minted: state.chain.total_minted(),
        reward_per_block: simchain::reward_at(height),
        claim_reward,
        next_claim_height,
        claim_floor,
        block_interval_secs: simchain::BLOCK_INTERVAL_SECS,
        decimals: simchain::DECIMALS,
        fee: simchain::FEE,
        hard_cap: simchain::HARD_CAP,
        started_unix: state.chain.ledger_started(),
    }
}

#[derive(serde::Serialize)]
pub struct TxRecordDto {
    pub hash_short: String,
    pub height: u64,
    pub ts_unix: u64,
    pub direction: String, // "out" | "in"
    pub counterparty: String,
    /// 全量明细：完整地址与完整交易哈希（活动页点击复制用）
    pub from: String,
    pub to: String,
    pub hash: String,
    pub amount: u64,
    pub fee: u64,
}

#[tauri::command]
pub fn tx_history(state: AppCtx) -> Vec<TxRecordDto> {
    let core = state.core.read().unwrap().clone();
    let my = match core {
        Some(c) => c.address.clone(),
        None => return vec![],
    };
    state
        .chain
        .records()
        .iter()
        .map(|r| TxRecordDto {
            hash_short: format!("{}…{}", &r.hash_hex[..10], &r.hash_hex[54..]),
            height: r.height,
            ts_unix: r.ts_unix,
            direction: if r.from == my { "out" } else { "in" }.into(),
            counterparty: if r.from == my {
                r.to.clone()
            } else {
                r.from.clone()
            },
            from: r.from.clone(),
            to: r.to.clone(),
            hash: r.hash_hex.clone(),
            amount: r.amount,
            fee: r.fee,
        })
        .collect()
}

#[derive(serde::Serialize)]
pub struct PendingTxDto {
    pub hash_short: String,
    pub hash: String,
    pub to: String,
    pub amount: u64,
    pub fee: u64,
}

/// 内存池中本钱包的待确认交易（活动页「确认中」；约 2 秒入块后转为正式记录）
#[tauri::command]
pub fn tx_pending(state: AppCtx) -> Vec<PendingTxDto> {
    let core = state.core.read().unwrap().clone();
    let my = match core {
        Some(c) => c.address.clone(),
        None => return vec![],
    };
    state
        .chain
        .pending_of(&my)
        .into_iter()
        .map(|p| PendingTxDto {
            hash_short: p.hash_short,
            hash: p.hash,
            to: p.to,
            amount: p.amount,
            fee: p.fee,
        })
        .collect()
}

// ---------------------------------------------------------------- 钱包生命周期

#[derive(serde::Serialize)]
pub struct UnlockedInfo {
    pub address: String,
    pub pk_short_hex: String,
}

/// 创建新钱包（写入加密密钥库，并立刻解锁）
#[tauri::command]
pub fn create_wallet(
    app: AppHandle,
    state: AppCtx,
    password: String,
) -> Result<UnlockedInfo, String> {
    if state.keystore_path.exists() {
        return Err("已存在钱包，请使用解锁".into());
    }
    let (ks, core) = keystore::create_wallet(&state.keystore_path, &password)?;
    let _ = ks; // keystore::create_wallet 已负责落盘
    let core = Arc::new(core);
    *state.core.write().unwrap() = Some(core.clone());
    emit_initial_state(&app, state, core.clone());
    Ok(UnlockedInfo {
        address: core.address.clone(),
        pk_short_hex: hex::encode(&core.pk_bytes[..8]),
    })
}

/// 用密码解锁
#[tauri::command]
pub fn unlock_wallet(
    app: AppHandle,
    state: AppCtx,
    password: String,
) -> Result<UnlockedInfo, String> {
    if state.core.read().unwrap().is_some() {
        return Err("已解锁".into());
    }
    let core = keystore::unlock_wallet(&state.keystore_path, &password)?;
    let core = Arc::new(core);
    *state.core.write().unwrap() = Some(core.clone());
    emit_initial_state(&app, state, core.clone());
    Ok(UnlockedInfo {
        address: core.address.clone(),
        pk_short_hex: hex::encode(&core.pk_bytes[..8]),
    })
}

#[tauri::command]
pub fn lock_wallet(state: AppCtx) {
    // net 层共用同一个核心槽：这里清空后联邦/单机出块自动停（私钥不在线签不了任何东西）
    *state.core.write().unwrap() = None;
}

// ---------------------------------------------------------------- 转账

#[derive(serde::Serialize)]
pub struct SendResult {
    pub tx_hash: String,
    pub height: u64,
    /// A 档口径：true = 已通过完整协议校验并进入交易池，下一块打包时才真实入账
    pub pending: bool,
}

/// 构造 QL-TX-v1 信封 → ML-DSA-65 签名 → 提交入池（下一块打包结算）
///
/// 真实节点上线后，此处替换为 ql_sendRawTx(cbor_hex) → 远端 mempool。
#[tauri::command]
pub fn send_qlx(
    state: AppCtx,
    to_addr: String,
    amount_qlx: String,
) -> Result<SendResult, String> {
    let core = state
        .core
        .read()
        .unwrap()
        .clone()
        .ok_or("钱包未解锁")?;

    let to_commitment = bech32_decode_addr(&to_addr)?;
    let amount_units = parse_amount(&amount_qlx)?;
    if to_addr == core.address {
        return Err("不能转给当前钱包自己".into());
    }
    let from_commitment = bech32_decode_addr(&core.address)?;

    let height = state.chain.height();
    let envelope = tx::TxEnvelope::new(
        from_commitment,
        state.chain.pending_nonce(&core.address),
        height + 50,
    )
    .transfer(to_commitment, amount_units);

    let digest = tx::tx_digest(&envelope);
    let sig_bytes = crypto::sign(&core.signing_key, &digest);

    // 提交：链内真 ML-DSA-65 验签 + nonce/可支配余额/过期校验 → 入交易池（不改状态）
    let tx_hash = state.chain.submit(&core, envelope.clone(), sig_bytes.clone())?;
    // 联邦中继：交易原件（含公钥与地址盐）撒给对端——对端不靠注册表也能独立验签入池
    let wire = crate::simchain::BlockWire::new(&envelope, &sig_bytes, &core.pk_bytes, &core.addr_salt);
    state.net.broadcast_tx(wire);

    Ok(SendResult {
        tx_hash: hex::encode(tx_hash),
        height,
        pending: true,
    })
}

/// 首笔账户绑定（pk 登记）：M1 直接输出 pk_hash 备查，M2 接入 ISchemeRegistry
#[tauri::command]
pub fn export_pk_hash(state: AppCtx) -> Result<String, String> {
    let core = state.core.read().unwrap().clone().ok_or("钱包未解锁")?;
    let hash = crypto::shake256(b"QL-PK-REG-V1", &core.pk_bytes, 32);
    Ok(hex::encode(hash))
}

// ---------------------------------------------------------------- 收款二维码

#[tauri::command]
pub fn receive_qr(state: AppCtx) -> Result<String, String> {
    let core = state.core.read().unwrap().clone().ok_or("钱包未解锁")?;
    let code = qrcode::QrCode::new(core.address.as_bytes())
        .map_err(|e| format!("二维码生成失败: {e}"))?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .dark_color(qrcode::render::svg::Color("#1A1F29"))
        .light_color(qrcode::render::svg::Color("#FFFFFF"))
        .quiet_zone(true)
        .build();
    Ok(svg)
}

// ---------------------------------------------------------------- .dat 备份 / 恢复
//
// 文件对话框在 Rust 侧调用；dialog 插件的 blocking API 不能阻塞主线程，
// 因此命令声明为 async，在 Tauri 异步运行时线程上执行。

/// 选择保存路径并写出 .dat 备份；返回保存路径，用户取消返回 None
#[tauri::command]
pub async fn export_backup(state: AppCtx<'_>, app: AppHandle) -> Result<Option<String>, String> {
    if !state.keystore_path.exists() {
        return Err("还没有钱包可备份（请先创建/解锁）".into());
    }
    let file_path = app
        .dialog()
        .file()
        .set_file_name("qlight-wallet.dat")
        .add_filter("QLight 钱包备份", &["dat"])
        .blocking_save_file();

    let Some(fp) = file_path else { return Ok(None) };
    let dest: PathBuf = match fp {
        FilePath::Path(p) => p,
        FilePath::Url(u) => u
            .to_file_path()
            .map_err(|_| "无法解析保存路径".to_string())?,
    };

    let raw = std::fs::read(&state.keystore_path).map_err(|e| format!("读取密钥库失败: {e}"))?;
    let ks: Keystore = serde_json::from_slice(&raw).map_err(|e| format!("解析密钥库失败: {e}"))?;
    keystore::write_backup(&dest, &ks)?;
    Ok(Some(dest.display().to_string()))
}

/// 选择 .dat 备份并还原 keystore（密码不变）；返回备份来源路径，用户取消返回 None。
/// 本机已有钱包时拒绝（防误覆盖：资产跟着密钥走，覆盖后旧钱包打不开）。
#[tauri::command]
pub async fn import_backup(state: AppCtx<'_>, app: AppHandle) -> Result<Option<String>, String> {
    if state.keystore_path.exists() {
        eprintln!("[QLight] 本机数据目录：{}", state.data_dir.display());
        return Err("本机已有钱包，未执行导入。如确需覆盖：先到数据目录删除 keystore.json 再重试（数据目录路径已打印在日志）".into());
    }

    let file_path = app
        .dialog()
        .file()
        .add_filter("QLight 钱包备份", &["dat"])
        .blocking_pick_file();

    let Some(fp) = file_path else { return Ok(None) };
    let src: PathBuf = match fp {
        FilePath::Path(p) => p,
        FilePath::Url(u) => u
            .to_file_path()
            .map_err(|_| "无法解析备份路径".to_string())?,
    };

    let bytes = std::fs::read(&src).map_err(|e| format!("读取备份失败: {e}"))?;
    let ks = keystore::parse_backup(&bytes)?;
    let out = serde_json::to_vec_pretty(&ks).map_err(|e| e.to_string())?;
    std::fs::write(&state.keystore_path, &out).map_err(|e| format!("写入密钥库失败: {e}"))?;
    Ok(Some(src.display().to_string()))
}

// ---------------------------------------------------------------- 内部工具

/// "1.23" → 1_230_000_000（QLX 9 位小数）
pub fn parse_amount(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("金额为空".into());
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() > 2 {
        return Err("金额格式非法".into());
    }
    let int_part: u64 = parts[0].parse().map_err(|_| "金额格式非法")?;
    let frac = match parts.get(1) {
        None => String::new(),
        Some(f) => {
            if f.len() > 9 {
                return Err("小数位最多 9 位".into());
            }
            format!("{:0<9}", f)
        }
    };
    let frac_part: u64 = if frac.is_empty() { 0 } else { frac.parse().map_err(|_| "金额格式非法")? };
    let amount = int_part
        .checked_mul(1_000_000_000)
        .and_then(|v| v.checked_add(frac_part))
        .ok_or("金额超出范围")?;
    if amount == 0 {
        return Err("金额必须大于 0".into());
    }
    Ok(amount)
}

/// 解锁后的初始状态推送（出块节奏已由 net 层的网络循环接管）
fn emit_initial_state(app: &AppHandle, state: AppCtx, core: Arc<WalletCore>) {
    let _ = core;
    // 立即推送一次当前状态
    let height = state.chain.height();
    let core_addr = state
        .core
        .read()
        .unwrap()
        .as_ref()
        .map(|c| c.address.clone())
        .unwrap_or_default();
    let _ = app.emit(
        "chain_block",
        BlockEvent {
            height,
            block_hash_hex: String::new(),
            vrf_hex: String::new(),
            reward: simchain::reward_at(height + 1),
            minted: 0,
            balance: state.chain.balance_of(&core_addr),
            total_minted: state.chain.total_minted(),
            ts_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            txs_count: 0,
        },
    );
}

/// 联邦网络状态（UI 网络条 chip）
#[tauri::command]
pub fn net_status(state: AppCtx) -> serde_json::Value {
    serde_json::json!({
        "count": state.net.peers_count(),
        "unlocked": state.core.read().unwrap().is_some(),
    })
}

/// 版本号唯一来源是 Cargo.toml —— 写进 8 个语言的文案里必然过期
#[tauri::command]
pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// ---------------------------------------------------------------- 世界实体（DESIGN.md §9.11）

/// 本机链的「已确认转账」索引（唯一数据来源是 blocks.jsonl）
fn chain_view(state: &AppState) -> world::ChainView {
    world::ChainView::load(&state.blocks_path)
}

/// 免费写入：签名 → 用重放同一套判据试算 → 追加 → 立刻广播。
///
/// v4 起建造不再和币相关（§9.14）：没有付款、没有"等确认"的中间态。
/// 唯一的代价是链高窗口配额（`FREE_QUOTA_MAX` / 1024 块），它是确定性的、
/// 第三方可复算的，也是脚本唯一会撞上的墙。
fn commit_record(
    state: &AppState,
    core: &Arc<WalletCore>,
    d: &world::Draft,
) -> Result<world::EntityRecord, String> {
    let view = chain_view(state);
    let rec = state
        .entities
        .commit_as(&world::signer_pk_hash(core), &view, |seq, prev| {
            world::sign_record(core, seq, &prev, d)
        })?;
    publish_entity(state, &rec);
    Ok(rec)
}

/// 世界快照：渲染与查询所需的全部输入。
///
/// 分工口径（§9.8）：**链只给坐标与归属，像素全部在本地算**。本命令不返回任何
/// 图片、不做任何渲染，只交出「创世哈希 + 每块 VRF + 实体状态」，由 ui/world.js
/// 在本机确定性渲染出大陆——同一份输入，任何一台机器都必须得到同一片世界。
#[derive(serde::Serialize)]
pub struct WorldSnapshot {
    pub genesis_hash_hex: String,
    pub vrfs: Vec<String>,
    pub height: u64,
    pub world_w: u16,
    pub world_h: u16,
    pub chunk: u16,
    pub era1_end_height: u64,
    pub slot_count: u8,
    /// 可用高度层数（Minecraft 的 Y 上限；layer ∈ 0..max_layer）
    pub max_layer: u16,
    pub entities: Vec<world::Entity>,
    pub chunks: Vec<world::Chunk>,
    pub report: world::Report,
    pub address: String,
    pub unlocked: bool,
    /// —— 矿脉（§9.13「世界即矿场」；挖矿不是第二个模块，它就是这个世界）——
    /// 矿脉规则版本，与 `tools/world/world.py`、`world.js`、`src/world/ore.rs` 同值
    pub ore_rule: String,
    /// 当前块高的矿脉配额（24 位种子空间里允许的命中数；随块高按 2 的幂减半）
    pub ore_quota: u32,
    /// 本回合还能挖的矿格数（扫描上限 512 之内；一个块重画一次）
    pub ore_candidates: usize,
    /// 本机地址名下活着的方块数（建造 + 挖矿都算）
    pub owned_by_me: usize,
    /// —— 免费写入的软限额（§9.14；v4 起建造不花币，防刷靠这张窗口表）——
    pub free_quota_window: u64,
    pub free_quota_max: u32,
    /// 本机在当前块高窗口内已写的条数
    pub my_writes_in_window: u32,
    /// —— 挖矿结算（§9.13）——
    /// 两笔挖矿发行之间至少间隔的块数
    pub mine_cooldown_blocks: u64,
    /// 上一笔挖矿发行所在的块高（0 = 从未）
    pub last_claim_height: u64,
    /// 下一笔挖矿最早能结算的块高
    pub next_claim_height: u64,
    /// 若此刻挖中一格，下一个块会发行多少（最小单位，已按硬顶截断）
    pub mine_reward: u64,
    /// —— v3「创造换节奏」（§9.16）——
    /// 这把钥匙当前挣到的最小间隔（块）：落过 0 格矿 = 50，最快 10
    pub mine_floor: u64,
    /// 这把钥匙在世界里已经落下的矿格数——v3 节奏阶梯的唯一输入（分身买不到速度）
    pub mine_claims_by_me: u64,
    /// 再落一格矿之后达到的间隔（块）：HUD 用它说清「建造换到什么」
    pub mine_next_floor: u64,
    /// 创造权重：占了几格、守了多少个块。只计量，不评价好坏。
    pub craft: world::Craft,
}

/// 逐块读出 VRF 序列（世界的历史即种子；创世行的 VRF 是全零，异或进去不改变种子）
fn vrf_sequence(blocks_path: &std::path::Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(blocks_path) else { return Vec::new() };
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if let Some(s) = v.get("vrf_hex").and_then(|x| x.as_str()) {
            if s.len() == 64 {
                out.push(s.to_string());
            }
        }
    }
    out
}

#[tauri::command]
pub fn world_snapshot(state: AppCtx) -> WorldSnapshot {
    let records = state.entities.load();
    let view = chain_view(&state);
    let (state_map, report) = world::replay(&records, &view);
    let core = state.core.read().unwrap().clone();
    let address = core.as_ref().map(|c| c.address.clone()).unwrap_or_default();
    let vrfs = vrf_sequence(&state.blocks_path);
    let height = state.chain.height();

    // 矿脉：与渲染器用同一份大地/地表，所以钱包算出的矿和网页上那片土地是同一片
    let words = world::terrain::seed_words(&simchain::genesis_hash_hex(), &vrfs);
    let taken = ore_taken(&state_map);
    let candidates = world::ore::scan(&words, height, &taken, 512).len();
    let owned_by_me = state_map
        .values()
        .filter(|e| !e.released && e.owner == address)
        .count();
    let my_writes_in_window = core.as_ref().map_or(0, |c| {
        let pk = world::signer_pk_hash(c);
        let window = height / world::FREE_QUOTA_WINDOW;
        records
            .iter()
            .filter(|r| {
                r.v == "QL-ENTREC-v4"
                    && r.pk_hash_hex == pk
                    && r.height / world::FREE_QUOTA_WINDOW == window
            })
            .count() as u32
    });
    // v3：节奏与发行量都按这把钥匙自己算（未解锁 = 落过 0 格 = 最慢一档）
    let miner_addr = core.as_ref().map(|c| c.address.clone()).unwrap_or_default();
    let (mine_claims_by_me, mine_floor) = state.chain.claim_pace(&miner_addr);
    let mine_next_floor = simchain::craft_floor(mine_claims_by_me + 1);
    let (_, mine_reward) = state.chain.claim_preview(&miner_addr);
    let next_claim = state.chain.next_claim_height(&miner_addr);
    // 创造权重：世界状态 + 链头高度的纯函数。同一份账本，任何人算出同一个数。
    let craft = world::craft_of(&state_map, &address, height);

    WorldSnapshot {
        genesis_hash_hex: simchain::genesis_hash_hex(),
        vrfs,
        height,
        world_w: world::WORLD_W,
        world_h: world::WORLD_H,
        chunk: world::CHUNK,
        era1_end_height: world::ERA1_END_HEIGHT,
        slot_count: world::SLOT_COUNT,
        max_layer: world::MAX_LAYER,
        chunks: world::chunk_roots(&state_map),
        entities: state_map.into_values().collect(),
        report,
        address,
        unlocked: core.is_some(),
        ore_rule: world::ore::ORE_RULE_VERSION.into(),
        ore_quota: world::ore::ore_quota(height),
        ore_candidates: candidates,
        owned_by_me,
        free_quota_window: world::FREE_QUOTA_WINDOW,
        free_quota_max: world::FREE_QUOTA_MAX,
        my_writes_in_window,
        mine_cooldown_blocks: simchain::MINE_COOLDOWN_BLOCKS,
        last_claim_height: state.chain.last_claim_height(),
        next_claim_height: next_claim,
        mine_reward,
        mine_floor,
        mine_claims_by_me,
        mine_next_floor,
        craft,
    }
}

/// 已占用的格子（`released` 的不算：断租即让位，与 `world::live_at` 同一判据）
fn ore_taken(map: &std::collections::BTreeMap<String, world::Entity>) -> world::ore::Taken {
    let mut t = world::ore::Taken::new();
    for e in map.values() {
        if !e.released {
            t.insert((e.x as u32, e.y as u32, e.layer as u32));
        }
    }
    t
}

/// 一次挖矿的产物：世界里的方块（立即落账）+ 等待进块的发行凭据。
#[derive(serde::Serialize)]
pub struct MineResult {
    pub record: world::EntityRecord,
    /// 发行凭据的交易摘要（下一个块若收下它，奖励进本机余额）
    pub claim_tx_hex: String,
    pub x: u16,
    pub y: u16,
    pub layer: u16,
    /// 预计结算块高与发行量（最小单位）
    pub claim_height: u64,
    pub reward: u64,
    /// 这一笔所用的节奏档位（最小间隔，块）
    pub pace_floor: u64,
    /// 这一格落进世界以后的下一档（创造换节奏：v3 唯一的加速口径）
    pub pace_next_floor: u64,
}

/// ⛏️ 挖一回合：在本回合的矿脉里按湿度从高到低取第一个还可挖的格子，
/// 在世界里落一个贴地方块（免费），同时提交一张**发行凭据**——
/// 下一个块收下它，才会按这一笔**最多一个当前节奏档**发行新币进本机余额（空块不补发）。
///
/// **没有造出来的方块，就没有发行**：币不是计时器印出来的，是世界里多了一格
/// 别人可复算的坐标换来的（§9.13「世界即矿场」）。两次发行至少隔这把钥匙
/// 当前挣到的节奏档（50→10 块），所以脚本刷得再快也快不过世界自己重画地表的节奏。
#[tauri::command]
pub fn world_mine_once(state: AppCtx, kind: u8) -> Result<MineResult, String> {
    let core = state.core.read().unwrap().clone().ok_or("钱包未解锁")?;
    let map = live_world(&state)?;
    let vrfs = vrf_sequence(&state.blocks_path);
    let height = state.chain.height();
    let ready_at = state.chain.next_claim_height(&core.address);
    if ready_at > height + 1 {
        return Err(format!(
            "本回合已结算：下一笔挖矿发行在块高 {ready_at}（约 {} 秒后）",
            (ready_at - height - 1) * simchain::BLOCK_INTERVAL_SECS
        ));
    }
    let words = world::terrain::seed_words(&simchain::genesis_hash_hex(), &vrfs);
    let taken = ore_taken(&map);
    let hits = world::ore::scan(&words, height, &taken, 8);
    let (x, y, layer) = *hits.first().ok_or_else(|| {
        format!(
            "本回合无矿可挖：配额 {}／{}，{} 格扫描内没有空闲矿脉——等下一个块重画地表",
            world::ore::ore_quota(height),
            world::ore::ORE_SEED_SPACE,
            world::ore::SCAN_CAP
        )
    })?;
    let kind = kind.min(world::SLOT_COUNT - 1);
    // 这一跳之前这把钥匙的节奏（已落矿格数 + 当前最小间隔）：块一结算它就往前挪一档
    let pace = state.chain.claim_pace(&core.address);

    // 先提交发行凭据：被拒就不该在世界里落方块（否则会造出一格"挖不出发行"的矿）
    let (wire, digest) = state.chain.submit_claim(&core, x, y, layer)?;
    state.net.broadcast_tx(wire);

    let name = format!("矿·{}_{}_{}", x, y, layer);
    let record = commit_record(
        &state,
        &core,
        &world::Draft {
            op: world::op::CLAIM,
            entity_id_hex: "",
            parent_hex: "",
            name: &name,
            x,
            y,
            layer,
            kind,
            height,
            ts: now_unix(),
        },
    )?;
    Ok(MineResult {
        record,
        claim_tx_hex: hex::encode(digest),
        x,
        y,
        layer,
        claim_height: height + 1,
        reward: state.chain.claim_preview(&core.address).1,
        // 这一格落下去，节奏就往前挪一档（v3 里创造唯一买得到的东西）：
        // UI 要能在同一跳里说出"下一笔最快 X 块后结算"，进度感才是真的进度感。
        pace_floor: pace.1,
        pace_next_floor: simchain::craft_floor(pace.0 + 1),
    })
}


fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn check_coords(x: u16, y: u16, layer: u16, kind: u8) -> Result<(), String> {
    if x >= world::WORLD_W || y >= world::WORLD_H {
        return Err(format!("坐标 ({x},{y}) 超出世界边界 {}×{}", world::WORLD_W, world::WORLD_H));
    }
    if layer >= world::MAX_LAYER {
        return Err(format!("高度层 {layer} 非法（0..{}）", world::MAX_LAYER - 1));
    }
    if kind >= world::SLOT_COUNT {
        return Err(format!("物品栏槽位 {kind} 非法（0..{}）", world::SLOT_COUNT - 1));
    }
    Ok(())
}

/// 写操作前的统一前置：整本重放必须完好。一条语义非法的记录一旦落盘，
/// 世界就从那条起再也重放不过去——所以宁可在这里就把话说明白。
fn live_world(state: &AppState) -> Result<std::collections::BTreeMap<String, world::Entity>, String> {
    let (map, report) = world::replay(&state.entities.load(), &chain_view(state));
    if !report.ok {
        return Err(format!(
            "实体账本第 {} 条起校验失败：{}。先排查账本再继续写入",
            report.first_bad, report.reason
        ));
    }
    Ok(map)
}

/// 那一格那一层是否已被活方块占用（`except_id` 允许"挪回原位"这类空操作）
fn voxel_taken(
    map: &std::collections::BTreeMap<String, world::Entity>,
    x: u16,
    y: u16,
    layer: u16,
    except_id: &str,
) -> Result<(), String> {
    if let Some(busy) = world::live_at(map, x, y, layer) {
        if busy.entity_id_hex != except_id {
            return Err(format!("({x},{y}) 第 {layer} 层已经有「{}」", busy.name));
        }
    }
    Ok(())
}

fn check_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("实体要有名字".into());
    }
    if name.len() > world::NAME_MAX {
        return Err(format!("名称最长 {} 字节", world::NAME_MAX));
    }
    Ok(name.to_string())
}

/// 创造一格世界：**免费、当场生效**（§9.14）。
///
/// 不付钱、不等确认——创造不该被币控制。防刷是确定性的窗口配额：
/// 同一归属者每 1024 个块高最多 4096 条，正常建造永远碰不到。
#[tauri::command]
pub fn world_place(
    state: AppCtx,
    name: String,
    x: u16,
    y: u16,
    layer: u16,
    kind: u8,
    parent_hex: String,
) -> Result<world::EntityRecord, String> {
    let core = state.core.read().unwrap().clone().ok_or("钱包未解锁")?;
    let name = check_name(&name)?;
    check_coords(x, y, layer, kind)?;
    let map = live_world(&state)?;
    voxel_taken(&map, x, y, layer, "")?;
    let parent_hex = parent_hex.trim().to_string();
    if !parent_hex.is_empty() {
        match map.get(&parent_hex) {
            Some(p) if !p.released => {}
            Some(_) => return Err("父实体已无归属，不能再衍生".into()),
            None => return Err("父实体不存在".into()),
        }
    }
    commit_record(
        &state,
        &core,
        &world::Draft {
            op: world::op::CLAIM,
            entity_id_hex: "",
            parent_hex: &parent_hex,
            name: &name,
            x,
            y,
            layer,
            kind,
            height: state.chain.height(),
            ts: now_unix(),
        },
    )
}

/// 三条改写操作共用的前置检查：目标存在、未释放、且签名者就是当前归属者。
///
/// 必须在追加**之前**做完：一条语义非法的记录一旦落盘，整本账本从那条起
/// 就再也重放不过去——世界不能因为一次误操作而消失。
fn owned_for(
    state: &AppState,
    entity_id_hex: &str,
    me: &str,
) -> Result<world::Entity, String> {
    let map = live_world(state)?;
    let e = map.get(entity_id_hex).ok_or("实体不存在")?.clone();
    if e.released {
        return Err("该实体已释放归属，不可再改写".into());
    }
    if e.owner != me {
        return Err("只有当前归属者能改写它".into());
    }
    Ok(e)
}

/// 改写操作与创造同一条路：签名即生效，不花币、不等确认。
fn op_record(
    state: &AppState,
    core: &Arc<WalletCore>,
    op: &str,
    e: &world::Entity,
    x: u16,
    y: u16,
    layer: u16,
    name: &str,
) -> Result<world::EntityRecord, String> {
    commit_record(
        state,
        core,
        &world::Draft {
            op,
            entity_id_hex: &e.entity_id_hex,
            parent_hex: "",
            name,
            x,
            y,
            layer,
            kind: e.kind,
            height: state.chain.height(),
            ts: now_unix(),
        },
    )
}

/// 落盘的实体记录立刻上联邦网络：别人的小窗里当场显形（DESIGN.md §9.15 M0）。
/// 单机（网络起不来）时这里只是没人应答，不影响本机记账。
fn publish_entity(state: &AppState, rec: &world::EntityRecord) {
    state.net.broadcast_entity(rec.clone());
}

#[tauri::command]
pub fn world_rename(
    state: AppCtx,
    entity_id_hex: String,
    name: String,
) -> Result<world::EntityRecord, String> {
    let core = state.core.read().unwrap().clone().ok_or("钱包未解锁")?;
    let name = check_name(&name)?;
    let e = owned_for(&state, &entity_id_hex, &core.address)?;
    op_record(&state, &core, world::op::RENAME, &e, e.x, e.y, e.layer, &name)
}

#[tauri::command]
pub fn world_move(
    state: AppCtx,
    entity_id_hex: String,
    x: u16,
    y: u16,
    layer: u16,
) -> Result<world::EntityRecord, String> {
    let core = state.core.read().unwrap().clone().ok_or("钱包未解锁")?;
    check_coords(x, y, layer, 0)?;
    let e = owned_for(&state, &entity_id_hex, &core.address)?;
    voxel_taken(&live_world(&state)?, x, y, layer, &e.entity_id_hex)?;
    op_record(&state, &core, world::op::MOVE, &e, x, y, layer, &e.name)
}

/// 释放归属：实体永久留在账本里可查，只是不再有主人（§9.9 断租不删除）
#[tauri::command]
pub fn world_release(
    state: AppCtx,
    entity_id_hex: String,
) -> Result<world::EntityRecord, String> {
    let core = state.core.read().unwrap().clone().ok_or("钱包未解锁")?;
    let e = owned_for(&state, &entity_id_hex, &core.address)?;
    op_record(&state, &core, world::op::RELEASE, &e, e.x, e.y, e.layer, &e.name)
}

/// 导出某条实体的原始记录（含公钥与签名）：任何人可离线重算 record_hash 并验签
#[tauri::command]
pub fn world_proof(state: AppCtx, entity_id_hex: String) -> Result<String, String> {
    let recs = state.entities.load();
    let hit = recs
        .iter()
        .filter(|r| r.entity_id_hex == entity_id_hex)
        .max_by_key(|r| r.seq)
        .ok_or("账本里没有这个实体")?;
    serde_json::to_string_pretty(hit).map_err(|e| e.to_string())
}

