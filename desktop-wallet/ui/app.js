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
// 光尘没有小数位：它按"个"计，不像 QLX 分 9 位。用 fmtAmt 会把 60 印成 0.00000006。
const fmtDust = (n) => Number(n || 0).toLocaleString("en-US");
const short = (addr) => `${addr.slice(0, 10)}…${addr.slice(-8)}`;
const ts = (unix) => new Date(unix * 1000).toLocaleString(LANG === "en" ? "en-US" : "zh-CN", { hour12: false });

// ---------------------------------------------------------------- i18n 接线

applyI18n();
invoke("app_version")
  .then((v) => { window.__QLX_VERSION__ = v; applyI18n(); })
  .catch(() => {});
$("lang-select").addEventListener("change", (e) => { setLang(e.target.value); setNetChip(); });

// ---------------------------------------------------------------- 联邦网络 chip

let netCount = 0;
// 版本普查两堆：规则指纹不同（一定分叉）／指纹相同但发行包不同（家族内改过判据也会分叉）
let netFork = [];
let netOdd = [];
let netRules = "";
let netVer = "";
const netRows = (list) => list.map((p) => `${p.app_version} / ${p.rules}`).join(" · ");
function setNetChip() {
  const el = $("net-chip");
  el.classList.toggle("fed", netCount > 0);
  el.classList.toggle("warn", netFork.length > 0);
  el.classList.toggle("odd", !netFork.length && netOdd.length > 0);
  const mine = `${netVer || "—"} / ${netRules || "—"}`;
  if (netFork.length) {
    el.textContent = t("net.fed.warn", { n: netFork.length });
    el.title = t("net.fed.warn.tip", { mine, peers: netRows(netFork) });
  } else if (netOdd.length) {
    el.textContent = t("net.fed.odd", { n: netOdd.length });
    el.title = t("net.fed.odd.tip", { mine, peers: netRows(netOdd) });
  } else {
    el.textContent = netCount > 0 ? t("net.fed", { n: netCount }) : t("net.solo");
    el.title = "";
  }
  // 欢迎页的同一句话：还没进主界面时也得知道"现在和谁在一个世界"
  const wt = $("welcome-net-tag");
  if (wt) {
    wt.textContent = netCount > 0 ? t("net.fed", { n: netCount }) : t("net.solo");
    wt.classList.toggle("fed", netCount > 0);
  }
}
let lastNet = null;
function applyNet(s) {
  lastNet = s;
  netCount = s.count || 0;
  netFork = s.rules_mismatch || [];
  netOdd = s.version_diff || [];
  netRules = s.rules || "";
  netVer = s.app_version || "";
  setNetChip();
  renderNetstate();
}
listen("net_peers", (ev) => {
  netCount = ev.payload.count || 0;
  setNetChip();
});
// 分歧集合变了（新邻居/邻居升级或掉线/公告过期）才推这一个事件，形状与 net_status 一致
listen("net_versions", (ev) => applyNet(ev.payload));
invoke("net_status").then(applyNet).catch(() => {});

// ---------------------------------------------------------------- 页面路由（侧边导航）

const pages = ["asset", "mine", "history", "world", "about"];
document.querySelectorAll(".sb-item").forEach((btn) => {
  btn.addEventListener("click", () => {
    document.querySelectorAll(".sb-item").forEach((b) => b.classList.toggle("active", b === btn));
    pages.forEach((p) => $(`page-${p}`).classList.toggle("hidden", p !== btn.dataset.page));
    if (btn.dataset.page !== "world") closeWorld3D(); // 离开世界页就把第一人称视图收掉，不停止渲染还白烧 GPU
    // 第 4 步是导航事实：看过活动就记下来（不改账本、不影响任何判据）
    if (btn.dataset.page === "history" && !prefsCache.opened_history) {
      setPref("opened_history", true).then(renderGuide);
    }
    if (btn.dataset.page === "world") renderGuide();
    if (btn.dataset.page === "about") { renderFeedback(); renderNetstate(); }
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
  for (const v of ["view-welcome", "view-create", "view-lock", "view-main"]) {
    $(v).classList.toggle("hidden", v !== view);
  }
}

// 外壳偏好（prefs.json）：欢迎页还要不要弹、引导走到哪一步。
// 进度跟账本走（同机多开的实例各有各的 prefs），所以存文件而不是 localStorage。
let prefsCache = { welcome_done: false, opened_world: false, opened_history: false, guide_done: false };
async function loadPrefs() {
  try { prefsCache = await invoke("get_prefs"); } catch { /* 旧后端：按首次运行处理 */ }
  return prefsCache;
}
async function setPref(key, value) {
  try { prefsCache = await invoke("set_pref", { key, value }); } catch { /* 写失败就别假装存下了 */ }
  return prefsCache;
}

// 从欢迎页点进来要不要直接进世界：解锁/创建完成后由 enterMain 兑现
let wantWorld = false;

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
  renderGuide();
  if (wantWorld) {
    wantWorld = false;
    document.querySelector('.sb-item[data-page="world"]').click();
    openWorld3D();
  }
}

async function boot() {
  await loadPrefs();
  await renderWelcome();
  show("view-welcome");
}

// ---------------------------------------------------------------- 欢迎页 / 引导 / 反馈 / 联机三态

/** 反馈包与欢迎页「技术参数」共用同一份聚合诊断：一个字段来源，少一处漂移 */
function diagRows(d) {
  if (!d) return "";
  const row = (k, v) => `<div class="f3k">${esc(k)}</div><div class="f3v">${esc(String(v))}</div>`;
  const bad = (n) => (n ? ` · ${t("fb.d.bad")} ${n}` : "");
  return [
    row(t("fb.d.version"), d.app_version),
    row(t("fb.d.rules"), d.rules),
    row(t("fb.d.os"), `${d.os} · ${d.arch}`),
    row(t("fb.d.height"), `#${d.height}`),
    row(t("fb.d.peers"), d.peers),
    row(t("fb.d.present"), d.present),
    row(t("fb.d.entity"), `${d.entity_lines}${bad(d.entity_bad_lines)}`),
    row(t("fb.d.dust"), `${d.dust_lines}${bad(d.dust_bad_lines)}`),
    row(t("fb.d.unlocked"), d.unlocked ? t("fb.yes") : t("fb.no")),
    row(t("fb.d.multi"), d.multi_instance ? t("fb.yes") : t("fb.no")),
  ].join("");
}

async function renderWelcome() {
  const d = await invoke("diag_report").catch(() => null);
  if (d) $("welcome-adv-list").innerHTML = diagRows(d);
  const s = await invoke("net_status").catch(() => null);
  if (s) applyNet(s);
}

/** 离开开场画面；想进世界就记下来，等解锁/创建完再兑现。 */
async function leaveWelcome(world) {
  wantWorld = !!world;
  await route();
}
$("btn-w-world").addEventListener("click", () => leaveWelcome(true));
$("btn-w-wallet").addEventListener("click", () => leaveWelcome(false));
$("btn-w-how").addEventListener("click", () => $("welcome-how").classList.toggle("hidden"));
$("btn-main-gate").addEventListener("click", async () => {
  closeWorld3D();
  await renderWelcome();
  show("view-welcome");
});

// —— 首次任务引导：第 2、3 步以账本为准，第 1、4 步只是导航状态（见 index.html 注释）
function guideSteps() {
  const s = worldSnap;
  const dug = s ? (s.dug || []).length : 0;
  const owned = s ? s.entities.filter((e) => !e.released && e.owner === s.address).length : 0;
  return [
    { done: !!prefsCache.opened_world, nav: true },
    { done: dug > 0, ledger: true, n: dug },
    { done: owned > 0, ledger: true, n: owned },
    { done: !!prefsCache.opened_history, nav: true },
  ];
}

function renderGuide() {
  const panel = $("panel-guide");
  if (!panel) return;
  panel.hidden = !!prefsCache.guide_done;
  if (prefsCache.guide_done) return;
  const steps = guideSteps();
  const done = steps.filter((s) => s.done).length;
  $("guide-count").textContent = t("guide.count", { done, total: steps.length });
  const go = ["world", "world", "world", "history"];
  $("guide-list").innerHTML = steps.map((s, i) => `
    <li class="${s.done ? "done" : ""}">
      <span class="gd-mark">${s.done ? "✓" : i + 1}</span>
      <div class="gd-text">
        <b>${esc(t(`guide.s${i + 1}t`))}</b>
        <span class="gd-kind ${s.ledger ? "ledger" : ""}">${t(s.ledger ? "guide.ledger" : "guide.nav")}</span>
        <div class="muted small">${esc(t(`guide.s${i + 1}`))}${s.n ? ` · <span class="mono">${s.n}</span>` : ""}</div>
      </div>
      ${s.done ? "" : `<button type="button" class="btn-ghost gd-go" data-go="${go[i]}">${t("guide.go")}</button>`}
    </li>`).join("");
}
$("guide-list").addEventListener("click", (e) => {
  const b = e.target.closest("[data-go]");
  if (!b) return;
  document.querySelector(`.sb-item[data-page="${b.dataset.go}"]`).click();
  if (b.dataset.go === "world") openWorld3D();
});
$("btn-guide-hide").addEventListener("click", async () => {
  await setPref("guide_done", true);
  renderGuide();
});

// —— 联机三态：前两档能用，公网没实现就说没实现
function renderNetstate() {
  const list = $("netstate-list");
  if (!list) return;
  const sc = (lastNet && lastNet.scope) || { loopback: 0, lan: 0, public: 0, unknown: 0 };
  const local = sc.loopback || 0, lan = sc.lan || 0, pub = sc.public || 0;
  const li = (cls, title, note, n) => `<li class="${cls}">
      <b>${esc(title)}</b>
      <span class="muted small">${esc(note)}</span>
      ${n ? `<span class="tag">${t("netstate.now", { n })}</span>` : ""}
    </li>`;
  list.innerHTML =
    li(local ? "on" : "off", t("netstate.local"), t("netstate.local.note"), local) +
    li(lan ? "on" : "off", t("netstate.lan"), t("netstate.lan.note"), lan) +
    li("todo", t("netstate.public"), t("netstate.public.note"), pub);
  if (!local && !lan && !pub) {
    list.insertAdjacentHTML("afterbegin", `<li class="off"><b>${t("netstate.none")}</b></li>`);
  }
}

// —— 反馈：只导出聚合数字；地址与截图都要显式勾选
let fbBusy = false;
async function renderFeedback() {
  const d = await invoke("diag_report").catch(() => null);
  if (d) $("fb-diag").innerHTML = diagRows(d);
  const inWorld = !!(w3 && $("w3d") && !$("w3d").hidden);
  $("fb-shot").disabled = !inWorld;
  $("fb-shot").checked = $("fb-shot").checked && inWorld;
  $("fb-shot").title = inWorld ? "" : t("fb.shot.off");
}

$("btn-fb").addEventListener("click", async () => {
  if (fbBusy) return;
  const err = $("err-fb"), ok = $("ok-fb");
  err.textContent = "";
  ok.hidden = true;
  const problem = $("fb-text").value.trim();
  if (!problem) { err.textContent = t("fb.err.empty"); return; }
  let screenshot = null;
  if ($("fb-shot").checked && !$("fb-shot").disabled) {
    screenshot = await new Promise((resolve) => {
      if (!w3) return resolve(null);
      if (w3three) return resolve(w3three.snapshot());
      let got = false;
      w3.snapshot((u) => { got = true; resolve(u); });
      setTimeout(() => { if (!got) resolve(null); }, 500);
    });
    if (!screenshot) { err.textContent = t("fb.shot.off"); return; }
  }
  fbBusy = true;
  $("btn-fb").disabled = true;
  try {
    const p = await invoke("feedback_write", {
      problem, includeAddr: $("fb-addr").checked, screenshot,
    });
    ok.hidden = false;
    ok.textContent = p ? t("fb.ok", { path: p }) : t("fb.cancel");
  } catch (e) {
    err.textContent = String(e);
  }
  $("btn-fb").disabled = false;
  fbBusy = false;
});

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
  // 单次发行 = 这把钥匙下一笔实际会结算的量（v3 起按自己的节奏窗口算），
  // 不是 reward_per_block × 50；后端已按硬顶截断，这里只做防御性再截一次。
  const oneClaim = BigInt(cs.claim_reward || 0);
  const remain = BigInt(cs.hard_cap) - BigInt(cs.total_minted);
  $("stat-reward").textContent = `${fmtAmt(oneClaim < remain ? oneClaim : (remain > 0n ? remain : 0n))} QLX`;
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

// 16 种物件的代表色直接取自同一张像素图：地图上的点、物品栏图标、世界里
// 贴图不可能再"看起来是另一个东西"。
const KIND_COLORS = (() => {
  const out = [];
  for (let i = 0; i < 16; i++) {
    const [r, g, b] = QLSprites.avgColor(i);
    out.push(`rgb(${r},${g},${b})`);
  }
  return out;
})();
const KIND_ICON = (i, size = 32) => QLSprites.dataUrl(Math.max(0, Math.min(15, i | 0)), size);
const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

const wcvs = $("world-canvas");
const wctx = wcvs.getContext("2d");
let worldSnap = null;
let terrain = { sig: "", rgb: null, era1: 0, ents: 0, fp: "", heights: null, blocks: null };
let wSlot = 0;
let wSel = ""; // 选中实体 id（空 = 未选中）
let wProof = { id: "", json: "" }; // 展开的可验证记录（2 秒轮询重渲染时不能丢）
let w3 = null; // 第一人称体素编辑器（惰性创建）
let w3three = null; // Three.js 视觉层；编辑与账本判定继续使用 w3
let w3VeilDone = false; // 封面已淡入真实画面（每次进世界重置）

const worldActive = () => !$("page-world").classList.contains("hidden");
// 旧世界账本是"解释"不是"故障"：写入被重放拒掉时不要甩一行红色原始报错，
// 直接把出口说清楚——红字会让人以为是新写坏了，其实账本一条都没动。
const worldErr = (m) => {
  const el = $("err-world");
  const s = worldSnap;
  const foreign = !!(s && s.ledger_foreign && s.report && s.report.reason);
  const replayRefused = foreign && String(m).includes(s.report.reason);
  el.className = replayRefused ? "muted small" : "error";
  el.textContent = replayRefused ? t("world.fix.banner") : m;
  $("ok-world").hidden = true;
};
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

// —— 光尘（QL-DUST-v1）：采来的自然物是唯一来源，采集记录才进账本
let dustSnap = null;
let worldRes = [];        // world_resources 的全图结果（含 taken）
let worldResSig = "";     // 只在光尘总量/轮次/昼夜变了才重扫
let dustBusy = false;

// —— 旅人摊位（QL-TRADER-v1）：只卖情报，光尘付给没有私钥的烧毁地址
let traderSnap = null;    // 这一程在哪、卖什么、你有多少光尘
let traderQuote = null;   // 刚买到的那件情报（2 秒轮询重渲染时不丢）
let traderQuoteLang = ""; // 上次渲染用的语言：只在切语言时重刷，不打断阅读
let traderBusy = false;

/** 该格上能不能采：资源点由种子算出，taken 来自整本光尘账本的重放 */
const resAtCell = (x, y) => worldRes.find((n) => n.x === x && n.y === y && !n.taken) || null;

async function loadDust() {
  const d = await invoke("world_dust_state").catch(() => null);
  if (!d) return;
  dustSnap = d;
  renderDust();
  // 采集状态的变化（自己采的、邻居采的）都会推动 minted_total 或轮次，签名因此够用
  const sig = `${d.epoch}|${d.night}|${d.minted_total}`;
  if (sig !== worldResSig) {
    worldResSig = sig;
    worldRes = await invoke("world_resources", { x0: 0, y0: 0, x1: QLWorld.W, y1: QLWorld.H }).catch(() => []);
    paintWorld();
    if (w3) w3.setResources(worldRes);
  }
}

function renderDust() {
  const d = dustSnap;
  if (!d) return;
  $("dust-balance").textContent = fmtDust(d.balance);
  $("dust-meta").textContent = t("dust.meta", {
    minted: fmtDust(d.minted_total), h: d.height, epoch: d.epoch + 1, n: d.epoch_blocks,
  });
  const pct = d.quota ? (d.harvested_this_epoch / d.quota) * 100 : 0;
  $("dust-quota-bar").style.width = `${Math.min(100, pct)}%`;
  $("dust-quota-line").textContent = t("dust.quota", {
    used: d.harvested_this_epoch, q: d.quota, left: Math.max(0, d.epoch_end - d.height),
  });
  const night = $("dust-night-line");
  night.textContent = d.night
    ? t("dust.night", { half: d.night_multiplier_permille / 1000 })
    : t("dust.day");
  night.classList.toggle("gold", !!d.night);

  const box = $("dust-recent");
  if (!d.recent.length) {
    box.innerHTML = `<div class="muted small">${t("dust.recent.empty")}</div>`;
    return;
  }
  const mine = d.address;
  const sink = traderSnap ? traderSnap.sink : "";
  box.innerHTML = d.recent
    .slice()
    .reverse()
    .map((r) => {
      const out = r.op === "send";
      const toTrader = out && sink && r.to === sink;
      const who = out
        ? (r.owner === mine
          ? (toTrader ? t("dust.row.trader") : t("dust.row.toself", { to: short(r.to || "") }))
          : t("dust.row.from", { from: short(r.owner) }))
        : t("dust.row.harvest", { kind: t(`world.kind.${r.kind}`) });
      const amt = out
        ? (r.owner === mine ? `−${fmtDust(r.amount)}` : `+${fmtDust(r.amount)}`)
        : `+${fmtDust(r.amount)}`;
      return `<div class="ent-row">
        <div class="er-main">
          <div class="er-n">${who}${r.op === "harvest" ? ` <span class="muted small">(${r.x}, ${r.y})</span>` : ""}</div>
          <div class="er-m">#${r.height}${r.op === "harvest" ? ` · ${t("dust.row.epoch", { n: r.epoch + 1 })}` : ""}</div>
        </div>
        <div class="er-o ${out && r.owner === mine ? "minus" : "plus"}">${amt}</div>
      </div>`;
    })
    .join("");
}

/** 采一格：种类与产量都由后端按坐标和链头重算，前端只提交坐标 */
async function harvestNode(x, y) {
  if (dustBusy) return;
  if (!worldSnap || !worldSnap.unlocked) { worldErr(t("world.err.locked")); return; }
  dustBusy = true;
  try {
    const r = await invoke("world_harvest", { x, y });
    QLSfx.chop();
    if (w3) w3.burst(x, surfaceLayer(x, y), y, r.kind);
    worldOk(t("dust.ok.harvest", {
      kind: t(`world.kind.${r.kind}`), n: fmtDust(r.amount), used: r.harvested_this_epoch, q: r.quota,
    }));
    await loadWorld();
  } catch (e) {
    QLSfx.deny();
    worldErr(String(e));
  }
  dustBusy = false;
}

// —— 旅人摊位：情报全由后端按链头算，前端只负责摆出来
async function loadTrader() {
  const s = await invoke("world_trader_state").catch(() => null);
  if (!s) return;
  traderSnap = s;
  renderTrader();
  renderDust(); // 摊位的烧毁地址到手后，"付给旅人"那几行才认得出来
  paintWorld(); // 摊子只在这一程内动地方，重画一次就够
}

function renderTrader() {
  const s = traderSnap;
  if (!s) return;
  $("trader-meta").textContent = t("trader.meta", {
    p: s.period + 1, x: s.x, z: s.y, left: Math.max(0, s.leaves_at - s.height),
  });
  const bal = dustSnap ? dustSnap.balance : s.balance;
  $("trader-offers").innerHTML = s.offers
    .map((o) => {
      const afford = bal >= o.price;
      return `<button class="trader-offer" type="button" data-id="${o.id}"${afford ? "" : " disabled"}>
        <div class="er-main">
          <div class="er-n">${esc(t(`trader.offer.${o.id}`))}</div>
          <div class="er-m">${esc(t(`trader.offer.${o.id}.d`))}</div>
        </div>
        <div class="to-price">${o.price} ✦</div>
      </button>`;
    })
    .join("");
  // 只在切换语言后重绘一次：每 2 秒重刷会把正在读的人手里的选区冲掉
  if (traderQuote && traderQuoteLang !== document.documentElement.lang) renderQuote(traderQuote);
}

/** 把一件情报讲成几行：路线给站，冷清之地给区域，账本给排行 */
function renderQuote(q) {
  const box = $("trader-result");
  let html = `<div class="tr-title">${esc(t(`trader.offer.${q.id}`))} · ${esc(t("trader.paid", { n: fmtDust(q.price) }))}</div>`;
  if (q.id === "route") {
    html += `<div class="ts-line">${esc(t("trader.route.total", { n: q.count, r: fmtDust(q.total) }))}</div>`;
    html += q.stops
      .map((s, i) => `<div class="ts-line">${i + 1}. <b>X ${s.x} · Z ${s.y}</b> · ${esc(t(`world.kind.${s.kind}`))} +${fmtDust(s.reward)} <span class="muted">(${esc(t("trader.steps", { n: s.steps }))})</span></div>`)
      .join("");
  } else if (q.id === "frontier") {
    const a = q.area || { x: 0, y: 0, size: 32 };
    html += `<div class="ts-line">${esc(t("trader.area.line", {
      x: a.x, z: a.y, x2: a.x + a.size - 1, z2: a.y + a.size - 1, n: q.count, r: fmtDust(q.total),
    }))}</div>`;
  } else {
    html += `<div class="ts-line">${esc(t("trader.ledger.line", { people: q.count, n: q.total }))}</div>`;
    html += q.top.map((h) => `<div class="ts-line">${esc(short(h.addr))} · ${h.count}</div>`).join("");
  }
  html += `<div class="ts-line muted">${esc(t("trader.world.line", { n: q.world_nodes, r: fmtDust(q.world_reward) }))}</div>`;
  if (q.night) html += `<div class="ts-line">${esc(t("trader.night"))}</div>`;
  box.innerHTML = html;
  box.hidden = false;
  traderQuoteLang = document.documentElement.lang;
}

async function buyInfo(id) {
  if (traderBusy) return;
  $("err-trader").textContent = "";
  if (!worldSnap || !worldSnap.unlocked) { $("err-trader").textContent = t("world.err.locked"); return; }
  traderBusy = true;
  try {
    const p = await invoke("world_trader_buy", { id });
    QLSfx.claim();
    traderQuote = p.quote;
    renderQuote(p.quote);
    await loadWorld();
  } catch (e) {
    QLSfx.deny();
    $("err-trader").textContent = String(e);
  }
  traderBusy = false;
}

/** 地图上点到摊子：把面板拉到眼前并闪一下 */
function focusTrader() {
  const el = $("panel-trader");
  el.scrollIntoView({ behavior: "smooth", block: "nearest" });
  el.classList.remove("flash");
  void el.offsetWidth;
  el.classList.add("flash");
}

$("trader-offers").addEventListener("click", (e) => {
  const b = e.target.closest(".trader-offer");
  if (b) buyInfo(b.dataset.id);
});

// —— 兑换池（QL-POOL-v1）：曲线在共识里，前端只提议方向与数量；报价是同一公式的只读预演
let poolSnap = null;

function qlxUnits(s) {
  const v = String(s).trim();
  if (!/^\d+(\.\d{0,9})?$/.test(v)) return null;
  const [i, f = ""] = v.split(".");
  return BigInt(i) * 1000000000n + BigInt((f + "000000000").slice(0, 9));
}

/** 与 pool.rs::quote 同一算式（u128 在这里用 BigInt）：只用来显示"约得多少" */
function poolPreview() {
  const p = poolSnap;
  if (!p || !p.active || !p.ready) return null;
  const dir = $("pool-dir").value === "dust" ? "dust" : "qlx";
  const raw = $("pool-amount").value.trim();
  if (!raw) return null;
  let amt;
  if (dir === "qlx") {
    amt = qlxUnits(raw);
    if (!amt || amt <= 0n) return null;
  } else {
    const n = Number(raw);
    if (!Number.isFinite(n) || n <= 0) return null;
    amt = BigInt(Math.round(n));
  }
  const inAfter = (amt * 9970n) / 10000n;
  if (inAfter <= 0n) return null;
  const [resIn, resOut] = dir === "qlx"
    ? [BigInt(Math.round(p.qlx)), BigInt(Math.round(p.dust))]
    : [BigInt(Math.round(p.dust)), BigInt(Math.round(p.qlx))];
  const out = (resOut * inAfter) / (resIn + inAfter);
  return out > 0n ? { dir, out } : null;
}

function renderPoolQuote() {
  const p = poolSnap;
  if (!p) return;
  if (!p.active) { $("pool-quote").textContent = t("pool.inactive", { h: p.activation_height, now: p.height }); return; }
  if (!p.ready) { $("pool-quote").textContent = t("pool.notready"); return; }
  const q = poolPreview();
  if (!q) { $("pool-quote").textContent = ""; return; }
  $("pool-quote").textContent = q.dir === "qlx"
    ? t("pool.quote.qlx", { out: fmtDust(q.out) })
    : t("pool.quote.dust", { out: fmtAmt(q.out) });
}

function renderPool() {
  const p = poolSnap;
  if (!p) return;
  $("pool-price").textContent = p.price ? Number(p.price).toFixed(6) : "–";
  $("pool-meta").textContent = t("pool.meta", {
    h: p.activation_height,
    bps: (p.fee_bps / 100).toFixed(2).replace(/\.?0+$/, ""),
  });
  $("pool-reserves").textContent = t("pool.reserves", { q: fmtAmt(p.qlx), d: fmtDust(p.dust) });
  $("pool-mine").textContent = t(p.my_lp > 0 ? "pool.mine.locked" : "pool.mine", {
    q: fmtAmt(p.my_qlx), d: fmtDust(p.my_dust), lp: fmtDust(p.my_lp), locked: fmtDust(p.locked_lp),
  });
  const off = !p.active || !p.unlocked;
  for (const id of ["btn-pool-swap", "btn-pool-add", "btn-pool-remove"]) $(id).disabled = off;
  renderPoolQuote();
}

async function loadPool() {
  try { poolSnap = await invoke("pool_state"); } catch { return; }
  renderPool();
}

let poolBusy = false;
async function poolAct(op) {
  if (poolBusy || !poolSnap) return;
  const err = $("err-pool"), ok = $("ok-pool");
  err.textContent = "";
  ok.hidden = true;
  try {
    let r;
    if (op === "swap") {
      const amount = $("pool-amount").value.trim();
      if (!(Number(amount) > 0)) throw t("pool.err.amount");
      // 滑点保护交给链上重算：min_out=0 是明示的"接受任意成交价"，不假装有保护
      r = await invoke("pool_swap", { dir: $("pool-dir").value, amount, minOut: 0 });
    } else if (op === "add") {
      const qlx = $("pool-qlx").value.trim(), dust = Math.round(Number($("pool-dust").value));
      if (!(Number(qlx) > 0) || !(dust > 0)) throw t("pool.err.amount");
      r = await invoke("pool_add", { qlx, dust });
    } else {
      const lp = poolSnap.my_lp || 0;
      if (lp <= 0) throw t("pool.err.balance");
      if (!confirm(t("pool.confirm.remove"))) return;
      r = await invoke("pool_remove", { lp });
    }
    QLSfx.claim();
    ok.hidden = false;
    ok.textContent = t("pool.ok", {
      op: t(`pool.op.${op}`), h: r.height, hash: String(r.tx_hash || "").slice(0, 12),
    });
    $("pool-amount").value = "";
    $("pool-qlx").value = "";
    $("pool-dust").value = "";
    await loadWorld();
  } catch (e) {
    QLSfx.deny();
    err.textContent = String(e);
  }
}
$("btn-pool-swap").addEventListener("click", () => poolAct("swap"));
$("btn-pool-add").addEventListener("click", () => poolAct("add"));
$("btn-pool-remove").addEventListener("click", () => poolAct("remove"));
$("pool-amount").addEventListener("input", renderPoolQuote);
$("pool-dir").addEventListener("change", renderPoolQuote);

const KIND_CANVAS = [];
function kindCanvas(kind) {
  const k = Math.max(0, Math.min(15, kind | 0));
  if (!KIND_CANVAS[k]) KIND_CANVAS[k] = QLSprites.icon(k, 16);
  return KIND_CANVAS[k];
}
const kdIcon = (kind, released) =>
  released
    ? `<i class="kd" style="background:#9AA7BD"></i>`
    : `<i class="kd" style="background-image:url('${KIND_ICON(kind, 32)}')"></i>`;

function paintWorld() {
  if (!worldSnap) return;
  const s = worldSnap;
  const tr = buildTerrain(s);
  QLWorld.paint(wctx, tr.rgb);
  wctx.imageSmoothingEnabled = false;

  // 可采点：石头灰点、树金点、花红点，各占一个像素。
  // 资源点约占全图 2.4%，画成 2×2 会把地形糊成一身麻点——1px 才看得出地貌。
  for (const n of worldRes) {
    if (n.taken) continue;
    wctx.fillStyle = n.kind === 1 ? "#E3B54A" : n.kind === 0 ? "#C7CDD8" : "#E8604C";
    wctx.fillRect(n.x, n.y, 1, 1);
  }

  for (const e of s.entities) {
    if (e.released) continue; // 断租不删除，但也不再显形/占格（与 world.rs live_at 同判据）
    // 地图上的物件不再是 3×3 色点：画同一张 16×16 像素图，俯视图和走进去看到的是同一个东西
    wctx.drawImage(kindCanvas(e.kind), e.x - 5, e.y - 5, 11, 11);
    if (s.address && e.owner === s.address) {
      wctx.strokeStyle = "#E3B54A";
      wctx.strokeRect(e.x - 5.5, e.y - 5.5, 12, 12);
    }
  }
  // 旅人摊位：金框黑心的小方块，和实体区分开（他不是方块，也拆不掉）
  if (traderSnap) {
    const { x, y } = traderSnap;
    wctx.fillStyle = "#12161F";
    wctx.fillRect(x - 4, y - 4, 9, 9);
    wctx.fillStyle = "#E3B54A";
    wctx.fillRect(x - 3, y - 3, 7, 7);
    wctx.fillStyle = "#12161F";
    wctx.fillRect(x - 1, y - 1, 3, 3);
  }

  const sel = entById(wSel);
  if (sel) {
    wctx.strokeStyle = "#FFFFFF";
    wctx.strokeRect(sel.x - 8.5, sel.y - 8.5, 17, 17);
    wctx.strokeStyle = "#12161F";
    wctx.strokeRect(sel.x - 9.5, sel.y - 9.5, 19, 19);
  }
}

function buildHotbar() {
  const n = worldSnap ? worldSnap.slot_count : 16;
  const bar = $("world-hotbar");
  bar.innerHTML = "";
  for (let i = 0; i < n; i++) {
    const b = document.createElement("button");
    b.className = "hs" + (i === wSlot ? " active" : "");
    b.dataset.slot = String(i);
    b.type = "button";
    b.title = `${i + 1} · ${t(`world.kind.${i}`)}`;
    b.innerHTML = `<i class="kd" style="background-image:url('${KIND_ICON(i, 32)}')"></i><b class="hs-n">${esc(t(`world.kind.${i}`))}</b><em class="hs-k">${i + 1}</em>`;
    bar.appendChild(b);
  }
  $("world-slot-tag").textContent = `slot ${wSlot + 1} · ${t(`world.kind.${wSlot}`)}`;
  renderHeld();
}

/** 第一人称右下角的手持物：图标 + 名字 + 数字键 */
function renderHeld() {
  const el = $("w3d-held");
  if (!el) return;
  el.innerHTML = `<img src="${KIND_ICON(wSlot, 64)}" alt=""><div><div>${esc(t(`world.kind.${wSlot}`))}</div><div class="wh-k">${wSlot + 1} / 16</div></div>`;
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
    [
      t("world.status.pace"),
      `${t("world.status.pace.line", {
        n: s.mine_claims_by_me || 0,
        f: s.mine_floor || s.mine_cooldown_blocks,
        nf: s.mine_next_floor || s.mine_cooldown_blocks,
      })} · ${t("world.status.craft", {
        cells: (s.craft || {}).cells || 0,
        kept: (s.craft || {}).kept_blocks || 0,
        score: (s.craft || {}).score || 0,
      })}`,
    ],
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
        ${kdIcon(e.kind, e.released)}
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
    <div class="es-name">${kdIcon(e.kind, e.released)}${esc(e.name)}</div>
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
  renderStatus();
  paintWorld();
  syncWorld3D();
  // 3D 覆盖层开着时切语言，物品栏与提示也要立刻换文字
  if (!$("w3d").hidden) { buildW3Hotbar(); w3Hint(); }
}

/** 免费建造窗口 + 挖矿结算：把"现在能不能写、下一笔币什么时候到"讲清楚 */
function renderStatus() {
  const s = worldSnap;
  if (!s) return;
  const used = s.my_writes_in_window || 0;
  const max = s.free_quota_max || 1;
  $("pending-count").textContent = `${used} / ${max}`;
  const pct = Math.min(100, (used / max) * 100);
  const ready = (s.next_claim_height || 0) <= s.height + 1;
  const mine = ready
    ? `<span class="ok-inline">${t("world.status.mine.ready")}</span> · +${fmtAmt(s.mine_reward)} QLX`
    : t("world.status.mine.wait", { h: s.next_claim_height, left: Math.max(0, s.next_claim_height - s.height) });
  $("world-fee-line").textContent = t("world.status.free", { max, win: s.free_quota_window });
  $("world-ore-note").textContent = t("world.status.ore", { n: s.ore_candidates, q: s.ore_quota, m: s.owned_by_me });
  // v3：创造换的是节奏，不是更多的币。阶梯只看这把钥匙落下过几格矿。
  const cr = s.craft || {};
  const pace = t("world.status.pace.line", {
    n: s.mine_claims_by_me || 0,
    f: s.mine_floor || s.mine_cooldown_blocks,
    nf: s.mine_next_floor || s.mine_cooldown_blocks,
  });
  const weight = t("world.status.craft", {
    cells: cr.cells || 0,
    kept: cr.kept_blocks || 0,
    score: cr.score || 0,
  });
  // 坏行只跳过它自己那一行，但界面上必须说得出跳了几行：静默少读几行等于
  // 本机悄悄拿另一本账去参与共识（两本的条数都会进块承诺）。
  const badEnt = s.entity_bad_lines || 0;
  const badDust = s.dust_bad_lines || 0;
  const badLine = badEnt + badDust
    ? `<div class="ent-row"><div class="er-main">
      <div class="er-n bad-inline">${t("world.status.ledgerbad", { ent: badEnt, dust: badDust })}</div>
    </div></div>`
    : "";
  $("world-pending").innerHTML = `
    <div class="ent-row"><div class="er-main">
      <div class="er-n">${t("world.status.quota", { used, max, win: s.free_quota_window })}</div>
      <div class="quota-bar"><i style="width:${pct}%"></i></div>
    </div></div>
    <div class="ent-row"><div class="er-main">
      <div class="er-n">${t("world.status.mine")}</div>
      <div class="er-m">${mine}</div>
    </div></div>
    <div class="ent-row"><div class="er-main">
      <div class="er-n">${t("world.status.pace")}</div>
      <div class="er-m">${pace}</div>
      <div class="er-m">${weight}</div>
    </div></div>${badLine}`;
  renderWorldFix();
}

// 旧世界账本（链被重置过）：旧账在新链上没有一条能通过重放，写入会被拒。
// 出口只有归档——改名保留、不删除——而且必须由玩家自己按下去，不自动替他决定。
function renderWorldFix() {
  const s = worldSnap;
  const show = !!(s && s.report && !s.report.ok && s.ledger_foreign);
  for (const id of ["btn-world-archive", "world-fix-note"]) {
    const el = $(id);
    if (el) el.hidden = !show;
  }
}

$("btn-world-archive").addEventListener("click", async () => {
  if (!worldSnap || !worldSnap.ledger_foreign) return;
  if (!confirm(t("world.fix.confirm"))) return;
  try {
    const r = await invoke("world_archive_ledger");
    await loadWorld();
    worldOk(t("world.fix.done", { files: (r.files || []).join(" · ") }));
  } catch (e) {
    worldErr(String(e));
  }
});

async function loadWorld() {
  worldSnap = await invoke("world_snapshot");
  renderWorld();
  // 3D 视图开着时必须同步实体表：写完的方块只进了 2D 面板的话，玩家会看到
  // 「坐标回执 + 一击火花」但币面上什么都没有，要关了重开才显形。
  if (w3) w3.setEntities(worldSnap.entities, worldSnap.address);
  // 挖地账本（v5）与实体表同一条纪律：快照每 2 秒推一次，挖过的坑必须在币面上
  // 当场变成坑，而不是等关了 3D 视图再开才看见。
  if (w3) w3.setDug(worldSnap.dug || []);
  renderGuide();
  await loadDust();
  await loadTrader();
  await loadPool();
}

/** 免费写入的即时回执：签名即落账，不存在"等一个块"的第二阶段 */
function paidMsg(p) {
  return t("world.ok.written", { op: t(`world.op.${p.op}`), name: p.name });
}

/** 挖矿回执：世界里的方块立刻落了，币要等下一个块收下发行凭据 */
function mineMsg(p) {
  return t("world.ok.mine", {
    x: p.x, z: p.y, y: p.layer,
    reward: fmtAmt(p.reward), h: p.claim_height,
    f: p.pace_floor, nf: p.pace_next_floor,
  });
}

// 刚落下、还没进块的那一格矿：出块动画要知道砖该压在哪格上
let lastMinedCell = null;

/** 挖到矿的现场反馈：矿格上方起一串 QLOR 符文，等下一个块把它铸成砖 */
function noteMinedCell(p) {
  lastMinedCell = { cx: p.x, cz: p.y, layer: p.layer, at: Date.now() };
  if (w3) w3.rainAt(p.x, p.y, p.layer, `QLOR|${p.x}|${p.y}|${p.layer}`);
}

// —— 交互：物品栏、地图、编辑器

$("world-hotbar").addEventListener("click", (e) => {
  const b = e.target.closest(".hs");
  if (!b) return;
  wSlot = Number(b.dataset.slot);
  buildHotbar();
  if (w3) w3.setSlot(wSlot);
});

document.addEventListener("keydown", (e) => {
  if (e.metaKey || e.ctrlKey || e.altKey) return; // 不抢系统/浏览器快捷键
  if (!worldActive() || $("view-main").classList.contains("hidden")) return;
  const tag = (e.target.tagName || "").toLowerCase();
  if (tag === "input" || tag === "select" || tag === "textarea") return;
  if (!(e.key >= "0" && e.key <= "9" && e.key.length === 1)) return;
  const total = worldSnap ? worldSnap.slot_count : 16;
  const idx = e.key === "0" ? 9 : Number(e.key) - 1;
  if (idx < total) {
    wSlot = idx;
    buildHotbar();
    if (w3) w3.setSlot(wSlot);
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
  const node = resAtCell(c.x, c.y);
  const onTrader = traderSnap && Math.abs(c.x - traderSnap.x) <= 3 && Math.abs(c.y - traderSnap.y) <= 3;
  el.textContent =
    `X ${c.x} · Z ${c.y} · Y ${surfaceLayer(c.x, c.y)}` +
    (onTrader ? ` · ✦ ${t("trader.map.tag")}` : "") +
    (node ? ` · ✦ ${t(`world.kind.${node.kind}`)} +${fmtDust(node.reward)}` : "") +
    (ch ? ` · chunk ${cx},${cy}` : "") +
    (ch ? ` · ${t("world.f3.ch", { n: ch.count })} ${ch.root_hex.slice(0, 8)}…` : "");
});

wcvs.addEventListener("mouseleave", () => ($("world-cursor").textContent = "X – · Z –"));

// 右键 = 采这一格。左键留给"选实体 / 填坐标"，采伐不会把填坐标的手感抢走。
wcvs.addEventListener("contextmenu", (ev) => {
  ev.preventDefault();
  const c = toCell(ev);
  if (!c || !worldSnap) return;
  const n = resAtCell(c.x, c.y);
  if (!n) {
    worldErr(t("dust.err.nonode", { x: c.x, z: c.y }));
    QLSfx.deny();
    return;
  }
  harvestNode(c.x, c.y);
});

wcvs.addEventListener("click", (ev) => {
  const c = toCell(ev);
  if (!c || !worldSnap) return;
  if (traderSnap && Math.abs(c.x - traderSnap.x) <= 3 && Math.abs(c.y - traderSnap.y) <= 3) {
    focusTrader();
    return;
  }
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
    const x = Number($("ent-x").value), y = Number($("ent-z").value), layer = Number($("ent-layer").value);
    const p = await invoke("world_place", {
      name,
      x,
      y,
      layer,
      kind: wSlot,
      parentHex: $("ent-parent").value,
    });
    $("ent-name").value = "";
    QLSfx.place();
    if (w3) w3.burst(x, layer, y, wSlot);
    worldOk(paidMsg(p));
    await loadWorld();
  } catch (e) {
    worldErr(String(e));
  }
  $("btn-place").disabled = false;
});

$("btn-mine-ore").addEventListener("click", async () => {
  worldErr("");
  if (!worldSnap || !worldSnap.unlocked) { worldErr(t("world.err.locked")); return; }
  $("btn-mine-ore").disabled = true;
  try {
    const p = await invoke("world_mine_once", { kind: wSlot });
    QLSfx.claim();
    if (w3) w3.burst(p.x, p.layer, p.y, wSlot);
    noteMinedCell(p);
    worldOk(mineMsg(p));
    await loadWorld();
  } catch (e) {
    worldErr(String(e));
  }
  $("btn-mine-ore").disabled = false;
});

$("ent-list").addEventListener("click", (e) => {
  const row = e.target.closest(".ent-row");
  if (row) selectEnt(row.dataset.id);
});

// 💸 转光尘：收款地址与余额都由重放裁判，客户端只能提议
$("btn-dust-send").addEventListener("click", async () => {
  $("err-dust").textContent = "";
  $("ok-dust").hidden = true;
  const to = $("dust-to").value.trim();
  const amount = Number($("dust-amount").value);
  if (!to.startsWith("ql1")) { $("err-dust").textContent = t("send.err.addr"); return; }
  if (!Number.isFinite(amount) || amount <= 0) { $("err-dust").textContent = t("dust.err.amount"); return; }
  const bal = dustSnap ? dustSnap.balance : 0;
  if (amount > bal) { $("err-dust").textContent = t("dust.err.balance", { b: fmtDust(bal) }); return; }
  $("btn-dust-send").disabled = true;
  try {
    const rec = await invoke("world_dust_send", { to, amount: Math.round(amount) });
    QLSfx.claim();
    $("dust-amount").value = "";
    const ok = $("ok-dust");
    ok.hidden = false;
    ok.textContent = t("dust.ok.send", { n: fmtDust(rec.amount), to: short(rec.to) });
    setTimeout(() => (ok.hidden = true), 6000);
    await loadWorld();
  } catch (e) {
    $("err-dust").textContent = String(e);
  }
  $("btn-dust-send").disabled = false;
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
      const p = await invoke("world_rename", { entityIdHex: wSel, name });
      worldOk(paidMsg(p));
    } else if (act === "move") {
      const x = Number($("ent-x").value), y = Number($("ent-z").value), layer = Number($("ent-layer").value);
      const p = await invoke("world_move", { entityIdHex: wSel, x, y, layer });
      worldOk(paidMsg(p));
    } else if (act === "release") {
      if (!confirm(t("world.confirm.release", { name: e0.name }))) return;
      const p = await invoke("world_release", { entityIdHex: wSel });
      worldOk(paidMsg(p));
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

/** 出生点：不是"最近的一块陆地"，而是附近视野最好的一块干地。
 *  第一眼决定别人会不会觉得被骗——站在低洼水边只能看见一堵黑墙，
 *  站到高一点、前方有落差的位置，才有封面里那种峡谷与远山的构图。 */
function pickSpawn(x, z) {
  const t = buildTerrain(worldSnap);
  if (!t.blocks || !t.heights) return { x: 128, z: 128 };
  const W = QLWorld.W, H = QLWorld.H, seaLv = QLWorld.RULES.SEA >> 3;
  const lvl = (cx, cz) => t.heights[cz * W + cx] >> 3;
  const kindAt = (cx, cz) => t.blocks[cz * W + cx] & 7;
  const water = (cx, cz) => kindAt(cx, cz) === 0;
  const tree = (cx, cz) => (t.blocks[cz * W + cx] & 8) !== 0;
  const at = (cx, cz) => [Math.min(W - 1, Math.max(0, cx)), Math.min(H - 1, Math.max(0, cz))];
  const inside = (cx, cz) => cx > 6 && cz > 6 && cx < W - 7 && cz < H - 7;
  let best = null, bestScore = -1e9;
  for (let dz = -34; dz <= 34; dz += 2) for (let dx = -34; dx <= 34; dx += 2) {
    const cx = Math.min(W - 7, Math.max(6, x + dx)), cz = Math.min(H - 7, Math.max(6, z + dz));
    if (!inside(cx, cz) || water(cx, cz) || kindAt(cx, cz) === 2) continue; // 雪峰只长云杉、露出白石，出生在那种地方第一眼是堵墙
    const lv = lvl(cx, cz);
    if (lv < seaLv + 2) continue; // 水边低地只能看见一堵黑墙，站在岸上才看得下去
    // 出生点不能落在树里：两格内有树干 = 睁眼就是一根柱子，树冠还会糊住整个上半屏
    let woods = 0, buried = false;
    for (let tz = -4; tz <= 4 && !buried; tz++) for (let tx = -4; tx <= 4; tx++) {
      const nx = cx + tx, nz = cz + tz;
      if (nx < 0 || nz < 0 || nx >= W || nz >= H || !tree(nx, nz)) continue;
      if (Math.abs(tx) <= 2 && Math.abs(tz) <= 2) { buried = true; break; }
      woods += 1;
    }
    if (buried) continue;
    // 站的地方四周不能比脚下高：贴脸一堵坡，视野就没了（雪峰那种白石墙最糟）
    let walls = 0;
    for (let a = 0; a < 8; a++) {
      const [nx, nz] = at(cx + Math.round(Math.cos((a * Math.PI) / 4) * 2), cz + Math.round(Math.sin((a * Math.PI) / 4) * 2));
      walls += Math.max(0, lvl(nx, nz) - lv);
    }
    if (walls > 0) continue;
    // 看八个方向：近处（5 格）和远处（12 格）各取一次落差——近处要有边才看得下去，
    // 远处要有景才不空。看得到水面再加分。
    let vista = 0, near = 0, face = { x: 0, z: 1 }, faceScore = -1e9;
    for (let a = 0; a < 8; a++) {
      const ux = Math.cos((a * Math.PI) / 4), uz = Math.sin((a * Math.PI) / 4);
      const [n5x, n5z] = at(cx + Math.round(ux * 5), cz + Math.round(uz * 5));
      const [n12x, n12z] = at(cx + Math.round(ux * 12), cz + Math.round(uz * 12));
      const drop5 = Math.max(0, lv - lvl(n5x, n5z));
      const drop12 = Math.max(0, lv - lvl(n12x, n12z));
      vista += drop12;
      if (water(n12x, n12z)) near += 1;
      const s2 = drop5 * 1.6 + drop12 + (water(n12x, n12z) ? 2.4 : 0) + (water(n5x, n5z) ? 1.2 : 0);
      if (s2 > faceScore) { faceScore = s2; face = { x: ux, z: uz }; }
    }
    const gentle = kindAt(cx, cz) === 5 ? 1.5 : 0;
    const score = vista * 1.4 + near * 2.2 + (lv - seaLv) * 1.0 - Math.hypot(dx, dz) * 0.3 - woods * 2.5 + gentle;
    if (score > bestScore) {
      bestScore = score;
      best = { x: cx, z: cz, yaw: Math.atan2(face.x, face.z) };
    }
  }
  return best || { x: 128, z: 128, yaw: 0.6 };
}

function ensureWorld3D() {
  if (w3) return true;
  try {
    w3 = QLWorld3D.create($("w3d-canvas"), {
      onUnsupported: () => { w3Toast(t("world3d.unsupported"), true); closeWorld3D(); },
      onSlot: (i) => { wSlot = i; buildHotbar(); buildW3Hotbar(); if (w3) w3.setSlot(i); QLSfx.chop(); },
      onPlace: (x, z, layer) => w3Place(x, z, layer, wSlot),
      onBreak: (x, z, layer, ent) => w3Break(ent),
      // 左键对准地形 = 挖掉露出来的那一层（签名进 v5 账本，材料当场进账）
      onDig: (x, z, layer) => w3Dig(x, z, layer),
      onDigBlocked: () => w3Toast(t("world3d.err.terrain"), true),
      // 左键对准资源 = 采（打树、撬石头、摘花），不拆地
      onHarvest: (x, z) => harvestNode(x, z),
      onBlocked: () => w3Toast(t("world3d.err.blocked"), true),
      onEmpty: () => w3Toast(t("world3d.err.notarget"), true),
      onSound: (on) => w3Toast(on ? t("world3d.sound.on") : t("world3d.sound.off")),
      onLock: (on) => { $("w3d-cross").classList.toggle("locked", on); w3Hint(); },
      onFrame: (s) => {
        if (w3three) {
          try {
            w3three.render(s);
            // 第一张真画面已经画出来了，才把封面淡掉：先看到的是"同一片地方"，
            // 不是先看到一块粗糙的替身。rAF 保证中间至少有完整一帧。
            if (!w3VeilDone) {
              w3VeilDone = true;
              requestAnimationFrame(() => $("w3d-veil").classList.add("gone"));
            }
          } catch (e) {
            console.warn('Three.js rendering stopped:', e);
            $("w3d-three-canvas").hidden = true;
            $("w3d-veil").classList.add("gone");
            w3three = null;
            w3Toast(`画质层已切回基础渲染：${String(e)}`, true);
          }
        }
        const n = performance.now();
        if (n - w3HudAt > 120) { w3HudAt = n; renderW3Hud(s); }
        presenceTick(s);
      },
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
  w3.setDug(worldSnap.dug || []);
  w3.setResources(worldRes);
  w3.setSlots(worldSnap.slot_count || 16);
  w3.setTime(worldSnap.height || 0);
  w3.setSlot(wSlot);
  if (w3three) w3three.setWorld(t.heights, t.blocks, worldSnap.entities, worldSnap.dug || [], worldRes);
  renderHeld();
}

/** 一件物件的造价（土/石/矿）：物品栏角标用它，挖地之前先知道要攒什么 */
function costText(c) {
  if (!c) return "";
  const parts = [];
  for (let i = 0; i < c.length; i++) if (c[i]) parts.push(`${c[i]}${t(`world.mat.${i}`)}`);
  return parts.join(" ");
}

function buildW3Hotbar() {
  const n = worldSnap ? worldSnap.slot_count : 16;
  const bar = $("w3d-hotbar");
  bar.innerHTML = "";
  for (let i = 0; i < n; i++) {
    const b = document.createElement("button");
    b.className = "hs" + (i === wSlot ? " active" : "");
    b.dataset.slot = String(i);
    b.type = "button";
    // 只有 1–9 与 0 是真实快捷键，第 11–16 格靠滚轮，别在角上标一个按不出的数字
    const keyLab = i < 9 ? String(i + 1) : i === 9 ? "0" : "";
    const cost = costText(worldSnap && worldSnap.build_costs && worldSnap.build_costs[i]);
    b.title = [keyLab, t(`world.kind.${i}`), cost].filter(Boolean).join(" · ");
    b.innerHTML = `<i class="kd" style="background-image:url('${KIND_ICON(i, 32)}')"></i><b class="hs-n">${esc(t(`world.kind.${i}`))}</b><em class="hs-k">${keyLab}</em>`;
    bar.appendChild(b);
  }
  renderHeld();
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
  const surf = row(t("world3d.f3.surface"),
    `${s.surface} · ${t(`world3d.biome.${s.block & 7}`)}` +
    (s.dug ? ` · ${t("world3d.f3.top")} ${s.top} (−${s.dug})` : ""));
  // 材料是建造的硬通货：挖地得来、造方块花掉。摆在 HUD 上，挖之前就知道够不够
  const mats = (s0.materials || [0, 0, 0]).map((n, i) => `${n}${t(`world.mat.${i}`)}`).join(" · ");
  const matRow = row(t("world3d.f3.mats"), mats, "gold");
  const tgt = row(t("world3d.f3.target"), tg
    ? `${tg.cx}, ${tg.layer}, ${tg.cz}` +
      (tg.node ? ` · ✦ ${t(`world.kind.${tg.node.kind}`)} +${fmtDust(tg.node.reward)}` : "") +
      (tg.entity ? ` · ${esc(tg.entity.name)}` : "")
    : "—", tg && tg.node ? "gold" : tg && tg.entity && tg.entity.owner === s0.address ? "gold" : "");
  const plc = row(t("world3d.f3.place"), tg ? (tg.place ? `${tg.place.cx}, ${tg.place.layer}, ${tg.place.cz}` : t("world3d.f3.nospace")) : "—");
  const slot = row(t("world3d.f3.slot"), `${wSlot + 1} / ${s.slots || 16} · ${t(`world.kind.${wSlot}`)}`);
  const tod = s.day > 0.72 ? t("world3d.time.day") : s.dusk > 0.4 ? t("world3d.time.dusk") : t("world3d.time.night");
  const time = row(t("world3d.f3.time"), `${tod} · #${s.height}`);
  const chn = row(t("world3d.f3.chunk"), ch ? `${ch.count} · ${ch.root_hex.slice(0, 8)}…` : "0");
  const fp = row(t("world3d.f3.fp"), `<span class="mono gold">${terrain.fp}</span>`);
  const fps = row("FPS", `${s.fps}${s.locked ? "" : " · " + t("world3d.f3.unlocked")}`);
  $("w3d-f3").innerHTML = [pos, surf, matRow, tgt, plc, slot, time, chn, fp, fps].join("");
}

async function w3Place(x, z, layer, slot) {
  if (w3Busy) return;
  if (!worldSnap || !worldSnap.unlocked) { w3Toast(t("world.err.locked"), true); return; }
  w3Busy = true;
  try {
    const p = await invoke("world_place", {
      name: w3Name(slot, x, z, layer), x, y: z, layer, kind: slot, parentHex: "",
    });
    QLSfx.place();
    if (w3) w3.burst(x, layer, z, slot);
    w3Toast(paidMsg(p));
    await loadWorld();
  } catch (e) {
    w3Toast(String(e), true);
  }
  w3Busy = false;
}

async function w3Dig(x, z, layer) {
  if (w3Busy) return;
  if (!worldSnap || !worldSnap.unlocked) { w3Toast(t("world.err.locked"), true); return; }
  w3Busy = true;
  try {
    // 只报坐标：挖哪一层由链上（地表层 − 已挖层数）算，前端说"挖第几层"不算数
    const p = await invoke("world_dig", { x, y: z });
    QLSfx.break();
    if (w3) w3.digBurst(x, z, p.layer, p.kind);
    w3Toast(t("world3d.ok.dig", {
      x, z, y: p.layer, mat: t(`world.mat.${p.kind}`), top: p.top,
    }));
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
    const p = await invoke("world_release", { entityIdHex: ent.entity_id_hex });
    QLSfx.break();
    if (w3) w3.burst(ent.x, ent.layer, ent.y, ent.kind);
    w3Toast(paidMsg(p));
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
  // 每次进世界都从封面开始：封面是这片地的黄昏，淡入后落在同一个地方
  $("w3d-veil").classList.remove("gone");
  w3VeilDone = false;
  if (!w3three) {
    try {
      const { ThreeWorld } = await import('./world3d-three.js');
      w3three = new ThreeWorld($("w3d-three-canvas"));
      $("w3d-three-canvas").hidden = false;
    } catch (e) {
      $("w3d-three-canvas").hidden = true;
      w3Toast(`画质层不可用，已切回基础渲染：${String(e)}`, true);
    }
  }
  if (!w3three) $("w3d-veil").classList.add("gone");
  if (!ensureWorld3D()) { closeWorld3D(); return; }
  buildW3Hotbar();
  w3Hint();
  if (!w3Spawned) {
    const c = pickSpawn(Number($("ent-x").value) || 128, Number($("ent-z").value) || 128);
    w3.jumpTo(c.x, c.z, null);
    // 面向这块地视野最开的方向：出生第一眼就要是"景"，不是一堵山壁
    if (c.yaw != null) w3.setView(c.yaw, -0.05);
    if (w3three) w3three.setSpawn(c.x, c.z);
    w3Spawned = true;
  }
  syncWorld3D();
  w3.open();
  // 第 1 步是导航事实：进过世界就记下来（不改账本）
  if (!prefsCache.opened_world) setPref("opened_world", true).then(renderGuide);
  renderFeedback();
}

function closeWorld3D() {
  if ($("w3d").hidden && !w3) return;
  if (w3) w3.close();
  $("w3d").hidden = true;
  document.body.classList.remove("w3d-on");
  $("w3d-pos").hidden = true;
  $("w3d-share").hidden = true;
  renderFeedback();
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

// —— 在场（§9.19）：只在内存与 gossip 里流动，不进账本、不进块、不换收益。
// 位置按 POS_PUBLISH_MIN_MS 的速率地板节流；没解锁时后端会拒，静默即可。
let posAt = 0;
function presenceTick(s) {
  const now = Date.now();
  if (now - posAt < 1500) return;
  posAt = now;
  invoke("pos_publish", {
    x: s.px, y: s.py, z: s.pz, heading: (s.yaw * 180) / Math.PI,
  }).catch(() => {});
}
setInterval(async () => {
  if (!w3 || !$("w3d") || $("w3d").hidden) return;
  try {
    const r = await invoke("world_presence");
    if (w3) w3.setPeers(r.peers || []);
    const n = r.count || 0;
    const el = $("w3d-pos");
    el.hidden = false;
    el.textContent = n > 0 ? t("presence.tag", { n }) : t("presence.none");
    el.title = t("presence.note");
  } catch { /* 旧后端没有这个命令：不画人，也不报错 */ }
}, 2000);

// —— 作品分享：截图必须在这一帧里抓（缓冲不保留），抓不到就明说，不给一张透明图
let shareShot = null;
function currentShot() {
  return new Promise((resolve) => {
    if (!w3 || $("w3d").hidden) return resolve(null);
    if (w3three) return resolve(w3three.snapshot());
    let got = false;
    w3.snapshot((u) => { got = true; resolve(u); });
    setTimeout(() => { if (!got) resolve(null); }, 500);
  });
}

$("w3d-shot").addEventListener("click", async () => {
  const url = await currentShot();
  if (!url) { w3Toast(t("share.need"), true); return; }
  const s = w3.state();
  shareShot = { url, s };
  $("w3d-share-img").src = url;
  $("w3d-share-meta").textContent = t("share.meta", {
    x: s.px.toFixed(1), y: s.py.toFixed(1), z: s.pz.toFixed(1), layer: s.layer,
  });
  $("share-out").textContent = "";
  $("w3d-share").hidden = false;
});

$("btn-share-save").addEventListener("click", async () => {
  if (!shareShot) return;
  const s = shareShot.s;
  const caption = t("share.caption", {
    x: Math.floor(s.px), z: Math.floor(s.pz), layer: s.layer, h: s.height,
  });
  try {
    const p = await invoke("export_share", { imageDataUrl: shareShot.url, caption });
    $("share-out").textContent = p ? t("share.saved", { path: p }) : t("share.cancel");
  } catch (e) {
    $("share-out").textContent = String(e);
  }
});

$("btn-share-copy").addEventListener("click", async () => {
  if (!shareShot) return;
  const s = shareShot.s;
  const line = t("share.meta", {
    x: Math.floor(s.px), y: Math.floor(s.py), z: Math.floor(s.pz), layer: s.layer,
  });
  try {
    await navigator.clipboard.writeText(line);
    $("share-out").textContent = t("share.copied");
  } catch (e) {
    $("share-out").textContent = String(e);
  }
});

$("btn-share-close").addEventListener("click", () => { $("w3d-share").hidden = true; });

// ---------------------------------------------------------------- 出块事件（实时推送）

// 邻居的光尘记录合进来 = 账本变了：面板要立刻反映，不能等下一次自己动一下
listen("world_dust", () => {
  if (worldActive() && !$("view-main").classList.contains("hidden")) loadWorld().catch(() => {});
});

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
  if (w3) {
    // 只在"这一块真的发了币"且"本机刚挖过矿"时铸金砖：
    // 没发行的块不落砖，避免每 2 秒在脚下凭空掉一块、让人误以为一直在挖到矿。
    const mintedHere = Number(e.minted || 0);
    const fresh = lastMinedCell && mintedHere > 0 && Date.now() - lastMinedCell.at < 60000;
    if (fresh) {
      w3.mintBlock({
        hash: e.block_hash_hex, vrf: e.vrf_hex,
        reward: mintedHere, height: e.height, cell: lastMinedCell,
      });
      lastMinedCell = null; // 一格矿只铸一次砖
    }
  }
  if (worldSnap && worldActive()) loadWorld().catch(() => {});
});

boot().catch((e) => console.error(e));
