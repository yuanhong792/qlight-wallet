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
    ChainState {
        unlocked: core.is_some(),
        address,
        balance,
        height,
        total_minted: state.chain.total_minted(),
        reward_per_block: simchain::reward_at(height),
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
    // 联邦中继：把这笔交易撒给对端（对端走同一套验签入池，下一块全网打包同一批交易）
    state.net.broadcast_tx(envelope, sig_bytes);

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

// ---------------------------------------------------------------- 世界实体（DESIGN.md §9.11）

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
    let (state_map, report) = world::replay(&records);
    let core = state.core.read().unwrap().clone();
    WorldSnapshot {
        genesis_hash_hex: simchain::genesis_hash_hex(),
        vrfs: vrf_sequence(&state.blocks_path),
        height: state.chain.height(),
        world_w: world::WORLD_W,
        world_h: world::WORLD_H,
        chunk: world::CHUNK,
        era1_end_height: world::ERA1_END_HEIGHT,
        slot_count: world::SLOT_COUNT,
        max_layer: world::MAX_LAYER,
        chunks: world::chunk_roots(&state_map),
        entities: state_map.into_values().collect(),
        report,
        address: core.as_ref().map(|c| c.address.clone()).unwrap_or_default(),
        unlocked: core.is_some(),
    }
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
    let (map, report) = world::replay(&state.entities.load());
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

/// 铸造一个方块：坐标 + 高度层 + 槽位 + 名字，由本机 ML-DSA-65 私钥签名后追加进实体账本
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
    let height = state.chain.height();
    let ts = now_unix();
    let draft = world::Draft {
        op: world::op::CLAIM,
        entity_id_hex: "",
        parent_hex: &parent_hex,
        name: &name,
        x,
        y,
        layer,
        kind,
        height,
        ts,
    };
    state.entities.commit(|seq, prev| world::sign_record(&core, seq, &prev, &draft))
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

/// 对非 claim 操作，记录里仍要带一个合法名字（名字本身参与哈希链）
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
    let draft = world::Draft {
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
    };
    state.entities.commit(|seq, prev| world::sign_record(core, seq, &prev, &draft))
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

