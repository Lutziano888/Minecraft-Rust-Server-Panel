"use strict";
// Horizon Pi – Oberfläche. Reines JavaScript, kein Build-Schritt.

const $ = (sel, el = document) => el.querySelector(sel);
const $$ = (sel, el = document) => [...el.querySelectorAll(sel)];
const ESC = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ESC[c]);
const view = () => $("#view");

// ---------- Hilfen ----------
function fmtBytes(b) {
  if (b == null) return "–";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (b >= 1024 && i < u.length - 1) { b /= 1024; i++; }
  return `${b.toFixed(b >= 100 || i === 0 ? 0 : 1)} ${u[i]}`;
}
function fmtDur(s) {
  if (s == null) return "–";
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d) return `${d} T ${h} h`;
  if (h) return `${h} h ${m} min`;
  if (m) return `${m} min`;
  return `${Math.floor(s)} s`;
}
function fmtTime(sec) {
  if (!sec) return "–";
  return new Date(sec * 1000).toLocaleString("de-DE", { dateStyle: "short", timeStyle: "short" });
}
const PHASE = {
  running: "Läuft", offline: "Aus", stopped: "Spiel gestoppt", starting: "Startet…", booting: "Startet…",
  stopping: "Stoppt…", restarting: "Startet neu…",
};
const store = {
  get(k, d) { try { const v = localStorage.getItem(k); return v == null ? d : JSON.parse(v); } catch { return d; } },
  set(k, v) { try { localStorage.setItem(k, JSON.stringify(v)); } catch { /* egal */ } },
};

let toastTimer;
function toast(msg, bad = false) {
  const t = $("#toast");
  t.textContent = msg;
  t.className = "toast" + (bad ? " bad" : "");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.add("hidden"), bad ? 6000 : 3000);
}

function dialog({ title, body = "", ok = "OK", danger = false, input = null }) {
  return new Promise((resolve) => {
    const m = $("#modal");
    $("#modal-title").textContent = title;
    $("#modal-body").innerHTML = body + (input ? `<input id="modal-input" autocomplete="off" placeholder="${esc(input)}">` : "");
    const okBtn = $("#modal-ok");
    okBtn.textContent = ok;
    okBtn.className = "btn " + (danger ? "danger" : "primary");
    m.classList.remove("hidden");
    const field = $("#modal-input");
    if (field) setTimeout(() => field.focus(), 50);
    const done = (v) => { m.classList.add("hidden"); okBtn.onclick = null; $("#modal-cancel").onclick = null; resolve(v); };
    okBtn.onclick = () => done(field ? field.value : true);
    $("#modal-cancel").onclick = () => done(null);
  });
}

class ApiError extends Error {}
async function api(method, path, body) {
  const opts = { method, headers: { "X-Panel": "1" }, credentials: "same-origin" };
  if (body !== undefined) {
    opts.headers["Content-Type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  let res;
  try { res = await fetch(path, opts); } catch { throw new ApiError("Pi nicht erreichbar"); }
  let data = null;
  try { data = await res.json(); } catch { /* kein JSON */ }
  if (res.status === 401 && path !== "/api/login") { showLogin(); throw new ApiError("Bitte anmelden"); }
  if (!res.ok) throw new ApiError((data && data.error) || `Fehler ${res.status}`);
  return data;
}
async function act(promise, okMsg) {
  try {
    const r = await promise;
    toast(okMsg || (r && r.message) || "Erledigt");
    return r;
  } catch (e) { toast(e.message, true); return null; }
}

// ---------- Timer & Streams pro Ansicht ----------
let timers = [];
let stream = null;
function every(fn, ms) { fn(); timers.push(setInterval(fn, ms)); }
function cleanup() {
  timers.forEach(clearInterval);
  timers = [];
  if (stream) { stream.close(); stream = null; }
}

// ---------- Anmeldung ----------
function showLogin() {
  cleanup();
  $("#app").classList.add("hidden");
  $("#login").classList.remove("hidden");
  setTimeout(() => $("#login-password").focus(), 50);
}
$("#login-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  $("#login-error").textContent = "";
  try {
    await api("POST", "/api/login", { password: $("#login-password").value });
    $("#login-password").value = "";
    start();
  } catch (err) { $("#login-error").textContent = err.message; }
});

// ---------- Kopfzeile ----------
let piCache = null;
async function refreshTop() {
  try {
    piCache = await api("GET", "/api/system");
    const p = piCache.pi;
    const chips = [];
    if (p.temp_c != null) {
      const cls = p.temp_c >= 80 ? "very-hot" : p.temp_c >= 70 ? "hot" : "";
      chips.push(`<span class="chip ${cls}">${p.temp_c.toFixed(0)} °C</span>`);
    }
    if (p.mem_total) chips.push(`<span class="chip">RAM ${fmtBytes(p.mem_total - p.mem_available)} / ${fmtBytes(p.mem_total)}</span>`);
    $("#top-chips").innerHTML = chips.join("");
  } catch { /* still */ }
}

// ---------- Router ----------
function route() {
  cleanup();
  const parts = (location.hash.replace(/^#\/?/, "") || "").split("/");
  const nav = parts[0] === "s" ? "servers" : (parts[0] || "servers");
  $$(".tabbar a").forEach((a) => a.classList.toggle("active", a.dataset.nav === nav));
  window.scrollTo(0, 0);
  if (parts[0] === "s" && parts[1]) return renderServer(decodeURIComponent(parts[1]), parts[2] || "konsole");
  if (parts[0] === "new") return renderNew();
  if (parts[0] === "pi") return renderPi();
  return renderServers();
}
window.addEventListener("hashchange", route);

// ---------- Serverliste ----------
function serverCard(s) {
  const meta = [];
  if (s.port != null) meta.push(`Port ${esc(s.port)}`);
  if (s.config_error) meta.push(`<span class="error">server.toml fehlerhaft</span>`);
  if (s.process) {
    meta.push(`${s.players ?? 0}${s.max_players != null ? "/" + esc(s.max_players) : ""} Spieler`);
    if (s.tps != null) meta.push(`TPS ${Number(s.tps).toFixed(1)}`);
    if (s.rss_bytes != null) meta.push(fmtBytes(s.rss_bytes));
  }
  if (s.autostart) meta.push("Autostart");
  return `<a class="server" href="#/s/${encodeURIComponent(s.id)}">
    <div class="server-top"><span class="dot ${esc(s.phase)}"></span>
      <span class="server-name grow">${esc(s.id)}</span>
      <span class="badge ${esc(s.phase)}">${esc(PHASE[s.phase] || s.phase)}</span></div>
    <div class="muted">${esc(s.motd || "")}</div>
    <div class="server-meta">${meta.map((m) => `<span>${m}</span>`).join("")}</div></a>`;
}

function renderServers() {
  view().innerHTML = `<div class="card-head"><h2>Server</h2><a class="btn small primary" href="#/new">+ Neuer Server</a></div><div id="list"></div><div id="setup-hint"></div>`;
  every(async () => {
    try {
      const { servers } = await api("GET", "/api/servers");
      const list = $("#list");
      if (!list) return;
      list.innerHTML = servers.length
        ? servers.map(serverCard).join("")
        : `<div class="card empty"><h3>Noch kein Server</h3><p>Leg deinen ersten Horizon-Server an.</p><a class="btn primary" href="#/new">Server anlegen</a></div>`;
      const h = piCache && piCache.horizon;
      const hint = $("#setup-hint");
      if (hint && h) {
        const miss = [];
        if (!h.server_bin || !h.engine_bin) miss.push("Horizon ist noch nicht gebaut");
        if (!h.game_files) miss.push("die Spieldateien fehlen");
        hint.innerHTML = miss.length ? `<div class="note warn">Bevor ein Server starten kann: ${miss.join(" und ")}. → <a href="#/pi"><b>Pi &amp; Horizon</b></a></div>` : "";
      }
    } catch { /* Anzeige bleibt */ }
  }, 3000);
}

// ---------- Neuer Server ----------
function switchRow(id, label, sub, checked) {
  return `<label class="switch"><span class="txt">${label}${sub ? `<small>${sub}</small>` : ""}</span><input type="checkbox" id="${id}" ${checked ? "checked" : ""}></label>`;
}

function renderNew() {
  view().innerHTML = `
  <h2>Neuer Server</h2><p class="muted">Ein eigener Ordner mit eigener Welt und eigenem Port.</p>
  <form id="new-form" class="card stack">
    <label class="field"><span>Name (für Ordner und Liste)</span><input id="n-id" required maxlength="32" placeholder="z. B. survival" autocapitalize="off" autocomplete="off"></label>
    <label class="field"><span>Servername in der Serverliste (MOTD)</span><input id="n-motd" maxlength="120" placeholder="Mein Horizon-Server"></label>
    <div class="grid2">
      <label class="field"><span>Spielmodus</span><select id="n-mode">
        <option value="survival">Überleben</option><option value="creative">Kreativ</option>
        <option value="adventure">Abenteuer</option><option value="spectator">Zuschauer</option></select></label>
      <label class="field"><span>Schwierigkeit</span><select id="n-diff">
        <option value="peaceful">Friedlich</option><option value="easy" selected>Einfach</option>
        <option value="normal">Normal</option><option value="hard">Schwer</option></select></label>
      <label class="field"><span>Port</span><input id="n-port" type="number" min="1024" max="65535" placeholder="automatisch"></label>
      <label class="field"><span>Max. Spieler</span><input id="n-max" type="number" min="1" max="1000" value="10"></label>
    </div>
    <label class="field"><span>Seed (leer = zufällig)</span><input id="n-seed" autocomplete="off"></label>
    <label class="field"><span>Dein Spielername (wird OP und auf die Whitelist gesetzt)</span><input id="n-op" maxlength="16" autocomplete="off" autocapitalize="off"></label>
    ${switchRow("n-online", "Online-Modus", "Prüft Minecraft-Konten. Aus nur im Heimnetz/Tailscale – dann kann jeder unter jedem Namen joinen.", true)}
    ${switchRow("n-white", "Whitelist", "Nur eingetragene Spieler dürfen rein.", true)}
    ${switchRow("n-auto", "Autostart", "Startet automatisch, wenn der Pi hochfährt.", true)}
    <label class="switch"><span class="txt">Ich akzeptiere die <a href="https://aka.ms/MinecraftEULA" target="_blank" rel="noopener"><u>Minecraft-EULA</u></a><small>Pflicht für jeden Minecraft-Server.</small></span><input type="checkbox" id="n-eula"></label>
    <button class="btn primary wide" type="submit" id="n-submit">Server anlegen</button>
    <p class="error" id="n-error"></p>
  </form>`;
  $("#n-id").addEventListener("input", (e) => {
    e.target.value = e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, "-");
  });
  $("#new-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    $("#n-error").textContent = "";
    const btn = $("#n-submit");
    btn.disabled = true;
    btn.textContent = "Wird angelegt…";
    try {
      const port = $("#n-port").value ? Number($("#n-port").value) : null;
      const r = await api("POST", "/api/servers", {
        id: $("#n-id").value.trim(),
        motd: $("#n-motd").value,
        game_mode: $("#n-mode").value,
        difficulty: $("#n-diff").value,
        port,
        max_players: Number($("#n-max").value) || null,
        seed: $("#n-seed").value,
        op: $("#n-op").value.trim(),
        online_mode: $("#n-online").checked,
        whitelist: $("#n-white").checked,
        autostart: $("#n-auto").checked,
        eula: $("#n-eula").checked,
      });
      toast(`Server „${r.server.id}“ angelegt`);
      location.hash = `#/s/${encodeURIComponent(r.server.id)}/uebersicht`;
    } catch (err) {
      $("#n-error").textContent = err.message;
      btn.disabled = false;
      btn.textContent = "Server anlegen";
    }
  });
}

// ---------- Serverdetail ----------
const TABS = [["konsole", "Konsole"], ["uebersicht", "Übersicht"], ["backups", "Backups"], ["einstellungen", "Einstellungen"]];

function renderServer(id, tab) {
  const enc = encodeURIComponent(id);
  view().innerHTML = `
    <div class="detail-head"><a class="back" href="#/">‹</a><span class="dot" id="d-dot"></span>
      <h2 class="grow">${esc(id)}</h2><span class="badge" id="d-phase"></span></div>
    <div class="actions">
      <button class="btn primary" id="b-start">▶ Start</button>
      <button class="btn" id="b-restart">↻ Neustart</button>
      <button class="btn danger" id="b-stop">■ Stop</button>
    </div>
    <nav class="segmented">${TABS.map(([k, l]) => `<a href="#/s/${enc}/${k}" class="${k === tab ? "active" : ""}">${l}</a>`).join("")}</nav>
    <div id="tab"></div>`;
  const base = `/api/servers/${enc}`;
  $("#b-start").onclick = () => act(api("POST", `${base}/start`)).then(refresh);
  $("#b-restart").onclick = () => act(api("POST", `${base}/restart`)).then(refresh);
  $("#b-stop").onclick = async () => {
    if (await dialog({ title: `${id} stoppen?`, body: "Die Welt wird vorher gespeichert. Spieler werden getrennt.", ok: "Stoppen", danger: true })) {
      act(api("POST", `${base}/stop`)).then(refresh);
    }
  };
  let info = null;
  async function refresh() {
    try { info = await api("GET", base); } catch (e) {
      if (e.message === "Server nicht gefunden") location.hash = "#/";
      return;
    }
    const dot = $("#d-dot");
    if (!dot) return;
    dot.className = `dot ${info.phase}`;
    $("#d-phase").textContent = PHASE[info.phase] || info.phase;
    $("#d-phase").className = `badge ${info.phase}`;
    const up = info.phase === "running";
    const busy = ["booting", "starting", "stopping", "restarting"].includes(info.phase);
    $("#b-start").disabled = up || busy;
    $("#b-stop").disabled = !info.process || info.phase === "stopping";
    $("#b-restart").disabled = busy;
    if (tab === "uebersicht") renderOverview(info);
  }
  every(refresh, 2000);
  if (tab === "konsole") tabConsole(id, base);
  else if (tab === "backups") tabBackups(id, base);
  else if (tab === "einstellungen") tabSettings(id, base);
  else if (tab === "uebersicht") $("#tab").innerHTML = `<div id="ov" class="muted">Lade…</div>`;
}

function renderOverview(s) {
  const el = $("#ov");
  if (!el) return;
  const host = location.hostname;
  const tps = s.tps != null ? Number(s.tps).toFixed(1) : "–";
  const mspt = s.mspt != null ? `${Number(s.mspt).toFixed(1)} ms/Tick` : "";
  el.className = "";
  el.innerHTML = `
    <div class="card"><div class="muted">Verbinden mit</div>
      <div class="row"><code class="grow" id="addr">${esc(host)}:${esc(s.port)}</code><button class="btn small" id="copy">Kopieren</button></div>
      <div class="hint">Im Heimnetz. Von unterwegs: Tailscale-Adresse des Pi mit demselben Port.</div></div>
    <div class="grid4">
      <div class="stat"><div class="label">Spieler</div><div class="value">${s.players ?? "–"}<span class="sub"> / ${esc(s.max_players ?? "?")}</span></div></div>
      <div class="stat"><div class="label">TPS</div><div class="value">${tps}</div><div class="sub">${mspt}</div></div>
      <div class="stat"><div class="label">RAM</div><div class="value">${fmtBytes(s.rss_bytes)}</div></div>
      <div class="stat"><div class="label">CPU</div><div class="value">${s.cpu_percent != null ? Number(s.cpu_percent).toFixed(0) + " %" : "–"}</div></div>
      <div class="stat"><div class="label">Läuft seit</div><div class="value">${fmtDur(s.uptime_seconds)}</div></div>
      <div class="stat"><div class="label">Abstürze</div><div class="value">${s.crashes ?? 0}</div></div>
      <div class="stat"><div class="label">Spielmodus</div><div class="value">${esc(s.game_mode ?? "–")}</div></div>
      <div class="stat"><div class="label">Version</div><div class="value sub">${esc(s.version ?? "–")}</div></div>
    </div>
    <div class="card">
      <h3>Online</h3>
      <div class="hint">${s.player_names && s.player_names.length ? s.player_names.map(esc).join(", ") : "Niemand online."}</div>
    </div>
    ${s.last_crash ? `<div class="note warn">Letzter Absturz: ${esc(typeof s.last_crash === "string" ? s.last_crash : JSON.stringify(s.last_crash))}</div>` : ""}`;
  $("#copy").onclick = () => {
    navigator.clipboard?.writeText($("#addr").textContent).then(() => toast("Kopiert"), () => toast("Kopieren nicht möglich", true));
  };
}

// ----- Konsole -----
const ANSI = /\x1b\[[0-9;?]*[A-Za-z]/g;
function lineClass(t) {
  if (t.startsWith("[Panel]")) return "l-panel";
  if (t.startsWith("> ")) return "l-cmd";
  if (/\b(ERROR|FATAL|SEVERE)\b|Exception/.test(t)) return "l-err";
  if (/\bWARN(ING)?\b/.test(t)) return "l-warn";
  if (/<[^>]{1,16}> /.test(t)) return "l-chat";
  return "";
}
function tabConsole(id, base) {
  $("#tab").innerHTML = `
    <div class="console" id="con"></div>
    <form class="cmdline" id="cmd-form"><input id="cmd" placeholder="Befehl, z. B. list" autocomplete="off" autocapitalize="off" enterkeyhint="send">
      <button class="btn primary" type="submit">Senden</button></form>
    <div class="quick">${["list", "save-all", "time set day", "weather clear", "whitelist list", "tick query"].map((c) => `<button class="btn small" data-cmd="${esc(c)}">${esc(c)}</button>`).join("")}
      <button class="btn small" data-say="1">say …</button><button class="btn small" data-op="1">op …</button><button class="btn small" data-wl="1">whitelist add …</button></div>`;
  const con = $("#con");
  const MAX = 1500;
  stream = new EventSource(`${base}/logs`);
  stream.onmessage = (ev) => {
    let l;
    try { l = JSON.parse(ev.data); } catch { return; }
    const atBottom = con.scrollHeight - con.scrollTop - con.clientHeight < 40;
    const div = document.createElement("div");
    const text = String(l.text).replace(ANSI, "");
    div.textContent = text;
    const c = lineClass(text);
    if (c) div.className = c;
    con.appendChild(div);
    while (con.childElementCount > MAX) con.firstElementChild.remove();
    if (atBottom) con.scrollTop = con.scrollHeight;
  };
  stream.onerror = () => { /* EventSource verbindet sich selbst neu */ };
  const history = store.get(`hist-${id}`, []);
  let hi = history.length;
  const send = async (line) => {
    if (!line.trim()) return;
    history.push(line);
    while (history.length > 50) history.shift();
    store.set(`hist-${id}`, history);
    hi = history.length;
    try { await api("POST", `${base}/command`, { line }); } catch (e) { toast(e.message, true); }
  };
  $("#cmd-form").onsubmit = (e) => { e.preventDefault(); const v = $("#cmd").value; $("#cmd").value = ""; send(v); };
  $("#cmd").onkeydown = (e) => {
    if (e.key === "ArrowUp" && hi > 0) { hi--; e.target.value = history[hi]; e.preventDefault(); }
    if (e.key === "ArrowDown") { hi = Math.min(history.length, hi + 1); e.target.value = history[hi] || ""; e.preventDefault(); }
  };
  $$(".quick .btn").forEach((b) => b.onclick = async () => {
    if (b.dataset.cmd) return send(b.dataset.cmd);
    const [title, prefix, ph] = b.dataset.say ? ["Nachricht an alle", "say ", "Text"]
      : b.dataset.op ? ["Spieler zum OP machen", "op ", "Spielername"] : ["Auf die Whitelist", "whitelist add ", "Spielername"];
    const v = await dialog({ title, input: ph, ok: "Senden" });
    if (v && v.trim()) send(prefix + v.trim());
  });
}

// ----- Backups -----
function jobBox(container, jobId, onDone) {
  container.innerHTML = `<div class="card"><div class="row"><b class="grow" id="jt-${jobId}">Läuft…</b><span class="badge" id="js-${jobId}">läuft</span></div><div class="job-log" id="jl-${jobId}"></div></div>`;
  const t = setInterval(async () => {
    let j;
    try { j = await api("GET", `/api/jobs/${jobId}`); } catch { return; }
    const log = $(`#jl-${jobId}`);
    if (!log) { clearInterval(t); return; }
    $(`#jt-${jobId}`).textContent = j.title;
    const atBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 30;
    log.textContent = j.lines.slice(-300).join("\n");
    if (atBottom) log.scrollTop = log.scrollHeight;
    const s = $(`#js-${jobId}`);
    s.textContent = j.state === "running" ? "läuft" : j.state === "ok" ? "fertig ✓" : "fehlgeschlagen";
    s.className = "badge" + (j.state === "ok" ? " running" : "");
    if (j.state !== "running") {
      clearInterval(t);
      toast(j.state === "ok" ? `${j.title}: fertig` : `${j.title}: fehlgeschlagen`, j.state !== "ok");
      onDone && onDone(j);
    }
  }, 1000);
  timers.push(t);
}

function tabBackups(id, base) {
  $("#tab").innerHTML = `
    <div class="card stack">
      <h3>Backup jetzt</h3>
      <div class="row"><input id="bk-name" class="grow" placeholder="Name (optional), z. B. vor-dem-update" maxlength="40" autocomplete="off">
        <button class="btn primary" id="bk-now">Sichern</button></div>
      <div class="hint">Spieler können online bleiben. Automatische Backups stellst du unter Einstellungen ein.</div>
    </div>
    <div id="bk-job"></div>
    <div class="card"><div class="card-head"><h3>Gesicherte Welten</h3><span class="muted" id="bk-count"></span></div><div id="bk-list" class="muted">Lade…</div></div>`;
  async function load() {
    let r;
    try { r = await api("GET", `${base}/backups`); } catch (e) { $("#bk-list").textContent = e.message; return; }
    $("#bk-count").textContent = `${r.backups.length}`;
    $("#bk-list").className = "";
    $("#bk-list").innerHTML = r.backups.length ? r.backups.map((b) => `
      <div class="list-item"><div class="grow"><div class="title">${esc(b.name)}</div>
        <div class="hint">${fmtTime(b.modified)} · ${fmtBytes(b.bytes)}${b.auto ? " · automatisch" : ""}</div></div>
        <div class="row">
          <button class="btn small" data-restore="${esc(b.name)}">Zurück</button>
          <a class="btn small" href="${base}/backups/${encodeURIComponent(b.name)}/download" download>⇩</a>
          <button class="btn small danger" data-del="${esc(b.name)}">✕</button></div></div>`).join("")
      : `<p class="muted">Noch keine Backups.</p>`;
    $$("[data-restore]").forEach((b) => b.onclick = async () => {
      const name = b.dataset.restore;
      if (!(await dialog({ title: "Welt zurücksetzen?", body: `Die Welt wird durch <b>${esc(name)}</b> ersetzt. Läuft der Server, startet er dafür neu. Die jetzige Welt wird in <code>backups/replaced-…</code> aufgehoben.`, ok: "Wiederherstellen", danger: true }))) return;
      const r2 = await act(api("POST", `${base}/restore`, { name }), "Wiederherstellung gestartet");
      if (r2) jobBox($("#bk-job"), r2.job, load);
    });
    $$("[data-del]").forEach((b) => b.onclick = async () => {
      const name = b.dataset.del;
      if (!(await dialog({ title: "Backup löschen?", body: `<b>${esc(name)}</b> wird endgültig gelöscht.`, ok: "Löschen", danger: true }))) return;
      await act(api("DELETE", `${base}/backups/${encodeURIComponent(name)}`), "Gelöscht");
      load();
    });
  }
  $("#bk-now").onclick = async () => {
    const r = await act(api("POST", `${base}/backups`, { name: $("#bk-name").value.trim() }), "Backup gestartet");
    if (r) { $("#bk-name").value = ""; jobBox($("#bk-job"), r.job, load); }
  };
  load();
}

// ----- Einstellungen -----
const FIELDS = [
  ["server.motd", "Servername (MOTD)", "text"],
  ["server.port", "Port", "number", "Ändern gilt ab dem nächsten Start."],
  ["server.max_players", "Max. Spieler", "number"],
  ["server.view_distance", "Sichtweite (Chunks)", "number", "Auf dem Pi: 6–10 ist ein guter Wert."],
  ["server.simulation_distance", "Simulationsweite (Chunks)", "number", "Kleiner = weniger CPU. 6–8 empfohlen."],
  ["world.game_mode", "Spielmodus", ["survival:Überleben", "creative:Kreativ", "adventure:Abenteuer", "spectator:Zuschauer"]],
  ["world.difficulty", "Schwierigkeit", ["peaceful:Friedlich", "easy:Einfach", "normal:Normal", "hard:Schwer"]],
  ["backups.every", "Automatisches Backup alle", "text", "z. B. 30m, 6h, 1d – „0“ schaltet es aus."],
  ["backups.keep", "Automatische Backups behalten", "number"],
  ["world.seed", "Seed", "readonly", "Gilt nur beim Erzeugen neuer Welten."],
];
const SWITCHES = [
  ["server.online_mode", "Online-Modus", "Prüft Minecraft-Konten. Aus = jeder kann unter jedem Namen joinen."],
  ["server.whitelist", "Whitelist", "Nur eingetragene Spieler."],
  ["world.hardcore", "Hardcore", "Ein Leben. Gilt ab dem nächsten Start."],
  ["engine.low_memory", "RAM-Sparmodus", "~60 MB weniger RAM für ~1 % mehr Tick-Zeit."],
  ["engine.restart_on_crash", "Nach Absturz neu starten", ""],
];

function tabSettings(id, base) {
  $("#tab").innerHTML = `<div class="muted">Lade…</div>`;
  api("GET", `${base}/settings`).then((s) => {
    const v = s.values;
    const input = ([key, label, type, hint]) => {
      const val = v[key] ?? "";
      let ctl;
      if (Array.isArray(type)) {
        ctl = `<select data-key="${key}">${type.map((o) => { const [k, l] = o.split(":"); return `<option value="${k}" ${k === val ? "selected" : ""}>${l}</option>`; }).join("")}</select>`;
      } else if (type === "readonly") {
        ctl = `<input value="${esc(val || "zufällig")}" disabled>`;
      } else {
        ctl = `<input data-key="${key}" type="${type}" value="${esc(val)}" autocomplete="off">`;
      }
      return `<label class="field"><span>${label}</span>${ctl}${hint ? `<div class="hint">${hint}</div>` : ""}</label>`;
    };
    $("#tab").innerHTML = `
      <div class="card stack">
        ${switchRow("set-autostart", "Autostart", "Startet automatisch mit dem Pi.", s.autostart)}
      </div>
      <form class="card stack" id="set-form">
        ${FIELDS.map(input).join("")}
        ${SWITCHES.map(([k, l, h]) => `<label class="switch"><span class="txt">${l}${h ? `<small>${h}</small>` : ""}</span><input type="checkbox" data-key="${k}" ${v[k] ? "checked" : ""}></label>`).join("")}
        <button class="btn primary wide" type="submit">Speichern</button>
        <div class="hint">MOTD, Spieleranzahl, Schwierigkeit, Spielmodus, Whitelist und Backups gelten sofort, der Rest ab dem nächsten Start.</div>
      </form>
      <details class="card"><summary>server.toml direkt bearbeiten</summary>
        <div class="stack"><textarea id="raw" spellcheck="false" autocapitalize="off"></textarea>
        <button class="btn wide" id="raw-save">server.toml speichern</button>
        <div class="hint">Für Profis. Wird beim nächsten Start von horizon-server geprüft.</div></div></details>
      <div class="card stack"><h3>Gefahrenzone</h3>
        <button class="btn danger wide" id="del">Server löschen…</button>
        <div class="hint">Löscht den Ordner mit Welt und allen Backups. Vorher ein Backup herunterladen!</div></div>`;
    $("#raw").value = s.raw;
    $("#set-autostart").onchange = (e) => act(api("PUT", `${base}/autostart`, { autostart: e.target.checked }), e.target.checked ? "Autostart an" : "Autostart aus");
    $("#set-form").onsubmit = async (e) => {
      e.preventDefault();
      const changes = {};
      $$("[data-key]", e.target).forEach((el) => {
        const key = el.dataset.key;
        let val = el.type === "checkbox" ? el.checked : el.type === "number" ? Number(el.value) : el.value;
        if (el.type === "number" && el.value === "") return;
        if (val !== v[key]) changes[key] = val;
      });
      if (!Object.keys(changes).length) return toast("Nichts geändert");
      const r = await act(api("PUT", `${base}/settings`, { changes }), "Gespeichert");
      if (r) tabSettings(id, base);
    };
    $("#raw-save").onclick = async () => {
      const r = await act(api("PUT", `${base}/settings/raw`, { text: $("#raw").value }));
      if (r) tabSettings(id, base);
    };
    $("#del").onclick = async () => {
      const typed = await dialog({ title: `${id} löschen?`, body: `Das kann man nicht rückgängig machen. Zum Bestätigen <b>${esc(id)}</b> eintippen. Der Server muss gestoppt sein.`, input: id, ok: "Endgültig löschen", danger: true });
      if (typed == null) return;
      const r = await act(api("DELETE", base, { confirm: typed }), "Server gelöscht");
      if (r) location.hash = "#/";
    };
  }).catch((e) => { $("#tab").innerHTML = `<p class="error">${esc(e.message)}</p>`; });
}

// ---------- Pi & Horizon ----------
function bar(id, frac) {
  const el = $(id);
  if (!el) return;
  el.parentElement.className = "bar" + (frac > 0.9 ? " bad" : frac > 0.75 ? " warn" : "");
  el.style.width = `${Math.min(100, Math.max(0, frac * 100)).toFixed(1)}%`;
}

function renderPi() {
  view().innerHTML = `
    <h2>Raspberry Pi</h2><p class="muted" id="pi-model"></p>
    <div class="grid4">
      <div class="stat"><div class="label">CPU</div><div class="value" id="pi-cpu">–</div><div class="bar"><i id="pi-cpu-bar"></i></div><div class="sub" id="pi-load"></div></div>
      <div class="stat"><div class="label">Temperatur</div><div class="value" id="pi-temp">–</div><div class="bar"><i id="pi-temp-bar"></i></div><div class="sub" id="pi-mhz"></div></div>
      <div class="stat"><div class="label">RAM</div><div class="value" id="pi-ram">–</div><div class="bar"><i id="pi-ram-bar"></i></div><div class="sub" id="pi-ram-sub"></div></div>
      <div class="stat"><div class="label">Speicher</div><div class="value" id="pi-disk">–</div><div class="bar"><i id="pi-disk-bar"></i></div><div class="sub" id="pi-disk-sub"></div></div>
    </div>
    <p class="muted" id="pi-up"></p>

    <h2>Horizon</h2>
    <div class="card stack" id="hz"></div>
    <div id="hz-job"></div>

    <div class="card stack">
      <h3>Spieldateien hochladen</h3>
      <div class="hint">Horizon braucht die vorbereiteten Dateien aus deinem eigenen Minecraft 26.3. Am Windows-PC: <code>horizon-prepare --prepare --server-jar server.jar</code>, dann den Ordner <code>26.3</code> (meist <code>%LOCALAPPDATA%\\Horizon\\game-files\\horizon-client\\26.3</code>) per Rechtsklick → „ZIP-komprimierter Ordner“ packen und hier hochladen. Alle Server müssen dafür aus sein.</div>
      <input type="file" id="gf-file" accept=".zip,application/zip">
      <button class="btn primary wide" id="gf-up" disabled>Hochladen</button>
      <div class="progress hidden" id="gf-prog"><i id="gf-prog-bar"></i></div>
      <div class="hint" id="gf-status"></div>
    </div>
    <div id="gf-job"></div>

    <div class="card"><div class="card-head"><h3>Letzte Aufgaben</h3></div><div id="jobs" class="muted">–</div></div>

    <form class="card stack" id="pw-form"><h3>Panel-Passwort ändern</h3>
      <input type="password" id="pw-old" placeholder="Altes Passwort" autocomplete="current-password" required>
      <input type="password" id="pw-new" placeholder="Neues Passwort (mind. 8 Zeichen)" autocomplete="new-password" minlength="8" required>
      <button class="btn wide" type="submit">Ändern</button></form>
    <button class="btn wide" id="logout">Abmelden</button>
    <p class="muted" id="panel-ver"></p>`;

  every(async () => {
    let s;
    try { s = await api("GET", "/api/system"); } catch { return; }
    piCache = s;
    const p = s.pi;
    if (!$("#pi-cpu")) return;
    $("#pi-model").textContent = [p.model, p.hostname].filter(Boolean).join(" · ");
    $("#pi-cpu").textContent = p.cpu_percent != null ? `${p.cpu_percent.toFixed(0)} %` : "–";
    bar("#pi-cpu-bar", (p.cpu_percent || 0) / 100);
    $("#pi-load").textContent = p.load ? `Last ${p.load}` : "";
    $("#pi-temp").textContent = p.temp_c != null ? `${p.temp_c.toFixed(1)} °C` : "–";
    bar("#pi-temp-bar", p.temp_c != null ? (p.temp_c - 30) / 55 : 0);
    $("#pi-mhz").textContent = p.cpu_mhz ? `${p.cpu_mhz} MHz${p.temp_c >= 80 ? " · drosselt!" : ""}` : "";
    if (p.mem_total) {
      const used = p.mem_total - p.mem_available;
      $("#pi-ram").textContent = fmtBytes(used);
      $("#pi-ram-sub").textContent = `von ${fmtBytes(p.mem_total)}`;
      bar("#pi-ram-bar", used / p.mem_total);
    }
    if (p.disk_total) {
      $("#pi-disk").textContent = `${fmtBytes(p.disk_free)} frei`;
      $("#pi-disk-sub").textContent = `von ${fmtBytes(p.disk_total)}`;
      bar("#pi-disk-bar", 1 - p.disk_free / p.disk_total);
    }
    $("#pi-up").textContent = p.uptime_seconds ? `Pi läuft seit ${fmtDur(p.uptime_seconds)}` : "";
    $("#panel-ver").textContent = `horizon-pi-panel ${s.panel_version} · Daten: ${s.data_dir}`;
    const h = s.horizon;
    const ok = (b) => (b ? "✓" : "✕");
    const built = h.server_bin && h.engine_bin;
    $("#hz").innerHTML = `
      <div class="row"><span class="grow">Programme (horizon-server, Spiel)</span><b>${ok(built)}</b></div>
      <div class="row"><span class="grow">Version</span><span class="muted">${esc(h.version || "–")}</span></div>
      <div class="row"><span class="grow">Spieldateien 26.3</span><b>${ok(h.game_files)}</b></div>
      ${h.game_files ? `<div class="hint">Eingespielt: ${fmtTime(h.game_files.modified)}</div>` : ""}
      <button class="btn ${built ? "" : "primary"} wide" id="hz-build" ${h.building || !h.build_script ? "disabled" : ""}>${h.building ? "Wird gebaut…" : built ? "Horizon aktualisieren (neu bauen)" : "Horizon bauen"}</button>
      <div class="hint">Lädt den neuesten Horizon-Code und baut ihn auf dem Pi – das dauert eine Weile (oft 30–90 Minuten). Server können weiterlaufen; danach neu starten. Schneller geht es am PC mit <code>scripts/cross-build-horizon.sh</code> (siehe README).</div>`;
    $("#hz-build").onclick = async () => {
      if (!(await dialog({ title: "Horizon bauen?", body: "Der Pi ist dabei stark ausgelastet und wird warm. Du kannst die Seite schließen – der Build läuft weiter.", ok: "Bauen" }))) return;
      const r = await act(api("POST", "/api/horizon/build"), "Build gestartet");
      if (r) jobBox($("#hz-job"), r.job);
    };
  }, 3000);

  async function loadJobs() {
    let r;
    try { r = await api("GET", "/api/jobs"); } catch { return; }
    const el = $("#jobs");
    if (!el) return;
    el.className = "";
    el.innerHTML = r.jobs.length ? r.jobs.slice(0, 10).map((j) => `
      <div class="list-item"><div class="grow"><div>${esc(j.title)}</div><div class="hint">${fmtTime(j.started)}${j.lines[0] ? " · " + esc(j.lines[0]).slice(0, 120) : ""}</div></div>
      <span class="badge ${j.state === "ok" ? "running" : ""}">${j.state === "running" ? "läuft" : j.state === "ok" ? "fertig" : "Fehler"}</span>
      <button class="btn small" data-job="${j.id}">Log</button></div>`).join("") : `<p class="muted">Noch nichts.</p>`;
    $$("[data-job]").forEach((b) => b.onclick = () => { jobBox($("#hz-job"), Number(b.dataset.job)); $("#hz-job").scrollIntoView({ behavior: "smooth" }); });
    const build = r.jobs.find((j) => j.kind === "build" && j.state === "running");
    if (build && !$(`#jl-${build.id}`)) jobBox($("#hz-job"), build.id);
  }
  every(loadJobs, 5000);

  $("#gf-file").onchange = () => { $("#gf-up").disabled = !$("#gf-file").files.length; };
  $("#gf-up").onclick = () => {
    const file = $("#gf-file").files[0];
    if (!file) return;
    const xhr = new XMLHttpRequest();
    xhr.open("POST", "/api/gamefiles");
    xhr.setRequestHeader("X-Panel", "1");
    xhr.setRequestHeader("Content-Type", "application/zip");
    $("#gf-prog").classList.remove("hidden");
    $("#gf-up").disabled = true;
    xhr.upload.onprogress = (e) => {
      if (!e.lengthComputable) return;
      $("#gf-prog-bar").style.width = `${(e.loaded / e.total * 100).toFixed(1)}%`;
      $("#gf-status").textContent = `${fmtBytes(e.loaded)} von ${fmtBytes(e.total)}`;
    };
    xhr.onload = () => {
      let data = {};
      try { data = JSON.parse(xhr.responseText); } catch { /* */ }
      $("#gf-up").disabled = false;
      if (xhr.status >= 200 && xhr.status < 300) {
        $("#gf-status").textContent = "Hochgeladen – wird entpackt…";
        jobBox($("#gf-job"), data.job);
      } else {
        $("#gf-status").textContent = data.error || `Fehler ${xhr.status}`;
        toast(data.error || "Upload fehlgeschlagen", true);
      }
    };
    xhr.onerror = () => { $("#gf-up").disabled = false; toast("Upload abgebrochen", true); };
    xhr.send(file);
  };

  $("#pw-form").onsubmit = async (e) => {
    e.preventDefault();
    const r = await act(api("POST", "/api/password", { old: $("#pw-old").value, new: $("#pw-new").value }));
    if (r) showLogin();
  };
  $("#logout").onclick = async () => { await api("POST", "/api/logout").catch(() => {}); showLogin(); };
}

// ---------- Start ----------
let topTimer = null;
async function start() {
  try { await api("GET", "/api/me"); } catch { return showLogin(); }
  $("#login").classList.add("hidden");
  $("#app").classList.remove("hidden");
  refreshTop();
  clearInterval(topTimer);
  topTimer = setInterval(refreshTop, 5000);
  route();
}
start();
