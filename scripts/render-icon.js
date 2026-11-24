// Render assets/icon/AppIcon.svg to PNGs at every macOS icon size.
//   node scripts/render-icon.js assets/icon/AppIcon.svg <out dir>
// Needs Playwright with Chromium (require("playwright")).
const { chromium } = require('playwright');
const fs = require('fs');
(async () => {
  const svg = fs.readFileSync(process.argv[2], 'utf8');
  const out = process.argv[3];
  const browser = await chromium.launch();
  const page = await browser.newPage();
  // Every size macOS uses: name -> pixels.
  const sizes = { 16: 16, 32: 32, 64: 64, 128: 128, 256: 256, 512: 512, 1024: 1024 };
  for (const px of Object.values(sizes)) {
    await page.setViewportSize({ width: px, height: px });
    await page.setContent(`<html><body style="margin:0;background:transparent">${svg.replace('width="1024" height="1024"', `width="${px}" height="${px}"`)}</body></html>`);
    await page.screenshot({ path: `${out}/icon_${px}.png`, omitBackground: true, clip: { x: 0, y: 0, width: px, height: px } });
  }
  await browser.close();
})();
