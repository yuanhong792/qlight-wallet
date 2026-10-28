//! Tauri 命令层：钱包生命周期 + 链状态 + 转账 + 挖矿心跳
//!
//! 对应 DESIGN.md §14.1 的钱包侧口径（本地闭环版）：
//! - getBalance  → chain_state
//! - sendRawTx   → send_qlx（构造信封 → 签名 → submit → 结算）

use crate::crypto;
use crate::feedback;
use crate::keystore::{self, Keystore, WalletCore};
use crate::net;
use crate::prefs;
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
    /// 外壳偏好（prefs.json：欢迎页是否还要弹、引导走到哪一步）
    pub prefs_path: PathBuf,
    pub prefs: RwLock<prefs::Prefs>,
    /// 世界实体账本（entities.jsonl，append-only 哈希链；DESIGN.md §9.11）
    pub entities: Arc<world::EntityLedger>,
    /// 光尘账本（dust.jsonl，独立前缀与独立哈希链；DESIGN.md §9.17）
    pub dust: Arc<world::dust::DustLedger>,
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

// ---------------------------------------------------------------- 外壳偏好 / 反馈 / 分享（DESIGN.md §9.19）

#[derive(serde::Serialize)]
pub struct PrefsDto {
    pub welcome_done: bool,
    pub opened_world: bool,
    pub opened_history: bool,
    pub guide_done: bool,
}

impl From<&prefs::Prefs> for PrefsDto {
    fn from(p: &prefs::Prefs) -> Self {
        PrefsDto {
            welcome_done: p.welcome_done,
            opened_world: p.opened_world,
            opened_history: p.opened_history,
            guide_done: p.guide_done,
        }
    }
}

#[tauri::command]
pub fn get_prefs(state: AppCtx) -> PrefsDto {
    PrefsDto::from(&*state.prefs.read().unwrap())
}

/// 白名单写一位偏好。先落盘再改内存：写失败时内存保持原值，界面上就不会出现
/// "我以为存下来了，重启又弹一遍"——那种错觉比一次报错难查得多。
#[tauri::command]
pub fn set_pref(state: AppCtx, key: String, value: bool) -> Result<PrefsDto, String> {
    let mut p = state.prefs.read().unwrap().clone();
    prefs::apply(&mut p, &key, value)?;
    prefs::save(&state.prefs_path, &p)?;
    let dto = PrefsDto::from(&p);
    *state.prefs.write().unwrap() = p;
    Ok(dto)
}

/// 聚合诊断数字（反馈包的机器部分）。全是内存里的计数，没有一个字段是文件内容。
fn diag_of(state: &AppState) -> feedback::Diag {
    let core = state.core.read().unwrap().clone();
    let entity_lines = state.entities.load().len();
    let (dust_recs, dust_rep) = state.dust.load_report();
    let present = state.net.presence_report()["count"]
        .as_u64()
        .unwrap_or(0) as usize;
    feedback::Diag {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        rules: simchain::rules_tag(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        height: state.chain.height(),
        peers: state.net.peers_count(),
        present,
        entity_lines,
        dust_lines: dust_recs.len(),
        entity_bad_lines: state.entities.bad_lines(),
        dust_bad_lines: dust_rep.bad_lines,
        unlocked: core.is_some(),
        // 只报"是不是同机多开的第 N 个实例"，不报路径：路径里带用户名
        multi_instance: state
            .data_dir
            .file_name()
            .map(|n| n.to_string_lossy().starts_with("node-"))
            .unwrap_or(false),
    }
}

/// 反馈包提交前给玩家看一眼会带走什么（透明是设计，不是客套）
#[tauri::command]
pub fn diag_report(state: AppCtx) -> feedback::Diag {
    diag_of(&state)
}

/// 从 data: URL 里解出 PNG 字节。界面只能交截图，别的形状一律拒。
fn decode_png_data_url(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let b64 = s
        .strip_prefix("data:image/png;base64,")
        .ok_or("截图必须是 PNG 的 data URL")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("截图数据解不开：{e}"))?;
    if !bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Err("截图不是合法 PNG".into());
    }
    // 128 MB 上限：正常全屏截图几 MB，超这个数只会是把谁的日志当成图片交了上来
    if bytes.len() > 128 * 1024 * 1024 {
        return Err("截图过大".into());
    }
    Ok(bytes)
}

/// 玩家选的保存路径落成 PathBuf（dialog 插件在两种平台上返回两种形状）
fn dest_path(fp: FilePath) -> Result<PathBuf, String> {
    match fp {
        FilePath::Path(p) => Ok(p),
        FilePath::Url(u) => u.to_file_path().map_err(|_| "无法解析保存路径".to_string()),
    }
}

/// 导出反馈包：只写玩家自己选的本地文件。**本版没有任何上传通道。**
/// 带截图时同目录再写一个同名 `.png`（一次对话框，两个文件）。
#[tauri::command]
pub async fn feedback_write(
    app: AppHandle,
    state: AppCtx<'_>,
    problem: String,
    include_addr: bool,
    screenshot: Option<String>,
) -> Result<Option<String>, String> {
    let png = match screenshot.as_deref() {
        None => None,
        Some(s) if s.trim().is_empty() => None,
        Some(s) => Some(decode_png_data_url(s)?),
    };
    let d = diag_of(&state);
    let addr = state
        .core
        .read()
        .unwrap()
        .as_ref()
        .map(|c| c.address.clone())
        .unwrap_or_default();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let payload = feedback::build(&d, &problem, png.is_some(), include_addr, &addr, ts);

    let stem = format!("qlight-feedback-{}", ts);
    let fp = app
        .dialog()
        .file()
        .set_file_name(format!("{stem}.json"))
        .add_filter("QLight 反馈包", &["json"])
        .blocking_save_file();
    let Some(fp) = fp else { return Ok(None) };
    let dest = dest_path(fp)?;
    let out = serde_json::to_vec_pretty(&payload).map_err(|e| e.to_string())?;
    std::fs::write(&dest, out).map_err(|e| format!("写入反馈包失败：{e}"))?;
    if let Some(bytes) = png {
        let shot = dest.with_extension("png");
        std::fs::write(&shot, bytes).map_err(|e| format!("写入截图失败：{e}"))?;
    }
    Ok(Some(dest.display().to_string()))
}

/// 导出作品分享：PNG + 同名 `.txt`（坐标/标题等说明文字，短视频简介直接复制）
#[tauri::command]
pub async fn export_share(
    app: AppHandle,
    image_data_url: String,
    caption: String,
) -> Result<Option<String>, String> {
    let bytes = decode_png_data_url(&image_data_url)?;
    let fp = app
        .dialog()
        .file()
        .set_file_name("qlight-world.png")
        .add_filter("PNG 图片", &["png"])
        .blocking_save_file();
    let Some(fp) = fp else { return Ok(None) };
    let dest = dest_path(fp)?;
    std::fs::write(&dest, bytes).map_err(|e| format!("写入截图失败：{e}"))?;
    if !caption.trim().is_empty() {
        // 说明文字限长：分享卡不是日志容器，超了截断而不是丢弃整段
        let text: String = caption.trim().chars().take(4000).collect();
        let side = dest.with_extension("txt");
        std::fs::write(&side, text).map_err(|e| format!("写入坐标说明失败：{e}"))?;
    }
    Ok(Some(dest.display().to_string()))
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
    // 版本普查部分与 `net_versions` 事件同形：chip 只有一套读法
    let mut v = state.net.version_report();
    v["count"] = serde_json::json!(state.net.peers_count());
    v["unlocked"] = serde_json::json!(state.core.read().unwrap().is_some());
    v["present"] = serde_json::json!(state.net.presence_report()["count"]);
    // 联机三态（本地 / 局域网 / 公网）：现在只可能长出前两档，公网那一档留给界面说实话
    v["scope"] = state.net.scope_report();
    v
}

/// 🚶 上报我在世界里的位置（DESIGN.md §9.19）。
///
/// 只在内存与 gossip 里流动：不写账本、不进块、不参与任何归属或发行判定，
/// 所以它既刷不出币，也不能替任何人声称"那一格是我的"。
#[tauri::command]
pub fn pos_publish(state: AppCtx, x: f64, y: f64, z: f64, heading: f64) -> Result<(), String> {
    state.net.publish_presence(x, y, z, heading)
}

/// 在场名单（前端画邻居、以及「同世界 N 人」的数据源）
#[tauri::command]
pub fn world_presence(state: AppCtx) -> serde_json::Value {
    state.net.presence_report()
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
    /// —— 账本健康（DESIGN.md §9.18）——
    /// 实体账本最近一次整本读入跳过的坏行数。坏行只伤它自己那一行，但必须让界面
    /// 说得出"少了 N 行"：静默跳过等于让本机悄悄用另一本账参与共识。
    pub entity_bad_lines: usize,
    /// 光尘账本同上。这两本里任何一本的条数都会进块承诺，掉行不是无痕的。
    pub dust_bad_lines: usize,
    /// —— 挖地（§9.20）——
    /// 已经被挖出坑的列：顶面层与已剥层数。渲染器只认这个，不自己猜。
    pub dug: Vec<DugColumn>,
    /// 本机地址手上的材料（土 / 石 / 矿），已扣掉建造消耗
    pub materials: [u32; world::MAT_COUNT],
    /// 物品栏 16 件的造价表（土 / 石 / 矿）
    pub build_costs: Vec<[u32; world::MAT_COUNT]>,
    /// 最后一层能挖到的层（岩床之上）：再往下就是第 0 层
    pub dig_floor: u16,
    /// 整本账都写在链头之上：它不是这本链的世界，而是链重置前那一代的
    /// （每条记录的高度都超过链尖）。界面据此给出「归档旧世界账本」的出口，
    /// 不把「一条坏行」和「换了条链」当成同一件事。
    pub ledger_foreign: bool,
}

/// 一列挖出来的坑：渲染器把顶面压到 `top`，坑壁按深度分带
#[derive(serde::Serialize)]
pub struct DugColumn {
    pub x: u16,
    pub y: u16,
    /// 当前露在外面的那一层（0 = 挖到岩床）
    pub top: u16,
    /// 已剥掉的层数
    pub removed: u32,
}

/// 逐块读出 VRF 序列（世界的历史即种子；创世行的 VRF 是全零，异或进去不改变种子）
fn vrf_sequence(blocks_path: &std::path::Path) -> Vec<String> {
    world::chain_vrfs(blocks_path)
}

#[tauri::command]
pub fn world_snapshot(state: AppCtx) -> WorldSnapshot {
    let records = state.entities.load();
    let view = chain_view(&state);
    let w = world::replay_world(&records, &view);
    let report = w.report.clone();
    let state_map = w.entities;
    let core = state.core.read().unwrap().clone();
    let address = core.as_ref().map(|c| c.address.clone()).unwrap_or_default();
    let vrfs = vrf_sequence(&state.blocks_path);
    let height = state.chain.height();
    // 旧世界账本：每条记录都写在链尖之上，或者版式 v3 的买单交易一笔都不在链上。
    // 后一条是长期判据——链会一直长，长过旧记录的块高之后前一条自然失效。
    let fee_known = records.iter().filter(|r| !r.fee_tx_hex.is_empty()).count();
    let fee_present = records
        .iter()
        .filter(|r| !r.fee_tx_hex.is_empty() && view.fee(&r.fee_tx_hex).is_some())
        .count();
    let ledger_foreign = world::ledger_is_foreign(
        report.ok,
        &records.iter().map(|r| r.height).collect::<Vec<_>>(),
        height,
        fee_known,
        fee_present,
    );

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
                (r.v == world::RECORD_VERSION || r.v == "QL-ENTREC-v4")
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
    let dug: Vec<DugColumn> = w
        .dug
        .iter()
        .map(|(&(x, y), &removed)| DugColumn {
            x,
            y,
            top: world::column_top(&w.dug, x, y),
            removed,
        })
        .collect();
    let materials = w
        .materials
        .get(&address)
        .copied()
        .unwrap_or([0; world::MAT_COUNT]);
    let build_costs = (0..world::SLOT_COUNT).map(world::build_cost).collect();

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
        // 实体账本在上面已整本读入；光尘账本这里显式读一次，坏行数与它自己的重放同一口径
        entity_bad_lines: state.entities.bad_lines(),
        dust_bad_lines: state.dust.load_report().1.bad_lines,
        dug,
        materials,
        build_costs,
        dig_floor: world::DUG_FLOOR,
        ledger_foreign,
    }
}

#[derive(serde::Serialize)]
pub struct LedgerArchive {
    pub tag: String,
    /// 归档后的文件名（原文件只改名，不删除）
    pub files: Vec<String>,
}

/// 把校验不过的世界账本归档（**只改名不删除**），让新世界从空账开始。
///
/// 只处理"换了条链"这一种情形：链被归档重置后，旧世界的记录引用的块已经不存在，
/// 留着的唯一效果是每次写入都被判「账本第 1 条起校验失败」。能通过重放的账本一律
/// 不动——那说明它属于这条链，归档等于抹掉活着的世界。
#[tauri::command]
pub fn world_archive_ledger(state: AppCtx) -> Result<LedgerArchive, String> {
    let records = state.entities.load();
    let ent_ok = world::replay_world(&records, &chain_view(&state)).report.ok;
    let (_, dust_rep) = world::dust::replay(&state.dust.load(), &dust_view(&state));
    if ent_ok && dust_rep.ok {
        return Err("这两本账都能通过校验，不需要归档".into());
    }
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    let mut files = Vec::new();
    for (name, bad) in [("entities.jsonl", !ent_ok), ("dust.jsonl", !dust_rep.ok)] {
        if !bad {
            continue;
        }
        let from = state.data_dir.join(name);
        if !from.exists() {
            continue;
        }
        let to = state.data_dir.join(format!("{name}.bak-world-{tag}"));
        std::fs::rename(&from, &to)
            .map_err(|e| format!("归档 {name} 失败：{e}"))?;
        files.push(
            to.file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
    }
    Ok(LedgerArchive { tag, files })
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


// ---------------------------------------------------------------- 光尘（§9.17）

/// 光尘层的外部输入：链头高度。世界种子由 `dust` 模块自己取创世种子，
/// 不接受调用方传入——防止把随出块漂移的地表种子混进判定。
fn dust_view(state: &AppState) -> world::dust::DustView {
    world::dust::DustView {
        tip: state.chain.height(),
    }
}

#[derive(serde::Serialize)]
pub struct DustSnapshot {
    pub unlocked: bool,
    pub address: String,
    pub balance: u64,
    /// 只在 v5 起才有意义：`balance` 是共识结算后的余额，`pending` 是还没进块的增量
    pub confirmed: bool,
    pub pending: u64,
    pub minted_total: u64,
    pub height: u64,
    pub epoch: u64,
    pub epoch_blocks: u64,
    /// 本轮结束（下一轮开始）的块高
    pub epoch_end: u64,
    pub night: bool,
    pub harvested_this_epoch: u32,
    pub quota: u32,
    /// 夜里采集减半，UI 要能提前说清楚
    pub night_multiplier_permille: u32,
    pub report: world::dust::DustReport,
    /// 与本机地址相关的最新记录（含收发）
    pub recent: Vec<world::dust::DustRecord>,
}

fn dust_settled(
    state: &AppState,
) -> Result<(world::dust::DustState, Vec<world::dust::DustRecord>), String> {
    let records = state.dust.load();
    let (st, rep) = world::dust::replay(&records, &dust_view(state));
    if !rep.ok {
        return Err(format!(
            "光尘账本第 {} 条起校验失败（{}）：{}",
            rep.first_bad, rep.bad_owner, rep.reason
        ));
    }
    Ok((st, records))
}

#[tauri::command]
pub fn world_dust_state(state: AppCtx) -> DustSnapshot {
    let core = state.core.read().unwrap().clone();
    let address = core.as_ref().map(|c| c.address.clone()).unwrap_or_default();
    let height = state.chain.height();
    let records = state.dust.load();
    let (st, report) = world::dust::replay(&records, &dust_view(&state));
    let epoch = world::resource::epoch_of(height);
    let mut recent: Vec<_> = records
        .iter()
        .filter(|r| r.owner == address || (!r.to.is_empty() && r.to == address))
        .cloned()
        .collect();
    if recent.len() > 40 {
        recent.drain(..recent.len() - 40);
    }
    // v5 起光尘是真共识：界面上先给"已确认"的数，再单独挂"待确认"，
    // 这样"我有 60 ✦ 却换不动"不会再被当成 bug。
    let active = state.chain.dust_fork_active();
    let confirmed_balance = state.chain.dust_balance_pub(&address);
    let (balance, pending) = if active {
        (
            confirmed_balance,
            st.balance_of(&address).saturating_sub(confirmed_balance),
        )
    } else {
        (st.balance_of(&address), 0)
    };
    DustSnapshot {
        unlocked: core.is_some(),
        balance,
        confirmed: active,
        pending,
        minted_total: st.minted_total,
        address,
        height,
        epoch,
        epoch_blocks: world::resource::EPOCH_BLOCKS,
        epoch_end: (epoch + 1) * world::resource::EPOCH_BLOCKS,
        night: world::resource::is_night(height),
        harvested_this_epoch: st.harvested_count(
            core.as_ref().map(|c| c.address.as_str()).unwrap_or(""),
            epoch,
        ),
        quota: world::resource::HARVEST_QUOTA,
        night_multiplier_permille: if world::resource::is_night(height) { 500 } else { 1000 },
        report,
        recent,
    }
}

#[derive(serde::Serialize)]
pub struct ResourceNode {
    pub x: u16,
    pub y: u16,
    pub kind: u8,
    pub reward: u64,
    /// 本轮已被采走（长出来的东西被采了，下一轮才回来）
    pub taken: bool,
}

/// 一块视口里的资源点。坐标是权威，`taken` 来自整本光尘账本的重放。
#[tauri::command]
pub fn world_resources(state: AppCtx, x0: u16, y0: u16, x1: u16, y1: u16) -> Vec<ResourceNode> {
    // 资源/光尘一律用创世种子：判定结果必须能被任意时刻重放复现
    let words = world::resource_words();
    let height = state.chain.height();
    let epoch = world::resource::epoch_of(height);
    let (st, _) = world::dust::replay(&state.dust.load(), &dust_view(&state));
    world::resource::scan(x0 as u32, y0 as u32, x1 as u32, y1 as u32, &words)
        .into_iter()
        .map(|(x, y, kind)| ResourceNode {
            x,
            y,
            kind,
            reward: world::resource::reward_at(kind, height),
            taken: st.harvested.contains(&(x, y, epoch)),
        })
        .collect()
}

#[derive(serde::Serialize)]
pub struct DustHarvestResult {
    pub record: world::dust::DustRecord,
    pub x: u16,
    pub y: u16,
    pub kind: u8,
    pub amount: u64,
    pub balance: u64,
    pub harvested_this_epoch: u32,
    pub quota: u32,
    pub epoch: u64,
}

/// ✋ 采一格：种类与产量都不接受前端输入，全部按坐标和链头重新算。
///
/// 采集记录里照抄这份结果，重放时每个邻居都会得到同一个金额——所以"采什么"
/// 和"采到多少"都不是客户端说了算，而是世界说了算。
#[tauri::command]
pub fn world_harvest(state: AppCtx, x: u16, y: u16) -> Result<DustHarvestResult, String> {
    let core = state.core.read().unwrap().clone().ok_or("钱包未解锁")?;
    // 采集也先认世界：实体账本从某条起重放不过，这台机器用的就是另一本世界，
    // 而光尘能经兑换池换回 QLX——产出侧不许带病开工，与 world_mine_once 同口径。
    live_world(&state)?;
    let height = state.chain.height();
    // 资源/光尘一律用创世种子：判定结果必须能被任意时刻重放复现
    let words = world::resource_words();
    let kind = world::resource::node_at(x as u32, y as u32, &words)
        .ok_or_else(|| format!("({x},{y}) 没有可采的东西"))?;
    let epoch = world::resource::epoch_of(height);
    let amount = world::resource::reward_at(kind, height);
    let (st, _) = dust_settled(&state)?;
    if st.harvested.contains(&(x, y, epoch)) {
        return Err(format!(
            "({x},{y}) 本轮已被采走——第 {} 块长出新的",
            (epoch + 1) * world::resource::EPOCH_BLOCKS
        ));
    }
    let used = st.harvested_count(&core.address, epoch);
    if used >= world::resource::HARVEST_QUOTA {
        return Err(format!(
            "本轮配额用完（{}／{}），第 {} 块进入新一轮",
            used,
            world::resource::HARVEST_QUOTA,
            (epoch + 1) * world::resource::EPOCH_BLOCKS
        ));
    }
    let pk_hash = world::signer_pk_hash(&core);
    let rec = state
        .dust
        .commit_as(&pk_hash, &dust_view(&state), |seq, prev| {
            world::dust::sign_record(
                &core,
                seq,
                &prev,
                &world::dust::DustDraft {
                    op: world::dust::op::HARVEST,
                    epoch,
                    x,
                    y,
                    kind,
                    amount,
                    to: "",
                    height,
                    ts: now_unix(),
                },
            )
        })?;
    state.net.broadcast_dust(rec.clone());
    Ok(DustHarvestResult {
        record: rec,
        x,
        y,
        kind,
        amount,
        balance: st.balance_of(&core.address) + amount,
        harvested_this_epoch: used + 1,
        quota: world::resource::HARVEST_QUOTA,
        epoch,
    })
}

/// 💸 转光尘：收款地址与余额都由重放裁判，客户端只能提议。
#[tauri::command]
pub fn world_dust_send(state: AppCtx, to: String, amount: u64) -> Result<world::dust::DustRecord, String> {
    let core = state.core.read().unwrap().clone().ok_or("钱包未解锁")?;
    let height = state.chain.height();
    let pk_hash = world::signer_pk_hash(&core);
    let rec = state
        .dust
        .commit_as(&pk_hash, &dust_view(&state), |seq, prev| {
            world::dust::sign_record(
                &core,
                seq,
                &prev,
                &world::dust::DustDraft {
                    op: world::dust::op::SEND,
                    epoch: 0,
                    x: 0,
                    y: 0,
                    kind: 0,
                    amount,
                    to: &to,
                    height,
                    ts: now_unix(),
                },
            )
        })?;
    state.net.broadcast_dust(rec.clone());
    Ok(rec)
}

// ---------------------------------------------------------------- 旅人摊位（§9.17）

#[derive(serde::Serialize)]
pub struct TraderOffer {
    pub id: String,
    pub price: u64,
}

#[derive(serde::Serialize)]
pub struct TraderSnapshot {
    pub rule: String,
    pub x: u16,
    pub y: u16,
    /// 第几程（每 50 块一程）
    pub period: u64,
    /// 这一程收摊的块高
    pub leaves_at: u64,
    /// 光尘的去处：烧毁地址
    pub sink: String,
    pub balance: u64,
    pub height: u64,
    pub night: bool,
    pub offers: Vec<TraderOffer>,
}

/// 旅人这一程在哪、卖什么、你有多少光尘。情报内容一律不在这里给。
#[tauri::command]
pub fn world_trader_state(state: AppCtx) -> TraderSnapshot {
    let core = state.core.read().unwrap().clone();
    let address = core.as_ref().map(|c| c.address.clone()).unwrap_or_default();
    let height = state.chain.height();
    let (x, y) = world::trader::stall_at(height);
    let (st, _) = world::dust::replay(&state.dust.load(), &dust_view(&state));
    TraderSnapshot {
        rule: world::trader::TRADER_RULE_VERSION.into(),
        x,
        y,
        period: world::trader::period_of(height),
        leaves_at: (world::trader::period_of(height) + 1) * world::trader::TRADER_PERIOD,
        sink: world::trader::sink_address(),
        balance: st.balance_of(&address),
        height,
        night: world::resource::is_night(height),
        offers: [world::trader::ROUTE, world::trader::FRONTIER, world::trader::LEDGER]
            .into_iter()
            .filter_map(|id| {
                world::trader::price_of(id).map(|price| TraderOffer {
                    id: id.to_string(),
                    price,
                })
            })
            .collect(),
    }
}

#[derive(serde::Serialize)]
pub struct TraderPurchase {
    pub record: world::dust::DustRecord,
    pub quote: world::trader::Quote,
    /// 付完钱之后的余额
    pub balance: u64,
}

/// 买一件情报：先算情报、再签一条 `send` 把光尘送去烧毁地址，最后才交货。
///
/// 顺序是刻意的——**算不出来就不该收钱**，所以报价放在前面；付款走的是已有的
/// `send` 操作，不新增协议操作，老节点照收不误；收款方没有私钥，光尘是真烧掉的。
#[tauri::command]
pub fn world_trader_buy(state: AppCtx, id: String) -> Result<TraderPurchase, String> {
    let core = state.core.read().unwrap().clone().ok_or("钱包未解锁")?;
    let price =
        world::trader::price_of(&id).ok_or_else(|| format!("旅人没有这件情报：{id}"))?;
    let height = state.chain.height();
    let (st, _) = dust_settled(&state)?;
    let balance = st.balance_of(&core.address);
    if balance < price {
        return Err(format!(
            "光尘不够：这件情报要 {price} ✦，你有 {balance} ✦——去采一棵树（60 ✦）就够了"
        ));
    }
    let quote = world::trader::quote(&id, height, &st)?;
    let sink = world::trader::sink_address();
    let pk_hash = world::signer_pk_hash(&core);
    let rec = state
        .dust
        .commit_as(&pk_hash, &dust_view(&state), |seq, prev| {
            world::dust::sign_record(
                &core,
                seq,
                &prev,
                &world::dust::DustDraft {
                    op: world::dust::op::SEND,
                    epoch: 0,
                    x: 0,
                    y: 0,
                    kind: 0,
                    amount: price,
                    to: &sink,
                    height,
                    ts: now_unix(),
                },
            )
        })?;
    state.net.broadcast_dust(rec.clone());
    Ok(TraderPurchase {
        record: rec,
        quote,
        balance: balance - price,
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
fn live_world_full(state: &AppState) -> Result<world::World, String> {
    let w = world::replay_world(&state.entities.load(), &chain_view(state));
    if !w.report.ok {
        return Err(format!(
            "实体账本第 {} 条起校验失败：{}。先排查账本再继续写入",
            w.report.first_bad, w.report.reason
        ));
    }
    Ok(w)
}

fn live_world(state: &AppState) -> Result<std::collections::BTreeMap<String, world::Entity>, String> {
    Ok(live_world_full(state)?.entities)
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

#[derive(serde::Serialize)]
pub struct DigResult {
    pub record: world::EntityRecord,
    pub x: u16,
    pub y: u16,
    /// 被剥掉的那一层
    pub layer: u16,
    /// 产出的材料（0 土 / 1 石 / 2 矿）
    pub kind: u8,
    /// 离地表的深度（0 = 地表那一层）
    pub depth: u16,
    /// 挖完以后这一列的新顶面
    pub top: u16,
    /// 挖完以后手上的材料
    pub materials: [u32; world::MAT_COUNT],
}

/// ⛏ 挖一格：剥掉这一列露在最上面的那一层，材料当场进账（§9.20）。
///
/// 坐标和层数都不接受前端"随便指"——层数由这一列的历史算出来（地表层 − 已挖层数），
/// 所以"我挖到了什么"不是客户端说了算，而是大地高度场说了算。想拿矿，就真往下挖。
#[tauri::command]
pub fn world_dig(state: AppCtx, x: u16, y: u16) -> Result<DigResult, String> {
    let core = state.core.read().unwrap().clone().ok_or("钱包未解锁")?;
    if x >= world::WORLD_W || y >= world::WORLD_H {
        return Err(format!("坐标 ({x},{y}) 超出世界边界 {}×{}", world::WORLD_W, world::WORLD_H));
    }
    let w = live_world_full(&state)?;
    if world::is_water(x, y) {
        return Err(format!("({x},{y}) 是水面，挖不动"));
    }
    let top = world::column_top(&w.dug, x, y);
    if top < world::DUG_FLOOR {
        return Err(format!("({x},{y}) 已经挖到岩床（第 {} 层）", world::DUG_FLOOR));
    }
    let depth = world::surface_layer(x, y) - top;
    let kind = world::material_at_depth(depth);
    let rec = commit_record(
        &state,
        &core,
        &world::Draft {
            op: world::op::DIG,
            entity_id_hex: "",
            parent_hex: "",
            name: "",
            x,
            y,
            layer: top,
            kind,
            height: state.chain.height(),
            ts: now_unix(),
        },
    )?;
    let after = world::replay_world(&state.entities.load(), &chain_view(&state));
    Ok(DigResult {
        record: rec,
        x,
        y,
        layer: top,
        kind,
        depth,
        top: world::column_top(&after.dug, x, y),
        materials: after
            .materials
            .get(&core.address)
            .copied()
            .unwrap_or([0; world::MAT_COUNT]),
    })
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
    let w = live_world_full(&state)?;
    voxel_taken(&w.entities, x, y, layer, "")?;
    // 还埋在地下的层不能放东西：那是石头的地盘，先挖开才轮得到你造
    let top = world::column_top(&w.dug, x, y);
    if layer < top {
        return Err(format!(
            "({},{}) 第 {layer} 层还埋在地下（这一列顶面在第 {top} 层），先挖开再建",
            x, y
        ));
    }
    // 材料硬拦放在钱包侧：共识只把账算准（见 world.rs 的说明），签名之前这里必须挡住
    let cost = world::build_cost(kind);
    let have = w
        .materials
        .get(&core.address)
        .copied()
        .unwrap_or([0; world::MAT_COUNT]);
    for (i, need) in cost.iter().enumerate() {
        if have[i] < *need {
            return Err(format!(
                "材料不足：造「{name}」要 {need} 份{}，手上只有 {}。先去挖地",
                world::mat_name(i as u8),
                have[i]
            ));
        }
    }
    let parent_hex = parent_hex.trim().to_string();
    if !parent_hex.is_empty() {
        match w.entities.get(&parent_hex) {
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


// ---------------------------------------------------------------- 兑换池（§9.17）

/// 池子的只读快照：储备、价格、我的份额与两条余额。
///
/// 光尘分两本账：**共识里的**（v5 起进块结算，能拿去做市/兑换）和
/// **gossip 里的**（刚签还没等到块）。UI 必须分开说，否则用户会以为
/// "我有 60 ✦" 却换不动——那不是 bug，是还没确认。
#[derive(serde::Serialize)]
pub struct PoolSnapshot {
    pub rule: String,
    pub id: String,
    pub fee_bps: u64,
    /// 硬分叉 v5 的激活高度
    pub activation_height: u64,
    /// 本机链头是否已经在 v5 规则下出块
    pub active: bool,
    pub ready: bool,
    /// QLX 储备（10^-9）
    pub qlx: u64,
    /// 光尘储备（✦）
    pub dust: u64,
    pub lp_total: u64,
    /// 首次做市永久锁死的份额
    pub locked_lp: u64,
    pub my_lp: u64,
    /// 1 QLX 值多少 ✦
    pub price: Option<f64>,
    pub my_qlx: u64,
    /// 共识确认的光尘（能做市的那部分）
    pub my_dust: u64,
    /// 已签名、还没进块的增量
    pub pending_dust: u64,
    pub height: u64,
    pub unlocked: bool,
}

#[tauri::command]
pub fn pool_state(state: AppCtx) -> PoolSnapshot {
    let core = state.core.read().unwrap().clone();
    let address = core.as_ref().map(|c| c.address.clone()).unwrap_or_default();
    let height = state.chain.height();
    let pool = state.chain.pool_pub();
    let active = state.chain.dust_fork_active();
    let confirmed = state.chain.dust_balance_pub(&address);
    let (gossip, _) = world::dust::replay(&state.dust.load(), &dust_view(&state));
    let total = gossip.balance_of(&address);
    let (my_dust, pending) = if active {
        (confirmed, total.saturating_sub(confirmed))
    } else {
        // 分叉前共识里没有光尘，整本账还没被块结算：按现状显示为"可用"，
        // 但 `active=false` 会让界面说清楚池子还不能开张。
        (total, 0)
    };
    PoolSnapshot {
        rule: world::pool::POOL_RULE_VERSION.into(),
        id: world::pool::POOL_ID.into(),
        fee_bps: world::pool::FEE_BPS,
        activation_height: simchain::BLOCK_V_V5_ACTIVATION,
        active,
        ready: pool.ready(),
        qlx: pool.qlx,
        dust: pool.dust,
        lp_total: pool.lp_total,
        locked_lp: world::pool::MIN_LIQUIDITY,
        my_lp: pool.shares_of(&address),
        price: pool.price_dust_per_qlx(),
        my_qlx: state.chain.balance_of(&address),
        my_dust,
        pending_dust: pending,
        height,
        unlocked: core.is_some(),
    }
}

#[derive(serde::Serialize)]
pub struct PoolTxResult {
    pub tx_hash: String,
    pub height: u64,
    pub pending: bool,
}

/// 池子交易与普通转账走同一条提交路径：链内验签 + nonce + 余额试算，
/// 入池后再由矿工按同一套 `apply_pool_op` 结算。池子没有地址、没有私钥，
/// 所以这里没有任何"给池子转钱"的动作——储备只由曲线放行。
fn submit_pool_op(state: &AppState, op: tx::PoolOp) -> Result<PoolTxResult, String> {
    let core = state.core.read().unwrap().clone().ok_or("钱包未解锁")?;
    let height = state.chain.height();
    let envelope = tx::TxEnvelope::new(
        bech32_decode_addr(&core.address)?,
        state.chain.pending_nonce(&core.address),
        height + 50,
    )
    .with_pool(op);
    let digest = tx::tx_digest(&envelope);
    let sig = crypto::sign(&core.signing_key, &digest);
    let tx_hash = state.chain.submit(&core, envelope.clone(), sig.clone())?;
    let wire =
        simchain::BlockWire::new(&envelope, &sig, &core.pk_bytes, &core.addr_salt);
    state.net.broadcast_tx(wire);
    Ok(PoolTxResult {
        tx_hash: hex::encode(tx_hash),
        height,
        pending: true,
    })
}

/// 做市：两边都按现有比例存入（首次存入时自己定比例）。
#[tauri::command]
pub fn pool_add(state: AppCtx, qlx: String, dust: u64) -> Result<PoolTxResult, String> {
    let qlx = parse_amount(&qlx)?;
    if qlx == 0 || dust == 0 {
        return Err("做市两边都要有：QLX 和光尘都得大于 0".into());
    }
    submit_pool_op(&state, tx::PoolOp::AddLiquidity { qlx, dust })
}

/// 撤资：按份额占比取回两边，锁死的那份永远取不回来。
#[tauri::command]
pub fn pool_remove(state: AppCtx, lp: u64) -> Result<PoolTxResult, String> {
    if lp == 0 {
        return Err("撤资份额必须大于 0".into());
    }
    submit_pool_op(&state, tx::PoolOp::RemoveLiquidity { lp })
}

/// 兑换：方向与数量由前端提议，报价、滑点与余额全部由链上重算。
#[tauri::command]
pub fn pool_swap(
    state: AppCtx,
    dir: String,
    amount: String,
    min_out: u64,
) -> Result<PoolTxResult, String> {
    match dir.as_str() {
        "qlx" => submit_pool_op(
            &state,
            tx::PoolOp::SwapQlxForDust {
                qlx_in: parse_amount(&amount)?,
                min_out,
            },
        ),
        "dust" => {
            let dust_in: u64 = amount
                .trim()
                .parse()
                .map_err(|_| "光尘数量必须是整数（✦）".to_string())?;
            submit_pool_op(&state, tx::PoolOp::SwapDustForQlx { dust_in, min_out })
        }
        other => Err(format!("未知的兑换方向：{other}")),
    }
}
