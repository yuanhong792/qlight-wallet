//! 无头节点（`qlight-wallet --node`）：用一台 7×24 开着的机器顶替"服务器"这个角色。
//!
//! 它和钱包跑的是同一份代码、同一套判据：验块、追账、中继交易与块 —— 这些就是
//! 一个"服务器"要做的全部事情。区别只有一条：**没有界面**，事件出口用
//! [`net::Sink::Headless`]，状态打在 stderr 上。
//!
//! 出块与否看有没有 `--mine`：
//! - 不加（默认，中继）：`core_slot` 永远是空的，出块状态机第一眼就 return，
//!   所以这台机器既拿不到奖励，也不能替任何人改变账本规则；
//! - 加 `--mine`：这台机器解锁**自己那把钥匙**，把界面上那颗「⛏ 启动挖矿」按钮
//!   背后跑的程序照搬进线程（[`mine_one_hop`] 是两边共用的唯一实现），
//!   收益进它自己的地址——和一台开着界面的钱包没有任何判据差别。
//!
//! `--send <ql1…> <金额>` 是第三件事：在没有界面的机器上把钱转出去。它同样走
//! 界面 `send_qlx` 那条路（同一份信封、同一次全量校验、同一句广播），但核心槽留空——
//! 签转账的进程不出块，也一分新币都发行不出来。
//!
//! 为什么需要它：gossipsub 本身就是中继机制，但前提是"有人一直在线、而且拨得到"。
//! 桌面钱包会关机、会睡眠、会换 IP，所以全网需要一个**固定端口 + 固定 PeerId** 的
//! 常驻入口，别人把它写进种子名单就不会失联（比特币的 seed node 同理）。
//! 去中心化买的从来不是"没有常驻机器"，而是"没有任何一台机器说了算"。
//!
//! 用法：
//! ```text
//! QL_DATA_DIR=/var/lib/qlight-node qlight-wallet --node
//! QL_LISTEN_PORT=47410                    # 默认值，写死它种子名单才有意义
//! QL_SEEDS="/ip4/203.0.113.9/tcp/47410/p2p/12D3Koo…"   # 从哪台接进网
//! QL_ADVERTISE="/ip4/203.0.113.9/tcp/47410"            # 公网 IP 程序猜不出，必须运营者给
//! qlight-wallet --node --mine             # 这台机器自己出块（目录里没钥匙就当场建一把）
//! QL_WALLET_PASSWORD=…                    # 解锁用；不设就现场问一次，别写进命令行历史
//! ```
//! Windows 的绿色版 exe 是 GUI 子系统（`windows_subsystem = "windows"`），
//! 直接双击没有控制台可打印；在 cmd 里跑请重定向：
//! `qlight-wallet.exe --node > node.log 2>&1`。

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::{keystore, net, simchain, world};

pub const USAGE: &str = "QLight 节点\n\
  qlight-wallet              打开桌面钱包\n\
  qlight-wallet --node       起一个无头节点（默认只验账与转发，不出块、不持币）\n\
  qlight-wallet --node --mine  同一份程序，但解锁自己的钥匙真出块、真发行\n\
  qlight-wallet --node --send <ql1…> <金额>  签一笔转账、广播、等到它进块再退出\n\
环境变量：QL_DATA_DIR 数据目录 / QL_LISTEN_PORT 固定端口（默认 47410）\n\
         QL_SEEDS 种子名单（逗号分隔完整多地址）/ QL_ADVERTISE 对外公布地址\n\
         QL_WALLET_PASSWORD 出块时解锁钥匙的密码（不设则现场问一次）";

/// 一个数据目录里的三本账：链（余额）+ 世界实体 + 光尘
pub struct Store {
    pub chain: Arc<simchain::SimChain>,
    pub entities: Arc<world::EntityLedger>,
    pub dust: Arc<world::dust::DustLedger>,
}

/// GUI 与无头节点共用同一套打开流程：路径拼法、世界层挂载、链重置后旧世界的归档。
/// 两边一处代码，不会出现"钱包跑得好、中继跑不通"的口径分裂。
pub fn open_store(data_dir: &Path) -> Store {
    let chain = simchain::SimChain::new(
        data_dir.join("ledger.json"),
        data_dir.join("blocks.jsonl"),
    );
    // 链被归档重置 = 换了一个世界：同目录下的实体/光尘账本记的是旧链的块高与
    // 手续费交易，留在新链上会让每一次写入都判定「账本第 1 条起校验失败」。
    // 与链一样只归档不删除，tag 相同，便于成对找回。
    if let Some(tag) = chain.take_reset_tag() {
        for name in ["entities.jsonl", "dust.jsonl"] {
            let from = data_dir.join(name);
            if !from.exists() {
                continue;
            }
            let to = data_dir.join(format!("{name}.bak-world-{tag}"));
            if std::fs::rename(&from, &to).is_ok() {
                eprintln!("[QLight] 链已重置：旧世界的 {name} 已归档为 {}", to.display());
            }
        }
    }
    let entities = Arc::new(world::EntityLedger::new(data_dir.join("entities.jsonl")));
    let dust = Arc::new(world::dust::DustLedger::new(data_dir.join("dust.jsonl")));
    // v2 出块要向整本实体账本承诺世界（块哈希绑 `entity_root`），
    // v5 起出块还要带光尘记录：所以账本必须在网络拿到副本之前就先挂上
    chain.attach_world(entities.clone());
    chain.attach_dust(dust.clone());
    Store { chain, entities, dust }
}

// ---------------------------------------------------------------- 挖矿的共享口径

/// 已占用的格子（`released` 的不算：断租即让位，与 `world::live_at` 同一判据）
pub fn ore_taken(map: &std::collections::BTreeMap<String, world::Entity>) -> world::ore::Taken {
    let mut t = world::ore::Taken::new();
    for e in map.values() {
        if !e.released {
            t.insert((e.x as u32, e.y as u32, e.layer as u32));
        }
    }
    t
}

/// 一回合挖矿的真回执：界面念的那几个数与无头日志打的这几个数是同一份。
pub struct MineHop {
    pub record: world::EntityRecord,
    /// 发行凭据的交易摘要（下一个块若收下它，奖励进本机余额）
    pub claim_digest: [u8; 32],
    pub x: u16,
    pub y: u16,
    pub layer: u16,
    /// 预计结算块高与那一笔的发行量（最小单位）
    pub claim_height: u64,
    pub reward: u64,
    /// 这一笔所用的节奏档位（最小间隔，块）
    pub pace_floor: u64,
    /// 这一格落进世界以后的下一档（创造换节奏：v3 唯一的加速口径）
    pub pace_next_floor: u64,
}

/// ⛏️ 挖一回合的**唯一实现**：先看这把钥匙的节奏到没到，到了才在本回合的矿脉里
/// 按湿度从高到低取第一个还可挖的格子，提交一张发行凭据，并在世界里落一个贴地方块。
///
/// 顺序是判据的一部分：**先提交凭据，被拒就不该在世界里落方块**——否则造出一格
/// "挖不出发行"的矿。界面的按钮与无头节点的出块循环必须共用这一份：两处各写一遍，
/// 迟早有一遍写反。
///
/// `on_claim` 在凭据提交成功之后、方块落账之前触发（界面就是在这个位置广播它）。
pub fn mine_one_hop(
    chain: &simchain::SimChain,
    entities: &Arc<world::EntityLedger>,
    blocks_path: &Path,
    core: &Arc<keystore::WalletCore>,
    kind: u8,
    on_claim: impl FnOnce(simchain::BlockWire),
) -> Result<MineHop, String> {
    let view = world::ChainView::load(blocks_path);
    let replayed = world::replay_world(&entities.load(), &view);
    if !replayed.report.ok {
        return Err(format!(
            "实体账本第 {} 条起校验失败：{}。先排查账本再继续写入",
            replayed.report.first_bad, replayed.report.reason
        ));
    }
    let height = chain.height();
    let ready_at = chain.next_claim_height(&core.address);
    if ready_at > height + 1 {
        return Err(format!(
            "本回合已结算：下一笔挖矿发行在块高 {ready_at}（约 {} 秒后）",
            (ready_at - height - 1) * simchain::BLOCK_INTERVAL_SECS
        ));
    }
    // 在飞的矿签只许一张：它只在"下一个块"里成立，攒着的第二张永远进不了块，
    // 却会被周期性再广播推给全网重验一次（界面上连点两次也会走到这里）
    if chain.pending_claims_of(&core.address) > 0 {
        return Err(
            "上一张矿签还挂在池里等打包：它进块、或被下一个块重画掉之后才能再落一格"
                .into(),
        );
    }
    let words = world::terrain::seed_words(&simchain::genesis_hash_hex(), &world::chain_vrfs(blocks_path));
    let hits = world::ore::scan(&words, height, &ore_taken(&replayed.entities), 8);
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
    let pace = chain.claim_pace(&core.address);

    // 先提交发行凭据：被拒就不该在世界里落方块（否则会造出一格"挖不出发行"的矿）
    let (wire, claim_digest) = chain.submit_claim(core, x, y, layer)?;
    on_claim(wire);

    let name = format!("矿·{}_{}_{}", x, y, layer);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let record = entities.commit_as(&world::signer_pk_hash(core), &view, |seq, prev| {
        world::sign_record(
            core,
            seq,
            &prev,
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
                ts,
            },
        )
    })?;
    Ok(MineHop {
        record,
        claim_digest,
        x,
        y,
        layer,
        claim_height: height + 1,
        reward: chain.claim_preview(&core.address).1,
        pace_floor: pace.1,
        pace_next_floor: simchain::craft_floor(pace.0 + 1),
    })
}

/// 出块要解锁自己的钥匙。目录里已有 `keystore.json` 就用密码开它（和界面那把同一把）；
/// 没有就当场建一把新的并把公钥库落盘。
///
/// 密码优先取 `QL_WALLET_PASSWORD`；没设就向 stdin 要一次——写进命令行参数会被同机
/// 任何人的 `ps` 看到，所以那是最后选项。
fn unlock_or_create(data_dir: &Path) -> Result<keystore::WalletCore, String> {
    let path = data_dir.join("keystore.json");
    let from_env = std::env::var("QL_WALLET_PASSWORD").ok().filter(|v| !v.is_empty());
    let password = match from_env {
        Some(v) => v,
        None => {
            eprint!("[QLight] 钱包密码：");
            let mut line = String::new();
            if std::io::stdin().lock().read_line(&mut line).is_err() || line.trim().is_empty() {
                return Err("没拿到密码：出块必须先解锁自己的钥匙".into());
            }
            line.trim_end_matches(['\n', '\r']).to_string()
        }
    };
    if path.exists() {
        return keystore::unlock_wallet(&path, &password).map_err(|e| format!("解锁失败：{e}"));
    }
    let (_, core) = keystore::create_wallet(&path, &password)?;
    eprintln!(
        "[QLight]   目录里没有钥匙，已新建一把：{}（备份办法见钱包的「导出备份」）",
        core.address
    );
    Ok(core)
}

/// 无头出块循环 = 界面上那颗按钮按下之后跑的同一个程序：1.2 秒一跳，先看链上到没到
/// 节奏，到了才按真命令；没到就什么都不做（不是重试，是不该按）。
fn spawn_hop_loop(node: Arc<net::Net>, core: Arc<keystore::WalletCore>, blocks_path: PathBuf) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(1200));
        let chain = node.chain.clone();
        // 先在内存里看节奏（和界面 mineTick 同一句）：没到就不值得为了判"没到"
        // 把整本块行与实体账本重读一遍——链跑到几千块时那是一次真真切切的几十兆。
        // 第二个条件是"在飞的只许一张"：矿签只在它签出后的那一个块里成立（下一个块
        // 一重画地表它就作废），窗口打开的那几秒里多签的每一张都是注定进不了块的垃圾，
        // 而且会被 5 秒一次的再广播推给全网重验一遍（§9.34 三进程实测踩到的就是这个）。
        if chain.next_claim_height(&core.address) > chain.height() + 1
            || chain.pending_claims_of(&core.address) > 0
        {
            continue;
        }
        let entities = node.entities.clone();
        let res = mine_one_hop(&chain, &entities, &blocks_path, &core, 0, |wire| {
            node.broadcast_tx(wire)
        });
        match res {
            Ok(hop) => {
                let (x, y, layer, claim_height, reward, next_floor) =
                    (hop.x, hop.y, hop.layer, hop.claim_height, hop.reward, hop.pace_next_floor);
                node.broadcast_entity(hop.record);
                eprintln!(
                    "[QLight] 落一格矿：({x},{y}) 第 {layer} 层 → 块高 {claim_height} 结算，预计 {reward} 微 QLX；这一档每 {} 块一次，落过这一格后变 {next_floor} 块",
                    hop.pace_floor
                );
            }
            // 这三条都是"这一跳不该按"，不是故障：节奏未到 / 在飞的那张还没落地 /
            // 本回合扫不到空闲矿脉。等下一跳就是了，不念日志。
            Err(e) if e.starts_with("本回合已结算") || e.starts_with("上一张矿签") || e.starts_with("本回合无矿可挖") => {}
            Err(e) => {
                eprintln!("[QLight] 本进程停止挖矿：{e}");
                if e.contains("余额") || e.contains("解锁") || e.contains("账本") || e.contains("拒绝") {
                    return;
                }
            }
        }
    });
}

fn env_port() -> u16 {
    std::env::var("QL_LISTEN_PORT")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(net::DEFAULT_LISTEN_PORT)
}

/// 没给 `QL_DATA_DIR` 时的落脚处：`~/.qlight/node`（拿不到 HOME 就退回当前目录）
fn default_data_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".qlight").join("node")
}

/// `QL_DATA_DIR` 给的目录（没给就用默认位置），并且保证它已经存在
fn data_dir_from_env() -> PathBuf {
    let dir = match std::env::var("QL_DATA_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => default_data_dir(),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[QLight] 数据目录建不起来：{}（{e}）", dir.display());
        std::process::exit(1);
    }
    dir
}

/// 接线（中继、出块机、一次性转账三处共用）：数据目录 + 三本账 + 核心槽 → 一个 [`net::Net`]。
/// 核心槽给空 = 这个进程不出块、不发行；界面事件出口换成 stderr。
fn start_net(
    data_dir: &Path,
    store: &Store,
    core_slot: Arc<RwLock<Option<Arc<keystore::WalletCore>>>>,
) -> Arc<net::Net> {
    net::Net::start(
        net::NodeOpts {
            sink: net::Sink::Headless,
            data_dir: data_dir.to_path_buf(),
            // 注册表留在自己目录里：同机跑几台中互不掺和，跨目录靠种子名单找人
            registry_path: data_dir.join("peers-registry.json"),
            listen_port: env_port(),
            announce: net::env_list("QL_ADVERTISE"),
            // `QL_DISABLE_MDNS=1`：只靠种子名单与地址 gossip 找人。中继闸门
            // （tests/relay_node.rs）要证明的就是这条路，组播在 Linux runner 上
            // 会让同机进程先相识，把前提抹掉（DESIGN.md §9.39）。
            use_mdns: std::env::var("QL_DISABLE_MDNS").is_err(),
        },
        store.chain.clone(),
        store.entities.clone(),
        store.dust.clone(),
        core_slot,
    )
}

/// 一次性转账（`--node --send <ql1…> <QLX>`）：在没有界面的机器上把钱转出去。
///
/// 走的是界面 `send_qlx` 那一条路：同一份信封构造、同一次 `submit` 全量校验
/// （真 ML-DSA-65 验签 + nonce + 可支配余额 + 过期）、同一句广播原件。差别只有两条：
/// - 核心槽留空——这台机器**不出块、不发行**，只是拿自己的钥匙签一笔转账；
/// - 先把本机链追上网络：余额和 nonce 必须以全网那条链为准，拿自己那份旧的会算错。
///
/// 到账的判据不是"我发出去了"，而是这把钥匙**已上链的 nonce 前进了**——
/// 那意味着某个块真的把这笔原件收进去并逐笔重验过签。
pub fn send_once(to: &str, amount: &str) {
    let data_dir = data_dir_from_env();
    let units = match crate::commands::parse_amount(amount) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[QLight] 金额看不懂：{e}");
            std::process::exit(1);
        }
    };
    let to_addr = match simchain::bech32_decode_addr(to) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[QLight] 收款地址不合法：{e}");
            std::process::exit(1);
        }
    };
    let store = open_store(&data_dir);
    let core = match unlock_or_create(&data_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[QLight] {e}");
            std::process::exit(1);
        }
    };
    if core.address == to {
        eprintln!("[QLight] 不能转给自己");
        std::process::exit(1);
    }
    let chain = store.chain.clone();
    // 空核心槽：这个进程不出块，也就一分新币都发行不出来
    let node = start_net(&data_dir, &store, Arc::new(RwLock::new(None)));
    eprintln!("[QLight] 无头转账：{} → {to}", core.address);
    eprintln!(
        "[QLight]   本机块高 {}，邻居 {} 台——先追平网络再签，否则 nonce 与余额都是旧的",
        chain.height(),
        node.peers_count()
    );

    // ① 追网络：见过邻居、而且本机块高被网络推着往前走了一段，余额和 nonce 才算有准
    let start_h = chain.height();
    let sync_deadline = std::time::Instant::now() + Duration::from_secs(90);
    while node.peers_count() == 0 || chain.height() < start_h + 3 {
        std::thread::sleep(Duration::from_millis(400));
        if std::time::Instant::now() >= sync_deadline {
            eprintln!(
                "[QLight]   一分半没追上网（块高 {}／本机原有 {start_h}，邻居 {} 台）——不签：nonce 与余额都还是旧的",
                chain.height(),
                node.peers_count()
            );
            std::process::exit(1);
        }
    }
    let to_before = chain.balance_of(to);
    eprintln!(
        "[QLight]   已追到块高 {}（邻居 {} 台）；收款方现有余额 {} QLX",
        chain.height(),
        node.peers_count(),
        to_before as f64 / 1e9
    );

    // ② 签 → 本机全量校验入池 → 广播原件；到账判据只看收款方余额真长出这一笔
    let from_addr = simchain::bech32_decode_addr(&core.address).unwrap_or([0u8; 32]);
    let mut sent: Vec<[u8; 32]> = Vec::new();
    let sign_and_send = |sent: &mut Vec<[u8; 32]>, first: bool| -> Result<(), String> {
        let nonce = chain.pending_nonce(&core.address);
        let envelope =
            crate::tx::TxEnvelope::new(from_addr, nonce, chain.height() + 50).transfer(to_addr, units);
        let digest = crate::tx::tx_digest(&envelope);
        let sig = crate::crypto::sign(&core.signing_key, &digest);
        chain.submit(&core, envelope.clone(), sig.clone())?;
        node.broadcast_tx(simchain::BlockWire::new(
            &envelope,
            &sig,
            &core.pk_bytes,
            &core.addr_salt,
        ));
        sent.push(digest);
        if first {
            eprintln!(
                "[QLight]   已入池并广播：nonce {nonce}，{} QLX（手续费 {} 微 QLX）",
                units as f64 / 1e9,
                simchain::FEE
            );
        } else {
            eprintln!("[QLight]   按当下的 nonce {nonce} 重签一份再发（上一份已被追账快照清掉）");
        }
        Ok(())
    };
    if let Err(e) = sign_and_send(&mut sent, true) {
        eprintln!("[QLight] 这笔转账本机就不成立：{e}");
        std::process::exit(1);
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    let mut next_retry = std::time::Instant::now() + Duration::from_secs(12);
    loop {
        std::thread::sleep(Duration::from_millis(600));
        // 收款方到账 = 某个块把这笔原件逐笔重验签收了进去（这比"我发出去了"硬得多）
        if chain.balance_of(to) >= to_before + units {
            let landed = sent
                .iter()
                .find(|d| chain.is_applied(d))
                .map(hex::encode)
                .unwrap_or_default();
            eprintln!(
                "[QLight]   到账确认：收款方 {} 余额 {} → {} QLX；本机余额 {} QLX；交易 {}",
                to,
                to_before as f64 / 1e9,
                chain.balance_of(to) as f64 / 1e9,
                chain.balance_of(&core.address) as f64 / 1e9,
                &landed[..16.min(landed.len())]
            );
            eprintln!("[QLight]   块高 {}，全网 {} 台各有一份同样的账本", chain.height(), node.peers_count() + 1);
            return;
        }
        if std::time::Instant::now() >= deadline {
            eprintln!(
                "[QLight]   三分钟没等到这笔进块（块高 {}，池子 {} 笔）——这笔还在池里或被拒，请核对日志",
                chain.height(),
                chain.mempool_len()
            );
            std::process::exit(1);
        }
        if std::time::Instant::now() >= next_retry {
            next_retry = std::time::Instant::now() + Duration::from_secs(12);
            // 池里还留着某一份，就继续等它；整链采用会清空 mempool，那时按新的 nonce 重签
            if !sent.iter().any(|d| chain.is_pooled(d)) {
                let _ = sign_and_send(&mut sent, false);
            }
        }
    }
}

/// 无头节点的入口：常驻，Ctrl-C 结束（账本都是逐条落盘，没有需要收尾的状态）。
/// `mine=false` 只做验账与转发；`mine=true` 解锁自己的钥匙，同一套程序真出块、真发行。
pub fn run(mine: bool) {
    let data_dir = data_dir_from_env();
    let listen_port = env_port();
    let announce = net::env_list("QL_ADVERTISE");
    let seeds = net::collect_seeds(&data_dir);
    let my_pid = net::load_identity(&data_dir).public().to_peer_id();
    // 运营者要往种子名单里写的地址，IP 与端口他自己知道，PeerId 只有本机知道
    if let Err(e) = std::fs::write(data_dir.join("peer-id.txt"), my_pid.to_string()) {
        eprintln!("[QLight] peer-id.txt 写不进去（{e}）——抄 PeerId 请看下面这行");
    }

    eprintln!(
        "[QLight] 无头{}节点启动",
        if mine { "出块" } else { "中继" }
    );
    eprintln!("[QLight]   数据目录：{}", data_dir.display());
    eprintln!("[QLight]   节点身份：{my_pid}（重启不变，可直接写进种子名单）");
    eprintln!("[QLight]   监听端口：tcp/{listen_port}（被占用会自动退回随机端口）");
    eprintln!("[QLight]   种子名单：{} 条", seeds.len());
    if announce.is_empty() {
        eprintln!(
            "[QLight]   QL_ADVERTISE 未设置：只会对外公布回环地址，同机/局域网能连，公网拨不到我"
        );
    }

    let store = open_store(&data_dir);
    // 空核心槽 = 永远"没解锁"：出块状态机不会启动，这台机器只做验账与转发。
    // --mine 就是往这个槽里放一把自己的钥匙——放下去之后跑的判据与钱包完全一致。
    let core_slot: Arc<RwLock<Option<Arc<keystore::WalletCore>>>> = Arc::new(RwLock::new(None));
    if mine {
        let core = match unlock_or_create(&data_dir) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[QLight] {e}");
                std::process::exit(1);
            }
        };
        eprintln!("[QLight]   出块钥匙：{}", core.address);
        eprintln!("[QLight]   节奏与判据：发行只认这把钥匙签的矿格坐标，收益只进它自己的余额");
        *core_slot.write().unwrap() = Some(Arc::new(core));
    } else {
        eprintln!("[QLight]   本进程不出块、不持币：收益仍然归各台钱包自己签的门票");
    }
    let node = start_net(&data_dir, &store, core_slot.clone());
    if mine {
        let core = core_slot.read().unwrap().clone().expect("上面刚放进去");
        spawn_hop_loop(node.clone(), core, data_dir.join("blocks.jsonl"));
    }

    loop {
        std::thread::sleep(Duration::from_secs(10));
        let scope = node.scope_report();
        eprintln!(
            "[QLight] {}：块高 {} tip {} 邻居 {}（本机 {} / 局域网 {} / 公网 {}）",
            if mine { "出块" } else { "中继" },
            store.chain.height(),
            short_hash(&store.chain.tip_hash_hex_pub()),
            node.peers_count(),
            scope["loopback"].as_u64().unwrap_or(0),
            scope["lan"].as_u64().unwrap_or(0),
            scope["public"].as_u64().unwrap_or(0),
        );
    }
}

fn short_hash(hex: &str) -> String {
    if hex.len() > 12 {
        format!("{}…", &hex[..12])
    } else {
        hex.to_string()
    }
}
