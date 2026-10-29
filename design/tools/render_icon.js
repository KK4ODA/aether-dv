// Render design/brand/icon.svg to a 1024x1024 PNG (transparent outside the tile).
const { chromium } = require('playwright');
const path = require('path');
(async () => {
  const svg = path.resolve(__dirname, '../brand/icon.svg');
  const out = path.resolve(__dirname, '../brand/icon-1024.png');
  const b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 1024, height: 1024 } });
  await p.goto('file://' + svg);
  await p.screenshot({ path: out, omitBackground: true });
  await b.close();
  console.log('wrote', out);
})();
