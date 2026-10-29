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
      if (p) {
        p.state = d.state;
        p.detail = d.detail;
      }
      break;
    }
    case 'chat.line':
      if (!S.lines.has(d.id) && d.direction === 'in' && S.view !== 'chat') S.unreadChat++;
      S.lines.set(d.id, d);
      break;
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
    case 'transfer.progress':
      S.transfers.set(d.id, d);
      break;
    case 'message.state': {
      const m = d.message;
      if (m.state === 'received') {
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
  if (!S.connected) {
    renderConnect();
    return;
  }
  $('#mycall').textContent = S.me;
  renderStrip();
  renderRail();
  renderDrawer();
  const main = $('#main');
  const views = { chat: renderChat, messages: renderMessages, transfers: renderTransfers, stations: renderStations, diagnostics: renderDiagnostics };
  (views[S.view] || renderChat)(main);
}

function renderStrip() {
  const strip = $('#strip');
  strip.replaceChildren(
    ...S.ports.map((p) => {
      const m = modeOf(p);
      const word = PORT_WORDS[p.state] || p.state;
      const down = ['absent', 'lost', 'auth_failed', 'backoff'].includes(p.state);
      const heard = [...S.stations.values()].filter((s) => s.portId === p.id && s.lastHeard && Date.now() - s.lastHeard < 3600e3).length;
      return h('li', { class: 'plate', 'data-mode': m.mode, 'data-state': down ? 'down' : 'ok', 'aria-label': `${p.name}, ${m.words}, ${word}` },
        h('span', { class: 'plate-name' }, p.name),
        h('span', { class: 'plate-mode' }, m.label),
        h('span', { class: 'plate-state' }, down ? h('span', { class: 'st fail' }, word) : word),
        h('span', { class: 'plate-nums' }, p.detail && down ? p.detail : [h('b', {}, heard), ' heard in the last hour']));
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
  const tauri = window.__TAURI__ && window.__TAURI__.core;
  const tokenFile = h('input', { class: 'field', id: 'tokfile', value: localStorage.getItem('aether.tokenFile') || '', placeholder: 'aetherd.token next to your database' });
  const api = h('input', { class: 'field', id: 'apiaddr', value: localStorage.getItem('aether.api') || '127.0.0.1:7373' });
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
      h('p', {}, 'The engine (aetherd) runs separately and owns the radios. Start it with your configuration file; it writes an access token file when it starts.'),
      S.lastError ? h('p', { class: 'error-text', role: 'alert' }, S.lastError) : null,
      tauri ? h('label', {}, 'Token file', tokenFile) : h('p', {}, 'In a browser preview, pass ?api=ADDRESS&token=TOKEN in the address.'),
      tauri ? h('label', {}, 'Engine address', api) : null,
      h('div', {}, h('button', { class: 'btn primary', type: 'submit' }, 'Connect'))));
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
      h('span', { class: 'muted' }, 'Send files with: aether-cli send-file PORT CALL PATH')),
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
            : h('span', { class: 'st pending' }, t.state)),
          h('td', {}, h('div', { class: 'prog' }, h('div', { class: `bar ${p ? modeOf(p).mode : ''}`, role: 'progressbar', 'aria-valuenow': pctv, 'aria-valuemin': 0, 'aria-valuemax': 100, 'aria-label': `${t.filename} progress` }, h('span', { style: `width:${pctv}%` })), h('span', { class: 'num' }, `${pctv}%`))),
          h('td', { class: 'num' }, t.retries),
          h('td', {}, active ? h('button', { class: 'btn quiet', onclick: () => Engine.call('transfers.cancel', { id: t.id }).catch((e) => alert(e.message)) }, 'Cancel') : null));
      }))) : h('p', { class: 'empty' }, 'No transfers yet.'))));
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
function renderDiagnostics(main) {
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
  main.replaceChildren(h('div', { class: 'xfer', style: 'grid-template-rows: auto 1fr' },
    h('div', { class: 'pane-head', style: 'height:auto; flex-wrap:wrap; padding-block:var(--s3)' }, h('h1', {}, 'Diagnostics'),
      ...S.ports.flatMap((p) => [act(`Start capture on ${p.name}`, 'diag.captureStart', { portId: p.id }), act(`Stop capture on ${p.name}`, 'diag.captureStop', { portId: p.id })]),
      act('Write support bundle', 'diag.supportBundle', { includeCaptures: true }), out),
    h('div', { class: 'tablewrap' }, h('table', { class: 'grid' },
      h('thead', {}, h('tr', {}, ...['Time', 'Level', 'Port', 'Event'].map((c) => h('th', { scope: 'col' }, c)))),
      h('tbody', {}, ...S.events.slice().reverse().map((e) => h('tr', {}, h('td', {}, hms(e.at)),
        h('td', {}, e.level === 'warn' || e.level === 'error' ? h('span', { class: 'st fail' }, e.level) : e.level),
        h('td', {}, e.portId || ''), h('td', { style: 'white-space:normal' }, e.message))))))));
}

// ---------------------------------------------------------------- routing
function route() {
  const v = (location.hash || '#chat').slice(1);
  S.view = ['chat', 'messages', 'transfers', 'stations', 'diagnostics'].includes(v) ? v : 'chat';
  if (S.view !== 'chat') $('#main').replaceChildren();
  if (S.view === 'messages' && S.connected) loadMessages().then(render);
  render();
}
window.addEventListener('hashchange', route);
setInterval(() => { if (S.connected && S.view === 'chat') render(); }, 10000); // refresh "ago"
route();
Engine.connect();
