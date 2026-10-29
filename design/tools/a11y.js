// Accessibility review for the mockups (project-written; axe-core not used).
// Checks in a real Chromium render:
//  1. WCAG 2.2 contrast of every visible text node against its effective background
//     (AA: 4.5:1 normal text, 3:1 large text >= 24px or >= 18.66px bold)
//  2. accessible names for buttons, links, inputs, selects, role=img, progressbars
//  3. landmarks (banner/nav/main), exactly one h1, html[lang]
//  4. keyboard: tab through all focusables and confirm a visible focus indicator
const { chromium } = require('playwright');
const path = require('path');
(async () => {
  const dir = path.resolve(__dirname, '../mockups');
  const browser = await chromium.launch();
  let total = 0;
  for (const name of process.argv.slice(2)) {
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    await page.goto('file://' + path.join(dir, name + '.html'));
    await page.evaluate(() => document.fonts.ready);
    const r = await page.evaluate(() => {
      const parse = c => { const m = c.match(/rgba?\(([^)]+)\)/); if (!m) return null; const p = m[1].split(',').map(Number); return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 }; };
      const lum = ({ r, g, b }) => { const f = v => { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); }; return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b); };
      const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
      const bgOf = el => { for (let e = el; e; e = e.parentElement) { const c = parse(getComputedStyle(e).backgroundColor); if (c && c.a > 0.5) return c; } return { r: 0, g: 0, b: 0, a: 1 }; };
      const issues = [];
      const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
      const seen = new Set();
      let n;
      while ((n = walker.nextNode())) {
        if (!n.textContent.trim()) continue;
        const el = n.parentElement; if (seen.has(el)) continue; seen.add(el);
        const cs = getComputedStyle(el); const rect = el.getBoundingClientRect();
        if (cs.visibility === 'hidden' || cs.display === 'none' || rect.width === 0 || el.closest('.sr')) continue;
        const fg = parse(cs.color); const bg = bgOf(el); const size = parseFloat(cs.fontSize); const bold = parseInt(cs.fontWeight) >= 600;
        const large = size >= 24 || (size >= 18.66 && bold); const need = large ? 3 : 4.5; const cr = ratio(fg, bg);
        if (cr < need) issues.push(`contrast ${cr.toFixed(2)} < ${need}: "${n.textContent.trim().slice(0, 40)}" (${cs.color} on rgb(${bg.r},${bg.g},${bg.b}), ${size}px)`);
      }
      const name = el => (el.getAttribute('aria-label') || el.getAttribute('aria-labelledby') && document.getElementById(el.getAttribute('aria-labelledby'))?.textContent || el.textContent || el.getAttribute('title') || el.getAttribute('placeholder') && '' || '').trim();
      for (const el of document.querySelectorAll('button, a[href]')) if (!name(el)) issues.push(`no accessible name: <${el.tagName.toLowerCase()} class="${el.className}">`);
      for (const el of document.querySelectorAll('input, select, textarea')) {
        if (el.type === 'radio') { if (!el.closest('label')?.textContent.trim()) issues.push('radio without label'); continue; }
        const lab = el.id && document.querySelector(`label[for="${el.id}"]`) || el.closest('label') || el.getAttribute('aria-label');
        if (!lab) issues.push(`form control without label: ${el.outerHTML.slice(0, 60)}`);
      }
      for (const el of document.querySelectorAll('[role="img"], [role="progressbar"]')) if (!el.getAttribute('aria-label')) issues.push(`${el.getAttribute('role')} without aria-label`);
      if (!document.documentElement.lang) issues.push('html without lang');
      if (document.querySelectorAll('h1').length !== 1) issues.push(`h1 count ${document.querySelectorAll('h1').length}`);
      if (!document.querySelector('main')) issues.push('no <main>');
      if (!document.querySelector('nav')) issues.push('no <nav>');
      return { issues, texts: seen.size };
    });
    // keyboard focus visibility
    const focusables = await page.evaluate(() => document.querySelectorAll('a[href], button, input, select, [tabindex]:not([tabindex="-1"])').length);
    let invisible = 0;
    for (let i = 0; i < focusables; i++) {
      await page.keyboard.press('Tab');
      const vis = await page.evaluate(() => { const el = document.activeElement; if (!el || el === document.body) return true;
        const cs = getComputedStyle(el); const lab = el.closest('label'); const lcs = lab ? getComputedStyle(lab) : null;
        return (cs.outlineStyle !== 'none' && parseFloat(cs.outlineWidth) > 0) || (lcs && lcs.outlineStyle !== 'none' && parseFloat(lcs.outlineWidth) > 0) || cs.boxShadow !== 'none'; });
      if (!vis) invisible++;
    }
    if (invisible) r.issues.push(`${invisible} of ${focusables} focus stops without a visible focus indicator`);
    console.log(`\n${name}: ${r.texts} text elements, ${focusables} focus stops, ${r.issues.length} issue(s)`);
    for (const i of r.issues) console.log('  - ' + i);
    total += r.issues.length;
    await page.close();
  }
  await browser.close();
  console.log(`\nTOTAL ISSUES: ${total}`);
  process.exit(total ? 1 : 0);
})();
