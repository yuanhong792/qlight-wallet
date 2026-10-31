/* 对拍用的假 __TAURI__：让外壳在没有桌面后端时也能起来。开发页专用，别提交。 */
const HEX = "0f3a9c51be7d2846ad15c0e97b3d5421f8c2a6105de94b73c1af0268e5d7309b";
const SNAP = {
  unlocked: true,
  has_keystore: true,
  address: "QL1demo0000000000000000000000000000000000",
  height: 42,
  genesis_hash_hex: HEX,
  vrfs: [HEX],
  entities: [],
  dug: [],
  chunks: [],
  max_layer: 64,
  slot_count: 16,
  materials: [0, 0, 0],
  build_costs: [],
  my_writes_in_window: 0,
  // 累计矿格数 = QL-ROAM-v1 唯一的自变量（第 k 处要 k² 格）。给 25 是为了让对拍页
  // 看得见"解锁 5 处 + 后面 59 处锁着"两种状态；0 的话那一栏只有一句话，什么也证明不了。
  mine_claims_by_me: 25,
  free_quota_max: 8,
  free_quota_window: 60,
  next_claim_height: 43,
  mine_reward: 1,
  // 这几条是 v3 之后快照里新增的口径，桩落后了就必须在屏上露馅
  // （「现在每 undefined 块结一次」就是这么来的），所以一次补齐。
  ore_rule: "QL-ORE-v1",
  mine_cooldown_blocks: 50,
  last_claim_height: 32,
  mine_floor: 10,
  mine_next_floor: 10,
  dig_floor: 1,
  entity_bad_lines: 0,
  dust_bad_lines: 0,
  ledger_foreign: false,
  ore_candidates: 0,
  ore_quota: 0,
  owned_by_me: 0,
  craft: { cells: 25, graded: 25, kept_blocks: 0, score: 0, floor: 10 },
  balance: "0",
  report: { ok: true },
  // 规则种子与席位：由 tools/world/seat.py（口径真身）拿这条演示地址 + 上面的创世哈希
  // 算出来的，不是手填的漂亮数字。对拍页连席位都编的话，它只能证明页面会画，
  // 不能证明画得对。
  rules_id: "50d296e0",
  seat: {
    rule: "QL-SEAT-v1", cell: 292673, i: 1857, j: 142,
    lat: 64.951171875, lon: 146.513671875, tries: 4,
    parts: 2097152, land_parts: 615081, parcel_m: 8960,
    walked_source: "derived",
  },
};
const STUB = {
  app_version: "0.4.2-对拍",
  wallet_status: { unlocked: true, has_keystore: true, address: SNAP.address },
  chain_state: {
    address: SNAP.address, balance: "0", height: 42,
    total_minted: "0", hard_cap: "8000000000", claim_reward: "1",
  },
  tx_history: [],
  tx_pending: [],
  world_resources: [],
  world_presence: { peers: [], count: 0 },
  receive_qr: { png: "", uri: "" },
  world_snapshot: SNAP,
  world_dust_state: {
    epoch: 0, epoch_blocks: 120, height: 42, night: false, night_multiplier_permille: 500,
    minted_total: 0, balance: 0, quota: 0, harvested_this_epoch: 0, epoch_end: 120,
    address: SNAP.address, recent: [], rounds: [],
  },
  world_trader_state: null,
  pool_state: null,
  get_prefs: null,
  diag_report: null,
  net_status: null,
};

/* world_mine_once 以前不在桩里：invoke 返回 null，mineTick 读 p.x 当场抛
 * "Cannot read properties of null"，红字挂在世界页上——看着像新功能坏了，
 * 其实桩少了一条命令。这里补一个假的 MineResult，并让快照跟着长一格：
 * 对拍页要能演出"又落了一格 → 累计矿格数变了 → 可去之处多一处"这条曲线。
 * 档位取 MINE_FLOORS 的真实阶梯（[50,40,30,25,20,16,12,10]，落满 8 格以后是 10）。 */
let mineSeed = 20261031;
const mineCell = (n) => {
  mineSeed = (Math.imul(mineSeed, 1103515245) + 12345) >>> 0;
  return mineSeed % n;
};
function mineOnce() {
  const floor = 10;
  SNAP.height += 1;
  SNAP.mine_claims_by_me = (SNAP.mine_claims_by_me || 0) + 1;
  SNAP.last_claim_height = SNAP.height;
  SNAP.next_claim_height = SNAP.height + 1 + floor;
  SNAP.mine_floor = floor;
  SNAP.mine_next_floor = floor;
  STUB.chain_state.height = SNAP.height;
  return {
    record: null, claim_tx_hex: '00'.repeat(8),
    x: mineCell(256), y: mineCell(256), layer: 28 + mineCell(4),
    claim_height: SNAP.next_claim_height, reward: SNAP.mine_reward,
    pace_floor: floor, pace_next_floor: floor,
  };
}

window.__TAURI__ = {
  core: {
    invoke: (cmd) => Promise.resolve(cmd === "world_mine_once" ? mineOnce() : (cmd in STUB ? STUB[cmd] : null)),
  },
  event: { listen: () => Promise.resolve(() => {}), emit: () => Promise.resolve() },
};
