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

const pages = ["asset", "mine", "history", "about"];
document.querySelectorAll(".sb-item").forEach((btn) => {
  btn.addEventListener("click", () => {
    document.querySelectorAll(".sb-item").forEach((b) => b.classList.toggle("active", b === btn));
    pages.forEach((p) => $(`page-${p}`).classList.toggle("hidden", p !== btn.dataset.page));
    // 切页即刷新：活动列表/余额立刻反映最新账本
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

  // 活动页（明细版：对方地址可点复制 / 精确时间 / 块高 / 完整哈希 / 真实手续费）
  const hist = await invoke("tx_history");
  const list = $("tx-list");
  if (!hist.length) {
    list.innerHTML = `<div class="muted small tx-empty">${t("hist.empty")}</div>`;
  } else {
    list.innerHTML = hist
      .map((x) => {
        const out = x.direction === "out";
        const cp = out ? x.to : x.from;
        return `<div class="tx-item">
          <div class="tx-ico ${out ? "out" : "in"}">${out ? "↑" : "↓"}</div>
          <div class="tx-main">
            <div class="t1">${t(out ? "hist.out" : "hist.in")} · <span class="cpy" data-copy="${cp}" title="${t("hist.copy")}">${short(cp)}</span></div>
            <div class="t2">⏱ ${ts(x.ts_unix)} · #${x.height}</div>
            <div class="t3"><span class="cpy" data-copy="${x.hash}" title="${t("hist.copy")}">${x.hash_short}</span></div>
          </div>
          <div class="tx-amt">
            <div class="a1 ${out ? "minus" : "plus"}">${out ? "−" : "+"}${fmtAmt(x.amount)} QLX</div>
            <div class="a2">${t("hist.fee", { f: fmtAmt(x.fee) })}</div>
          </div>
        </div>`;
      })
      .join("");
  }
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
    line.innerHTML = `<b>#${e.height}</b> <span class="vrf-reward">+${(e.reward / 1e9).toFixed(2)} QLX</span> · VRF ${e.vrfHex}…`;
    log.prepend(line);
    while (log.childElementCount > 50) log.lastChild.remove();
  }
});

route().catch((e) => console.error(e));
