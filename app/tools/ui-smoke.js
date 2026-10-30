// UI smoke test in browser-preview mode against a running aetherdv.
//   cd app/ui && python3 -m http.server 18080 --bind 127.0.0.1 &
//   node app/tools/ui-smoke.js 127.0.0.1:7373 path/to/aetherdv.token [outdir]
// Drives every view, sends a chat line, and fails on console errors or on
// any element created from received text. Screenshots go to outdir.
const { chromium } = require('playwright');
const path = require('path');
(async () => {
  const [api = '127.0.0.1:7373', tokenFile = 'aetherdv.token', outDir = '.'] = process.argv.slice(2);
  const token = require('fs').readFileSync(tokenFile, 'utf8').trim();
  const shot = (n) => path.join(outDir, n);
  const b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 1360, height: 820 } });
  const errors = [];
  p.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') errors.push(m.text()); });
  p.on('pageerror', (e) => errors.push('PAGEERROR ' + e.message));
  await p.goto(`http://127.0.0.1:18080/index.html?api=${api}&token=${token}#chat`);
  await p.waitForSelector('.chat .log li.line', { timeout: 10000 });
  await p.waitForTimeout(500);
  await p.screenshot({ path: shot('chat.png') });
  // send from the UI
  await p.fill('#msg', 'Copy W4DRS, send your traffic.');
  await p.press('#msg', 'Enter');
  await p.waitForSelector('text=Copy W4DRS, send your traffic.', { timeout: 5000 });
  await p.waitForTimeout(800);
  const lastState = await p.$$eval('.log li.line.mine .meta', (els) => els.map((e) => e.textContent));
  await p.screenshot({ path: shot('chat-sent.png') });
  await p.click('a[data-view="messages"]');
  await p.waitForSelector('.items li[role=option]', { timeout: 5000 });
  await p.click('.items li[role=option]');
  await p.waitForSelector('.ics', { timeout: 5000 });
  await p.waitForTimeout(300);
  await p.screenshot({ path: shot('messages.png') });
  await p.click('a[data-view="transfers"]');
  await p.waitForTimeout(300);
  await p.screenshot({ path: shot('transfers.png') });
  await p.click('a[data-view="stations"]');
  await p.waitForTimeout(300);
  await p.screenshot({ path: shot('stations.png') });
  await p.click('a[data-view="diagnostics"]');
  await p.waitForTimeout(300);
  await p.screenshot({ path: shot('diagnostics.png') });
  // hostile text is shown as text, never parsed as HTML
  await p.click('a[data-view="chat"]');
  await p.waitForSelector('.chat');
  const hostile = await p.evaluate(() => document.querySelectorAll('.log img, .log script').length);
  console.log(JSON.stringify({ errors, lastState, hostileElements: hostile }));
  await b.close();
  if (errors.length || hostile) process.exit(1);
})();
