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
  free_quota_max: 8,
  free_quota_window: 60,
  next_claim_height: 43,
  mine_reward: 1,
  ore_candidates: 0,
  ore_quota: 0,
  owned_by_me: 0,
  craft: { built: 0, weight: 0, pace: 0 },
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
window.__TAURI__ = {
  core: { invoke: (cmd) => Promise.resolve(cmd in STUB ? STUB[cmd] : null) },
  event: { listen: () => Promise.resolve(() => {}), emit: () => Promise.resolve() },
};
