// Aether DV desktop UI (Phase C minimal UI).
//
// Talks only to the engine's local API (JSON-RPC 2.0 over WebSocket); it
// never touches ports, files or the database (ADR-0001). Every string that
// came from the air or a ratflector is untrusted: it is only ever inserted
// as text (textContent / text nodes), never as HTML.
'use strict';

// ---------------------------------------------------------------- helpers
const $ = (sel, root = document) => root.querySelector(sel);

function h(tag, attrs, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === 'class') e.className = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else if (k === 'value') e.value = v;
    else e.setAttribute(k, v === true ? '' : String(v));
  }
  for (const kid of kids.flat(Infinity)) {
    if (kid == null || kid === false) continue;
    e.append(kid instanceof Node ? kid : String(kid));
  }
  return e;
}

const pad = (n) => String(n).padStart(2, '0');
function hms(ms) {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}
function ago(ms) {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.round(s / 60)} m`;
  if (s < 86400) return `${Math.round(s / 3600)} h`;
  return `${Math.round(s / 86400)} d`;
}
function kb(n) {
  return n >= 1024 ? `${(n / 1024).toFixed(1)} kB` : `${n} B`;
}

const MODES = {
  rf_slow: { mode: 'slow', label: '◇ Slow', words: 'DV slow data', rate: 119 },
  rf_fast: { mode: 'fast', label: '◆ Fast', words: 'DV fast data', rate: 350 },
  internet: { mode: 'net', label: '⌁ Internet', words: 'internet', rate: 0 },
};
const modeOf = (p) => MODES[p && p.pathKind] || MODES.rf_slow;

// Why a radio port is not sending (scheduler, docs/10 §4.4).
const WAIT_WORDS = {
  voice: 'Voice on the channel', voice_gap: 'Waiting after voice', other_mode: 'Repeater busy in another mode',
  signal: 'Channel busy', receiving: 'Receiving', holdoff: 'Waiting for a turn', gap: 'Leaving room for others',
  courtesy: 'Courtesy pause', paused: 'Paused by you', route: 'Held until the route is OK',
};
const GATE_WORDS = {
  open: 'Route OK', checking: 'Checking the radio', confirm: 'Confirm the route',
  held: 'Radio set for another route', blocked: 'Blocked by the radio’s UR',
};
const SCOPE_WORDS = { direct: 'Direct (simplex)', repeater: 'This repeater only', reflector: 'Linked reflector' };

function channelWords(p) {
  const c = p.channel;
  if (!c) return '';
  if (c.state === 'sending') return 'Sending';
  if (c.state === 'waiting' && c.why) {
    const w = WAIT_WORDS[c.why] || c.why;
    return c.until && ['courtesy', 'paused', 'voice_gap'].includes(c.why) ? `${w} until ${hms(c.until)}` : w;
  }
  return 'Clear';
}

const routeHeld = (p) => p.route && p.route.gate !== 'open';

const PORT_WORDS = {
  starting: 'Starting', connecting: 'Connecting', up: 'Connected', ready: 'Ready',
  backoff: 'Reconnecting', absent: 'Radio not found', lost: 'Connection lost',
  auth_failed: 'Login refused',
};

// ---------------------------------------------------------------- state
const S = {
  connected: false,
  me: '',
  ports: [],
  stations: new Map(), // callsign -> {callsign, lastHeard, software, portId}
  lines: new Map(), // chat line id -> line
  sys: [], // {at, portId, who, text, link}
  transfers: new Map(),
  messages: [],
  events: [],
  positions: new Map(),
  channel: 'all',
  view: 'chat',
  folder: 'inbox',
  msgSel: null,
  composing: false,
  unreadChat: 0,
  unreadMsg: 0,
  lastError: '',
  checks: new Map(), // portId -> last repeater access check result
  portOpen: null, // port whose route panel is open
};

// ---------------------------------------------------------------- engine API
const Engine = {
  ws: null,
  next: 1,
  pending: new Map(),
  retry: 1000,

  async endpoint() {
    const tauri = window.__TAURI__;
    if (tauri && tauri.core) {
      return tauri.core.invoke('engine_endpoint', {
        tokenFile: localStorage.getItem('aether.tokenFile') || null,
        api: localStorage.getItem('aether.api') || null,
      });
    }
    // Browser preview: ?api=127.0.0.1:7373&token=...
    const q = new URLSearchParams(location.search);
    return { api: q.get('api') || '127.0.0.1:7373', token: q.get('token') || '' };
  },

  async connect() {
    let ep;
    try {
      ep = await this.endpoint();
    } catch (e) {
      S.lastError = String(e);
      renderConnect();
      return;
    }
    const ws = new WebSocket(`ws://${ep.api}`);
    this.ws = ws;
    ws.onmessage = (m) => this.onMessage(m.data);
    ws.onclose = () => {
      for (const [, p] of this.pending) p.reject(new Error('engine connection closed'));
      this.pending.clear();
      if (S.connected) {
        S.connected = false;
        render();
      }
      setTimeout(() => this.connect(), this.retry);
      this.retry = Math.min(this.retry * 2, 15000);
    };
    ws.onopen = async () => {
      try {
        const hello = await this.call('system.hello', {
          clientName: 'aether-desktop', apiVersion: '0.1.0', token: ep.token,
        });
        this.retry = 1000;
        S.me = hello.profile;
        S.connected = true;
        S.lastError = '';
        await this.call('events.subscribe', { topics: [] });
        await loadAll();
        render();
      } catch (e) {
        S.lastError = e.message;
        renderConnect();
        ws.close();
      }
    };
  },

  call(method, params) {
    return new Promise((resolve, reject) => {
      if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
        reject(new Error('not connected to the engine'));
        return;
      }
      const id = this.next++;
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params: params || {} }));
    });
  },

  onMessage(text) {
    let m;
    try {
      m = JSON.parse(text);
    } catch {
      return;
    }
    if (m.id != null && this.pending.has(m.id)) {
      const p = this.pending.get(m.id);
      this.pending.delete(m.id);
      if (m.error) p.reject(new Error(m.error.message));
      else p.resolve(m.result);
      return;
    }
    if (m.method === 'event' && m.params) onEvent(m.params.topic, m.params.data || {});
  },
};

async function loadAll() {
  const [ports, stations, transfers, events] = await Promise.all([
    Engine.call('ports.list'),
    Engine.call('stations.list'),
    Engine.call('transfers.list', { includeFinished: true }),
    Engine.call('diag.events', { limit: 50 }),
  ]);
  S.ports = ports;
  S.stations = new Map(stations.map((s) => [s.callsign, s]));
  S.transfers = new Map(transfers.map((t) => [t.id, t]));
  S.events = events.reverse();
  for (const p of await Engine.call('stations.positions', {})) S.positions.set(p.callsign, p);
  await loadTestStatus();
  await loadChat();
  await loadMessages();
}

async function loadChat() {
  const port = S.channel === 'all' ? null : S.channel;
  const lines = await Engine.call('chat.history', { portId: port, limit: 300 });
  for (const l of lines) S.lines.set(l.id, l);
}

async function loadMessages() {
  S.messages = await Engine.call('messages.list', { folder: S.folder, limit: 200 });
}

// ---------------------------------------------------------------- events
let renderQueued = false;
function render() {
  if (renderQueued) return;
  renderQueued = true;
  requestAnimationFrame(() => {
    renderQueued = false;
    renderAll();
  });
}

function onEvent(topic, d) {
  switch (topic) {
    case 'port.state': {
      const p = S.ports.find((x) => x.id === d.portId);
      if (p && d.state) {
        const down = (st) => ['absent', 'lost', 'auth_failed', 'backoff'].includes(st);
        const ok = (st) => ['up', 'ready'].includes(st);
        if (ok(p.state) && down(d.state)) Sounds.play('linkLost');
        if (down(p.state) && ok(d.state)) Sounds.play('linkRestored');
      }
      if (p) {
        p.state = d.state;
        p.detail = d.detail;
      }
      break;
    }
    case 'chat.line': {
      const prev = S.lines.get(d.id);
      if (!prev && d.direction === 'in') {
        if (d.position) Sounds.play('position');
        else if (d.dst === S.me) Sounds.play('direct');
        else if (d.kind === 'text') Sounds.play('chat');
      }
      if (prev && prev.txState !== 'failed' && d.txState === 'failed') Sounds.play('failed');
      if (!S.lines.has(d.id) && d.direction === 'in' && S.view !== 'chat') S.unreadChat++;
      S.lines.set(d.id, d);
      break;
    }
    case 'station.heard': {
      const s = S.stations.get(d.callsign) || { callsign: d.callsign };
      if (d.software) s.software = d.software;
      else {
        s.lastHeard = Date.now();
        s.portId = d.portId;
      }
      S.stations.set(d.callsign, s);
      break;
    }
    case 'station.position':
      // Shown in chat through the chat line that carried it.
      S.positions.set(d.callsign, d);
      break;
    case 'transfer.progress': {
      const prev = S.transfers.get(d.id);
      if (!prev || prev.state !== d.state) {
        if (d.state === 'done' && d.direction === 'receive' && d.protocol === 'legacy_file') Sounds.play('file');
        if (d.state === 'done' && d.direction === 'send' && d.protocol === 'legacy_file') Sounds.play('sent');
        if (d.state === 'failed' && d.stateReason !== 'cancelled') Sounds.play('failed');
      }
      S.transfers.set(d.id, d);
      break;
    }
    case 'message.state': {
      const m = d.message;
      if (m.state === 'handed_off') Sounds.play('sent');
      if (m.state === 'failed' && m.stateReason !== 'cancelled') Sounds.play('failed');
      if (m.state === 'received') {
        Sounds.play('message');
        S.unreadMsg++;
        S.sys.push({
          at: Date.now(), portId: d.event && d.event.portId, who: 'Message',
          text: [`${m.formId === 'ICS213_US_OS' ? 'ICS-213' : 'Form'} "${m.subject || ''}" received from `, h('span', { class: 'call' }, m.src)],
          link: m.id,
        });
      }
      if (S.view === 'messages') loadMessages().then(render);
      break;
    }
    case 'port.route': {
      const p = S.ports.find((x) => x.id === d.portId);
      if (p) p.route = d.route;
      break;
    }
    case 'port.channel': {
      const p = S.ports.find((x) => x.id === d.portId);
      if (p) p.channel = d.channel;
      break;
    }
    case 'port.accessCheck':
      S.checks.set(d.portId, { at: Date.now(), ...d.result });
      break;
    case 'engine.event':
      S.events.push(d);
      if (S.events.length > 200) S.events.shift();
      break;
    default:
      break;
  }
  render();
}

function fmtPos(lat, lon) {
  return `${Math.abs(lat).toFixed(3)} ${lat >= 0 ? 'N' : 'S'}, ${Math.abs(lon).toFixed(3)} ${lon >= 0 ? 'E' : 'W'}`;
}

// ---------------------------------------------------------------- frame
function renderAll() {
  renderRail();
  if (!S.connected && !['radios', 'sounds'].includes(S.view)) {
    renderConnect();
    return;
  }
  if (!S.connected) {
    // Setup help and sound settings don't need the engine.
    (S.view === 'radios' ? renderRadios : renderSounds)($('#main'));
    return;
  }
  $('#mycall').textContent = S.me;
  renderStrip();
  renderRail();
  renderDrawer();
  const main = $('#main');
  const views = { chat: renderChat, messages: renderMessages, transfers: renderTransfers, stations: renderStations, radios: renderRadios, sounds: renderSounds, diagnostics: renderDiagnostics };
  (views[S.view] || renderChat)(main);
  if (S.portOpen) renderPort();
}

function renderStrip() {
  const strip = $('#strip');
  strip.replaceChildren(
    ...S.ports.map((p) => {
      const m = modeOf(p);
      const word = PORT_WORDS[p.state] || p.state;
      const down = ['absent', 'lost', 'auth_failed', 'backoff'].includes(p.state);
      const heard = [...S.stations.values()].filter((s) => s.portId === p.id && s.lastHeard && Date.now() - s.lastHeard < 3600e3).length;
      const r = p.route;
      const held = !down && routeHeld(p);
      const where = r && r.via === 'repeater'
        ? `${r.repeater}, ${r.scope === 'reflector' ? `reflector${r.link ? ` ${r.link.reflector}` : ''}` : 'this repeater'}`
        : null;
      const status = held ? h('span', { class: 'st pending' }, GATE_WORDS[r.gate] || r.gate) : channelWords(p);
      const label = `${p.name}, ${m.words}, ${word}${where ? `, ${where}` : ''}${held ? `, ${GATE_WORDS[r.gate]}` : ''}`;
      const kids = [
        h('span', { class: 'plate-name' }, p.name),
        h('span', { class: 'plate-mode' }, m.label),
        h('span', { class: 'plate-state' }, down ? h('span', { class: 'st fail' }, word) : word),
        h('span', { class: 'plate-nums' }, p.detail && down ? p.detail : [h('b', {}, heard), ' heard in the last hour']),
      ];
      if (r) kids.push(h('span', { class: 'plate-route' }, where || 'Direct'), h('span', { class: 'plate-chan' }, status));
      if (!r) return h('li', { class: 'plate', 'data-mode': m.mode, 'data-state': down ? 'down' : 'ok', 'aria-label': label }, ...kids);
      return h('li', { class: 'plate', 'data-mode': m.mode, 'data-state': down ? 'down' : held ? 'held' : 'ok' },
        h('button', { class: 'plate-btn', 'aria-label': `${label}. Open route and channel`, onclick: () => openPort(p.id) }, ...kids));
    }),
    h('li', { class: 'strip-end' }, S.ports.length ? '' : 'No ports configured'));
}

function renderRail() {
  for (const a of document.querySelectorAll('.rail a')) {
    if (a.dataset.view === S.view) a.setAttribute('aria-current', 'page');
    else a.removeAttribute('aria-current');
  }
  const count = (id, n, label) => {
    const e = $(id);
    e.hidden = !n;
    e.textContent = n;
    e.setAttribute('aria-label', `${n} ${label}`);
  };
  count('#unread-chat', S.unreadChat, 'unread');
  count('#unread-msg', S.unreadMsg, 'unread');
  count('#active-xfer', [...S.transfers.values()].filter((t) => !['done', 'failed', 'cancelled'].includes(t.state)).length, 'active');
}

function renderDrawer() {
  const last = S.events.slice(-3).reverse();
  $('#drawer').replaceChildren(...last.map((e) => h('span', {}, h('time', {}, hms(e.at)), e.message)));
}

function renderConnect() {
  $('#strip').replaceChildren(h('li', { class: 'plate-empty' }, 'Not connected to the engine'));
  const existing = $('#main .connect');
  if (existing) {
    // Keep what the operator typed; only refresh the error line.
    $('#cx-error').textContent = S.lastError || '';
    return;
  }
  const tauri = window.__TAURI__ && window.__TAURI__.core;
  const tokenFile = h('input', { class: 'field', id: 'tokfile', value: localStorage.getItem('aether.tokenFile') || '', placeholder: 'aetherdv.token next to your database' });
  const api = h('input', { class: 'field', id: 'apiaddr', value: localStorage.getItem('aether.api') || '127.0.0.1:7373' });
  const config = h('input', { class: 'field', id: 'cfgfile', value: localStorage.getItem('aether.config') || '', placeholder: 'Full path of your aether.toml' });
  const startBtn = h('button', {
    class: 'btn primary', type: 'button',
    onclick: async () => {
      $('#cx-error').textContent = '';
      localStorage.setItem('aether.config', config.value.trim());
      localStorage.setItem('aether.api', api.value.trim());
      startBtn.disabled = true;
      startBtn.textContent = 'Starting…';
      try {
        await tauri.invoke('engine_start', { config: config.value.trim(), api: api.value.trim() || null });
        Engine.retry = 1000;
        if (Engine.ws) Engine.ws.close();
        else Engine.connect();
      } catch (e) {
        $('#cx-error').textContent = String(e);
      } finally {
        startBtn.disabled = false;
        startBtn.textContent = 'Start the engine';
      }
    },
  }, 'Start the engine');
  $('#main').replaceChildren(
    h('form', {
      class: 'connect', 'aria-labelledby': 'cx-title',
      onsubmit: (ev) => {
        ev.preventDefault();
        localStorage.setItem('aether.tokenFile', tokenFile.value.trim());
        localStorage.setItem('aether.api', api.value.trim());
        Engine.retry = 1000;
        if (Engine.ws) Engine.ws.close();
        else Engine.connect();
      },
    },
      h('h1', { id: 'cx-title' }, 'Connect to the Aether engine'),
      h('p', {}, 'The engine (aetherdv) owns the radios and keeps running traffic. It needs your configuration file (see examples/ic9700.toml).'),
      h('p', { class: 'error-text', role: 'alert', id: 'cx-error' }, S.lastError || ''),
      !tauri ? h('p', {}, 'In a browser preview, pass ?api=ADDRESS&token=TOKEN in the address.') : null,
      tauri && Shell.info && Shell.info.bundledEngine ? [
        h('label', {}, 'Configuration file', config),
        h('div', {}, startBtn),
        h('p', { class: 'faint' }, 'Or connect to an engine you started yourself:'),
      ] : null,
      tauri ? h('label', {}, 'Token file', tokenFile) : null,
      tauri ? h('label', {}, 'Engine address', api) : null,
      h('div', {}, h('button', { class: tauri && Shell.info && Shell.info.bundledEngine ? 'btn' : 'btn primary', type: 'submit' }, 'Connect'))));
}

// ---------------------------------------------------------------- app shell: version and updates
// Updates are checked once shortly after start and on request; they are
// installed only when the operator clicks Install. The shell verifies each
// package's signature before installing it.
const Shell = {
  info: null, // {version, updatesConfigured, bundledEngine, engineStartedHere}
  update: null, // {version, notes, date}
  state: 'idle', // idle | checking | available | none | installing | error
  message: '',
  progress: null,

  tauri() {
    return window.__TAURI__ && window.__TAURI__.core;
  },

  async init() {
    const t = this.tauri();
    if (!t) return;
    try {
      this.info = await t.invoke('app_info');
    } catch {
      return;
    }
    if (window.__TAURI__.event) {
      window.__TAURI__.event.listen('update-progress', (e) => {
        this.progress = e.payload;
        renderUpdate();
      });
    }
    render();
    if (this.info.updatesConfigured) setTimeout(() => this.check(true), 8000);
  },

  async check(quiet) {
    const t = this.tauri();
    if (!t) return;
    this.state = 'checking';
    this.message = '';
    renderUpdate();
    try {
      this.update = await t.invoke('update_check');
      this.state = this.update ? 'available' : 'none';
      if (this.update) Sounds.play('update');
      if (!this.update && quiet) this.state = 'idle';
    } catch (e) {
      this.state = quiet ? 'idle' : 'error';
      this.message = String(e);
    }
    renderUpdate();
    render();
  },

  async install() {
    this.state = 'installing';
    this.progress = null;
    renderUpdate();
    try {
      await this.tauri().invoke('update_install'); // restarts the app on success
    } catch (e) {
      this.state = 'error';
      this.message = String(e);
      renderUpdate();
    }
  },
};

function renderUpdate() {
  const box = $('#update');
  if (!box) return;
  const show = ['available', 'installing'].includes(Shell.state) || (Shell.state === 'error' && Shell.update);
  box.hidden = !show;
  if (!show) return;
  const u = Shell.update || {};
  const pct = Shell.progress && Shell.progress.total ? Math.round((100 * Shell.progress.downloaded) / Shell.progress.total) : null;
  box.replaceChildren(
    h('h2', {}, `Aether DV ${u.version} is available`),
    h('p', { class: 'muted' }, `You have ${Shell.info ? Shell.info.version : '?'}.`),
    u.notes ? h('p', { class: 'notes' }, u.notes.length > 400 ? `${u.notes.slice(0, 400)}…` : u.notes) : null,
    Shell.state === 'installing'
      ? [h('p', {}, pct == null ? 'Downloading and checking the signature…' : `Downloading… ${pct}%`),
        h('div', { class: 'bar', role: 'progressbar', 'aria-valuenow': pct || 0, 'aria-valuemin': 0, 'aria-valuemax': 100, 'aria-label': 'Update download' }, h('span', { style: `width:${pct || 0}%` }))]
      : [h('p', { class: 'faint' }, Shell.info && Shell.info.engineStartedHere
          ? 'The engine started by this app stops during the update and does not restart by itself. Install between transfers.'
          : 'Install between transfers. If you run aetherdv from this installation yourself, stop it first.'),
        Shell.state === 'error' ? h('p', { class: 'error-text', role: 'alert' }, Shell.message) : null,
        h('div', { class: 'acts' },
          h('button', { class: 'btn primary', onclick: () => Shell.install() }, 'Install and restart'),
          h('button', { class: 'btn quiet', onclick: () => { Shell.state = 'idle'; renderUpdate(); } }, 'Later'))]);
}

// ---------------------------------------------------------------- chat
function channelPorts() {
  return S.channel === 'all' ? S.ports : S.ports.filter((p) => p.id === S.channel);
}

function lineState(l) {
  if (l.direction !== 'out') return null;
  const port = S.ports.find((p) => p.id === l.portId);
  switch (l.txState) {
    case 'queued':
      return h('span', { class: 'st pending' }, 'Queued');
    case 'transmitted':
      return h('span', {
        class: 'st unknown',
        title: l.dst === 'CQCQCQ'
          ? `Broadcast chat cannot be confirmed. It was transmitted on ${port ? port.name : l.portId}.`
          : 'Transmitted. D-RATS chat has no delivery receipt.',
      }, 'Transmitted');
    case 'failed':
      return h('span', { class: 'st fail' }, 'Not sent');
    default:
      return null;
  }
}

function renderChat(main) {
  let view = $('.chat', main);
  if (!view) {
    view = buildChat();
    main.replaceChildren(view);
  }
  S.unreadChat = 0;
  updateChat(view);
}

function buildChat() {
  const input = h('input', { id: 'msg', placeholder: 'Type a message', autocomplete: 'off' });
  const to = h('select', { 'aria-label': 'Recipient', id: 'to' });
  const via = h('select', { 'aria-label': 'Send via', id: 'via' });
  const estimate = h('span', { class: 'estimate', id: 'estimate' });
  const err = h('span', { class: 'error-text', id: 'send-err', role: 'alert' });
  const send = async () => {
    const body = input.value;
    if (!body.trim()) return;
    err.textContent = '';
    try {
      const line = await Engine.call('chat.send', { portId: via.value, dst: to.value, body });
      S.lines.set(line.id, line);
      input.value = '';
      render();
    } catch (e) {
      err.textContent = e.message;
    }
  };
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      send();
    }
  });
  input.addEventListener('input', () => updateEstimate());
  via.addEventListener('change', () => updateEstimate());
  to.addEventListener('change', () => updateEstimate());
  return h('div', { class: 'chat' },
    h('section', { class: 'convo', 'aria-labelledby': 'chan-title' },
      h('div', { class: 'pane-head' }, h('h1', { id: 'chan-title' }), h('span', { id: 'chan-tag' })),
      h('ul', { class: 'channels', id: 'channels', 'aria-label': 'Chat channels' }),
      h('ol', { class: 'log', id: 'log', role: 'log', 'aria-live': 'polite', 'aria-label': 'Chat' }),
      h('form', { class: 'compose', 'aria-label': 'Write a chat message', onsubmit: (e) => { e.preventDefault(); send(); } },
        h('div', { class: 'compose-row' },
          h('label', {}, 'To ', to), h('label', {}, 'Send via ', via), err, estimate),
        h('div', { class: 'compose-input' },
          h('label', { class: 'sr', for: 'msg' }, 'Message'), input,
          h('button', { class: 'btn primary', type: 'submit' }, 'Send')))),
    h('aside', { class: 'context', 'aria-label': 'Channel context' },
      h('h2', {}, 'Heard'), h('ul', { class: 'stations', id: 'heard' }),
      h('h2', {}, 'Waiting to send'), h('ul', { class: 'queue', id: 'queue' })));
}

function updateEstimate() {
  const via = $('#via');
  const est = $('#estimate');
  if (!via || !est) return;
  const port = S.ports.find((p) => p.id === via.value);
  const m = modeOf(port);
  const bytes = ($('#msg').value.length || 0) + 61;
  const bcast = $('#to').value === 'CQCQCQ' ? ' Broadcasts can’t be confirmed.' : '';
  est.textContent = m.rate ? `About ${(1.1 + bytes / m.rate).toFixed(1)} s on air.${bcast}` : `Internet: sent at once.${bcast}`;
  if (port && routeHeld(port)) {
    est.replaceChildren(h('span', { class: 'st pending' }, `${GATE_WORDS[port.route.gate]}: messages wait.`), ' ',
      h('button', { class: 'btn quiet', type: 'button', onclick: () => openPort(port.id) }, 'Review route'));
  } else if (port && port.route && port.route.scope === 'reflector') {
    est.textContent += ' Goes to every repeater on the linked reflector.';
  }
}

function updateChat(view) {
  const title = $('#chan-title', view);
  const port = S.ports.find((p) => p.id === S.channel);
  title.textContent = port ? port.name : 'All traffic';
  $('#chan-tag', view).replaceChildren(port ? h('span', { class: `tag ${modeOf(port).mode}` }, `${modeOf(port).label}`) : '');

  // channels
  $('#channels', view).replaceChildren(
    ...[...S.ports.map((p) => [p.id, p.name, p]), ['all', 'All traffic', null]].map(([id, name, p]) =>
      h('li', {}, h('a', {
        href: '#chat', 'aria-current': S.channel === id ? 'true' : null,
        onclick: async (e) => {
          e.preventDefault();
          S.channel = id;
          await loadChat();
          render();
        },
      }, name, p ? h('span', { class: `tag ${modeOf(p).mode}` }, modeOf(p).label) : null))));

  // log
  const log = $('#log', view);
  const atBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
  const lines = [...S.lines.values()].filter((l) => S.channel === 'all' || l.portId === S.channel);
  const sys = S.sys.filter((x) => S.channel === 'all' || !x.portId || x.portId === S.channel);
  const items = [
    ...lines.map((l) => ({ at: l.at, key: l.id, l })),
    ...sys.map((x, i) => ({ at: x.at, key: `s${i}`, x })),
  ].sort((a, b) => a.at - b.at || String(a.key).localeCompare(String(b.key))).slice(-400);
  log.replaceChildren(...(items.length ? items.map(({ l, x }) => {
    if (x) {
      return h('li', { class: 'line sys' }, h('time', {}, hms(x.at)), h('span', { class: 'who' }, x.who),
        h('span', { class: 'body' }, x.text, x.link ? [' ', h('a', { href: '#messages', style: 'color:var(--text-1)', onclick: () => { S.msgSel = x.link; S.folder = 'inbox'; } }, 'Open')] : null),
        h('span', { class: 'meta' }));
    }
    if (l.position) {
      const q = l.position;
      return h('li', { class: 'line sys' }, h('time', {}, hms(l.at)), h('span', { class: 'who' }, 'Position'),
        h('span', { class: 'body', title: l.body }, h('span', { class: 'call', style: 'color:var(--text-1)' }, q.callsign),
          ` ${fmtPos(q.lat, q.lon)}${q.comment ? `, “${q.comment}”` : ''} (${q.sourceFmt === 'dprs' ? 'DPRS' : 'NMEA'}${l.direction === 'out' ? ', sent by us' : ''})`),
        h('span', { class: 'meta' }));
    }
    const kind = l.kind === 'text' ? null : { ping_req: 'ping', ping_rsp: 'ping reply', echo_req: 'echo', echo_rsp: 'echo reply', status: 'status', raw: 'raw' }[l.kind];
    return h('li', { class: `line${l.direction === 'out' ? ' mine' : ''}${kind ? ' sys' : ''}` },
      h('time', { datetime: new Date(l.at).toISOString() }, hms(l.at)),
      h('span', { class: 'who call' }, l.src),
      h('span', { class: 'body' }, l.dst !== 'CQCQCQ' ? h('span', { class: 'faint' }, `to ${l.dst}: `) : null, kind ? `${kind}: ` : null, l.body),
      h('span', { class: 'meta' }, lineState(l)));
  }) : [h('li', { class: 'empty' }, 'Nothing heard yet. Messages on this channel appear here as they arrive.')]));
  if (atBottom) log.scrollTop = log.scrollHeight;

  // compose selects (keep the user's choice)
  const to = $('#to', view);
  const keepTo = to.value || 'CQCQCQ';
  const calls = [...S.stations.values()].filter((s) => s.callsign !== S.me).sort((a, b) => (b.lastHeard || 0) - (a.lastHeard || 0));
  to.replaceChildren(h('option', { value: 'CQCQCQ' }, 'Everyone (CQCQCQ)'), ...calls.map((s) => h('option', { value: s.callsign }, s.callsign)));
  to.value = calls.some((s) => s.callsign === keepTo) ? keepTo : 'CQCQCQ';
  const via = $('#via', view);
  const keepVia = via.value;
  via.replaceChildren(...channelPorts().map((p) => h('option', { value: p.id }, `${p.name}, ${modeOf(p).words}`)));
  if (channelPorts().some((p) => p.id === keepVia)) via.value = keepVia;
  updateEstimate();

  // aside
  const heard = [...S.stations.values()].filter((s) => s.lastHeard && (S.channel === 'all' || s.portId === S.channel))
    .sort((a, b) => b.lastHeard - a.lastHeard).slice(0, 30);
  $('#heard', view).replaceChildren(...(heard.length ? heard.map((s) => {
    const p = S.ports.find((x) => x.id === s.portId);
    return h('li', { title: s.software || '' }, h('span', { class: 'call' }, s.callsign),
      p ? h('span', { class: `tag ${modeOf(p).mode}` }, modeOf(p).label) : h('span'), h('span', { class: 'ago' }, ago(s.lastHeard)));
  }) : [h('li', { class: 'faint' }, 'No stations heard yet')]));
  const active = [...S.transfers.values()].filter((t) => !['done', 'failed', 'cancelled'].includes(t.state));
  $('#queue', view).replaceChildren(...(active.length ? active.map(transferCard) : [h('li', { class: 'faint' }, 'Nothing waiting')]));
}

function transferCard(t) {
  const pctv = t.totalBytes ? Math.round((100 * t.doneBytes) / t.totalBytes) : 0;
  return h('li', {},
    h('div', { class: 'q1' }, h('span', {}, `${t.filename} ${t.direction === 'send' ? 'to' : 'from'} `, h('span', { class: 'call' }, t.peer)),
      h('span', { class: 'st pending' }, t.state === 'opening' ? 'Opening' : t.state === 'offering' ? 'Offering' : t.direction === 'send' ? 'Sending' : 'Receiving')),
    h('div', { class: 'bar', role: 'progressbar', 'aria-valuenow': pctv, 'aria-valuemin': 0, 'aria-valuemax': 100, 'aria-label': `${t.filename} progress` },
      h('span', { style: `width:${pctv}%` })),
    h('div', { class: 'q2' }, h('span', {}, `${pctv}% of ${kb(t.totalBytes)}`), h('span', {}, t.retries ? `${t.retries} retries` : '')));
}

// ---------------------------------------------------------------- messages
const MSG_STATE = {
  draft: ['unknown', 'Draft'], queued: ['pending', 'Queued'], sending: ['pending', 'Sending'],
  handed_off: ['pending', 'Handed off'], delivered: ['ok', 'Delivered'], unconfirmed: ['unknown', 'Unconfirmed'],
  failed: ['fail', 'Failed'], expired: ['fail', 'Expired'], received: ['ok', 'Received'],
};
const EVIDENCE = { arq_ack: 'acknowledged by the station', operator: 'by you', tcp_accepted: 'accepted by the server', receipt: 'receipt', alp_complete: 'complete' };
const stBadge = (state) => {
  const [c, w] = MSG_STATE[state] || ['unknown', state];
  return h('span', { class: `st ${c}` }, w);
};

async function renderMessages(main) {
  S.unreadMsg = 0;
  // Never re-render over a form being written.
  if (S.composing && $('.compose-form', main)) return;
  const list = h('section', { class: 'list', 'aria-label': 'Messages' },
    h('div', { class: 'folders' }, ...['inbox', 'outbox', 'sent', 'drafts'].map((f) =>
      h('a', { href: '#messages', 'aria-current': S.folder === f ? 'true' : null, onclick: async (e) => { e.preventDefault(); S.folder = f; S.msgSel = null; S.composing = false; await loadMessages(); render(); } },
        f[0].toUpperCase() + f.slice(1))),
    h('button', { class: 'btn', style: 'margin-left:auto', onclick: () => { S.composing = true; S.msgSel = null; render(); } }, 'New ICS-213')),
    h('ul', { class: 'items', role: 'listbox', 'aria-label': `${S.folder} messages` }, ...(S.messages.length ? S.messages.map((m) =>
      h('li', { role: 'option', tabindex: 0, 'aria-selected': S.msgSel === m.id ? 'true' : 'false', onclick: () => { S.msgSel = m.id; S.composing = false; render(); },
        onkeydown: (e) => { if (e.key === 'Enter') { S.msgSel = m.id; S.composing = false; render(); } } },
        h('div', { class: 'r1' }, h('span', { class: 'call' }, S.folder === 'inbox' ? m.src : m.dst || '(no destination)'), h('span', { class: 'faint' }, hms(m.updatedAt))),
        h('div', { class: 'r2' }, m.subject || '(no subject)'),
        h('div', { class: 'r3' }, stBadge(m.state)))) : [h('li', { class: 'faint' }, 'No messages here')])));
  const doc = h('section', { class: 'doc', 'aria-label': 'Message' });
  const delivery = h('aside', { class: 'delivery', 'aria-label': 'Delivery' });
  main.replaceChildren(h('div', { class: 'msgview' }, list, doc, delivery));
  if (S.composing) {
    doc.replaceChildren(composeForm());
    return;
  }
  if (!S.msgSel) {
    doc.replaceChildren(h('p', { class: 'empty' }, 'Choose a message, or write a new ICS-213.'));
    return;
  }
  try {
    const [m, events] = await Promise.all([Engine.call('messages.get', { id: S.msgSel }), Engine.call('messages.events', { id: S.msgSel })]);
    const f = m.fields || {};
    const box = (label, value, span, extra) => h('div', { class: `box ${span}${extra ? ' ' + extra : ''}` }, h('span', { class: 'k' }, label), h('span', { class: 'v' }, value || ''));
    doc.replaceChildren(
      h('div', { class: 'doc-head' }, h('h1', {}, m.subject || '(no subject)'),
        h('div', { class: 'doc-actions' }, ['draft', 'failed'].includes(m.state) ? h('button', { class: 'btn primary', onclick: () => sendMessage(m.id) }, m.state === 'failed' ? 'Retry' : 'Send') : null)),
      h('p', { class: 'mid' }, `Message id ${m.mid}`),
      h('div', { class: 'ics' },
        h('div', { class: 'ics-title' }, h('b', {}, 'ICS-213 General Message'), h('span', { class: 'faint' }, m.formId || '')),
        h('div', { class: 'ics-grid' },
          box('Incident', f.incident, 'c2'), box('Date', f.date, 'c2'), box('Time', f.time, 'c2', 'edge'),
          box('To', f.recip, 'c3'), box('Position', f.to_pos, 'c3', 'edge'),
          box('From', f.sender, 'c3'), box('Position', f.from_pos, 'c3', 'edge'),
          box('Subject', f.subject, 'c6', 'edge'),
          box('Message', f.message, 'c6', 'body edge'),
          f.reply ? box('Reply', f.reply, 'c6', 'body edge') : null)));
    const [c, w] = MSG_STATE[m.state] || ['unknown', m.state];
    delivery.replaceChildren(
      h('h2', {}, 'Delivery'),
      h('div', { class: 'verdict' }, h('span', { class: `st ${c}` }, w),
        h('p', {}, m.state === 'handed_off' ? 'Every block was acknowledged by the station. D-RATS sends no delivery receipt, so this is the strongest evidence available.' : m.stateReason || '')),
      h('ol', { class: 'ladder' }, ...events.map((e) => h('li', { class: e.state === 'failed' ? 'fail' : 'done' },
        h('span', { class: 'what' }, (MSG_STATE[e.state] || [0, e.state])[1]),
        h('span', { class: 'when' }, hms(e.at)),
        h('span', { class: 'how' }, [e.portId, e.hop ? `${e.state === 'received' ? 'from' : 'to'} ${e.hop}` : null, EVIDENCE[e.evidence]].filter(Boolean).join(' · '))))));
  } catch (e) {
    doc.replaceChildren(h('p', { class: 'error-text' }, e.message));
  }
}

async function sendMessage(id) {
  try {
    await Engine.call('messages.send', { id });
    S.folder = 'outbox';
    await loadMessages();
  } catch (e) {
    alert(e.message);
  }
  render();
}

function composeForm() {
  const field = (id, label, span, multiline) => h('label', { class: span }, label,
    multiline ? h('textarea', { class: 'field', name: id }) : h('input', { class: 'field', name: id }));
  const err = h('p', { class: 'error-text', role: 'alert' });
  const via = h('select', { class: 'field', name: 'port' }, h('option', { value: '' }, 'Where the station was last heard'),
    ...S.ports.map((p) => h('option', { value: p.id }, p.name)));
  const form = h('form', {
    class: 'compose-form', 'aria-label': 'New ICS-213',
    onsubmit: async (e) => {
      e.preventDefault();
      const fd = new FormData(form);
      const fields = {};
      for (const k of ['incident', 'recip', 'to_pos', 'sender', 'from_pos', 'subject', 'message']) fields[k] = fd.get(k) || '';
      try {
        const m = await Engine.call('messages.createDraft', { formId: 'ICS213_US_OS', fields, dst: fd.get('dst') });
        await Engine.call('messages.send', { id: m.id, portId: fd.get('port') || undefined });
        S.composing = false;
        S.folder = 'outbox';
        S.msgSel = m.id;
        await loadMessages();
        render();
      } catch (ex) {
        err.textContent = ex.message;
      }
    },
  },
    h('label', { class: 'c2' }, 'Send to station (callsign)', h('input', { class: 'field', name: 'dst', required: true, autocomplete: 'off' })),
    h('label', { class: 'c2' }, 'Send via', via),
    field('incident', 'Incident', 'c2'),
    field('recip', 'To', 'c3'), field('to_pos', 'Position', 'c3'),
    field('sender', 'From', 'c3'), field('from_pos', 'Position', 'c3'),
    field('subject', 'Subject', 'c6'),
    field('message', 'Message', 'c6', true),
    h('div', { class: 'c6' }, err, h('button', { class: 'btn primary', type: 'submit' }, 'Send'), ' ',
      h('button', { class: 'btn quiet', type: 'button', onclick: () => { S.composing = false; render(); } }, 'Cancel')));
  return h('div', {}, h('div', { class: 'doc-head' }, h('h1', {}, 'New ICS-213')), form);
}

// ---------------------------------------------------------------- transfers
function renderTransfers(main) {
  const rows = [...S.transfers.values()].sort((a, b) => (a.id < b.id ? 1 : -1));
  main.replaceChildren(h('div', { class: 'xfer', style: 'grid-template-rows: 44px 1fr' },
    h('div', { class: 'pane-head' }, h('h1', {}, 'Transfers'),
      h('span', { class: 'muted' }, 'Send files with: aetherdv-cli send-file PORT CALL PATH')),
    h('div', { class: 'tablewrap' }, rows.length ? h('table', { class: 'grid' },
      h('thead', {}, h('tr', {}, ...['File', 'Direction', 'Station', 'Port', 'State', 'Progress', 'Retries', ''].map((c) => h('th', { scope: 'col' }, c)))),
      h('tbody', {}, ...rows.map((t) => {
        const p = S.ports.find((x) => x.id === t.portId);
        const pctv = t.totalBytes ? Math.round((100 * t.doneBytes) / t.totalBytes) : 0;
        const active = !['done', 'failed', 'cancelled'].includes(t.state);
        return h('tr', {},
          h('td', {}, t.filename, t.wrapWarning ? h('span', { class: 'note', title: 'Over 256 blocks: a D-RATS receiver can lose data without either side noticing (legacy defect P19).' }, ' ⚠ large') : null),
          h('td', {}, t.direction === 'send' ? 'Send' : 'Receive'),
          h('td', { class: 'call' }, t.peer),
          h('td', {}, p ? p.name : t.portId || ''),
          h('td', {}, t.state === 'done' ? h('span', { class: 'st ok', title: t.stateReason || '' }, t.direction === 'send' ? 'Handed off' : 'Received')
            : t.state === 'failed' ? h('span', { class: 'st fail', title: t.stateReason || '' }, t.stateReason || 'Failed')
            : h('span', { class: 'st pending', title: p && p.channel && p.channel.holdsTimers ? 'Timers are paused while the channel is held' : '' },
              p && (routeHeld(p) || (p.channel && p.channel.holdsTimers)) ? `${t.state}, ${routeHeld(p) ? GATE_WORDS[p.route.gate] : channelWords(p)}`.toLowerCase().replace(/^./, (c) => c.toUpperCase()) : t.state)),
          h('td', {}, h('div', { class: 'prog' }, h('div', { class: `bar ${p ? modeOf(p).mode : ''}`, role: 'progressbar', 'aria-valuenow': pctv, 'aria-valuemin': 0, 'aria-valuemax': 100, 'aria-label': `${t.filename} progress` }, h('span', { style: `width:${pctv}%` })), h('span', { class: 'num' }, `${pctv}%`))),
          h('td', { class: 'num' }, t.retries),
          h('td', {}, active ? h('button', { class: 'btn quiet', onclick: () => Engine.call('transfers.cancel', { id: t.id }).catch((e) => alert(e.message)) }, 'Cancel') : null));
      }))) : h('p', { class: 'empty' }, 'No transfers yet.'))));
}

// ---------------------------------------------------------------- route panel
// One port's route (radio UR/R1/R2 vs the scope) and channel (docs/10 §5).
function openPort(id) {
  S.portOpen = id;
  let dlg = $('#port-dialog');
  if (!dlg) {
    dlg = h('dialog', { id: 'port-dialog', class: 'sheet', 'aria-labelledby': 'port-title' });
    dlg.addEventListener('close', () => { S.portOpen = null; });
    document.body.append(dlg);
  }
  renderPort();
  if (!dlg.open) dlg.showModal();
}

function callsRow(label, have, need) {
  const shown = (v) => (v === '' ? '(blank)' : v);
  const differs = have != null && need != null && have !== need;
  return h('tr', {}, h('th', { scope: 'row' }, label),
    h('td', { class: `call${differs ? ' differs' : ''}` }, have == null ? '—' : shown(have)),
    h('td', { class: 'call' }, need == null ? '—' : shown(need)));
}

function renderPort() {
  const dlg = $('#port-dialog');
  const p = S.ports.find((x) => x.id === S.portOpen);
  if (!dlg || !p) return;
  const r = p.route || {};
  const c = p.channel || {};
  const out = h('p', { class: 'error-text', role: 'alert' });
  const call = (method, params, label) => h('button', {
    class: 'btn', type: 'button', onclick: async () => {
      out.textContent = '';
      try {
        const res = await Engine.call(method, { id: p.id, ...(params || {}) });
        if (res && res.gate) p.route = res;
        render();
      } catch (e) {
        out.textContent = e.message;
      }
    },
  }, label);
  const obs = r.observed && r.observed.calls;
  const req = r.required;
  const reach = r.scope === 'reflector'
    ? `Every repeater and hotspot linked to ${r.link ? r.link.reflector : `whatever ${r.repeater} is linked to right now`}. That can change at any time.`
    : r.scope === 'repeater'
      ? `Stations in range of ${r.repeater}. Stays local even when the repeater is linked to a reflector.`
      : 'Stations in range of this radio.';
  const route = [];
  if (r.gate === 'confirm') {
    if (r.needs && r.needs.route) {
      route.push(h('p', {}, 'This radio can’t report its call signs to Aether DV. Check them on the radio’s screen. Storing them in a memory channel used only for data keeps them from changing.'));
    }
    if (r.needs && r.needs.reflector) {
      route.push(h('p', { class: 'warn' }, `Data on this port goes to ${r.link ? `every repeater linked to ${r.link.reflector}` : `whatever ${r.repeater} is linked to`}. Keep messages short and files small.`));
    }
  } else if (r.message) {
    route.push(h('p', { class: r.gate === 'blocked' ? 'error-text' : 'warn' }, r.message));
  }
  const buttons = [];
  if (r.gate === 'confirm') {
    buttons.push(h('button', { class: 'btn primary', type: 'button', onclick: async () => {
      out.textContent = '';
      try {
        p.route = await Engine.call('ports.confirmRoute', { id: p.id });
        render();
      } catch (e) {
        out.textContent = e.message;
      }
    } }, r.needs && r.needs.route ? 'The radio matches' : 'Send to the reflector'));
  }
  if (r.civRoute) buttons.push(call('ports.verifyRoute', null, 'Check the radio again'));
  if (r.canApply && ['held'].includes(r.gate)) buttons.push(call('ports.applyRoute', { confirm: true }, 'Set the radio for this route'));
  if (r.via === 'repeater') {
    buttons.push(h('a', { class: 'btn quiet', href: '#radios', onclick: () => $('#port-dialog').close() }, 'Radio setup help'));
  }
  const src = (c.sources || []).filter((x) => x !== 'data');
  const sensing = c.sensesVoice
    ? 'Data and voice are detected from the radio (CI-V).'
    : src.includes('inferred_voice')
      ? 'Only data is detected directly. Voice is noticed only when the speaker’s radio sends position data, so pause sending during nets.'
      : 'Only data is detected. Voice is not, so pause sending during voice contacts and nets.';
  const budget = c.airBudgetMs || 0;
  const used = Math.min(c.airUsedMs || 0, budget);
  const check = S.checks.get(p.id);
  const paused = c.why === 'paused';
  // Keep keyboard focus on the same control across re-renders.
  const focused = dlg.contains(document.activeElement) ? document.activeElement.textContent : null;
  dlg.replaceChildren(...[
    h('div', { class: 'sheet-head' }, h('h2', { id: 'port-title' }, p.name),
      h('span', { class: 'muted' }, SCOPE_WORDS[r.scope] || ''),
      h('button', { class: 'btn quiet', type: 'button', 'aria-label': 'Close', onclick: () => dlg.close() }, '✕')),
    h('section', {}, h('h3', {}, 'Who receives your data'), h('p', {}, reach),
      r.lastAck ? h('p', { class: 'muted' }, `Repeater last acknowledged us at ${hms(r.lastAck.at)}${r.lastAck.ber ? `, BER ${r.lastAck.ber}` : ''}.`) : null),
    r.via === 'repeater' ? h('section', {}, h('h3', {}, 'Route'),
      h('p', {}, h('span', { class: `st ${r.gate === 'open' ? 'ok' : r.gate === 'blocked' ? 'fail' : 'pending'}` }, GATE_WORDS[r.gate] || r.gate),
        h('span', { class: 'muted' }, r.source === 'civ' ? ' Read from the radio.' : r.source === 'operator' ? ' Confirmed by you.' : '')),
      ...route,
      obs
        ? h('table', { class: 'calls' }, h('thead', {}, h('tr', {}, h('th', {}), h('th', { scope: 'col' }, 'Radio'), h('th', { scope: 'col' }, 'Needed'))),
          h('tbody', {}, callsRow('UR', obs.ur, req && req.ur), callsRow('R1', obs.r1, req && req.r1), callsRow('R2', obs.r2, req && req.r2)))
        : req ? h('table', { class: 'calls' }, h('thead', {}, h('tr', {}, h('th', {}), h('th', { scope: 'col' }, 'Set on the radio'))),
          h('tbody', {}, ...[['UR', req.ur], ['R1', req.r1], ['R2', req.r2]].map(([k, v]) => h('tr', {}, h('th', { scope: 'row' }, k), h('td', { class: 'call' }, v === '' ? '(blank)' : v))))) : null,
      r.myCallMatches === false ? h('p', { class: 'warn' }, `The radio’s MY call is ${r.myCall}; this station is ${S.me}.`) : null,
      h('div', { class: 'sheet-actions' }, ...buttons)) : null,
    h('section', {}, h('h3', {}, 'Channel'),
      h('p', {}, channelWords(p) || 'Clear', c.queued ? h('span', { class: 'muted' }, `, ${c.queued} waiting`) : null),
      h('p', { class: 'muted' }, sensing),
      budget ? h('div', { class: 'meter' }, h('div', { class: 'bar', role: 'meter', 'aria-valuenow': Math.round(used / 1000), 'aria-valuemin': 0, 'aria-valuemax': Math.round(budget / 1000), 'aria-label': 'Air time used' },
        h('span', { style: `width:${Math.round((100 * used) / budget)}%` })),
        h('span', { class: 'muted' }, `${Math.round(used / 1000)} of ${Math.round(budget / 1000)} s air time used in the last 10 minutes`)) : null,
      h('div', { class: 'sheet-actions' },
        paused ? call('ports.resume', null, 'Resume sending') : call('ports.pause', null, 'Pause sending'),
        paused ? null : call('ports.pause', { minutes: 30 }, 'Pause for 30 minutes'))),
    r.via === 'repeater' && r.civRoute ? h('section', {}, h('h3', {}, 'Repeater access'),
      h('p', { class: 'muted' }, 'Sends one short check and reads the repeater’s acknowledgement.'),
      check ? h('p', {}, check.ok
        ? h('span', { class: 'st ok' }, `Acknowledged${check.ber ? `, BER ${check.ber}` : ''}${check.link ? `, linked to ${check.link}` : ', not linked'}`)
        : h('span', { class: 'st fail' }, check.message)) : null,
      h('div', { class: 'sheet-actions' }, call('diag.repeaterCheck', null, 'Check access'))) : null,
    out].filter(Boolean));
  if (focused) {
    const again = [...dlg.querySelectorAll('button, a')].find((b) => b.textContent === focused);
    if (again) again.focus();
  }
}

// ---------------------------------------------------------------- stations
function renderStations(main) {
  const rows = [...S.stations.values()].sort((a, b) => (b.lastHeard || 0) - (a.lastHeard || 0));
  main.replaceChildren(h('div', { class: 'xfer', style: 'grid-template-rows: 44px 1fr' },
    h('div', { class: 'pane-head' }, h('h1', {}, 'Stations')),
    h('div', { class: 'tablewrap' }, rows.length ? h('table', { class: 'grid' },
      h('thead', {}, h('tr', {}, ...['Callsign', 'Last heard', 'Software', 'Position', 'Comment'].map((c) => h('th', { scope: 'col' }, c)))),
      h('tbody', {}, ...rows.map((s) => {
        const pos = S.positions.get(s.callsign);
        return h('tr', {}, h('td', { class: 'call' }, s.callsign), h('td', {}, s.lastHeard ? `${ago(s.lastHeard)} ago` : ''),
          h('td', {}, s.software || ''), h('td', {}, pos ? fmtPos(pos.lat, pos.lon) : ''), h('td', {}, pos ? pos.comment || '' : ''));
      }))) : h('p', { class: 'empty' }, 'No stations heard yet.'))));
}

// ---------------------------------------------------------------- diagnostics
// Radio test: record everything on every port, add markers, then hand the
// summary (paste) and bundle (upload) to the developer.
const UPLOAD_URL = 'https://github.com/KK4ODA/aether-dv/upload/test-results/bundles';
const Test = { status: { running: false }, result: null, error: '', busy: false };
const QUICK_MARKS = ['Keyed voice', 'Voice ended', 'Changed a radio setting', 'Something went wrong'];

async function loadTestStatus() {
  try {
    Test.status = await Engine.call('diag.testStatus');
  } catch {
    Test.status = { running: false };
  }
}

setInterval(() => {
  if (S.connected && S.view === 'diagnostics' && Test.status.running) render();
}, 1000);

async function testCall(method, params) {
  Test.error = '';
  Test.busy = true;
  render();
  try {
    return await Engine.call(method, params);
  } catch (e) {
    Test.error = e.message;
    return null;
  } finally {
    Test.busy = false;
    render();
  }
}

async function copyText(text, area) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    if (area) {
      area.focus();
      area.select();
      try {
        return document.execCommand('copy');
      } catch {
        return false;
      }
    }
    return false;
  }
}

function testCard(state) {
  if (state === 'idle') {
    const what = h('input', { id: 'test-what', placeholder: 'What are you testing? (e.g. simplex, IC-9700 to ID-52 PLUS)', autocomplete: 'off' });
    const start = async () => {
      const r = await testCall('diag.testStart', { description: what.value });
      if (r) Test.status = r;
    };
    what.addEventListener('keydown', (e) => { if (e.key === 'Enter') start(); });
    return h('section', { class: 'testcard', 'aria-labelledby': 'test-h' },
      h('h2', { id: 'test-h' }, 'Radio test'),
      h('p', { class: 'muted' }, 'Records everything on every port: data, CI-V and timing. Mark what you do as you go, then send the summary.'),
      h('div', { class: 'test-row' }, what, h('button', { class: 'btn primary', id: 'test-start', onclick: start }, 'Start test')),
      h('p', { class: 'error-text', id: 'test-err', role: 'alert' }));
  }
  if (state === 'running') {
    const note = h('input', { id: 'test-note', placeholder: 'What just happened?', autocomplete: 'off' });
    const mark = async (text) => {
      const r = await testCall('diag.testMark', { text });
      if (r) {
        Test.status.markers = [...(Test.status.markers || []), r];
        note.value = '';
      }
    };
    note.addEventListener('keydown', (e) => { if (e.key === 'Enter' && note.value.trim()) mark(note.value); });
    return h('section', { class: 'testcard running', 'aria-labelledby': 'test-h' },
      h('h2', { id: 'test-h' }, h('span', { class: 'rec', 'aria-hidden': 'true' }), 'Recording ', h('span', { id: 'test-elapsed' })),
      h('p', { class: 'muted', id: 'test-desc' }),
      h('div', { class: 'test-row' }, ...QUICK_MARKS.map((m) => h('button', { class: 'btn', onclick: () => mark(m) }, m))),
      h('div', { class: 'test-row' }, note, h('button', { class: 'btn', onclick: () => note.value.trim() && mark(note.value) }, 'Add marker')),
      h('ol', { class: 'marks', id: 'test-marks', 'aria-label': 'Markers' }),
      h('div', { class: 'test-row' }, h('button', {
        class: 'btn primary', id: 'test-finish', onclick: async () => {
          const r = await testCall('diag.testFinish');
          if (r) {
            Test.result = r;
            Test.status = { running: false };
          }
        },
      }, 'Finish test')),
      h('p', { class: 'error-text', id: 'test-err', role: 'alert' }));
  }
  const r = Test.result;
  const area = h('textarea', { class: 'summary', readonly: '', rows: 8, 'aria-label': 'Test summary' });
  area.value = r.summary;
  const copied = h('span', { class: 'muted', role: 'status' });
  const tauri = window.__TAURI__ && window.__TAURI__.core;
  return h('section', { class: 'testcard done', 'aria-labelledby': 'test-h' },
    h('h2', { id: 'test-h' }, 'Test saved'),
    h('ol', { class: 'steps' },
      h('li', {}, h('button', {
        class: 'btn primary', onclick: async () => {
          copied.textContent = (await copyText(r.summary, area)) ? ' Copied. Paste it into the chat with Claude.' : ' Select the text below and copy it.';
        },
      }, 'Copy summary'), copied),
      h('li', {}, 'Only if asked for the full bundle: ',
        tauri ? h('button', { class: 'btn', onclick: () => tauri.invoke('reveal_bundle', { path: r.path }).catch((e) => { Test.error = String(e); render(); }) }, 'Show file') : null,
        ' ', h('button', { class: 'btn', onclick: () => openLink(UPLOAD_URL) }, 'Upload for the developer'),
        h('span', { class: 'muted' }, ' then drag the file onto the page and click Commit changes.'))),
    h('p', { class: 'muted' }, `${r.path} (${Math.max(1, Math.round(r.bytes / 1024))} kB)`),
    area,
    h('div', { class: 'test-row' }, h('button', { class: 'btn quiet', onclick: () => { Test.result = null; render(); } }, 'Start another test')),
    h('p', { class: 'error-text', id: 'test-err', role: 'alert' }));
}

function updateTestCard(card) {
  const err = $('#test-err', card);
  if (err) err.textContent = Test.error;
  for (const b of card.querySelectorAll('button')) b.disabled = Test.busy;
  if (!Test.status.running) return;
  const secs = Math.max(0, Math.round((Date.now() - Test.status.startedAt) / 1000));
  $('#test-elapsed', card).textContent = `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, '0')}`;
  $('#test-desc', card).textContent = Test.status.description || 'No description';
  const marks = Test.status.markers || [];
  const list = $('#test-marks', card);
  if (list.childElementCount !== marks.length) {
    list.replaceChildren(...marks.map((m) => h('li', {}, h('time', {}, hms(m.at)), ' ', m.text)));
  }
}

function renderDiagnostics(main) {
  let view = $('.diag', main);
  if (!view) {
    view = h('div', { class: 'xfer diag', style: 'grid-template-rows: auto auto 1fr' },
      h('div', { class: 'pane-head', id: 'diag-head', style: 'height:auto; flex-wrap:wrap; padding-block:var(--s3)' }),
      h('div', { id: 'diag-test' }),
      h('div', { class: 'tablewrap' }, h('table', { class: 'grid' },
        h('thead', {}, h('tr', {}, ...['Time', 'Level', 'Port', 'Event'].map((c) => h('th', { scope: 'col' }, c)))),
        h('tbody', { id: 'diag-events' }))));
    main.replaceChildren(view);
  }
  const out = h('p', { class: 'muted', role: 'status' });
  const act = (label, method, params) => h('button', {
    class: 'btn', onclick: async () => {
      try {
        const r = await Engine.call(method, params);
        out.textContent = r.path ? `${label}: ${r.path}` : `${label}: done`;
      } catch (e) {
        out.textContent = e.message;
      }
    },
  }, label);
  const about = Shell.info
    ? h('span', { class: 'muted' }, `Aether DV ${Shell.info.version}`,
      Shell.info.updatesConfigured
        ? [' · ', h('button', { class: 'btn quiet', onclick: () => Shell.check(false) }, Shell.state === 'checking' ? 'Checking…' : 'Check for updates'),
          Shell.state === 'none' ? ' You have the latest version.' : '',
          Shell.state === 'error' && !Shell.update ? h('span', { class: 'error-text' }, ` ${Shell.message}`) : '']
        : ' · this build cannot update itself (no release key)')
    : null;
  const head = $('#diag-head', view);
  if (!head.contains(document.activeElement)) {
    head.replaceChildren(...[h('h1', {}, 'Diagnostics'), about,
      ...S.ports.filter((p) => p.route && p.route.via === 'repeater' && p.route.civRoute).map((p) => act(`Check access to ${p.route.repeater}`, 'diag.repeaterCheck', { id: p.id })),
      act('Write support bundle', 'diag.supportBundle', { includeCaptures: true }), out].filter(Boolean));
  }
  const state = Test.result ? 'done' : Test.status.running ? 'running' : 'idle';
  const slot = $('#diag-test', view);
  if (slot.dataset.state !== state) {
    slot.dataset.state = state;
    slot.replaceChildren(testCard(state));
    const first = $('#test-note', slot) || $('#test-what', slot);
    if (first && state !== 'done') first.focus();
  }
  updateTestCard(slot.firstChild);
  $('#diag-events', view).replaceChildren(...S.events.slice().reverse().map((e) => h('tr', {}, h('td', {}, hms(e.at)),
    h('td', {}, e.level === 'warn' || e.level === 'error' ? h('span', { class: 'st fail' }, e.level) : e.level),
    h('td', {}, e.portId || ''), h('td', { style: 'white-space:normal' }, e.message))));
}

// ---------------------------------------------------------------- radio setup
// Guides are data (radio-guides.json), maintained from operators' reports
// (docs/radio-guides.md). Every string is shown as text; links open in the
// system browser.
const Guides = { data: null, error: '', sel: null, open: new Set(['icom']) };

async function loadGuides() {
  if (Guides.data || Guides.error) return;
  try {
    const r = await fetch('radio-guides.json', { cache: 'no-store' });
    Guides.data = await r.json();
  } catch (e) {
    Guides.error = `Radio guides could not be loaded: ${e.message}`;
  }
  render();
}

function openLink(url) {
  const t = window.__TAURI__ && window.__TAURI__.core;
  if (t) t.invoke('open_link', { url }).catch((e) => alert(String(e)));
  else window.open(url, '_blank', 'noopener');
}

const GUIDE_STATUS = {
  documented: ['unknown', 'From manuals only'],
  reported: ['pending', 'Reported working'],
  tested: ['ok', 'Tested with Aether DV'],
};

function findRadio(id) {
  for (const b of (Guides.data && Guides.data.brands) || []) {
    const r = b.radios.find((x) => x.id === id);
    if (r) return [b, r];
  }
  return [null, null];
}

function renderRadios(main) {
  loadGuides();
  const d = Guides.data;
  const tree = h('ul', { class: 'gtree', role: 'tree', 'aria-label': 'Radios by brand' });
  const items = [];
  const select = (id) => { Guides.sel = id; render(); };
  for (const b of (d && d.brands) || []) {
    const open = Guides.open.has(b.id);
    const bi = h('li', { role: 'treeitem', 'aria-expanded': open ? 'true' : 'false', 'aria-selected': Guides.sel === b.id ? 'true' : 'false', tabindex: -1, 'data-id': b.id, class: 'gbrand' },
      h('span', { class: 'gtw', 'aria-hidden': 'true' }, open ? '▾' : '▸'), b.name, h('span', { class: 'faint' }, ` ${b.radios.length}`));
    bi.addEventListener('click', (e) => { e.stopPropagation(); if (open) Guides.open.delete(b.id); else Guides.open.add(b.id); Guides.sel = b.id; render(); });
    items.push(bi);
    if (open) {
      const grp = h('ul', { role: 'group' }, ...b.radios.map((r) => {
        const [c] = GUIDE_STATUS[r.status] || ['unknown'];
        const li = h('li', { role: 'treeitem', 'aria-selected': Guides.sel === r.id ? 'true' : 'false', tabindex: -1, 'data-id': r.id, class: 'gmodel' },
          r.name, h('span', { class: `st ${c}`, 'aria-hidden': 'true' }));
        li.addEventListener('click', (e) => { e.stopPropagation(); select(r.id); });
        items.push(li);
        return li;
      }));
      bi.append(grp);
    }
    tree.append(bi);
  }
  // Roving tabindex + arrow keys (WAI-ARIA tree pattern).
  const cur = items.find((i) => i.getAttribute('aria-selected') === 'true') || items[0];
  if (cur) cur.tabIndex = 0;
  tree.addEventListener('keydown', (e) => {
    const i = items.indexOf(document.activeElement);
    if (i < 0) return;
    const it = items[i];
    const id = it.dataset.id;
    const isBrand = it.classList.contains('gbrand');
    const move = (j) => { if (items[j]) { Guides.sel = items[j].dataset.id; render(); requestAnimationFrame(() => { const n = $(`.gtree [data-id="${CSS.escape(items[j].dataset.id)}"]`); if (n) n.focus(); }); } };
    if (e.key === 'ArrowDown') move(i + 1);
    else if (e.key === 'ArrowUp') move(i - 1);
    else if (e.key === 'ArrowRight' && isBrand && !Guides.open.has(id)) { Guides.open.add(id); render(); requestAnimationFrame(() => { const n = $(`.gtree [data-id="${id}"]`); if (n) n.focus(); }); }
    else if (e.key === 'ArrowLeft' && isBrand && Guides.open.has(id)) { Guides.open.delete(id); render(); requestAnimationFrame(() => { const n = $(`.gtree [data-id="${id}"]`); if (n) n.focus(); }); }
    else if (e.key === 'ArrowLeft' && !isBrand) { const b = d.brands.find((x) => x.radios.some((r) => r.id === id)); if (b) { Guides.sel = b.id; render(); requestAnimationFrame(() => { const n = $(`.gtree [data-id="${b.id}"]`); if (n) n.focus(); }); } }
    else return;
    e.preventDefault();
  });

  const [brand, radio] = findRadio(Guides.sel);
  const linkList = (links) => h('ul', { class: 'glinks' }, ...links.map((l) =>
    h('li', {}, h('span', { class: 'tag' }, l.kind), ' ',
      h('a', { href: l.url, rel: 'noopener', onclick: (e) => { e.preventDefault(); openLink(l.url); } }, l.title),
      h('span', { class: 'faint' }, ` ${new URL(l.url).hostname}`))));
  let detail;
  if (Guides.error) detail = h('p', { class: 'error-text' }, Guides.error);
  else if (!d) detail = h('p', { class: 'muted' }, 'Loading…');
  else if (!radio) {
    const b = d.brands.find((x) => x.id === Guides.sel);
    detail = h('div', {},
      h('h1', {}, b ? `${b.name} radios` : 'Radio setup'),
      h('p', { class: 'muted' }, 'Choose your radio on the left. These steps apply to every radio:'),
      h('ol', { class: 'gsteps' }, ...d.general.map((g) => h('li', {}, g))),
      h('h2', {}, 'How sure is this?'),
      h('dl', { class: 'gfacts' }, ...Object.entries(d.statusLevels).flatMap(([k, v]) => {
        const [c, w] = GUIDE_STATUS[k] || ['unknown', k];
        return [h('dt', {}, h('span', { class: `st ${c}` }, w)), h('dd', {}, v)];
      })),
      h('p', { class: 'faint' }, `Guides updated ${d.updated}. Tried a radio? Share what worked (menu settings, firmware, what you sent) with the project, and it will be added here.`));
  } else {
    const [c, w] = GUIDE_STATUS[radio.status] || ['unknown', radio.status];
    detail = h('div', {},
      h('div', { class: 'doc-head' }, h('h1', {}, `${brand.name} ${radio.name}`), h('span', { class: `st ${c}` }, w)),
      h('p', {}, radio.summary),
      h('dl', { class: 'gfacts' },
        h('dt', {}, 'DV slow data'), h('dd', {}, radio.slowData),
        h('dt', {}, 'DV Fast Data'), h('dd', {}, radio.fastData),
        h('dt', {}, 'Connection'), h('dd', {}, radio.connection),
        radio.profile ? [h('dt', {}, 'Aether profile'), h('dd', {}, h('code', {}, radio.profile))] : null),
      ...radio.steps.map((g) => [h('h2', {}, g.title), h('ol', { class: 'gsteps' }, ...g.items.map((x) => h('li', {}, x)))]),
      h('h2', {}, 'Links'),
      radio.links.length ? linkList(radio.links) : h('p', { class: 'faint' }, 'No links yet.'),
      h('h2', {}, `Field reports (${radio.reports.length})`),
      radio.reports.length ? h('ul', { class: 'greports' }, ...radio.reports.slice().sort((a, b) => (a.date < b.date ? 1 : -1)).map((r) =>
        h('li', {},
          h('div', { class: 'r1' }, h('b', {}, r.summary)),
          h('div', { class: 'faint' }, [r.date, r.reporter, r.source, r.software, r.firmware ? `firmware ${r.firmware}` : null].filter(Boolean).join(' · ')),
          r.setup ? h('div', {}, `Setup: ${r.setup}`) : null,
          r.worked && r.worked.length ? h('div', {}, h('span', { class: 'st ok' }, 'Worked'), ' ', r.worked.join('; ')) : null,
          r.problems && r.problems.length ? h('div', {}, h('span', { class: 'st fail' }, 'Problems'), ' ', r.problems.join('; ')) : null,
          r.url ? h('a', { href: r.url, rel: 'noopener', onclick: (e) => { e.preventDefault(); openLink(r.url); } }, 'Original post') : null)))
        : h('p', { class: 'faint' }, 'No reports yet. If you use this radio with Aether DV or D-RATS, share what worked.'));
  }
  main.replaceChildren(h('div', { class: 'guides' },
    h('nav', { class: 'gnav', 'aria-label': 'Radio setup' }, h('div', { class: 'pane-head' }, h('h1', {}, 'Radio setup')), tree),
    h('section', { class: 'doc', 'aria-live': 'polite' }, detail)));
}

// ---------------------------------------------------------------- sounds
// Short tones made with Web Audio (no audio files). Each event has its own
// pattern and its own switch; settings stay on this computer. Sounds only
// accompany what the screen shows, never replace it.
const SOUND_EVENTS = [
  { id: 'direct', label: 'Chat addressed to me', notes: [[660, 90], [990, 140]], on: true },
  { id: 'chat', label: 'Chat to everyone (CQCQCQ)', notes: [[880, 70]], on: false },
  { id: 'message', label: 'Message (ICS-213) received', notes: [[523, 110], [659, 110], [784, 180]], on: true },
  { id: 'file', label: 'File received', notes: [[784, 100], [1047, 160]], on: true },
  { id: 'sent', label: 'Message or file handed off', notes: [[1047, 60], [1047, 60]], on: false },
  { id: 'failed', label: 'Send or transfer failed', notes: [[440, 180], [330, 260]], on: true, wave: 'square' },
  { id: 'linkLost', label: 'Port lost (radio or ratflector)', notes: [[300, 160], [300, 160]], on: true, wave: 'square' },
  { id: 'linkRestored', label: 'Port back up', notes: [[440, 90], [660, 140]], on: true },
  { id: 'position', label: 'Position report received', notes: [[1320, 40]], on: false },
  { id: 'update', label: 'App update available', notes: [[587, 120], [880, 200]], on: true },
];

const Sounds = {
  ctx: null,
  last: {},
  settings: (() => {
    const def = { enabled: true, volume: 0.5, events: Object.fromEntries(SOUND_EVENTS.map((e) => [e.id, e.on])) };
    try {
      const s = JSON.parse(localStorage.getItem('aether.sounds') || 'null');
      return s ? { ...def, ...s, events: { ...def.events, ...(s.events || {}) } } : def;
    } catch {
      return def;
    }
  })(),
  save() {
    try { localStorage.setItem('aether.sounds', JSON.stringify(this.settings)); } catch { /* not persisted */ }
  },
  play(id, force) {
    const ev = SOUND_EVENTS.find((e) => e.id === id);
    if (!ev) return;
    if (!force && (!this.settings.enabled || !this.settings.events[id])) return;
    const now = Date.now();
    if (!force && this.last[id] && now - this.last[id] < 1500) return; // no machine-gunning
    this.last[id] = now;
    try {
      this.ctx = this.ctx || new (window.AudioContext || window.webkitAudioContext)();
      if (this.ctx.state === 'suspended') this.ctx.resume();
      let t = this.ctx.currentTime + 0.02;
      const vol = Math.max(0, Math.min(1, this.settings.volume)) * 0.25;
      for (const [f, ms] of ev.notes) {
        const o = this.ctx.createOscillator();
        const g = this.ctx.createGain();
        o.type = ev.wave || 'sine';
        o.frequency.value = f;
        const d = ms / 1000;
        g.gain.setValueAtTime(0, t);
        g.gain.linearRampToValueAtTime(vol, t + 0.01);
        g.gain.exponentialRampToValueAtTime(0.0001, t + d);
        o.connect(g).connect(this.ctx.destination);
        o.start(t);
        o.stop(t + d + 0.02);
        t += d + 0.04;
      }
    } catch { /* no audio device: stay silent */ }
  },
};

function renderSounds(main) {
  const st = Sounds.settings;
  const vol = h('input', { type: 'range', min: 0, max: 1, step: 0.05, value: st.volume, id: 'snd-vol', 'aria-label': 'Volume' });
  vol.addEventListener('change', () => { st.volume = Number(vol.value); Sounds.save(); Sounds.play('direct', true); });
  main.replaceChildren(h('div', { class: 'doc sounds' },
    h('div', { class: 'doc-head' }, h('h1', {}, 'Sounds')),
    h('p', { class: 'muted' }, 'Short sounds for events, so you notice traffic while looking elsewhere. Each one can be turned on or off; they are saved on this computer.'),
    h('div', { class: 'srow master' },
      h('label', {}, h('input', { type: 'checkbox', checked: st.enabled ? true : null, onchange: (e) => { st.enabled = e.target.checked; Sounds.save(); render(); } }), ' Play sounds'),
      h('label', {}, 'Volume ', vol)),
    h('table', { class: 'grid', 'aria-label': 'Sound for each event' },
      h('thead', {}, h('tr', {}, h('th', { scope: 'col' }, 'On'), h('th', { scope: 'col' }, 'Event'), h('th', { scope: 'col' }, ''))),
      h('tbody', {}, ...SOUND_EVENTS.map((ev) => {
        const id = `snd-${ev.id}`;
        return h('tr', {},
          h('td', {}, h('input', { type: 'checkbox', id, checked: st.events[ev.id] ? true : null, disabled: st.enabled ? null : true, onchange: (e) => { st.events[ev.id] = e.target.checked; Sounds.save(); } })),
          h('td', {}, h('label', { for: id }, ev.label)),
          h('td', {}, h('button', { class: 'btn quiet', type: 'button', onclick: () => Sounds.play(ev.id, true), 'aria-label': `Play the sound for ${ev.label}` }, 'Play')));
      })))));
}

// ---------------------------------------------------------------- routing
function route() {
  const v = (location.hash || '#chat').slice(1);
  S.view = ['chat', 'messages', 'transfers', 'stations', 'radios', 'sounds', 'diagnostics'].includes(v) ? v : 'chat';
  if (S.view !== 'chat') $('#main').replaceChildren();
  if (S.view === 'messages' && S.connected) loadMessages().then(render);
  render();
}
window.addEventListener('hashchange', route);
setInterval(() => { if (S.connected && S.view === 'chat') render(); }, 10000); // refresh "ago"
route();
Shell.init();
Engine.connect();
