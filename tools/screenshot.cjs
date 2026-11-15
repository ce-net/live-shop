// Proof screenshots of the store page: a test customer buys a tent with a test payment.
// Env: PUPPETEER (path to puppeteer-core), CHROME (browser path), PAGE (shop.web URL), OUT (dir).
const puppeteer = require(process.env.PUPPETEER || 'puppeteer-core');
const path = require('path');
const PAGE = process.env.PAGE; if (!PAGE) { console.error('set PAGE to the shop.web URL (shop-net dir prints it)'); process.exit(2); }
const OUT = process.env.OUT || path.join(__dirname, '..', 'proof');
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
(async () => {
  const b = await puppeteer.launch({ executablePath: process.env.CHROME || '/usr/bin/chromium', args: ['--no-sandbox'], headless: 'new' });
  const p = await b.newPage();
  const shot = async (n, w = 1280, h = 860) => { await p.setViewport({ width: w, height: h }); await wait(300); await p.screenshot({ path: path.join(OUT, n), fullPage: true }); console.log('shot', n); };
  p.on('console', (m) => console.log('page:', m.text()));
  await p.goto(PAGE, { waitUntil: 'networkidle0' });
  await p.waitForSelector('.grid .card');
  await shot(process.env.PREFIX + '1-products.png');
  await p.goto(PAGE + '#/p/tent-2p'); await p.waitForSelector('#add');
  await p.click('#add'); await p.waitForFunction(() => document.querySelector('#msg').textContent.includes('In your cart'));
  await shot(process.env.PREFIX + '2-product.png');
  await p.goto(PAGE + '#/cart'); await p.waitForSelector('#pay');
  await shot(process.env.PREFIX + '3-cart.png');
  await p.click('#pay');
  await p.waitForFunction(() => location.hash.startsWith('#/order/'));
  await p.waitForFunction(() => /Confirmed|Failed/.test(document.querySelector('.stage')?.textContent || ''), { timeout: 30000 });
  await shot(process.env.PREFIX + '4-order-confirmed.png');
  await shot(process.env.PREFIX + '5-order-phone.png', 390, 844);
  console.log('order', await p.evaluate(() => document.querySelector('.crumb').textContent), await p.evaluate(() => document.querySelector('.stage').textContent));
  await b.close();
})().catch((e) => { console.error(e); process.exit(1); });
