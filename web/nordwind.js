// Nordwind dashboard. Renders ONLY what the hub reports (control-plane topology + real
// measurements). All dynamic strings go through textContent / setAttribute (no innerHTML).
'use strict';
const T = {
  de: {
    h1: 'Fünf Standorte, ein Firmennetz: Agenten, die ohne offene Ports zusammenarbeiten.',
    go: 'Szenario starten', running: 'Läuft …', skip: 'Zum Start-Button',
    mapTitle: 'Live-Topologie des Kontos', legend: 'grün = verbunden · gestrichelt = offline · Punkt = laufender Aufruf',
    s_result: 'Ergebnis', s_crypto: 'Verschlüsselung', s_source: 'Quelle der Topologie',
    who: 'Angemeldet als', lab: 'Lab-Modus: lokal, ohne Login.', noresult: 'Noch kein Lauf.',
    online: 'online', offline: 'offline', planned: 'geplant', unknown: 'unbekannt', connected: 'verbunden',
    live: 'live', trace: 'Trace', hub: 'Hub',
    done: 'Fertig in', failed: 'Fehlgeschlagen', login: 'Login erforderlich.', busy: 'Es läuft bereits ein Szenario.',
    cryptoText: 'Jeder Hop ist ein eigener Kanal mit Noise-Ende-zu-Ende-Verschlüsselung. Der Edge reicht nur Chiffretext durch. Gemessen werden die Bytes auf der Leitung (vom Agenten unterhalb der Verschlüsselung gezählt), der Klartext liegt nur an den Endpunkten.',
    hop: 'Hop', plain: 'Klartext', wire: 'Leitung', viaTrace: 'Trace = aus der verschlüsselten Antwort rekonstruiert, nicht live gemeldet.',
    srcCp: 'Control-Plane-Topologie', srcMf: 'Control-Plane nicht erreichbar, lokale Vorlage', at: 'Stand',
    model: 'Modell', route: 'Route', weather: 'Wetter', km: 'km', kn: 'kn', gust: 'Böen', by: 'Text: Werkstatt · Fakten: Hafenbüro · Ablauf: Zentrale',
    nodeLine: (l, s, os) => `${l}: ${s}${os ? ', ' + os : ''}`,
  },
  en: {
    h1: 'Five sites, one company network: agents cooperating without a single open port.',
    go: 'Start scenario', running: 'Running …', skip: 'Skip to start button',
    mapTitle: 'Live topology of the account', legend: 'green = connected · dashed = offline · dot = call in flight',
    s_result: 'Result', s_crypto: 'Encryption', s_source: 'Topology source',
    who: 'Signed in as', lab: 'Lab mode: local, no login.', noresult: 'No run yet.',
    online: 'online', offline: 'offline', planned: 'planned', unknown: 'unknown', connected: 'connected',
    live: 'live', trace: 'trace', hub: 'hub',
    done: 'Done in', failed: 'Failed', login: 'Login required.', busy: 'A scenario is already running.',
    cryptoText: 'Every hop is its own channel with Noise end-to-end encryption. The edge only forwards ciphertext. Wire bytes are counted by the agent below the encryption layer; plaintext exists only at the endpoints.',
    hop: 'Hop', plain: 'plaintext', wire: 'wire', viaTrace: 'trace = reconstructed from the encrypted reply, not reported live.',
    srcCp: 'Control-plane topology', srcMf: 'Control plane unreachable, local template', at: 'as of',
    model: 'Model', route: 'Route', weather: 'Weather', km: 'km', kn: 'kn', gust: 'gusts', by: 'Text: workshop · facts: harbour office · flow: HQ',
    nodeLine: (l, s, os) => `${l}: ${s}${os ? ', ' + os : ''}`,
  },
};
function safeGet(k) { try { return localStorage.getItem(k); } catch (_) { return null; } }
const NS = 'http://www.w3.org/2000/svg';
const FIXED = { zentrale: [180, 200], cloud: [180, 42], hafenbuero: [58, 175], werkstatt: [302, 175], aussendienst: [180, 312] };
const ICON = { hub: 'i-desktop', site: 'i-server', gpu: 'i-gpu', mobile: 'i-phone', cloud: 'i-cloud', unknown: 'i-server' };
let lang = (safeGet('nw-lang') || navigator.language || 'de').slice(0, 2) === 'en' ? 'en' : 'de';
let graph = null, me = null, running = false, lastRun = [];
const $ = (id) => document.getElementById(id);
const t = (k) => T[lang][k];

function el(name, attrs, text) {
  const e = document.createElementNS(NS, name);
  for (const [k, v] of Object.entries(attrs || {})) e.setAttribute(k, v);
  if (text !== undefined) e.textContent = text;
  return e;
}
function h(tag, cls, text) { const e = document.createElement(tag); if (cls) e.className = cls; if (text !== undefined) e.textContent = text; return e; }
const fmtB = (n) => n == null ? '–' : n < 1024 ? n + ' B' : (n / 1024).toFixed(1) + ' kB';
const fmtT = (ms) => ms == null ? '–' : ms < 1000 ? ms + ' ms' : (ms / 1000).toFixed(1) + ' s';
const name = (k) => { const n = graph && graph.nodes.find((x) => x.key === k); return n ? n.label[lang] : k; };

function positions() {
  const pos = {}; const others = [];
  for (const n of graph.nodes) { if (FIXED[n.key]) pos[n.key] = FIXED[n.key]; else others.push(n); }
  others.forEach((n, i) => { const a = (i / Math.max(others.length, 1)) * 2 * Math.PI + 0.6; pos[n.key] = [180 + 135 * Math.cos(a), 200 + 115 * Math.sin(a)]; });
  return pos;
}
const keyOf = (id) => (graph.nodes.find((n) => n.id === id) || {}).key;

function renderGraph() {
  if (!graph) return;
  const pos = positions(); const E = $('edges'), N = $('nodes');
  E.replaceChildren(); N.replaceChildren(); $('sr').replaceChildren();
  for (const e of graph.edges) {
    const a = pos[keyOf(e.a)], b = pos[keyOf(e.b)]; if (!a || !b) continue;
    const g = el('g', { 'data-a': keyOf(e.a), 'data-b': keyOf(e.b) });
    g.appendChild(el('line', { class: 'edge ' + (e.state === 'connected' ? '' : 'offline'), x1: a[0], y1: a[1], x2: b[0], y2: b[1] }));
    const lab = e.state === 'connected' && e.rtt_ms != null ? `${fmtT(e.rtt_ms)}` : '';
    g.appendChild(el('text', { class: 'elabel', x: (a[0] + b[0]) / 2, y: (a[1] + b[1]) / 2 - 4 }, lab));
    E.appendChild(g);
  }
  for (const n of graph.nodes) {
    const p = pos[n.key];
    const g = el('g', { class: `node ${n.state}`, 'data-key': n.key, tabindex: '0', transform: `translate(${p[0]} ${p[1]})` });
    g.appendChild(el('circle', { class: 'body', r: 26 }));
    const u = el('use', { href: '#' + (ICON[n.role] || ICON.unknown), x: -13, y: -13, width: 26, height: 26 }); g.appendChild(u);
    g.appendChild(el('text', { class: 'nlabel', y: 42 }, n.label[lang]));
    g.appendChild(el('text', { class: 'nbadge', y: 54 }, [n.os || (n.role === 'cloud' ? 'Cloud' : ''), t(n.state)].filter(Boolean).join(' · ')));
    const desc = t('nodeLine')(n.label[lang], t(n.state), n.os);
    g.setAttribute('aria-label', desc);
    N.appendChild(g);
    $('sr').appendChild(h('li', '', desc));
  }
  $('map').setAttribute('data-ready', '1');
  const s = graph.source;
  $('source').replaceChildren(h('p', '', s.kind === 'control-plane' ? `${t('srcCp')} ${s.topology_id.slice(0, 8)}…` : `${t('srcMf')}: ${s.error || ''}`),
    h('p', '', `${t('at')}: ${new Date(s.fetched_at_ms).toLocaleTimeString()}`));
  $('banner').hidden = !(s.kind !== 'control-plane') && !(me && me.lab);
  $('banner').textContent = s.kind !== 'control-plane' ? `${t('srcMf')}: ${s.error || ''}` : (me && me.lab ? t('lab') : '');
}

function animate(from, to, ms) {
  const pos = positions(), a = pos[from], b = pos[to]; if (!a || !b) return;
  const g = document.querySelector(`#edges g[data-a="${from}"][data-b="${to}"], #edges g[data-a="${to}"][data-b="${from}"]`);
  if (g) { g.firstChild.classList.add('active'); setTimeout(() => g.firstChild.classList.remove('active'), Math.max(ms || 600, 600)); }
  for (const k of [from, to]) { const n = document.querySelector(`.node[data-key="${k}"]`); if (n) { n.classList.add('working'); setTimeout(() => n.classList.remove('working'), Math.max(ms || 600, 600)); } }
  if (matchMedia('(prefers-reduced-motion: reduce)').matches) return;
  const d = el('circle', { class: 'dot', r: 6, cx: a[0], cy: a[1] }); $('dots').appendChild(d);
  const dur = 700, t0 = performance.now();
  (function step(now) {
    const k = Math.min((now - t0) / dur, 1);
    d.setAttribute('cx', a[0] + (b[0] - a[0]) * k); d.setAttribute('cy', a[1] + (b[1] - a[1]) * k);
    if (k < 1) requestAnimationFrame(step); else d.remove();
  })(t0);
}

function logLine(cls, text, tag) {
  const li = h('li', cls, text); if (tag) li.appendChild(h('span', 'tag', tag));
  const L = $('log'); L.prepend(li); while (L.children.length > 9) L.lastChild.remove();
}

function onEvent(ev) {
  if (ev.type === 'graph') { loadGraph(); return; }
  if (ev.type === 'run_start') { setRunning(true); $('log').replaceChildren(); logLine('run', ev.topic); lastRun = [ev]; return; }
  lastRun.push(ev);
  if (ev.type === 'hop') {
    if (ev.state === 'start') { animate(ev.from, ev.to, 900); return; }
    const txt = `${name(ev.from)} → ${name(ev.to)} · ${ev.service} · ${fmtT(ev.dur_ms)} · ↑${fmtB(ev.sent_bytes)} ↓${fmtB(ev.recv_bytes)}` + (ev.state === 'error' ? ` · ${ev.error || ''}` : '');
    logLine(ev.state === 'ok' ? 'ok' : 'err', txt, ev.source === 'trace' ? t('trace') : t('live'));
    if (ev.source === 'trace') animate(ev.from, ev.to, 600);
    renderCrypto();
  } else if (ev.type === 'run_done') {
    setRunning(false); logLine('ok', `${t('done')} ${fmtT(ev.total_ms)}`); renderResult(ev.briefing); $('d-result').open = true; renderCrypto();
  } else if (ev.type === 'run_error') {
    setRunning(false); logLine('err', `${t('failed')}: ${ev.error}`); $('status').textContent = t('failed');
  }
}

function renderResult(b) {
  const box = $('result'); box.replaceChildren();
  if (!b) { box.textContent = t('noresult'); return; }
  box.appendChild(h('p', '', b.text || ''));
  const r = b.route, w = b.wetter;
  if (r) box.appendChild(h('p', '', `${t('route')}: ${r.name}, ${r.gesamt_km} ${t('km')}`));
  if (w) box.appendChild(h('p', '', `${t('weather')} ${w.datum} (${w.ort}): ${w.wind_kn_max} ${t('kn')}, ${t('gust')} ${w.boeen_kn_max} ${t('kn')}, ${w.temp_min_c}–${w.temp_max_c} °C, ${w.regen_mm} mm`));
  box.appendChild(h('p', 'chip', `${t('model')}: ${b.modell || '?'}${b.gpu ? ' · ' + b.gpu : ''} · ${t('by')}`));
}

function renderCrypto() {
  const box = $('crypto'); box.replaceChildren(h('p', '', t('cryptoText')));
  const hops = lastRun.filter((e) => e.type === 'hop' && e.state !== 'start'); if (!hops.length) return;
  const tb = h('table'); const hd = h('tr');
  ['#', 'Hop', t('plain') + ' ↑↓', t('wire') + ' ↑↓', 'transport'].forEach((x) => hd.appendChild(h('th', '', x))); tb.appendChild(hd);
  hops.forEach((e) => { const tr = h('tr'); [e.seq, `${name(e.from)} → ${name(e.to)}`, `${fmtB(e.plain_out)} / ${fmtB(e.plain_in)}`, `${fmtB(e.sent_bytes)} / ${fmtB(e.recv_bytes)}`, e.transport || '–'].forEach((x) => tr.appendChild(h('td', '', String(x)))); tb.appendChild(tr); });
  box.appendChild(tb); box.appendChild(h('p', 'chip', t('viaTrace')));
}

function setRunning(r) { running = r; const b = $('go'); b.disabled = r; b.setAttribute('aria-busy', String(r)); b.textContent = r ? t('running') : t('go'); if (!r) $('status').textContent = ''; }

async function loadGraph() { try { const r = await fetch('/api/topology'); if (r.ok) { graph = await r.json(); renderGraph(); } } catch (_) { /* keep last */ } }

async function start() {
  const r = await fetch('/api/scenario', { method: 'POST' });
  if (r.status === 403) $('status').textContent = t('login');
  else if (r.status === 409 || r.status === 429) $('status').textContent = t('busy');
}

function applyLang() {
  document.documentElement.lang = lang; $('h1').textContent = t('h1'); $('lang').textContent = lang === 'de' ? 'EN' : 'DE';
  $('maptitle').textContent = t('mapTitle'); $('legend').textContent = t('legend');
  ['result', 'crypto', 'source'].forEach((k) => { $('s-' + k).textContent = t('s_' + k); });
  document.querySelector('.skip').textContent = t('skip');
  $('who').textContent = me ? `${t('who')} ${me.email}` : '';
  setRunning(running); renderGraph(); renderCrypto();
  if (!$('result').firstChild) $('result').textContent = t('noresult');
}

async function init() {
  $('lang').addEventListener('click', () => { lang = lang === 'de' ? 'en' : 'de'; try { localStorage.setItem('nw-lang', lang); } catch (_) {} applyLang(); });
  $('go').addEventListener('click', start);
  applyLang();
  const r = await fetch('/api/state');
  if (r.status === 403) { $('status').textContent = t('login'); return; }
  const s = await r.json(); me = s.me; graph = s.graph; running = s.running;
  lastRun = s.last_run || [];
  applyLang();
  const done = lastRun.find((e) => e.type === 'run_done'); if (done) renderResult(done.briefing);
  const es = new EventSource('/api/events'); es.onmessage = (m) => { try { onEvent(JSON.parse(m.data)); } catch (_) {} };
}
init();
