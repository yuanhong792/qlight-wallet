# 抗量子虚拟世界结算链 · 设计书

> 版本：v0.1（白皮书底稿）  
> 日期：2026-10-22  
> 关联文档：[`DESIGN.md`](./DESIGN.md)（主技术设计）、[`TOKEN.md`](./TOKEN.md)（命名与代币经济）、[`benchmark.md`](./benchmark.md)（容量与性能基准）  
> 本文档的目标读者：核心开发、审计、潜在合作的项目方/企业、社区研究者

---

## 0. 摘要

本项目构建**一条以抗量子密码为默认、面向虚拟世界（元宇宙）经济、支持通用智能合约的结算层**，并配套 **L2 场景运行时**实现游戏/UGC 场景的高频支付与可迁移资产。

设计建立在三笔已经验证可行的基础之上：

1. **账户签名默认使用 ML-DSA-65**（NIST FIPS 204），配合 SLH-DSA 冷/治理密钥、ML-KEM-768 P2P 加密；
2. **共识层签名采用哈希基 XMSS + STARK 聚合**（借鉴 Tezos Quantumnet / LeanEthereum），解决“抗量子签名体积大”的核心瓶颈——该路线已有公开 benchmark（0.8 秒聚合 900 个 XMSS 签名、295KiB 证明、验证明 16.5ms）；
3. **元宇宙层不追求每笔游戏操作都上链**：通过会话密钥 + 流式支付 + 资产状态机 + 场景 Rollup，让百万级日操作只产生少量链上结算。

这份文档的职责是：把技术边界画清楚，把接口、威胁模型、迁移路径、治理机制写到底稿级别，供开发落地、审计立项、社区评审、白皮书产出四件事共用。

---

## 1. 项目定位与范围

### 1.1 我们做什么 / 不做什么

| 做的事 | 不做的事 | 理由 |
|---|---|---|
| 抗量子签名层（账户、共识、P2P） | 量子货币（Quantum Money）产品化 | 需要量子硬件，只能作叙事 |
| EVM 兼容执行层 + 应用链 | 自研共识/VM | 工程成本高且抗量子非必需 |
| 元宇宙资产标准、状态机、流式支付 | QKD / 量子通信共识 | 与无信任公链不匹配 |
| 迁移工具（从 ECDSA 账户） | 破解/改造 ML-DSA 等标准算法 | NIST 标准已足够好，改动是安全风险 |
| PQ 锁仓金库 / 治理 / 算法敏捷机制 | 无限期保留 ECDSA | 违背抗量子目标 |

### 1.2 与其他“抗量子链”的差异定位

| 案例 | 主要思路 | 参考价值 | 我们的差异 |
|---|---|---|---|
| QANplatform | EVM 兼容，PQ 跨签（XLINK），多语言合约 | 迁移思路 | 我们把 PQ 应用到**共识/DA/隐私**全栈，并聚焦元宇宙场景 |
| PQCN | XMSS + 量子随机数 | 冷/治理层保守哈希方案 | 不把 XMSS 用作热账户（状态管理会杀体验）；QRNG 只作熵源增强，不作安全假设 |
| Tezos / Quantumnet | 全栈 PQ：账户 ML-DSA-44、共识 XMSS+STARK、DA 用 ZODA、去 VDF | **最值得借鉴的工程范式** | 我们以“元宇宙高频支付体验”为核心产品价值，不只是协议改写 |
| NEAR NEP-655 | 承诺账户（资产不绑公钥，升级无需迁移） | 老账户升级路径 | 我们把它归入账户分类的 Class-2，作为中期目标 |

---

## 2. 性能与容量预算（引用 benchmark.md）

关键数字：**ML-DSA-65 签名 3,309 B**，简单转账约 3,437 B；1MB/2s 区块上限约 152.5 TPS，8MB/2s 约 1,220 TPS。验签极快（0.103ms，9,700/s/核），“签名体积”是真实成本。

由此确定的容量目标：

| 层 | 目标 TPS | 虽采用的手段 |
|---|---:|---|
| L1 结算层 | 500–1,100 | 4–8MB 区块、2 秒出块、批量转账、聚合签名（可选） |
| L2 场景运行时 | 5,000–20,000+ | Rollup、批量提交、流式结算、支付通道 |
| 游戏/元宇宙内 | 亚秒级感知 | 会话密钥 + L2 预确认 |

**产品原则**：任何一笔“玩家觉得是操作”的行为，不应直接消耗 L1 TPS；它要么是 L2 状态转移，要么是“攒一堆一起来结算”的批量操作。

---

## 3. 总体架构

```
┌────────────────────────────────────────────────────────────────┐
│ L2 场景运行时（Scene Rollup / Paymaster / 会话引擎 / 流式支付）       │
│   ─ 每场景一个状态机，由场景方运营，PQ 签名仅用于进入/退出/结算           │
│   ─ 用户通过 session_key 执行游戏内操作，L1 只看周期性资产根             │
├────────────────────────────────────────────────────────────────┤
│ L1 结算层（QLChain）                                 │
│   ├ 共识：SWRR（无 VDF）+ ML-DSA 出块 + XMSS/STARK聚合 attestation     │
│   ├ 执行：EVM 兼容 VM（sender 由协议注入，合约不验签）                 │
│   ├ DA：Phase1 走 Ethereum blob（不可全PQ）；Phase2 ZODA(哈希基)       │
│   └ 经济：字节计费 + 状态租金 + 会话积分 + 治理签名                   │
├────────────────────────────────────────────────────────────────┤
│ L0 密码学敏捷层（Crypto-Agility）                                 │
│   - 算法注册表 + scheme_id；每个签名块带 scheme_id 与版本             │
│   - 破老/破新算法都能靠治理迁移，而不是全链重生成                     │
├────────────────────────────────────────────────────────────────┤
│ 基础设施：节点（libp2p + 混合 TLS + ML-KEM-768）                    │
│            钱包 SDK / PQ-HSM / 浏览器扩展 / 企业 PQ 托管 API          │
└────────────────────────────────────────────────────────────────┘
```

**两个关键选型（ADR 式记录）**：

- **ADR-1 默认签名 ML-DSA-65**，不选 ML-DSA-87（体积大）或 Falcon/FN-DSA（实现复杂、有未标准化的风险）。治理、大额金库用 SLH-DSA/XMSS。
- **ADR-2 Phase 1 采用 Rollup（结算到 Ethereum）而不是自建 L1**，理由：绕开共识/DA/P2P 三层都要自己上线 PQ 的工程风险；但**共识层会在 Phase 1 就跑 SWRR + PQ attestation**，主网 Phase 2 切到独立 PQ L1。

---

## 4. 威胁模型

### 4.1 敌人定义

| 敌人 | 定义 | 能力边界 |
|---|---|---|
| A1 经典攻击者 | 当前互联网/区块链常见攻击者 | 无量子算力，可破解弱实现、偷私钥、钓鱼 |
| A2 Q-day 攻击者 | 拥有足够破 ECDSA 的量子算力 | 可破 ECDSA/Ed25519/RSA，**破不了 hash、格签名** |
| A3 采集者（Harvest-Now） | 记录当前网络数据，等QC破隐私层 | 针对签名/加密元数据/流量分析 |
| A4 实现有故障的链方 | 实现不安全 | 侧信道、非强随机数、状态漂移 |
| A5 解构协议攻击者 | 针对治理/共识/桥的攻击者 | 恶意下载大量大签名（DoS）、恶意熔断 |
| A6 监管/合规变更 | 长期监管压力 | 不通过抗量子算力，但影响产品可行性 |

### 4.2 威胁清单与缓解

| 编号 | 威胁 | 影响 | 缓解 |
|---|---|---|---|
| T1 | ECDSA 私钥被 Shor 破解 | 老账户资产被盗 | 默认 ML-DSA 账户；迁移工具（§8.2）；冷金库强制 SLH-DSA |
| T2 | 混合签名下被“降级”为仅 ECDSA | 表面抗量子、实际破 | 双签必须绑定同一消息；`scheme_id`强制匹配；测试用例覆盖降级场景 |
| T3 | PQ 签名过大导致 DoS | 网络拥堵、节点崩 | 按字节计费 + 每区块 PQ 交易比例上限 + 验签缓存 + 大签名进 DA 而非 mempool |
| T4 | XMSS 状态漂移/复用 | 密钥被破解 | XMSS 仅用于冷/共识、每 epoch 固定页；钱包层做多层树高度/页计数与备份；**绝不用作热账户** |
| T5 | KZG/配对/椭圆曲线组件被量子破 | DA/随机性失效 | DA 及出块权用哈希基（ZODA / SWRR）；弃用 VDF/协议随机数 |
| T6 | 采集型隐私攻击（元数据/负载） | 交易隐私被破 | 支付通道、L2、加密元数据；大规模ML-KEM-768 P2P |
| T7 | 治理签名链被破解/被买通 | 算法升级被滥用 | 治理签名用 SLH-DSA；提案带 7 天公示期 + 执行窗口，可被社区否决 |
| T8 | Rollup 依赖 base chain 非全 PQ | 数据可用被攻 | Phase 1 明确为风险；限资金规模；Phase 2 切自带 PQ DA |
| T9 | 实现级漏洞 | 验签绕过、重放、重排序 | 只用标准库参考实现；代码审计 + bug bounty；模糊测试渗透 |
| T10 | 密钥被盗/丢失 | 资产永久损失 | 社交恢复、硬件绑定、真人客服流程、密钥丢失保险 |

### 4.3 Q-day 演练

**运营原则**：支持“经常假设已被破解”的运营心态。每季度在测试网跑一次“Q-day 演练”：假设 ML-DSA-65 被破，走一遍紧急迁移剧本，出公开报告。这是 Tezos Quantumnet 未做、值得先做的事。

---

## 5. 密码学基线

### 5.1 算法清单

| 用途 | 算法 | 备选 | 何时弃用 |
|---|---|---|---|
| 账户签名 | **ML-DSA-65** | ML-DSA-44（gas 优先） | —— |
| 冷/治理/大额签名 | **SLH-DSA-128s** | XMSS-M（h=32） | —— |
| 共识验证者密钥 | **ML-DSA-87** | —— | —— |
| 共识 attestation | **XMSS-L1（hash-based）** | SPHINCS+ | —— |
| P2P / TLS / ML-KEM 密钥封装 | **ML-KEM-1024**（混合 X25519） | ML-KEM-768 | —— |
| 隐私池 | **ML-KEM-768 + hash OTS + STARK** | —— | —— |
| 链上哈希（地址/tx digest） | **SHAKE-256**（FIPS 202） | keccak256 in EVM | 兼容 EVM 时同步 keccak |
| zk 内部状态树哈希 | Poseidon / GMiMC | BLAKE2s | —— |

### 5.2 密码学敏捷（Crypto-Agility）的落地规则

1. 每条账户 / 每笔交易均带 `scheme_id` + `sig_version`；加入“算法注册表”合约，用治理投票决定每个算法的启用/弃用时间点。
2. 任何算法被标记 **deprecated**，禁止新账户开通、强制存量在窗口内迁移；**discontinued** 后验签直接 fail。
3. 建立双周期：**研究周期**（半年预读一次 NIST/标准化动向）+ **迁移周期**（一次性、公开、带回滚）。
4. 采用 NIST IR 8547 的语汇（ECDSA 2030 deprecated / 2035 disallowed）作为内部节奏参考。

---

## 6. 账户与身份模型

### 6.1 地址格式

```
ql1<bech32 32字节 commitment>
commitment[0..3]  : version + scheme_id
commitment[4..31] : SHAKE-256(pk || salt) 截断
```

- 链上**不在每笔交易携带公钥**，账户地址即为 `H(pk)`；公钥仅首次出现（用于绑定）时登记。
- 老链（Ethereum/BSC 等）账户的地址保留 `0x…` 原样，但仅作为迁移源头；本链新地址统一使用 `ql1` 格式，不再生成 `0x` 新地址。

### 6.2 账户三层分类

| 分类 | 场景 | 授权方式 | 存不存公钥 |
|---|---|---|---|
| **Class 1 常规账户** | 每日转账、游戏内支付 | ML-DSA-65 签名 | 首次可见，之后隐藏 |
| **Class 2 优先级账户** | 国库、治理、大额托管 | SLH-DSA/XMSS + 多签 | 首次可见 |
| **Class 3 承诺账户（R&D）** | 隐私场景、高级用户 | 哈希树 OTS 在 STARK 内验（TzEL 式） | **永不公开公钥** |

Class 3 是长期方向，**MVP 不上线**；但它决定了本链能走多远（是否变成绝对零椭圆曲线）。

### 6.3 会话密钥（Session Keys）

- 账户签发带有效期的授权，生成临时密钥对（同样 PQ）；
- 玩游戏内操作全部由 session key 签，链上**只验“授权与上限”**一次；
- 到期/额度耗尽必须用主密钥续签；
- **主密钥永远不出现在游戏运行时的热内存里**。

### 6.4 身份与恢复

- 钱包地址的一生通过 **DID Doc Hash** 绑定，外层 DID 负责跨世界身份。
- 社交恢复：3-5 位守护人（Guardian Set），守护人密钥**签名升级**才可替换，避免密钥被偷就换不掉。
- 恢复动作链上公开审计，**不可静默改绑**（“显式 + 公告 + 宽限期”）。

---

## 7. 执行与合约抽象

- **EVM 兼容但重组**：禁止合约依赖 `ecrecover` 做权限；调用方通过**协议注入的 authenticated sender**进入合约上下文，使合约对签名算法无感。
- **资产标准 QRC-20 / 721 / 1155**（本链命名）：兼容 ERC-20/721/1155 接口，允许原合约地址桥接到本链，为以太坊生态兼容留位。
- **跨链资产必须走协议官方桥**，不开放自定义 bridge（这是行业被黑最多的环节）。
- **oracle 原则**：链下预言机必须 PQ 签名（禁止 ECDSA）约定签名格式；预言机合约只接受 scheme_id ∈ 允许集。

---

## 8. 迁移与升级

### 8.1 三种迁移场景

| 场景 | 来源 | 工作量 |
|---|---|---|
| 全新用户 | — | 默认 Class 1 账户即可 |
| 从 ECDSA 链迁移 | Ethereum / BSC / 联盟链 | 跨签迁移 + MigrationRegistry 合约对接 |
| 本链算法升级 | 更抗量子的下一代算法 | 算法注册表 + 全网投票 |

### 8.2 ECDSA → PQ 跨签流程（受 QAN XLINK 启发）

**目标**：老账户不动资产就能切换到 PQ 控制权。流程：

1. 老账户用 **ECDSA 签署**：`MigrationIntent(target_ql_addr, target_pk_commitment, deadline, chain_id)`.
2. 目标 PQ 账户用 **ML-DSA-65 签署同一消息**，形成双签。
3. 中继提交双签合约，合约验证双方签名署名一致（**防降级：两签名必须对同一 digest 生成**）。
4. 准备期结束，老 ECDSA 授权作废，PQ 授权生效；**资产本身不需要移动**。
5. 全流程状态在链上公开，用户可见迁移进度。

**量化承诺（写进白皮书）**：所有超过一定金额、且未迁移的 ECDSA 地址，在迁移窗口内 100% 被 SDK 主动提醒并协助迁移；未迁移地址在 Q-day 后沦为公开靶子，被占用损失属于可审计的残余风险。迁移进度 `MigrationRegistry` 公开 manifest 是本设计对社区的硬承诺。

### 8.3 算法升级流程

每个算法注册到 `SchemeRegistry` 合约：
```
struct Scheme {
  uint8  scheme_id;
  uint32 activationEpoch;
  uint32 deprecationEpoch;
  uint32 disallowEpoch;
  bytes32 regulatorHint;
  bool   audited;
}
```
升级由治理（§13）执行，具备 7 日公示、冷却期、可回滚机制。

---

## 9. 元宇宙运行时（L2）

### 9.1 场景 Rollup

- **场景 = 一个状态机**，含场景根、资产根、会话记账、物证链等。
- **根提交节奏**：每 K 秒或每 N 场景操作，场景方把 `scene_state_root` 和 `asset_root` 提交到 L1。
- **证据与争议**：采用轻量 fraud-proof（不追求 zero-knowledge trustless rollup 的复杂度）；争议期 N 小时，超额金额进托管。

### 9.2 资产状态机

```
状态转移 : (sceneState, assetOp) -> (sceneState', Event[])
效果     : owner / royalty / amount 等字段，全部可复现
```

每一步都可由任何节点复演算；放进“公开审计账本”，让玩家可以**复现整条游戏经济**。

### 9.3 流式支付

- 支付按秒渐进累积，根据`rate × time`结论；**合同只记 `StreamGrant{rate, start, end, deposit}`**。
- 每 1 分钟由场景方或用户提交结算 batch（L1 批量确认）；争议周期为 fraud-proof 时间窗。

### 9.4 元宇宙内 UX

- **打开游戏 = 一次 ML-DSA 授权**：登录授权 session key，游戏内 0 链上操作。
- **购买 = 一次流式结算**：价格写在场景合约，拖拽即买，用户不感知链上 gas。
- **松散手**：给玩家 5 个不同世界身份，全部放到 DID passport，不必带助记词到处跑。

---

## 10. 数据可用性

| Phase | DA 方案 | 备注 |
|---|---|---|
| **1（Rollup 运营）** | Ethereum blob | 快、可靠；明确记录“底层非PQ”风险 |
| **2（PQ DA）** | **ZODA(哈希编码)** 借鉴 Tezos | 用二维张量码 + 哈希承诺替代 KZG，不依赖椭圆曲线 |

Phase 2 前禁止把"非 PQ 依赖"包装为"PQ"；对外沟通必须明确注明残余风险。

---

## 11. 共识与验证

- **出块权**：SWRR（Smooth Weighted Round Robin）根据质押权重分配，**不使用 VDF/协议随机数**（借鉴 Tezos Quantumnet 的选择）。
- **attestation**：每个验证者在 epoch 开头重置 XMSS 状态树；每块 attest 一次，**由 STARK 聚合器批量聚合**为 1 份证明（参考量级：单台消费级 CPU 约 1,000 签名/秒，见 LeanEthereum benchmark）。**Phase 1 不启用聚合**，用 ML-DSA 串行签容错。
- **最终性**：Peer 共识（Tenderbake/PBFT 风格）2 块最终，约 4–6 秒。
- **环境**：节点 P2P 走 **hybrid TLS（X25519 + ML-KEM-1024）**；握手强制 PQ。
- **出块权验证**：验证者注册时登记 ML-DSA-87 公钥，禁止使用 ECDSA/Ed25519。

---

## 12. 执行与费用模型

```text
fee = base_fee + bytes × byte_rate + exec_gas × gas_price + state_rent
```

- `bytes` 计费用 **PQ 签名 + 压缩后字节数** 计。
- 状态写入费按**每年生存成本 / TPS 实测**推导（可调治理参数），避免链上无限膨胀。
- 会话下支付（paymaster）给场景方提供“代玩家付款”合约接口，提高转化率。

---

## 13. 治理

- **最低门槛**：所有合约升级、算法切换、参数变更，都要走链上提案，投票由**PQ 账户**执行。
- **权重结构**：投票权重与质押或真实贡献挂钩，支持二次方（quadratic）投票，避免大户一次挟持。
- **熔断机制**：可触发“算法紧急下线 / 迁移执行”为白名单情景，**不用于增发或转账**。
- **可审计性的白皮书承诺**：所有提案资金流向、合约行为在浏览器可见；禁止“只放白皮书、不公示执行情况”的做法。

---

## 14. 接口规格

### 14.1 JSON-RPC / REST 端点

```text
ql_getBalance(addr) → {balance, nonce, scheme}
ql_sendRawTx(cbor_hex) → {tx_hash}
ql_getAssetRoot(sceneId) → {asset_root, state_root, height}
ql_createSession(parentNonce, cap, expiry) → {session_id, grant_tx_hash}
ql_openStream(config) → {stream_id, contract_addr}
ql_estimateFeeBytes(tx) → {base, bytes_rate, exec_gas}
ql_getMigrationStatus(addr) → {snapshot, qt_migration%, legacy_regression}
```

### 14.2 TX 交易信封（Canonical CBOR）

```c
struct TxEnvelope {
  uint8    version;          // 1
  uint64   chain_id;
  uint8    scheme_id;        // 1=ml_dsa_65, 2=slh_dsa_128s, 3=ml_dsa_87, ...
  bytes32  from_addr;
  bytes32  pk_hash;          // 仅首笔迁移或注册时出现
  uint64   nonce;
  uint64   expiry;           // 区块高度或时间戳
  uint64   feeCap;
  Action[] actions;          // Transfer / Call / Stream / Bridge
  bytes    session_nonce;    // session key 时必需
}
digest = SHAKE-256("QL-TX-v1" || canonical_cbor(TxEnvelope))
```

### 14.3 跨签迁移消息（ECDSA → PQ）

```c
struct MigrationIntent {
  string  magic = "QL1Migration";
  uint64  chain_id;
  bytes32 legacy_evm_addr;      // 0x...
  bytes32 target_ql_addr;
  bytes32 target_pk_commitment; // SHA-256 of ML-DSA-65 public key
  uint64  deadline;
}
// 步骤：ECDSA(target) + ML-DSA(target) 对同一 canonical bytes 双签
```

### 14.4 智能合约接口（伪代码）

```solidity
interface IPaymaster {
  function sponsor(address user, bytes32 txDigest) external returns (uint128 quotaLeft);
  function setPolicy(uint128 perDayCap, bytes32 scopeTag) external;
}

interface IStream {
  function open( address recipient, uint128 ratePerSec, uint64 start, uint64 end ) external payable returns (uint64 streamId);
  function anchor( uint64 streamId, bytes32 newStateRoot ) external;   // L1 anchor
  function settle( uint64 streamId, bytes32 fraudProof ) external returns (uint128 payout);
}

interface IQGovern {
  function propose( bytes32 agenda, uint32 epochOfVoting ) external;
  function vote( uint32 epoch, bytes32 agenda, uint8 choice ) external;   // PQ-signed
  function execute( bytes32 agenda ) external;
}

interface ISchemeRegistry {
  function register( SchemeId id, uint32 actEpoch, uint32 depEpoch ) external;
  function status( SchemeId id ) external view returns (scheme_status);
  function emergencyDown(SchemeId id) external;   // 仅当治理白名单
}
```

---

## 15. 路线图与里程碑

| 阶段 | 时窗 | 驱动产出（gates） |
|---|---|---|
| **M0 基线与复现** | 2026 Q4–2027 Q1 | benchmark.md 落到目标硬件；威胁模型 v1；算法注册表草案 |
| **M1 PQ 钱包 PoC** | 2026 Q4–2027 Q1 | ML-DSA-65 账户可收发；SessionGrant 与 Class-1 签名脚手架 |
| **M2 PQ AppChain 测试网** | 2027 Q1–Q2 | Rollup 到 Ethereum；场景 Rollup、流支付、跨签迁移上线；**公开 Q-day 演练 #1** |
| **M3 PQ 共识 + PQ DA** | 2027 Q2–2027 Q4 | XMSS/STARK attestation 进入测试网，ZODA DA PoC；Class-3 承诺账户 PoC；审计 #1 |
| **M4 主网试点** | 2028 | 主网 beta + 5 场景正式接入；治理与算法高级机制上线 |

**上线条件 gate**：审计通过、Q-day 演练报告可被公开审查、安全运营无重大事故、迁移演示端到端跑通。

---

## 16. 参考与背景

- NIST FIPS 203 / 204 / 205；SP 800-208（XMSS/LMS）；NIST IR 8547（ECDSA 弃用时间表）
- Tezos Spotlight（2026.9）：Ushuaia 升级、Quantumnet 实验网、TzEL 隐私 rollup、LeanEthereum STARK 聚合 benchmark
- NEAR NEP-655：承诺账户（不暴露公钥的资产承载方式）
- QANplatform XLINK（跨签迁移思路借鉴）
- LeanEthereum 项目（pqSTARK / leanVM 相关）
- 本库：[`benchmark.md`](./benchmark.md)，ML-DSA-65 实测数据

---

## 17. 下一步

- [ ] 将 M0 基线复现到目标硬件（x86 + ARM 双平台）
- [ ] L0/SchemeRegistry 与治理提案流程进入详细设计评审
- [ ] 尽早发布 `session-key SDK`（社区/合作方接入的最短路径）
- [ ] 启动社区评审：UX 反馈通道、白皮书评议组、多语言文档
