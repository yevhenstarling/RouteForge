#!/usr/bin/env node
// Tries the page as people use it, in a headless browser (D86).
//
// The page has no tests of its own (D52): the API's tests check that it is served and
// what it loads, and not what it does. This drives Edge or Chrome over the DevTools
// protocol through what the page is for, against an API that is running:
//
//  1. The admin logs in, sets up the demo city if there is none, and gives a day its
//     demo orders.
//  2. An order is placed by a click on the map.
//  3. The day is planned. The routes are stored and drawn, and a route sheet opens.
//  4. The plan is discarded, and the day can be planned again.
//  5. Somebody opens an account, places an order by a click, and withdraws it.
//
// All the while the browser has to hold nothing against the page: no error in its
// console, no script or style it refused, no request that failed but the one a run
// on a new database brings about, and no request to another host. The map tiles are
// switched off for it, so that it asks only the API.
//
// It needs Node 22 or later and no package: Node's own `fetch` and `WebSocket` are
// all it uses. And Edge or Chrome, found where they are installed or named in BROWSER.
//
//   node scripts/page-check.mjs                    the stack, on 127.0.0.1:8080
//   node scripts/page-check.mjs --url http://127.0.0.1:8081/ --day 2026-11-20
//
// The admin is ADMIN_EMAIL with ADMIN_PASSWORD, or the demonstration's. The day is a
// month from today unless one is named, out of the way of what is done on the page.
// The run leaves that day with its demo orders, an order placed by a click and its
// plan discarded, and an account of its own with an order that was withdrawn.

import { spawn, spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';

/** How many demo orders the day has at least. */
const ORDERS = 40;

/** What went wrong, in words for whoever runs the check. */
class Failure extends Error {}

function option(name, fallback) {
  const at = process.argv.indexOf(`--${name}`);
  return at >= 0 && process.argv[at + 1] ? process.argv[at + 1] : fallback;
}

function aMonthFromToday() {
  const day = new Date();
  day.setDate(day.getDate() + 30);
  const two = (number) => String(number).padStart(2, '0');
  return `${day.getFullYear()}-${two(day.getMonth() + 1)}-${two(day.getDate())}`;
}

const site = new URL(option('url', 'http://127.0.0.1:8080/'));
const day = option('day', aMonthFromToday());
const admin = {
  email: process.env.ADMIN_EMAIL || 'admin@example.com',
  password: process.env.ADMIN_PASSWORD || 'routeforge',
};

/** Asks `look` every tenth of a second until it gives something, for `ms` at most. */
async function within(ms, what, look) {
  const giveUp = Date.now() + ms;
  for (;;) {
    const found = await look();
    if (found !== null && found !== undefined && found !== false) return found;
    if (Date.now() > giveUp) throw new Failure(`gave up waiting for ${what}`);
    await sleep(100);
  }
}

// ------------------------------------------------------------------ the API itself

/** A request to the API, for setting the day up, outside the page. */
async function call(token, method, path, body) {
  const response = await fetch(new URL(path, site), {
    method,
    headers: {
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(body ? { 'Content-Type': 'application/json' } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await response.text();
  if (!response.ok) throw new Failure(`${method} ${path} was answered ${response.status}: ${text}`);
  return text ? JSON.parse(text) : null;
}

/**
 * The day as a run wants it: no plan in progress and none that is done, so that it
 * can be planned (D83), and its demo orders. What another run left is cleared away.
 */
async function prepareTheDay() {
  const { token } = await call(null, 'POST', '/api/auth/login', admin);
  for (const plan of await call(token, 'GET', `/api/plans?service_date=${day}`)) {
    if (plan.status === 'queued' || plan.status === 'routing') {
      await call(token, 'POST', `/api/plans/${plan.id}/cancel`);
    } else if (plan.status === 'done') {
      await call(token, 'POST', `/api/plans/${plan.id}/discard`);
    }
  }
  const orders = await call(token, 'GET', `/api/orders?service_date=${day}&status=pending&limit=1000`);
  return orders.length;
}

// --------------------------------------------------------------------- the browser

function browser() {
  if (process.env.BROWSER) return process.env.BROWSER;
  const installed = {
    win32: [
      'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
      'C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe',
      'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
      'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
    ],
    darwin: [
      '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
      '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
      '/Applications/Chromium.app/Contents/MacOS/Chromium',
    ],
  }[process.platform] ?? [];
  const names = ['google-chrome', 'google-chrome-stable', 'chromium', 'chromium-browser', 'microsoft-edge'];
  const onPath = (process.env.PATH ?? '').split(delimiter).flatMap((dir) => names.map((name) => join(dir, name)));
  const found = [...installed, ...onPath].find((path) => existsSync(path));
  if (!found) throw new Failure('found no Edge or Chrome: name one in BROWSER');
  return found;
}

/** A headless browser of its own, with a profile of its own, and its first tab. */
async function launch() {
  const profile = await mkdtemp(join(tmpdir(), 'routeforge-page-check-'));
  const child = spawn(browser(), [
    '--headless=new',
    '--remote-debugging-port=0',
    `--user-data-dir=${profile}`,
    '--window-size=1500,950',
    // The vans of a plan that is done stay put until a button sends them out (D74).
    '--force-prefers-reduced-motion',
    '--no-first-run',
    '--no-default-browser-check',
    'about:blank',
  ], { stdio: 'ignore' });
  // The browser writes the port it took into its profile.
  const ported = join(profile, 'DevToolsActivePort');
  const port = await within(20_000, 'the browser to start', async () => {
    if (!existsSync(ported)) return null;
    const [line] = (await readFile(ported, 'utf8')).split('\n');
    return line ? Number(line) : null;
  });
  const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
  const tab = targets.find((target) => target.type === 'page');
  if (!tab) throw new Failure('the browser has no tab to drive');
  return { child, profile, port, address: tab.webSocketDebuggerUrl };
}

/**
 * Closes the browser, and takes its profile away.
 *
 * The process that was started is not the browser for long: on Windows Edge hands over
 * to another one and ends, so killing its tree kills nothing. So the browser is told to
 * close over the protocol, and is waited for until it no longer answers. One that still
 * does is ended by what it was started with, its profile.
 */
async function shutDown({ child, profile, port }) {
  const answers = async () => {
    try {
      return (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
    } catch {
      return null;
    }
  };
  const browser = await answers();
  if (browser) {
    const cdp = await Cdp.connect(browser.webSocketDebuggerUrl).catch(() => null);
    cdp?.send('Browser.close').catch(() => {});
  }
  const closed = await within(10_000, 'the browser to close', async () => (await answers()) === null)
    .catch(() => false);
  if (!closed) {
    child.kill('SIGKILL');
    if (process.platform === 'win32') {
      const ofThisRun = `Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*${profile}*' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }`;
      spawnSync('powershell', ['-NoProfile', '-Command', ofThisRun], { stdio: 'ignore' });
    } else {
      spawnSync('pkill', ['-f', profile], { stdio: 'ignore' });
    }
  }
  await rm(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 300 }).catch(() => {});
}

/** The DevTools protocol over one socket: commands and their answers, and events. */
class Cdp {
  static async connect(address) {
    const socket = new WebSocket(address);
    await new Promise((resolve, reject) => {
      socket.addEventListener('open', resolve, { once: true });
      socket.addEventListener('error', () => reject(new Failure('could not reach the browser')), { once: true });
    });
    return new Cdp(socket);
  }

  constructor(socket) {
    this.socket = socket;
    this.sent = 0;
    this.waiting = new Map();
    this.handlers = new Map();
    socket.addEventListener('message', (event) => {
      const message = JSON.parse(event.data);
      if (message.id === undefined) {
        for (const handler of this.handlers.get(message.method) ?? []) handler(message.params);
        return;
      }
      const waiter = this.waiting.get(message.id);
      this.waiting.delete(message.id);
      if (message.error) waiter.reject(new Failure(`${waiter.method}: ${message.error.message}`));
      else waiter.resolve(message.result);
    });
  }

  send(method, params = {}) {
    this.sent += 1;
    const id = this.sent;
    this.socket.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => this.waiting.set(id, { resolve, reject, method }));
  }

  on(method, handler) {
    this.handlers.set(method, [...(this.handlers.get(method) ?? []), handler]);
  }
}

/** The page in the tab: what it shows, and what a person does on it. */
class Page {
  constructor(cdp) {
    this.cdp = cdp;
    /** What the browser held against the page. */
    this.complaints = [];
    /** Every address the page asked for, and what it was answered. */
    this.requests = [];
    this.answers = [];
    cdp.on('Runtime.consoleAPICalled', ({ type, args }) => {
      if (type === 'error' || type === 'assert') {
        this.complaints.push(`console.${type}: ${args.map((arg) => arg.value ?? arg.description).join(' ')}`);
      }
    });
    cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => {
      this.complaints.push(`uncaught: ${exceptionDetails.exception?.description ?? exceptionDetails.text}`);
    });
    cdp.on('Log.entryAdded', ({ entry }) => {
      if (entry.level !== 'error') return;
      // A new database has no depot, and the page asks for one before it is set up.
      if (entry.source === 'network' && /\/api\/depot$/.test(entry.url ?? '') && entry.text.includes('404')) return;
      this.complaints.push(`${entry.source}: ${entry.text}${entry.url ? ` (${entry.url})` : ''}`);
    });
    cdp.on('Network.requestWillBeSent', ({ request }) => this.requests.push(request.url));
    cdp.on('Network.responseReceived', ({ response }) => this.answers.push(response.url));
  }

  static async in(address) {
    const page = new Page(await Cdp.connect(address));
    await Promise.all(['Page', 'Runtime', 'Log', 'Network'].map((domain) => page.cdp.send(`${domain}.enable`)));
    return page;
  }

  async open(url) {
    const loaded = new Promise((resolve) => this.cdp.on('Page.loadEventFired', resolve));
    await this.cdp.send('Page.navigate', { url });
    await loaded;
  }

  async eval(expression) {
    const { result, exceptionDetails } = await this.cdp.send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (exceptionDetails) {
      throw new Failure(`the page could not run ${expression}: ${exceptionDetails.exception?.description ?? exceptionDetails.text}`);
    }
    return result.value;
  }

  until(what, expression, ms = 15_000) {
    return within(ms, what, () => this.eval(expression));
  }

  /** Waits until the page has been answered for an address with all of `parts` in it. */
  heard(what, parts, since, ms = 15_000) {
    return within(ms, what, async () => this.answers.slice(since).some((url) => parts.every((part) => url.includes(part))));
  }

  async fill(selector, value) {
    await this.eval(`(() => {
      const field = document.querySelector(${JSON.stringify(selector)});
      field.value = ${JSON.stringify(value)};
      field.dispatchEvent(new Event('input', { bubbles: true }));
      field.dispatchEvent(new Event('change', { bubbles: true }));
    })()`);
  }

  async press(selector) {
    await this.until(`${selector} to be there to press`, `${shown(selector)} && !document.querySelector(${JSON.stringify(selector)}).disabled`);
    await this.eval(`document.querySelector(${JSON.stringify(selector)}).click()`);
  }

  async submit(selector) {
    await this.eval(`document.querySelector(${JSON.stringify(selector)}).requestSubmit()`);
  }

  /** A click of the mouse at a point of the window, as a person makes one. */
  async click(x, y) {
    const mouse = { x, y, button: 'left', clickCount: 1 };
    await this.cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
    await this.cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', ...mouse });
    await this.cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', ...mouse });
  }

  /** Begins an order by a click on the map, where no dot or line is in the way. */
  async beginOrder() {
    const map = await this.eval(`(() => {
      const box = document.querySelector('#map').getBoundingClientRect();
      return { left: box.left, top: box.top, width: box.width, height: box.height };
    })()`);
    for (const [across, down] of [[0.3, 0.3], [0.7, 0.3], [0.3, 0.7], [0.7, 0.7], [0.15, 0.5]]) {
      await this.click(map.left + map.width * across, map.top + map.height * down);
      await sleep(300);
      if (await this.eval(shown('#order'))) return;
    }
    throw new Failure('no click on the map began an order');
  }

  text(selector) {
    return this.eval(`document.querySelector(${JSON.stringify(selector)}).textContent`);
  }

  async picture(path) {
    const { data } = await this.cdp.send('Page.captureScreenshot', { format: 'png' });
    await writeFile(path, Buffer.from(data, 'base64'));
  }
}

/** Whether an element is shown: neither it nor anything round it is hidden. */
function shown(selector) {
  return `(() => { const element = document.querySelector(${JSON.stringify(selector)}); return element !== null && element.closest('[hidden]') === null; })()`;
}

function message(words) {
  return `document.querySelector('#message').textContent.includes(${JSON.stringify(words)})`;
}

// ---------------------------------------------------------------------- the steps

async function logIn(page, email, password) {
  await page.until('the login', shown('#login-view'));
  await page.fill('#email', email);
  await page.fill('#password', password);
  await page.submit('#login');
  await page.until('the login to be taken', shown('#app'));
}

/** Shows `day`, and waits until its orders and its plans have been read. */
async function showTheDay(page) {
  const since = page.answers.length;
  await page.fill('#date', day);
  await page.heard('the orders of the day', ['/api/orders?', `service_date=${day}`], since);
  await sleep(300);
}

async function asTheAdmin(page, pending) {
  await logIn(page, admin.email, admin.password);
  await showTheDay(page);
  if (await page.eval(shown('#setup'))) {
    await page.fill('#demo-count', String(ORDERS));
    await page.press('#set-up');
    await page.until('the demo city', message('demo city is set up'));
  }
  if (pending < ORDERS) {
    await page.fill('#demo-count', String(ORDERS));
    await page.fill('#demo-seed', '12');
    await page.submit('#demo');
    await page.until('the demo orders', message('orders generated'));
  }

  await page.beginOrder();
  await page.press('#place');
  await page.until('the order to be placed', message('The order is placed.'));

  await page.fill('#budget', '1');
  await page.fill('#budget-unit', 'seconds');
  await page.fill('#seed', '7');
  await page.press('#plan');
  await page.until('the plan to be done', `(() => {
    const status = document.querySelector('#plan-line .status');
    if (status && status.textContent === 'failed') throw new Error(document.querySelector('#plan-error').textContent);
    return status !== null && status.textContent === 'done';
  })()`, 120_000);

  const routes = Number((await page.text('#plan-line')).match(/(\d+) of \d+ routes/)?.[1]);
  if (!(routes > 0)) throw new Failure(`the plan says ${await page.text('#plan-line')}`);
  await page.until(`a row for each of ${routes} routes`, `document.querySelectorAll('#route-list li').length === ${routes}`);
  await page.until(`a line for each of ${routes} routes`, `document.querySelectorAll('#map .leaflet-overlay-pane svg path.leaflet-interactive:not([stroke-dasharray])').length >= ${routes}`);
  await page.until('the streams to be live', `document.querySelector('#link').textContent === 'live'`);
  await page.until('the jobs of the day to be counted', `document.querySelector('#queue').textContent.includes('In the last day')`);
  if (!(await page.eval(`document.querySelector('#plan').disabled`))) {
    throw new Failure('Plan can be pressed for a day whose plan is done');
  }

  await page.press('#route-list li button.sheet');
  await page.until('the route sheet', `${shown('#sheet')} && document.querySelectorAll('#sheet-body tr').length >= 3`);
  await page.press('#sheet-close');
  await page.until('the sheet to close', `!${shown('#sheet')}`);

  await page.press('#discard');
  await page.until('the plan to be discarded', `document.querySelector('#plan-line .status')?.textContent === 'discarded'`);
  await page.until('Plan to be there again', `!document.querySelector('#plan').disabled`);
  await page.press('#logout');
}

async function asAClient(page) {
  await page.until('the login', shown('#login-view'));
  await page.fill('#email', `page-check-${Date.now()}@example.com`);
  await page.fill('#password', 'the password of a page check');
  await page.press('#register');
  await page.until('the account to be opened', `${shown('#app')} && ${shown('#mine')}`);
  await showTheDay(page);

  await page.beginOrder();
  await page.press('#place');
  await page.until('the order in the list', `[...document.querySelectorAll('#order-list li')].some((row) => row.textContent.includes('It waits for its day to be planned.'))`);
  await page.eval(`[...document.querySelectorAll('#order-list li button')].find((button) => button.textContent === 'Withdraw').click()`);
  await page.until('the order to be withdrawn', `[...document.querySelectorAll('#order-list li')].some((row) => row.textContent.includes('Withdrawn.'))`);
  await page.press('#logout');
  await page.until('the login again', shown('#login-view'));
}

// ------------------------------------------------------------------------ the run

const started = Date.now();
const steps = [];
async function step(what, run) {
  const at = Date.now();
  await run();
  steps.push(`  ok  ${what} (${((Date.now() - at) / 1000).toFixed(1)} s)`);
  console.log(steps.at(-1));
}

let browserRun = null;
let page = null;
try {
  console.log(`Trying the page at ${site.href} on ${day}`);
  let pending = 0;
  await step('the day is ready to be planned', async () => {
    pending = await prepareTheDay();
  });
  browserRun = await launch();
  page = await Page.in(browserRun.address);
  await step('the page opens, without map tiles', async () => {
    await page.open(site.href);
    await page.eval(`localStorage.setItem('routeforge.tiles', 'off')`);
    await page.open(site.href);
  });
  await step('the admin plans the day, and takes the plan back', () => asTheAdmin(page, pending));
  await step('a client opens an account, places an order and withdraws it', () => asAClient(page));
  await step('the browser held nothing against the page', async () => {
    if (page.complaints.length > 0) throw new Failure(`the browser said:\n    ${page.complaints.join('\n    ')}`);
    const elsewhere = page.requests.filter((url) => !url.startsWith(site.origin) && !url.startsWith('data:'));
    if (elsewhere.length > 0) throw new Failure(`the page asked other hosts:\n    ${[...new Set(elsewhere)].join('\n    ')}`);
  });
  console.log(`The page did what it is for, in ${((Date.now() - started) / 1000).toFixed(1)} s.`);
} catch (error) {
  console.error(`FAILED: ${error instanceof Failure ? error.message : error.stack}`);
  if (page) {
    const picture = join(tmpdir(), `routeforge-page-check-${Date.now()}.png`);
    await page.picture(picture).then(() => console.error(`What the page showed: ${picture}`), () => {});
    if (page.complaints.length > 0) console.error(`What the browser said:\n    ${page.complaints.join('\n    ')}`);
  }
  process.exitCode = 1;
} finally {
  if (browserRun) await shutDown(browserRun);
  process.exit();
}
