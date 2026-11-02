//! 中继节点闸门（DESIGN.md §9.32）：本机 loopback 起两个 `--node` 进程，
//! 证明「固定端口 + 种子名单 + 地址 gossip」这条路真的能把第三台机器接进网，
//! 并且证明无头节点**一块都不出**（它只验账与转发，收益仍然归钱包自己的门票）。
//!
//! 这里没有测试运行器之外的假设：端口现取现用（避免与本机正在跑的钱包抢 47410），
//! 邻居之间只靠种子地址相识；每个进程的数据目录各自独立，同机引导注册表
//! （`peers-registry.json`）因此帮不上忙——连上了就只能是种子/地址 gossip 的功劳。

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Node {
    dir: PathBuf,
    log: PathBuf,
    child: Child,
}

impl Node {
    fn start(tag: &str, port: u16, seeds: &[String]) -> Node {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("qlight-relay-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("node.log");
        let logfile = std::fs::File::create(&log).unwrap();
        let stdout = logfile.try_clone().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_qlight-wallet"))
            .arg("--node")
            .env("QL_DATA_DIR", &dir)
            .env("QL_LISTEN_PORT", port.to_string())
            .env("QL_SEEDS", seeds.join(","))
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(logfile))
            .spawn()
            .expect("起无头节点失败");
        Node { dir, log, child }
    }

    fn text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// 节点自己写下的 PeerId（运营者就是抄这一份填种子名单）
    fn peer_id(&self) -> String {
        let path = self.dir.join("peer-id.txt");
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(&path) {
                if !s.trim().is_empty() {
                    return s.trim().to_string();
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("{} 里始终没有 peer-id.txt：\n{}", path.display(), self.text());
    }

    fn dialed_addr(&self, port: u16) -> String {
        format!("/ip4/127.0.0.1/tcp/{port}/p2p/{}", self.peer_id())
    }

    /// 本节点有没有出过块：blocks.jsonl 只有创世那一行（或还没有文件）= 一块没出
    fn mined_blocks(&self) -> usize {
        match std::fs::read_to_string(self.dir.join("blocks.jsonl")) {
            Ok(txt) => txt.lines().filter(|l| !l.trim().is_empty()).count(),
            Err(_) => 0,
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    l.local_addr().unwrap().port()
}

/// 轮询到条件成立；超时返回 false，由调用方把日志原文打出来
fn wait_for(deadline: Duration, mut f: impl FnMut() -> bool) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

#[test]
fn seed_node_relay_grows_the_mesh() {
    let port_a = free_port();
    let port_b = free_port();
    let port_c = free_port();

    // A：第一台常驻节点，谁也不认识
    let a = Node::start("a", port_a, &[]);
    assert!(
        wait_for(Duration::from_secs(10), || a.text().contains("监听固定端口 tcp/")),
        "A 没绑上固定端口：\n{}",
        a.text()
    );
    assert!(a.text().contains(&format!("监听固定端口 tcp/{port_a}")), "A 的固定端口不是 {port_a}：\n{}", a.text());

    // B：只认 A 的种子地址
    let seeds_b = vec![a.dialed_addr(port_a)];
    let mut b = Node::start("b", port_b, &seeds_b);
    assert!(
        wait_for(Duration::from_secs(30), || b.text().contains("种子名单发现邻居")),
        "B 没有从种子名单拨到 A：\nB:\n{}\nA:\n{}",
        b.text(),
        a.text()
    );
    assert!(
        wait_for(Duration::from_secs(30), || a.text().contains("已连接邻居")
            && b.text().contains("已连接邻居")),
        "A/B 之间没有真的建立连接：\nA:\n{}\nB:\n{}",
        a.text(),
        b.text()
    );

    // C：也只认 A。它和 B 之间没有任何直接线索——
    // 连上 B 只能是因为 B 自己把地址发到了 qlx/peers/v1，而 A 把它转发给了 C。
    let seeds_c = vec![a.dialed_addr(port_a)];
    let mut c = Node::start("c", port_c, &seeds_c);
    assert!(
        wait_for(Duration::from_secs(60), || c.text().contains("地址gossip发现邻居")),
        "C 没通过地址 gossip 认识 B（种子只有 A 一个）：\nC:\n{}\nA:\n{}\nB:\n{}",
        c.text(),
        a.text(),
        b.text()
    );
    let pid_b = b.peer_id();
    assert!(
        wait_for(Duration::from_secs(30), || c.text().contains(&format!("已连接邻居 {pid_b}"))),
        "C 最终没连上 B（{pid_b}）：\nC:\n{}",
        c.text()
    );

    // 三台中继一块都不该出：没有解锁的钱包，出块状态机第一眼就 return
    std::thread::sleep(Duration::from_secs(6));
    for (tag, n) in [("A", &a), ("B", &mut b), ("C", &mut c)] {
        let mined = n.mined_blocks();
        assert!(mined <= 1, "{tag} 作为中继出了块：blocks.jsonl 有 {mined} 行\n{}\n{}", tag, n.text());
    }

    // 收尾前把三台的邻居数打出来，方便看是不是真的长成了 a—b / a—c / b—c
    let lines = |n: &Node| {
        n.text()
            .lines()
            .filter(|l| l.contains("已连接邻居") || l.contains("发现邻居"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    println!("A:\n{}\nB:\n{}\nC:\n{}", lines(&a), lines(&b), lines(&c));
}
