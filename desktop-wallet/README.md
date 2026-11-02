# QLight Wallet · 量子闪电桌面钱包（M1 PoC）

> 品牌：量子闪电 / QLight · 代币凭证符号 **QLX（权益表述）**
> 形态：macOS 桌面钱包（Tauri 2，后续复用内核出 Windows/Linux/Web 版）

## 1. 技术栈

| 层 | 选型 | 理由 |
|---|---|---|
| 桌面框架 | **Tauri 2**（`webkit2/wkwebview`） | 安装包 ~10MB，Rust 原生内核，密钥不进 JS 世界 |
| 签名 | **ml-dsa 0.1.x**（RustCrypto 纯 Rust，FIPS 204） | ML-DSA-65，无 C 依赖 |
| 哈希 | **sha3**（SHAKE-256，FIPS 202） | 地址 / tx digest / 块哈希 / VRF 全线统一 |
| 编码 | canonical CBOR（自实现，RFC 8949 最短编码） + bech32m | QL-TX-v1 信封、ql1 地址 |
| 密钥库 | Argon2id（32MB, t=3）→ AES-256-GCM 包裹种子 | 丢失密码无法找回，密文等价"种子丢失" |
| UI | 纯静态 HTML/CSS/JS（无 npm、无打包链） | 品牌视觉：金 #E3B54A × 深空蓝 #081A38 |

## 2. 密码学口径（全部来自 DESIGN.md）

- **账户签名**：ML-DSA-65（scheme_id = 1）。私钥持久化标准 = **32 字节种子**（FIPS 204 推荐），展开密钥由种子确定性导出，`SigningKey::from_seed`。
- **地址**：`ql1` bech32m，32 字节 commitment =
  - `[0]` 版本 0x00、`[1]` scheme_id 0x01、`[2]` 保留 0x00
  - `[3..32]` SHAKE-256("ql-addr-v1" ‖ pk ‖ salt)[0..29]
- **交易**（§14.2 子集）：
  ```
  TxEnvelope[v, chain_id, scheme_id, from, pk_hash?, nonce, expiry, feeCap, actions]  ← canonical CBOR
  digest = SHAKE-256("QL-TX-v1" ‖ cbor)[0..32]
  sig    = ML-DSA-65.sign(digest)
  ```
- **会话密钥（SessionGrant）**：§6.3 的钱包侧骨架已预留字段（session_nonce、expiry）；
  M2 起签发带 cap 的 PQ 临时密钥，主密钥退出游戏运行时热内存。
- **挖矿心跳（§3.3 轻节点）**：每块出示 VRF = SHAKE-256("QL-VRF-v1" ‖ seed ‖ height ‖ prev_hash)；
  M1 为本地哨兵，主网换真实 VRF 与存活挑战。

## 3. 内置账本链

M1 采用**内置单机账本链**，协议口径与设计文档一致，让收发/挖矿端到端真的跑起来：

- 出块 2s，奖励 = `3.33 QLX / 块`，每 31.5M 块（≈2 年）减半，硬顶 2.1 亿 QLX；
- 挖矿收益直接给钱包节点（主网对应"钱包节点 15% 份额"）；
- 手续费 0.001 QLX，nonce 严格递增，expiry = 当前高度 + 50；
- 账本 JSON 落盘 `app_data/ledger.json`，密钥库落盘 `app_data/keystore.json`。

测试网上线后：`send_qlx` / `chain_state` 换成真实节点 RPC（§14.1 里的 `ql_sendRawTx`、`ql_getBalance`），钱包 UI 与签名内核零改动。

## 4. 运行

```bash
cd desktop-wallet
scripts/build-macos.sh   # 一键：cargo build --release → .app → QLight-Wallet.dmg
```

首次运行 → 设置密码 → 生成钱包 → 每 2s 出块挖矿 → 复制地址收款 → 签发交易转账。

### 4.1 无头中继节点（`--node`，DESIGN.md §9.32）

一台 24 小时开着的机器就能当邻居的入口，**不需要服务器软件**：它验账、转发、追缺口，**一块不出、一分钱不拿**（收益仍归各台钱包自己签的门票）。

```bash
qlight-wallet --node                       # 数据目录默认 ~/.qlight/node，监听 tcp/47410
QL_DATA_DIR=/srv/qlight QL_LISTEN_PORT=47410 qlight-wallet --node
```

| 环境变量 | 作用 |
|---|---|
| `QL_DATA_DIR` | 节点数据目录（账本、身份密钥、`seeds.json`、`peer-id.txt` 都在这里面） |
| `QL_LISTEN_PORT` | 固定监听端口，默认 `47410`；被占用会自动退回系统分配端口（那时别人拨不到固定地址，日志会说明） |
| `QL_SEEDS` | 种子地址，逗号/空格分隔：`/ip4/<IP>/tcp/47410/p2p/<PeerId>` |
| `QL_ADVERTISE` | 对外可拨地址（公网机器必填，程序猜不出自己的公网 IP）；不设就只报回环/内网地址 |

种子名单还有两个来源：编译期 `net::SEEDS_BUILTIN`（**当前是空的**——还没有一台对外常驻的公网节点，写进编译期就是写假话）与 `<数据目录>/seeds.json`（字符串数组）。节点启动会把 `peer-id.txt` 和自己算出的「可拨地址」打在日志里，运营者抄走那一行，就是别人该填的种子。

Windows 上那个 GUI 程序是 `windows_subsystem = "windows"`（无控制台），要日志得重定向：`qlight-wallet.exe --node > node.log 2>&1`。

**闸门**：`cargo test --test relay_node` 用本机 loopback 起三台中继，C 的种子只有 A、B 的种子也只有 A，三台各自独立数据目录（同机引导注册表帮不上忙），最后断言 C 靠 **A 转发过来的签名地址自报**认识 B，且三台 `blocks.jsonl` 都只有创世那一行。跨真实公网（NAT / 防火墙 / 公网 IP）仍未跑过一次。

## 5. 目录

```
desktop-wallet/
├── Cargo.toml / build.rs / tauri.conf.json
├── capabilities/default.json     # Tauri 2 权限
├── icons/                        # .icns + .png（源自 logo/draft/icon-dark.svg）
├── src/
│   ├── lib.rs / main.rs          # Tauri 入口与命令注册（main 认 --node / --help）
│   ├── node.rs                   # 无头中继节点：共用开账本 + 空核心槽（永不出块）
│   ├── crypto.rs                 # ML-DSA-65 + SHAKE-256 + ql1 地址
│   ├── keystore.rs               # Argon2id + AES-256-GCM 密钥库
│   ├── tx.rs                     # QL-TX-v1 canonical CBOR + digest
│   ├── simchain.rs               # 内置账本链 + VRF 心跳
│   ├── net.rs                    # libp2p 联邦网络（8 个主题）+ 种子名单 + 地址自报
│   ├── world/                    # 实体账本 / 光尘 / 地形 / 席位
│   └── commands.rs               # Tauri 命令层
└── ui/                           # index.html / style.css / app.js（品牌深空蓝+金）
```

## 6. 路线图

- **M1（当前）**：ML-DSA-65 账户可收发 + Session 字段预留 + 挖矿心跳 ✔（本仓库）
- **M1.5**：SessionGrant 完整实现（subkey 生成 + cap 签发）；真实 RPC 客户端骨架随时可插
- **M2**：接入测试网；SLH-DSA-128s 治理密钥（Class 2）；钱包节点 15% 矿池真实份额分配
- **M3**：Class 3 承诺账户 PoC；社交恢复 Guardian Set UI
