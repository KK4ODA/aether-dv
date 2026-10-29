// Render mockup pages to PNG at 1440x900 (and a 1280x800 check) for review.
const { chromium } = require('playwright');
const path = require('path');
(async () => {
  const dir = path.resolve(__dirname, '../mockups');
  const pages = process.argv.slice(2);
  const browser = await chromium.launch();
  for (const name of pages) {
    for (const [w, h] of [[1440, 900], [1280, 800]]) {
      const page = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: 1 });
      await page.goto('file://' + path.join(dir, name + '.html'));
      await page.evaluate(() => document.fonts.ready);
      const fonts = await page.evaluate(() => [...document.fonts].filter(f => f.status === 'loaded').map(f => f.family + ' ' + f.weight));
      const overflowX = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
      await page.screenshot({ path: path.join(dir, 'screens', `${name}-${w}.png`) });
      console.log(name, w, 'fonts:', [...new Set(fonts)].join(', ') || 'NONE', overflowX ? 'H-OVERFLOW' : '');
      await page.close();
    }
  }
  await browser.close();
})();
