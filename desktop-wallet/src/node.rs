//! 无头中继节点（`qlight-wallet --node`）：用一台 7×24 开着的机器顶替"服务器"这个角色。
//!
//! 它和钱包跑的是同一份代码、同一套判据：验块、追账、中继交易与块 —— 这些就是
//! 一个"服务器"要做的全部事情。区别只有两条：
//! - **不出块、不持币**：`core_slot` 永远是空的，出块状态机第一眼就 return，
//!   所以中继节点既拿不到奖励，也不能替任何人改变账本规则；
//! - **没有界面**：事件出口用 [`net::Sink::Headless`]，状态打在 stderr 上。
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
//! ```
//! Windows 的绿色版 exe 是 GUI 子系统（`windows_subsystem = "windows"`），
//! 直接双击没有控制台可打印；在 cmd 里跑请重定向：
//! `qlight-wallet.exe --node > node.log 2>&1`。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::{net, simchain, world};

pub const USAGE: &str = "QLight 节点\n\
  qlight-wallet              打开桌面钱包\n\
  qlight-wallet --node       起一个无头中继节点（不出块、不持币，只验账与转发）\n\
环境变量：QL_DATA_DIR 数据目录 / QL_LISTEN_PORT 固定端口（默认 47410）\n\
         QL_SEEDS 种子名单（逗号分隔完整多地址）/ QL_ADVERTISE 对外公布地址";

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

/// 无头节点的入口：常驻，Ctrl-C 结束（账本都是逐条落盘，没有需要收尾的状态）
pub fn run() {
    let data_dir = match std::env::var("QL_DATA_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => default_data_dir(),
    };
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("[QLight] 数据目录建不起来：{}（{e}）", data_dir.display());
        std::process::exit(1);
    }
    let listen_port = env_port();
    let announce = net::env_list("QL_ADVERTISE");
    let seeds = net::collect_seeds(&data_dir);
    let my_pid = net::load_identity(&data_dir).public().to_peer_id();
    // 运营者要往种子名单里写的地址，IP 与端口他自己知道，PeerId 只有本机知道
    if let Err(e) = std::fs::write(data_dir.join("peer-id.txt"), my_pid.to_string()) {
        eprintln!("[QLight] peer-id.txt 写不进去（{e}）——抄 PeerId 请看下面这行");
    }

    eprintln!("[QLight] 无头中继节点启动");
    eprintln!("[QLight]   数据目录：{}", data_dir.display());
    eprintln!("[QLight]   节点身份：{my_pid}（重启不变，可直接写进种子名单）");
    eprintln!("[QLight]   监听端口：tcp/{listen_port}（被占用会自动退回随机端口）");
    eprintln!("[QLight]   种子名单：{} 条", seeds.len());
    if announce.is_empty() {
        eprintln!(
            "[QLight]   QL_ADVERTISE 未设置：只会对外公布回环地址，同机/局域网能连，公网拨不到我"
        );
    }
    eprintln!("[QLight]   本进程不出块、不持币：收益仍然归各台钱包自己签的门票");

    let store = open_store(&data_dir);
    // 空核心槽 = 永远"没解锁"：出块状态机不会启动，中继只做验账与转发
    let core_slot: Arc<RwLock<Option<Arc<crate::keystore::WalletCore>>>> =
        Arc::new(RwLock::new(None));
    let node = net::Net::start(
        net::NodeOpts {
            sink: net::Sink::Headless,
            data_dir: data_dir.clone(),
            // 注册表留在自己目录里：同机跑几台中互不掺和，跨目录靠种子名单找人
            registry_path: data_dir.join("peers-registry.json"),
            listen_port,
            announce,
        },
        store.chain.clone(),
        store.entities.clone(),
        store.dust.clone(),
        core_slot.clone(),
    );

    loop {
        std::thread::sleep(Duration::from_secs(10));
        let scope = node.scope_report();
        eprintln!(
            "[QLight] 中继：块高 {} tip {} 邻居 {}（本机 {} / 局域网 {} / 公网 {}）",
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
