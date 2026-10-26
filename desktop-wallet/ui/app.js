/* QLight Wallet · 量子闪电桌面钱包前端（仿主流钱包布局版）
 * Tauri withGlobalTauri：window.__TAURI__.core / .event，无打包链 */
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const fmtQlx = (units) => {
  const s = BigInt(units || 0).toString().padStart(10, "0");
  const int = s.slice(0, -9), frac = s.slice(-9);
  return { int: BigInt(int).toLocaleString("en-US"), frac };
};
const fmtAmt = (units) => {
  const q = fmtQlx(units);
  return q.frac === "000000000" ? `${q.int}` : `${q.int}.${q.frac.replace(/0+$/, "")}`;
};
const short = (addr) => `${addr.slice(0, 10)}…${addr.slice(-8)}`;
const ts = (unix) => new Date(unix * 1000).toLocaleString(LANG === "en" ? "en-US" : "zh-CN", { hour12: false });

// ---------------------------------------------------------------- i18n 接线

applyI18n();
$("lang-select").addEventListener("change", (e) => { setLang(e.target.value); setNetChip(); });

// ---------------------------------------------------------------- 联邦网络 chip

let netCount = 0;
function setNetChip() {
  const el = $("net-chip");
  el.textContent = netCount > 0 ? t("net.fed", { n: netCount }) : t("net.solo");
  el.classList.toggle("fed", netCount > 0);
}
listen("net_peers", (ev) => {
  netCount = ev.payload.count || 0;
  setNetChip();
});
invoke("net_status").then((s) => { netCount = s.count || 0; setNetChip(); }).catch(() => {});

// ---------------------------------------------------------------- 页面路由（侧边导航）

const pages = ["asset", "mine", "history", "world", "about"];
document.querySelectorAll(".sb-item").forEach((btn) => {
  btn.addEventListener("click", () => {
    document.querySelectorAll(".sb-item").forEach((b) => b.classList.toggle("active", b === btn));
    pages.forEach((p) => $(`page-${p}`).classList.toggle("hidden", p !== btn.dataset.page));
    if (btn.dataset.page !== "world") closeWorld3D(); // 离开世界页就把第一人称视图收掉，不停止渲染还白烧 GPU
    // 切页即刷新：活动列表/余额立刻反映最新账本
    if (!$("view-main").classList.contains("hidden")) refreshAll().catch(() => {});
  });
});

// 活动页筛选：全部 / 转入 / 转出（点击即刷新，随轮询保持）
let histFilter = "all";
document.querySelectorAll(".hf-btn").forEach((b) => {
  b.addEventListener("click", () => {
    histFilter = b.dataset.f || "all";
    document.querySelectorAll(".hf-btn").forEach((x) => x.classList.toggle("active", x === b));
    if (!$("view-main").classList.contains("hidden")) refreshAll().catch(() => {});
  });
});
function openPage(name) {
  document.querySelector(`.sb-item[data-page="${name}"]`).click();
  $(name === "asset" ? "receive-card" : "send-card").scrollIntoView({ behavior: "smooth" });
}
$("btn-receive").addEventListener("click", () => {
  $("send-card").hidden = true;
  $("receive-card").hidden = false;
  loadReceiveQr();
});
$("btn-send-open").addEventListener("click", () => {
  $("receive-card").hidden = true;
  $("send-card").hidden = false;
  $("input-to").focus();
});
$("btn-mine-open").addEventListener("click", () => { openPage("mine"); });

// ---------------------------------------------------------------- 视图路由

function show(view) {
  for (const v of ["view-create", "view-lock", "view-main"]) {
    $(v).classList.toggle("hidden", v !== view);
  }
}

async function route() {
  const st = await invoke("wallet_status");
  if (st.unlocked) { await enterMain(); }
  else if (st.has_keystore) { show("view-lock"); }
  else { show("view-create"); }
}

async function enterMain() {
  show("view-main");
  await refreshAll();
  $("input-unlock").value = "";
}

// ---------------------------------------------------------------- 认证

$("btn-create").addEventListener("click", async () => {
  $("err-create").textContent = "";
  const p1 = $("input-pass").value, p2 = $("input-pass2").value;
  if (p1.length < 8) { $("err-create").textContent = t("create.err.short"); return; }
  if (p1 !== p2) { $("err-create").textContent = t("create.err.mismatch"); return; }
  $("btn-create").disabled = true;
  try {
    await invoke("create_wallet", { password: p1 });
    await enterMain();
  } catch (e) { $("err-create").textContent = String(e); }
  $("btn-create").disabled = false;
});

$("btn-unlock").addEventListener("click", async () => {
  $("err-unlock").textContent = "";
  $("btn-unlock").disabled = true;
  try {
    await invoke("unlock_wallet", { password: $("input-unlock").value });
    await enterMain();
  } catch (e) { $("err-unlock").textContent = String(e); }
  $("btn-unlock").disabled = false;
});
$("input-unlock").addEventListener("keydown", (e) => e.key === "Enter" && $("btn-unlock").click());

$("btn-lock").addEventListener("click", async () => {
  await invoke("lock_wallet");
  await route();
});

$("btn-copy").addEventListener("click", async () => {
  await navigator.clipboard.writeText($("addr").textContent);
  $("btn-copy").textContent = t("recv.copied");
  setTimeout(() => ($("btn-copy").textContent = t("recv.copy")), 1400);
});
$("addr-mini").addEventListener("click", () => navigator.clipboard.writeText($("addr").textContent));

// 活动页：点击地址/哈希复制完整值（事件委托——2 秒轮询重渲染列表也不失效）
$("tx-list").addEventListener("click", async (e) => {
  const el = e.target.closest(".cpy");
  if (!el) return;
  const v = el.dataset.copy || "";
  if (!v) return;
  try { await navigator.clipboard.writeText(v); } catch { return; }
  const old = el.textContent;
  el.textContent = t("recv.copied");
  setTimeout(() => (el.textContent = old), 1200);
});

// ---------------------------------------------------------------- 数据刷新

let stateCache = { balance: 0, height: 0, minted: 0, hard_cap: 1, reward_per_block: 0, decimals: 9 };

async function refreshAll() {
  const cs = await invoke("chain_state");
  stateCache = cs;

  const b = fmtQlx(cs.balance);
  $("bal-int-big").textContent = b.int;
  $("bal-frac-big").textContent = `.${b.frac}`;
  $("asset-bal").textContent = `${fmtAmt(cs.balance)}.${b.frac.slice(0, 2)} QLX`;
  $("stat-height").textContent = cs.height;
  $("addr-mini").textContent = short(cs.address || "ql1…");
  if (cs.address) { $("addr").textContent = cs.address; }
  loadReceiveQr();

  // 挖矿页
  const capPct = (cs.total_minted / cs.hard_cap) * 100;
  $("mine-earned").textContent = `${fmtAmt(cs.balance)} QLX`;
  $("stat-reward").textContent = `${(cs.reward_per_block / 1e9).toFixed(2)} QLX`;
  $("stat-minted").textContent = `${fmtAmt(cs.total_minted)} QLX`;
  $("stat-cap").textContent = `${capPct.toFixed(6)}%`;
  $("cap-bar").style.width = `${Math.max(capPct, 0.0000002)}%`;

  // 活动页（完整明细 + 确认中 + 筛选：付款/收款双方完整地址直接可见、
  // 本钱包金色标注、精确时间、块高、真实手续费、哈希可点复制）
  const hist = await invoke("tx_history");
  const pend = await invoke("tx_pending").catch(() => []);
  const my = cs.address || "";
  const list = $("tx-list");
  const party = (a) =>
    `<span class="addr cpy" data-copy="${a}" title="${t("hist.copy")}">${a}</span>` +
    (a && a === my ? ` <span class="me">${t("hist.me")}</span>` : "");
  // 确认中条目（已签发未入块，约 2 秒后转为正式记录；只有转出方向）
  const pendRows = (histFilter === "all" || histFilter === "out" ? pend : [])
    .map(
      (p) => `<div class="tx-item pend">
        <div class="tx-ico pend">⏳</div>
        <div class="tx-main">
          <div class="t1">${t("hist.pending")}</div>
          <div class="t-party"><span class="lbl">${t("hist.to")}</span>${party(p.to)}</div>
        </div>
        <div class="tx-amt">
          <div class="a1 minus">−${fmtAmt(p.amount)} QLX</div>
          <div class="a2">${t("hist.wait")}</div>
        </div>
      </div>`
    )
    .join("");
  const rows = hist
    .filter((x) => histFilter === "all" || x.direction === histFilter)
    .map((x) => {
      const out = x.direction === "out";
      return `<div class="tx-item">
        <div class="tx-ico ${out ? "out" : "in"}">${out ? "↑" : "↓"}</div>
        <div class="tx-main">
          <div class="t1">${t(out ? "hist.out" : "hist.in")}</div>
          <div class="t-party"><span class="lbl">${t("hist.from")}</span>${party(x.from)}</div>
          <div class="t-party"><span class="lbl">${t("hist.to")}</span>${party(x.to)}</div>
          <div class="t2">⏱ ${ts(x.ts_unix)} · #${x.height} · ${t("hist.fee", { f: fmtAmt(x.fee) })}</div>
          <div class="t3"><span class="cpy" data-copy="${x.hash}" title="${t("hist.copy")}">${x.hash_short}</span></div>
        </div>
        <div class="tx-amt">
          <div class="a1 ${out ? "minus" : "plus"}">${out ? "−" : "+"}${fmtAmt(x.amount)} QLX</div>
        </div>
      </div>`;
    })
    .join("");
  if (!pendRows && !rows) {
    list.innerHTML = `<div class="muted small tx-empty">${t("hist.empty")}</div>`;
  } else {
    list.innerHTML = pendRows + rows;
  }

  // 世界页只在可见时重载：地形按种子签名缓存，链每 2 秒长一格才重算一次
  if (worldActive()) await loadWorld().catch(() => {});
}

// 主界面轮询（2 秒）：余额/块高/活动列表随链自动更新——
// 收款到账后无需任何手动操作，活动页自己会出新记录（锁屏/解锁视图不轮询）
setInterval(() => {
  if (!$("view-main").classList.contains("hidden")) refreshAll().catch(() => {});
}, 2000);

// ---------------------------------------------------------------- 转账

$("btn-send").addEventListener("click", async () => {
  $("err-send").textContent = "";
  $("tx-ok").hidden = true;
  const to = $("input-to").value.trim();
  const amount = $("input-amount").value.trim();
  if (!to.startsWith("ql1")) { $("err-send").textContent = t("send.err.addr"); return; }
  $("btn-send").disabled = true;
  try {
    const r = await invoke("send_qlx", { toAddr: to, amountQlx: amount });
    $("err-send").textContent = "";
    $("input-amount").value = "";
    const ok = $("tx-ok");
    ok.hidden = false;
    // A 档口径：提交 → 真验签入交易池 → 下一块打包时真实入账
    ok.textContent = t("send.pending", { hash: r.tx_hash.slice(0, 12) });
    setTimeout(() => (ok.hidden = true), 6000);
    await refreshAll();
  } catch (e) {
    $("err-send").textContent = String(e);
  }
  $("btn-send").disabled = false;
});

// ---------------------------------------------------------------- 收款二维码

let qrLoadedAddr = "";
async function loadReceiveQr() {
  const addr = $("addr").textContent;
  if (!addr || !addr.startsWith("ql1") || addr === qrLoadedAddr) return;
  try {
    const svg = await invoke("receive_qr");
    $("recv-qr").innerHTML = svg;
    qrLoadedAddr = addr;
  } catch (e) { /* 未解锁等场景静默重试 */ }
}

// ---------------------------------------------------------------- .dat 备份 / 恢复

$("btn-backup").addEventListener("click", async () => {
  const st = $("backup-status");
  st.hidden = true;
  try {
    const saved = await invoke("export_backup");
    if (!saved) return; // 用户取消
    st.hidden = false;
    st.textContent = t("backup.export.done", { path: saved.split("/").pop() });
    setTimeout(() => (st.hidden = true), 6000);
  } catch (e) {
    st.hidden = false;
    st.textContent = String(e);
  }
});

$("btn-restore-dat").addEventListener("click", async () => {
  $("err-create").textContent = "";
  try {
    const restored = await invoke("import_backup");
    if (!restored) return; // 用户取消
    const okBox = $("ok-create");
    okBox.hidden = false;
    okBox.textContent = t("backup.import.done");
    await route(); // 密钥库已还原 → 转入解锁页
  } catch (e) {
    $("err-create").textContent = String(e);
  }
});

// ---------------------------------------------------------------- 🌍 世界页（本地渲染 + 实体编辑器）
//
// 分工（DESIGN.md §9.8 / §9.11）：链只提供**坐标与归属**，地形像素全部由本机
// world.js 从「创世哈希 ⊕ 全部区块 VRF 折叠」算出来。钱包不下载地图、不存贴图，
// 任何一台机器拿同一份 blocks.jsonl 都必须渲染出逐位相同的大陆（对拍指纹为证）。

const KIND_COLORS = ["#9AA7BD", "#FF6A2A", "#2FBF71", "#29B6F6", "#F06292", "#00E0C8", "#B388FF", "#FFD23F"];
const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

const wcvs = $("world-canvas");
const wctx = wcvs.getContext("2d");
let worldSnap = null;
let terrain = { sig: "", rgb: null, era1: 0, ents: 0, fp: "", heights: null, blocks: null };
let wSlot = 0;
let wSel = ""; // 选中实体 id（空 = 未选中）
let wProof = { id: "", json: "" }; // 展开的可验证记录（2 秒轮询重渲染时不能丢）
let w3 = null; // 第一人称体素编辑器（惰性创建）

const worldActive = () => !$("page-world").classList.contains("hidden");
const worldErr = (m) => { $("err-world").textContent = m; $("ok-world").hidden = true; };
const worldOk = (m) => { $("err-world").textContent = ""; const o = $("ok-world"); o.hidden = false; o.textContent = m; setTimeout(() => (o.hidden = true), 6000); };
const entById = (id) => (worldSnap ? worldSnap.entities.find((e) => e.entity_id_hex === id) : null);
const idShort = (id) => (id ? `${id.slice(0, 10)}…${id.slice(-8)}` : "—");

/** 种子签名：创世哈希 + 历史长度 + 最后一个 VRF（任一变化地形就变） */
function terrainSig(s) {
  return `${s.genesis_hash_hex}|${s.vrfs.length}|${s.vrfs[s.vrfs.length - 1] || ""}`;
}

/** 地形只算一次并缓存；链长一格才重算（每格 2 秒，重算约十几毫秒） */
function buildTerrain(s) {
  const sig = terrainSig(s);
  if (terrain.sig !== sig || !terrain.rgb) {
    const out = QLWorld.render(QLWorld.seedWords(s.genesis_hash_hex, s.vrfs));
    terrain = {
      sig, rgb: out.rgb, era1: out.era1, ents: out.entities,
      fp: QLWorld.fingerprint(out.rgb), heights: out.heights, blocks: out.blocks,
    };
  }
  return terrain;
}

/** 贴地高度层：与链侧同一个算式（layer = 高度场 >> 3），2D 页放的东西落地就在这个层 */
function surfaceLayer(x, z) {
  const t = buildTerrain(worldSnap);
  if (!t.heights) return 0;
  return t.heights[z * QLWorld.W + x] >> QLWorld3D.LEVEL_SHIFT;
}

function paintWorld() {
  if (!worldSnap) return;
  const s = worldSnap;
  QLWorld.paint(wctx, buildTerrain(s).rgb);
  for (const e of s.entities) {
    if (e.released) continue; // 断租不删除，但也不再显形/占格（与 world.rs live_at 同判据）
    wctx.fillStyle = KIND_COLORS[e.kind] || KIND_COLORS[0];
    wctx.fillRect(e.x - 1, e.y - 1, 3, 3);
    if (s.address && e.owner === s.address) {
      wctx.strokeStyle = "#E3B54A";
      wctx.strokeRect(e.x - 2.5, e.y - 2.5, 6, 6);
    }
  }
  const sel = entById(wSel);
  if (sel) {
    wctx.strokeStyle = "#FFFFFF";
    wctx.strokeRect(sel.x - 4.5, sel.y - 4.5, 9, 9);
    wctx.strokeStyle = "#12161F";
    wctx.strokeRect(sel.x - 5.5, sel.y - 5.5, 11, 11);
  }
}

function buildHotbar() {
  const n = worldSnap ? worldSnap.slot_count : 8;
  const bar = $("world-hotbar");
  bar.innerHTML = "";
  for (let i = 0; i < n; i++) {
    const b = document.createElement("button");
    b.className = "hs" + (i === wSlot ? " active" : "");
    b.dataset.slot = String(i);
    b.type = "button";
    b.title = t(`world.kind.${i}`);
    b.innerHTML = `<i class="kd" style="background:${KIND_COLORS[i]}"></i><b>${i + 1}</b>`;
    bar.appendChild(b);
  }
  $("world-slot-tag").textContent = `slot ${wSlot + 1} · ${t(`world.kind.${wSlot}`)}`;
}

function renderF3() {
  const s = worldSnap;
  const tr = buildTerrain(s);
  const r = (k, v, cls = "") => `<div class="f3k">${k}</div><div class="f3v ${cls}">${v}</div>`;
  const rows = [
    [t("world.f3.genesis"), `<span class="cpy mono" data-copy="${s.genesis_hash_hex}">${s.genesis_hash_hex.slice(0, 16)}…</span>`],
    [t("world.f3.blocks"), `${s.vrfs.length} · #${s.height}`],
    [t("world.f3.rules"), `${QLWorld.RULES_VERSION}`],
    [t("world.f3.fp"), `<span class="mono gold">${tr.fp}</span>`],
    [t("world.f3.era1"), `${tr.era1}`],
    [t("world.f3.nat"), `${tr.ents}`],
    [t("world.f3.ent"), `${s.entities.length}`],
    [t("world.f3.layers"), `${s.max_layer} · ${t("world.f3.layers.note")}`],
    [t("world.f3.chunks"), `${s.chunks.length}`],
    [t("world.f3.addr"), s.address ? `<span class="cpy mono" data-copy="${s.address}">${short(s.address)}</span>` : `<span class="muted">${t("world.locked")}</span>`],
  ];
  let led;
  if (!s.report.total) led = `<span class="muted">${t("world.ledger.empty")}</span>`;
  else if (s.report.ok) led = `<span class="ok-inline">${t("world.ledger.ok", { n: s.report.total })}</span>`;
  else led = `<span class="bad-inline">${t("world.ledger.bad", { n: s.report.first_bad, r: s.report.reason })}</span>`;
  rows.push([t("world.f3.ledger"), led]);
  $("world-f3").innerHTML = rows.map(([k, v]) => r(k, v)).join("");
}

function renderParentSelect() {
  const s = worldSnap;
  const sel = $("ent-parent");
  const keep = sel.value;
  const mine = s.entities.filter((e) => e.owner === s.address && !e.released);
  sel.innerHTML =
    `<option value="">${t("world.parent.none")}</option>` +
    mine.map((e) => `<option value="${e.entity_id_hex}">${esc(e.name)} · ${e.x},${e.y},${e.layer}</option>`).join("");
  if (keep && [...sel.options].some((o) => o.value === keep)) sel.value = keep;
}

function renderEntList() {
  const s = worldSnap;
  const box = $("ent-list");
  const mine = s.entities.filter((e) => e.owner === s.address).length;
  $("ent-count").textContent = t("world.list.count", { n: s.entities.length, m: mine });
  if (!s.entities.length) {
    box.innerHTML = `<div class="muted small">${t("world.list.empty")}</div>`;
    return;
  }
  const sorted = s.entities.slice().sort((a, b) => Number(b.owner === s.address) - Number(a.owner === s.address) || b.last_height - a.last_height);
  const keepScroll = box.scrollTop; // 2 秒轮询重渲染不打断阅读位置
  box.innerHTML = sorted
    .map(
      (e) => `<div class="ent-row${e.entity_id_hex === wSel ? " active" : ""}" data-id="${e.entity_id_hex}">
        <i class="kd" style="background:${e.released ? "#9AA7BD" : KIND_COLORS[e.kind] || KIND_COLORS[0]}"></i>
        <div class="er-main">
          <div class="er-n">${esc(e.name)}${e.era === 1 ? ` <span class="tag era1">${t("world.sel.era1")}</span>` : ""}${e.released ? ` <span class="tag">${t("world.unowned")}</span>` : ""}</div>
          <div class="er-m">X ${e.x} · Z ${e.y} · Y ${e.layer} · ${t(`world.kind.${e.kind}`)} · #${e.last_height}</div>
        </div>
        <div class="er-o">${e.owner === s.address ? t("world.own") : ""}</div>
      </div>`
    )
    .join("");
  box.scrollTop = keepScroll;
}

/** 选中一个实体：名字输入框为空时顺手填上，改名按钮立刻可用 */
function selectEnt(id) {
  wSel = id;
  const e = entById(id);
  if (!e) return;
  $("ent-x").value = e.x;
  $("ent-z").value = e.y;
  $("ent-layer").value = e.layer;
  if (!$("ent-name").value.trim()) $("ent-name").value = e.name;
  renderEntList();
  renderSelected();
  paintWorld();
}

function renderSelected() {
  const box = $("ent-sel");
  const e = entById(wSel);
  if (!e) {
    box.hidden = true;
    $("ent-sel-hint").textContent = t("world.sel.hint");
    return;
  }
  box.hidden = false;
  $("ent-sel-hint").textContent = "";
  const s = worldSnap;
  const own = !!s.address && e.owner === s.address;
  const open = wProof.id === e.entity_id_hex && !!wProof.json;
  const r = (k, v) => `<div class="f3k">${k}</div><div class="f3v">${v}</div>`;
  const btn = (act, label, cls = "") => `<button type="button" class="es-btn ${cls}" data-act="${act}">${label}</button>`;
  const acts = [];
  if (own && !e.released) {
    acts.push(btn("rename", t("world.btn.rename")));
    acts.push(btn("move", t("world.btn.move")));
    acts.push(btn("release", t("world.btn.release"), "danger"));
  }
  acts.push(btn("proof", t("world.btn.proof")));
  box.innerHTML = `
    <div class="es-name"><i class="kd" style="background:${e.released ? "#9AA7BD" : KIND_COLORS[e.kind] || KIND_COLORS[0]}"></i>${esc(e.name)}</div>
    <dl class="f3">
      ${r(t("world.sel.id"), `<span class="cpy mono" data-copy="${e.entity_id_hex}" title="${t("hist.copy")}">${idShort(e.entity_id_hex)}</span>`)}
      ${r("X · Z · Y", `${e.x} · ${e.y} · ${e.layer}`)}
      ${r(t("world.sel.kind"), t(`world.kind.${e.kind}`))}
      ${r(t("world.sel.owner"), own ? t("world.own") : e.released ? t("world.unowned") : `<span class="mono">${short(e.owner)}</span>`)}
      ${r(t("world.sel.era"), e.era === 1 ? t("world.sel.era1") : t("world.sel.eraN", { n: e.era }))}
      ${r(t("world.sel.created"), `#${e.created_height}`)}
      ${e.parent_hex ? r(t("world.sel.parent"), `<span class="mono">${idShort(e.parent_hex)}</span>`) : ""}
    </dl>
    <div class="es-btns">${acts.join("")}</div>
    <pre class="proof" id="ent-proof" ${open ? "" : "hidden"}>${open ? esc(wProof.json) : ""}</pre>`;
}

function renderWorld() {
  buildHotbar();
  renderF3();
  renderParentSelect();
  renderEntList();
  renderSelected();
  paintWorld();
  syncWorld3D();
}

async function loadWorld() {
  worldSnap = await invoke("world_snapshot");
  renderWorld();
}

// —— 交互：物品栏、地图、编辑器

$("world-hotbar").addEventListener("click", (e) => {
  const b = e.target.closest(".hs");
  if (!b) return;
  wSlot = Number(b.dataset.slot);
  buildHotbar();
});

document.addEventListener("keydown", (e) => {
  if (e.metaKey || e.ctrlKey || e.altKey) return; // 不抢系统/浏览器快捷键
  if (!worldActive() || $("view-main").classList.contains("hidden")) return;
  const tag = (e.target.tagName || "").toLowerCase();
  if (tag === "input" || tag === "select" || tag === "textarea") return;
  const n = Number(e.key);
  if (n >= 1 && n <= (worldSnap ? worldSnap.slot_count : 8)) {
    wSlot = n - 1;
    buildHotbar();
  }
});

/** 鼠标位置 → 格子坐标（offsetX/Y 以内容盒为原点，不受 1px 边框和 CSS 缩放影响） */
function toCell(ev) {
  const x = Math.floor((ev.offsetX * QLWorld.W) / wcvs.clientWidth);
  const y = Math.floor((ev.offsetY * QLWorld.H) / wcvs.clientHeight);
  if (x < 0 || y < 0 || x >= QLWorld.W || y >= QLWorld.H) return null;
  return { x, y };
}

wcvs.addEventListener("mousemove", (ev) => {
  const c = toCell(ev);
  const el = $("world-cursor");
  if (!c || !worldSnap) {
    el.textContent = "X – · Z –";
    return;
  }
  const cx = c.x >> 4, cy = c.y >> 4;
  const ch = worldSnap.chunks.find((k) => k.cx === cx && k.cy === cy);
  el.textContent =
    `X ${c.x} · Z ${c.y} · Y ${surfaceLayer(c.x, c.y)}` +
    (ch ? ` · chunk ${cx},${cy}` : "") +
    (ch ? ` · ${t("world.f3.ch", { n: ch.count })} ${ch.root_hex.slice(0, 8)}…` : "");
});

wcvs.addEventListener("mouseleave", () => ($("world-cursor").textContent = "X – · Z –"));

wcvs.addEventListener("click", (ev) => {
  const c = toCell(ev);
  if (!c || !worldSnap) return;
  const hit = worldSnap.entities
    .filter((e) => !e.released)
    .map((e) => ({ e, d: Math.abs(e.x - c.x) + Math.abs(e.y - c.y) }))
    .sort((a, b) => a.d - b.d)[0];
  if (hit && hit.d <= 3) {
    selectEnt(hit.e.entity_id_hex);
  } else {
    $("ent-x").value = c.x;
    $("ent-z").value = c.y;
    $("ent-layer").value = surfaceLayer(c.x, c.y);
    renderEntList();
    renderSelected();
    paintWorld();
  }
});

$("btn-place").addEventListener("click", async () => {
  worldErr("");
  const name = $("ent-name").value.trim();
  if (!name) { worldErr(t("world.err.noname")); return; }
  if (!worldSnap || !worldSnap.unlocked) { worldErr(t("world.err.locked")); return; }
  $("btn-place").disabled = true;
  try {
    const rec = await invoke("world_place", {
      name,
      x: Number($("ent-x").value),
      y: Number($("ent-z").value),
      layer: Number($("ent-layer").value),
      kind: wSlot,
      parentHex: $("ent-parent").value,
    });
    wSel = rec.entity_id_hex;
    $("ent-name").value = "";
    worldOk(t("world.ok.placed", { name: rec.name, id: idShort(rec.entity_id_hex) }));
    await loadWorld();
  } catch (e) {
    worldErr(String(e));
  }
  $("btn-place").disabled = false;
});

$("ent-list").addEventListener("click", (e) => {
  const row = e.target.closest(".ent-row");
  if (row) selectEnt(row.dataset.id);
});

$("ent-sel").addEventListener("click", async (e) => {
  const cpy = e.target.closest(".cpy");
  if (cpy) {
    try { await navigator.clipboard.writeText(cpy.dataset.copy); } catch { return; }
    const old = cpy.textContent;
    cpy.textContent = t("recv.copied");
    setTimeout(() => (cpy.textContent = old), 1200);
    return;
  }
  const b = e.target.closest(".es-btn");
  if (!b || !wSel) return;
  const act = b.dataset.act;
  const e0 = entById(wSel);
  if (!e0) return;
  worldErr("");
  try {
    if (act === "rename") {
      const name = $("ent-name").value.trim();
      if (!name) { worldErr(t("world.err.noname")); return; }
      await invoke("world_rename", { entityIdHex: wSel, name });
      worldOk(t("world.ok.renamed", { name }));
    } else if (act === "move") {
      const x = Number($("ent-x").value), y = Number($("ent-z").value), layer = Number($("ent-layer").value);
      await invoke("world_move", { entityIdHex: wSel, x, y, layer });
      worldOk(t("world.ok.moved", { x, y, layer }));
    } else if (act === "release") {
      if (!confirm(t("world.confirm.release", { name: e0.name }))) return;
      await invoke("world_release", { entityIdHex: wSel });
      worldOk(t("world.ok.released", { name: e0.name }));
    } else if (act === "proof") {
      if (wProof.id === wSel && wProof.json) { wProof = { id: "", json: "" }; } // 再点一次收起
      else wProof = { id: wSel, json: await invoke("world_proof", { entityIdHex: wSel }) };
      renderSelected();
      return;
    }
    await loadWorld();
  } catch (err) {
    worldErr(String(err));
  }
});

// —— 复制 F3 里的哈希/地址（事件委托，重渲染不失效）
$("world-f3").addEventListener("click", async (e) => {
  const el = e.target.closest(".cpy");
  if (!el) return;
  try { await navigator.clipboard.writeText(el.dataset.copy); } catch { return; }
  const old = el.textContent;
  el.textContent = t("recv.copied");
  setTimeout(() => (el.textContent = old), 1200);
});

// —— 第一人称体素编辑（Minecraft 式创造）：地形仍是本机算出来的，只有签了名的方块才占格

const w3toast = $("w3d-toast");
let w3Busy = false, w3HudAt = 0, w3ToastTimer = 0, w3Spawned = false;

function w3Toast(msg, bad) {
  w3toast.hidden = false;
  w3toast.textContent = msg;
  w3toast.classList.toggle("bad", !!bad);
  clearTimeout(w3ToastTimer);
  w3ToastTimer = setTimeout(() => (w3toast.hidden = true), 3800);
}

/** 方块自动命名「类型·坐标」：第一人称里手打名字不现实，回来在列表里改 */
function w3Name(slot, x, z, layer) {
  return `${t(`world.kind.${slot}`)}·${x}_${z}_${layer}`.slice(0, 40);
}

/** 出生点：以当前编辑框里的格子为圆心往外找一块不是水的陆地 */
function pickSpawn(x, z) {
  const t = buildTerrain(worldSnap);
  if (!t.blocks) return { x: 128, z: 128 };
  const W = QLWorld.W, H = QLWorld.H;
  const land = (cx, cz) => cx >= 0 && cz >= 0 && cx < W && cz < H && (t.blocks[cz * W + cx] & 7) !== 0;
  if (land(x, z)) return { x, z };
  for (let r = 1; r <= 60; r++) {
    for (let d = 0; d < 8 * r; d++) {
      const side = d % r, ring = (d / r) | 0;
      const cx = ring === 0 ? x + r - side : ring === 1 ? x - r + side : ring === 2 ? x + r : x - r;
      const cz = ring === 0 ? z - r : ring === 1 ? z + r : ring === 2 ? z + r - side : z - r + side;
      if (land(cx, cz)) return { x: cx, z: cz };
    }
  }
  return { x, z };
}

function ensureWorld3D() {
  if (w3) return true;
  try {
    w3 = QLWorld3D.create($("w3d-canvas"), {
      onUnsupported: () => { w3Toast(t("world3d.unsupported"), true); closeWorld3D(); },
      onSlot: (i) => { wSlot = i; buildHotbar(); buildW3Hotbar(); if (w3) w3.setSlot(i); },
      onPlace: (x, z, layer) => w3Place(x, z, layer, wSlot),
      onBreak: (x, z, layer, ent) => w3Break(ent),      onBlocked: () => w3Toast(t("world3d.err.blocked"), true),
      onEmpty: () => w3Toast(t("world3d.err.notarget"), true),
      onLock: (on) => { $("w3d-cross").classList.toggle("locked", on); w3Hint(); },
      onFrame: (s) => { const n = performance.now(); if (n - w3HudAt > 120) { w3HudAt = n; renderW3Hud(s); } },
      onClose: () => closeWorld3D(),
    });
  } catch (e) {
    w3 = null;
    worldErr(String(e));
    return false;
  }
  if (!w3) return false;
  syncWorld3D();
  return true;
}

function syncWorld3D() {
  if (!w3 || !worldSnap) return;
  const t = buildTerrain(worldSnap);
  w3.setWorld(t.heights, t.blocks);
  w3.setMaxLayer(worldSnap.max_layer || QLWorld3D.MAX_LAYER);
  w3.setColors(KIND_COLORS);
  w3.setEntities(worldSnap.entities, worldSnap.address);
  w3.setSlot(wSlot);
}

function buildW3Hotbar() {
  const n = worldSnap ? worldSnap.slot_count : 8;
  const bar = $("w3d-hotbar");
  bar.innerHTML = "";
  for (let i = 0; i < n; i++) {
    const b = document.createElement("button");
    b.className = "hs" + (i === wSlot ? " active" : "");
    b.dataset.slot = String(i);
    b.type = "button";
    b.title = t(`world.kind.${i}`);
    b.innerHTML = `<i class="kd" style="background:${KIND_COLORS[i]}"></i><b>${i + 1}</b>`;
    bar.appendChild(b);
  }
}

function w3Hint() {
  $("w3d-hint").textContent = t("world3d.hint");
}

function renderW3Hud(s) {
  const s0 = worldSnap;
  if (!s0) return;
  const tg = s.target;
  const ch = s0.chunks.find((k) => k.cx === (s.x >> 4) && k.cy === (s.z >> 4));
  const row = (k, v, cls = "") => `<div class="f3k">${k}</div><div class="f3v ${cls}">${v}</div>`;
  const pos = row(t("world3d.f3.pos"), `X ${s.x} · Y ${s.layer} · Z ${s.z}`);
  const surf = row(t("world3d.f3.surface"), `${s.surface} · ${t(`world3d.biome.${s.block & 7}`)}`);
  const tgt = row(t("world3d.f3.target"), tg ? `${tg.cx}, ${tg.layer}, ${tg.cz}${tg.entity ? ` · ${esc(tg.entity.name)}` : ""}` : "—", tg && tg.entity && tg.entity.owner === s0.address ? "gold" : "");
  const plc = row(t("world3d.f3.place"), tg ? (tg.place ? `${tg.place.cx}, ${tg.place.layer}, ${tg.place.cz}` : t("world3d.f3.nospace")) : "—");
  const slot = row(t("world3d.f3.slot"), `${wSlot + 1} · ${t(`world.kind.${wSlot}`)}`);
  const chn = row(t("world3d.f3.chunk"), ch ? `${ch.count} · ${ch.root_hex.slice(0, 8)}…` : "0");
  const fp = row(t("world3d.f3.fp"), `<span class="mono gold">${terrain.fp}</span>`);
  const fps = row("FPS", `${s.fps}${s.locked ? "" : " · " + t("world3d.f3.unlocked")}`);
  $("w3d-f3").innerHTML = [pos, surf, tgt, plc, slot, chn, fp, fps].join("");
}

async function w3Place(x, z, layer, slot) {
  if (w3Busy) return;
  if (!worldSnap || !worldSnap.unlocked) { w3Toast(t("world.err.locked"), true); return; }
  w3Busy = true;
  try {
    const rec = await invoke("world_place", {
      name: w3Name(slot, x, z, layer), x, y: z, layer, kind: slot, parentHex: "",
    });
    wSel = rec.entity_id_hex;
    w3Toast(t("world.ok.placed", { name: rec.name, id: idShort(rec.entity_id_hex) }));
    await loadWorld();
  } catch (e) {
    w3Toast(String(e), true);
  }
  w3Busy = false;
}

async function w3Break(ent) {
  if (!ent) { w3Toast(t("world3d.err.terrain"), true); return; }
  if (!worldSnap || !worldSnap.unlocked) { w3Toast(t("world.err.locked"), true); return; }
  if (ent.owner !== worldSnap.address) { w3Toast(t("world3d.err.notown"), true); return; }
  if (!confirm(t("world.confirm.release", { name: ent.name }))) return;
  try {
    await invoke("world_release", { entityIdHex: ent.entity_id_hex });
    w3Toast(t("world.ok.released", { name: ent.name }));
    await loadWorld();
  } catch (e) {
    w3Toast(String(e), true);
  }
}

async function openWorld3D() {
  if (!worldSnap) {
    try {
      await loadWorld();
    } catch (e) {
      worldErr(String(e));
      return;
    }
  }
  $("w3d").hidden = false;
  document.body.classList.add("w3d-on");
  if (!ensureWorld3D()) { closeWorld3D(); return; }
  buildW3Hotbar();
  w3Hint();
  if (!w3Spawned) {
    const c = pickSpawn(Number($("ent-x").value) || 128, Number($("ent-z").value) || 128);
    w3.jumpTo(c.x, c.z, null);
    w3Spawned = true;
  }
  w3.open();
}

function closeWorld3D() {
  if ($("w3d").hidden && !w3) return;
  if (w3) w3.close();
  $("w3d").hidden = true;
  document.body.classList.remove("w3d-on");
}

$("btn-w3d").addEventListener("click", openWorld3D);
$("w3d-exit").addEventListener("click", () => closeWorld3D());
$("w3d-hotbar").addEventListener("click", (e) => {
  const b = e.target.closest(".hs");
  if (!b) return;
  wSlot = Number(b.dataset.slot);
  buildHotbar();
  buildW3Hotbar();
  if (w3) w3.setSlot(wSlot);
});

// ---------------------------------------------------------------- 出块事件（实时推送）

listen("chain_block", (ev) => {
  const e = ev.payload;
  const b = fmtQlx(e.balance);
  $("bal-int-big").textContent = b.int;
  $("bal-frac-big").textContent = `.${b.frac}`;
  $("asset-bal").textContent = `${fmtAmt(e.balance)}.${b.frac.slice(0, 2)} QLX`;
  $("stat-height").textContent = e.height;

  const capPct = (e.total_minted / stateCache.hard_cap) * 100;
  $("stat-minted").textContent = `${fmtAmt(e.total_minted)} QLX`;
  $("stat-cap").textContent = `${capPct.toFixed(6)}%`;
  $("cap-bar").style.width = `${Math.max(capPct, 0.0000002)}%`;
  $("mine-earned").textContent = `${fmtAmt(e.balance)} QLX`;

  if (e.vrf_hex) {
    const log = $("vrf-log");
    const line = document.createElement("div");
    line.innerHTML = `<b>#${e.height}</b> <span class="vrf-reward">+${(e.reward / 1e9).toFixed(2)} QLX</span> · VRF ${e.vrf_hex.slice(0, 16)}…`;
    log.prepend(line);
    while (log.childElementCount > 50) log.lastChild.remove();
  }

  // 新块 = 新的历史 = 世界真的会变（种子含全部 VRF 折叠）
  if (worldSnap && worldActive()) loadWorld().catch(() => {});
});

route().catch((e) => console.error(e));
