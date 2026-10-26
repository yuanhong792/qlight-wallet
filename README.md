# QLight Wallet · 抗量子桌面钱包

<p align="center">
  <img src="docs/assets/coin.png" width="120" alt="QLX">
</p>

<h3 align="center">🛡 下一代抗量子桌面钱包 · 联邦网络 · 公平启动</h3>

<p align="center">
  <a href="https://github.com/yuanhong792/qlight-wallet/releases/latest/download/QLight-Wallet.dmg"><img alt="macOS 下载" src="https://img.shields.io/badge/macOS-下载_DMG-E3B54A?logo=apple&logoColor=white"></a>
  <a href="https://github.com/yuanhong792/qlight-wallet/releases/latest/download/QLight-Wallet-Windows.zip"><img alt="Windows 下载" src="https://img.shields.io/badge/Windows-下载_ZIP-0078D4?logo=windows95&logoColor=white"></a>
  <a href="https://yuanhong792.github.io/qlight-wallet/"><img alt="官网" src="https://img.shields.io/badge/官网-yuanhong792.github.io-A87A18"></a>
  <img alt="签名" src="https://img.shields.io/badge/签名-ML--DSA--65_·_FIPS_204-34A853">
  <img alt="许可证" src="https://img.shields.io/badge/状态-演示内测版-8A93A6">
</p>

---

> **⚠️ 诚实声明（必读）**：QLX 是**演示与教学用途的「凭证/权益」记账单位，不是投资品**，不承诺任何价值，不构成任何证券或虚拟货币发行。当前版本为**双机演示 / 小圈内测阶段**，共识规则为演示级，不具备对抗恶意节点的拜占庭容错。在您所在司法辖区使用前，请自行确认合规性。

## 📸 界面一览

| 主界面（余额 / 收款码 / 联邦状态） | 活动明细（收款方视角 · 双方地址直显） |
|:---:|:---:|
| ![主界面](docs/screenshots/wallet-asset.png) | ![活动明细](docs/screenshots/wallet-activity.png) |

**同一笔转账，在付款方钱包里的样子**（英文界面 · From/To 完整地址 + "This wallet" 标注）：

<p align="center"><img src="docs/screenshots/wallet-activity-sent.png" width="720" alt="付款方视角"></p>

## ✨ 特性

- **🛡 抗量子签名** — 全部交易使用 **ML-DSA-65**（NIST **FIPS 204**，Dilithium 家族）。今天截获的交易数据，未来量子计算机也无法伪造。
- **🌐 联邦网络** — libp2p + mDNS + gossipsub。同一局域网的钱包**自动发现彼此**，无需服务器、无需公网 IP，账本全网一致；快照压缩同步 + 分叉即时裁决。
- **🌱 公平启动** — QLX 全部通过**挖矿产出**：无预挖、无 ICO、无投资人份额。总量封顶 **2.1 亿**，每 31.5M 块减半，约 60 年排完。
- **📜 创世宣言上链** — 借鉴比特币创世块：宣言原文经 SHAKE-256 嵌入创世哈希，**改一个字全网即可识破**，链的价值观从第 0 块被锚死。
- **💱 2 秒结算** — 出块周期 2 秒，手续费固定 0.001 QLX；「确认中」状态一目了然，入块即到账。
- **🔐 本地加密存储** — 密钥库 **AES-GCM + Argon2** 加密落盘；账本与块文件**全路径原子写 + fsync**，启动时哈希链校验，崩溃自愈。
- **🔍 活动明细** — 每笔转账：付款方 / 收款方**完整地址**、精确时间、块高、哈希、真实手续费**全部直显**；8 种语言界面。
- **💾 备份恢复** — 一键导出 / 导入 `.dat` 备份（密码不变）。

## 🚀 快速开始（两台电脑联邦转账）

1. **安装**：从 [Releases](https://github.com/yuanhong792/qlight-wallet/releases/latest) 下载并安装（macOS 拖入应用程序；Windows 解压双击 exe，三文件同目录）。
2. **组网**：两台设备连同一 Wi-Fi，各自打开钱包，左上角出现「🌐 联邦 · 2 节点」即成功（同一台电脑开两个实例也可以，第二个实例自动落到 `node-1` 数据目录）。
3. **转账**：收款方「收款」页复制 `ql1` 地址 → 付款方「转账」页粘贴、输入金额 → 签发。
4. **到账**：约 2 秒入块，双方「活动」页自动刷新，完整明细直接可见。

## 🏗 技术架构

```
┌─────────────────────────────────────────────┐
│  QLight Wallet (Tauri 2 + Rust)             │
│                                             │
│  UI (8 语言)  ←→  Tauri Commands            │
│                     │                       │
│      ┌──────────────┼──────────────┐        │
│      ▼              ▼              ▼        │
│  SimChain        ML-DSA-65      libp2p      │
│  · 2s 出块       (FIPS 204)     · mDNS      │
│  · 门票选举      AES-GCM        · gossipsub │
│  · 封顶排放      + Argon2       · 快照同步  │
│  · 哈希链校验    密钥库         · 分叉裁决  │
└─────────────────────────────────────────────┘
```

- **桌面**：Tauri 2（Rust 后端 + WebView 前端，无打包链，UI 嵌入二进制）
- **密码学**：`ml-dsa`（FIPS 204）· SHAKE-256 · AES-GCM + Argon2id · bech32 地址（`ql1…`，32 字节 commitment）
- **链**：SimChain 演示链（创世宣言锚定 → 门票选举出块 → 奖励减半 → 2.1 亿硬顶；blocks.jsonl 逐块审计，启动重放校验）
- **网络**：B 档联邦（libp2p mDNS 发现 + gossipsub 广播 + 共享引导注册表；gzip 压缩快照追账；票/块双探测分叉裁决）

## 🌍 世界是算出来的（确定性世界）

本项目对"元宇宙"的立场是一句**可证伪**的话：`world = render(种子, 规则, 历史)`，
而它与区块链的状态转移 `state(n+1) = f(state(n), tx(n))` 是同一个数学对象——**链就是世界的存档，币就是世界本身**。

这句话已经被实验证明了一次：`tools/world/` 下有 Python 与 JS 两份**运行时完全不同**的参考实现，而钱包里跑的是第三份（浏览器内嵌渲染器）——三者从同一份链历史渲染出**逐位一致**的像素。

```bash
python3 tools/world/world.py                        # 默认：只用创世哈希作种子
node tools/world/world.js --blocks <你的>/blocks.jsonl   # 用你自己的链历史
```

| 输入 | 像素指纹（双 FNV-1a，16 hex） |
|---|---|
| 只有创世哈希 | `9171377071cd48c4` |
| 创世 + 718 个真实块 | `eabda6f443480716` |
| 同一历史，规则常量 `0xca3318e0` → `+1` | `41e82c04605a381e` |
| 创世 + 240 个任意 VRF（钱包渲染器 ↔ CPython） | `15a38d98548335cc` |

三份实现在四个用例上**全部逐位吻合**。所以：多挖一个块，大陆就真的多长出一块；改一个规则常量，整片世界面目全非，而所有实现同时跳。

- 想直接看：官网 [「世界是算出来的」](https://yuanhong792.github.io/qlight-wallet/#world) 一节可在浏览器里跑同一个渲染器，并拖入自己的 `blocks.jsonl`（本地解析，不上传）。
- 想直接造：钱包里的 **🌍 世界**页就是编辑器——本地实时渲染整片大陆，点一格、选一个槽位（石碑/篝火/树屋/灯塔/花园/水晶/工坊/图腾）、签一条 ML-DSA-65 记录，实体就永久进账本。**链只保存坐标与归属，像素全部在每台本机各自算出来。**
- 完整规格：[DESIGN.md §9 元宇宙运行时](DESIGN.md#9-元宇宙运行时l2)（确定性算术规范、规则版本化、VRF 随机源、可验证边界、世界经济学、彼此指认的存在证明）。
- **诚实边界**：状态可验，像素不可验——渲染器不是共识的一部分；实体账本与 `chunk_root` 已落地（`src/world.rs`，逐条验签 + 哈希链），但**还没有 `entity_root` 进块**，租约与 `StreamGrant` 仍是**只有规格，0 行代码**（见 §9.12）。

## 📚 文档

| 文档 | 内容 |
|---|---|
| [whitepaper.md](whitepaper.md) | 白皮书（架构 / 共识 / 经济模型 / 合规红线） |
| [DESIGN.md](DESIGN.md) | 技术设计底稿（§14 协议规格） |
| [TOKEN.md](TOKEN.md) | QLX 凭证经济 |
| [benchmark.md](benchmark.md) | 性能基准（实测） |

## 🗺 路线图

| 阶段 | 内容 | 状态 |
|---|---|---|
| M1 | 双机演示 / 自用（联邦转账闭环 + 崩溃安全存储 + 创世锚定） | ✅ **当前** |
| M1.5 | 小圈内测（启动交叉校验完善 + 基本防恶意） | 🚧 |
| M2.2 | 门票 VRF 证明闭环 + 作恶罚没 + 拜占庭容错 | 规划 |
| 公开上线 | 签名分发 + 自动更新 + **合规评估（硬前置）** | 规划 |

## 🪟 / 🍎 平台备注

- **Windows**：三文件（exe + WebView2Loader.dll + libunwind.dll）须同目录；SmartScreen「更多信息 → 仍要运行」；防火墙选「允许」。
- **macOS**：首次打开「无法验证开发者」→ 右键 → 打开（未公证提示，仅一次）。
- **数据位置**：macOS `~/Library/Application Support/net.qlight.wallet/`；Windows `%APPDATA%\net.qlight.wallet\`。

## 📄 许可与免责

本项目用于研究与教学演示。QLX 非投资品，不构成任何发行行为；公开上线前需完成合规评估。使用本项目产生的任何后果由使用者自行承担。
