// The built-in fixture both pages fall back to in a browser without Tauri:
// what the pop-up and the large window show without the tray app.
// Moved out of panel.js unchanged; its state lives here, not in either page.

import { PALETTE, clone, contrastText, hexToRgb, isObject } from "./common.js";

const DEFAULT_SETTINGS = {
  trayText: "percent",
  iconLimit: "binding",
  pollActiveSecs: 120,
  pollInactiveSecs: 900,
  autoBurn: false,
  suggestSwitches: true,
  notifyOnSwitch: true,
  mirrorLive: true,
  refreshInactive: true,
  hotkey: "Ctrl+Alt+C",
  startAtLogin: true,
  theme: "system",
  claudeConfigDir: null,
  historyDir: null,
  hostName: null,
  readSessions: true,
  notifyUsableAgain: true,
  notifyWaiting: true,
  notifyWaitingHosts: "thisHost",
  notifyLoginEnds: true,
  agentsRead: true,
  agentsSwitch: false,
  statuslineFeed: true,
  currency: "EUR",
  usdRate: null,
  lookUpPrices: true,
  statusline: {
    left: [
      { kind: "userHost", color: { rule: "fixed", color: "#22c55e" } },
      { kind: "text", color: { rule: "none" }, text: ":" },
      { kind: "folder", color: { rule: "fixed", color: "#3b82f6" } },
      { kind: "model", color: { rule: "byModelFamily" }, prefix: " " },
      { kind: "effort", color: { rule: "byEffort" }, prefix: " " },
    ],
    right: [
      { kind: "account", color: { rule: "account" }, suffix: " " },
      {
        kind: "fiveHourPercent",
        color: { rule: "byLoad", warn: 50, bad: 80 },
        suffix: "%",
        drop: 2,
      },
      {
        kind: "fiveHourReset",
        color: { rule: "none" },
        prefix: " ",
        drop: 1,
        timeFormat: "compactLeft",
      },
      {
        kind: "weekPercent",
        color: { rule: "byLoad", warn: 50, bad: 80 },
        prefix: "  ",
        suffix: "%",
      },
      {
        kind: "weekReset",
        color: { rule: "none" },
        prefix: " ",
        drop: 1,
        timeFormat: "compactLeft",
      },
    ],
    margin: 4,
  },
};

let fixture = null;

// Made once, on the first call, and shared by every module that asks.
export function ensureFixture() {
  if (!fixture) {
    fixture = makeFixture();
    fixture.priceLookedUp = Object.create(null);
    for (const row of fixture.priceList) {
      fixture.priceLookedUp[row.key] = clone(row.price);
    }
    console.info("[fixture] no window.__TAURI__; using the built-in snapshot");
  }
  return fixture;
}

// The snapshot the fixture commands change, or null before `ensureFixture`.
export function currentFixture() {
  return fixture;
}

const GLYPHS = {
  A: ["010", "101", "111", "101", "101"],
  B: ["110", "101", "110", "101", "110"],
  C: ["011", "100", "100", "100", "011"],
  D: ["110", "101", "101", "101", "110"],
  E: ["111", "100", "110", "100", "111"],
  F: ["111", "100", "110", "100", "100"],
  G: ["011", "100", "101", "101", "011"],
  H: ["101", "101", "111", "101", "101"],
  I: ["111", "010", "010", "010", "111"],
  J: ["001", "001", "001", "101", "010"],
  K: ["101", "101", "110", "101", "101"],
  L: ["100", "100", "100", "100", "111"],
  M: ["101", "111", "111", "101", "101"],
  N: ["110", "101", "101", "101", "101"],
  O: ["010", "101", "101", "101", "010"],
  P: ["110", "101", "110", "100", "100"],
  Q: ["010", "101", "101", "110", "011"],
  R: ["110", "101", "110", "101", "101"],
  S: ["011", "100", "010", "001", "110"],
  T: ["111", "010", "010", "010", "010"],
  U: ["101", "101", "101", "101", "111"],
  V: ["101", "101", "101", "101", "010"],
  W: ["101", "101", "111", "111", "101"],
  X: ["101", "101", "010", "101", "101"],
  Y: ["101", "101", "010", "010", "010"],
  Z: ["111", "001", "010", "100", "111"],
  0: ["111", "101", "101", "101", "111"],
  1: ["010", "110", "010", "010", "111"],
  2: ["110", "001", "010", "100", "111"],
  3: ["110", "001", "010", "001", "110"],
  4: ["101", "101", "111", "001", "001"],
  5: ["111", "100", "110", "001", "110"],
  6: ["011", "100", "110", "101", "010"],
  7: ["111", "001", "010", "010", "010"],
  8: ["010", "101", "010", "101", "010"],
  9: ["010", "101", "011", "001", "110"],
};

function fixtureId() {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (character) => {
    const random = Math.floor(Math.random() * 16);
    const value = character === "x" ? random : (random & 0x3) | 0x8;
    return value.toString(16);
  });
}

function isoIn(milliseconds) {
  return new Date(Date.now() + milliseconds).toISOString();
}

function makeFixture() {
  const day = 86400000;
  const hour = 3600000;
  const acmeId = "5d1c2a9b-e0d3-4c11-9e21-7c8f1a2b3c4d";
  const labId = "8b4e7f20-6a1d-4d33-b8c5-2f9a0e6d7c81";
  const privateId = "c0a1b2d3-e4f5-4a6b-8c7d-9e0f1a2b3c4d";
  return {
    views: [
      {
        id: privateId,
        label: "Private",
        abbr: "PR",
        color: "#8b5cf6",
        email: "sam.private@mail.example",
        orgName: "sam",
        plan: "Ultra",
        price: 180.0,
        autoBurn: true,
        isActive: false,
        loginState: "ok",
        loginEndsAtMs: Date.now() + 4 * day,
        loginDaysLeft: 4,
        usage: {
          limits: [
            {
              kind: "weekly_all",
              percent: 70,
              severity: "normal",
              resetsAt: isoIn(3 * day + 5 * hour),
              model: null,
              isActive: true,
            },
            {
              kind: "weekly_scoped",
              percent: 22,
              severity: "normal",
              resetsAt: isoIn(3 * day),
              model: "Opus",
              isActive: false,
            },
            // An Other limit keeps the card's generic row in the fixture too.
            {
              kind: { other: "daily_tokens" },
              percent: 42,
              severity: "normal",
              resetsAt: isoIn(6 * hour),
              model: null,
              isActive: false,
            },
          ],
          extraUsageEnabled: false,
        },
        fetchedAt: isoIn(-8 * 60000),
        lastError: null,
      },
      {
        id: labId,
        label: "Lab",
        abbr: "LB",
        color: "#10b981",
        email: "sam@lab.example",
        orgName: "sam",
        plan: "Max",
        price: null,
        autoBurn: true,
        isActive: false,
        loginState: "ok",
        loginEndsAtMs: Date.now() + 12 * day,
        loginDaysLeft: 12,
        usage: {
          limits: [
            {
              kind: "weekly_all",
              percent: 40,
              severity: "normal",
              resetsAt: isoIn(5 * day + 3 * hour),
              model: null,
              isActive: true,
            },
            {
              kind: "weekly_scoped",
              percent: 21,
              severity: "normal",
              resetsAt: isoIn(5 * day),
              model: "Fable",
              isActive: false,
            },
          ],
          extraUsageEnabled: false,
        },
        fetchedAt: isoIn(-60000),
        lastError: null,
      },
      {
        id: acmeId,
        label: "Acme",
        abbr: "AC",
        color: "#3b82f6",
        email: "sam@acme.example",
        orgName: "acme",
        plan: "Team 5x",
        price: 320.0,
        autoBurn: true,
        isActive: true,
        loginState: "ok",
        loginEndsAtMs: Date.now() + 41 * day,
        loginDaysLeft: 41,
        usage: {
          limits: [
            {
              kind: "session",
              percent: 97,
              severity: "critical",
              resetsAt: isoIn(72 * 60000),
              model: null,
              isActive: true,
            },
            {
              kind: "weekly_all",
              percent: 48,
              severity: "normal",
              resetsAt: isoIn(5 * day + 3 * hour),
              model: null,
              isActive: false,
            },
            {
              kind: "weekly_scoped",
              percent: 43,
              severity: "normal",
              resetsAt: isoIn(5 * day),
              model: "Fable",
              isActive: false,
            },
          ],
          extraUsageEnabled: false,
        },
        fetchedAt: isoIn(-12000),
        lastError: null,
      },
    ],
    activeId: acmeId,
    live: { kind: "known", accountId: acmeId },
    burn: {
      on: true,
      pausedUntil: null,
      early: true,
      why: "switching before 100 % pays",
      learning: [],
      line: "On AC until its 5 h window is full, about 2h. Then LB.",
      planned: { [labId]: isoIn(2 * hour) },
      waiting: { to: privateId, latest: isoIn(40 * 60000), tiedBusy: 1 },
      suggestion: null,
      measured: {
        windowPercentOfWeek: {
          [privateId]: 8.6,
          [labId]: 9.4,
          [acmeId]: 10.2,
        },
        switchCostPercentOfWindow: 0.8,
        switchesPerDay: 5.6,
      },
    },
    skill: {
      path: "~/.claude/skills/seatswap/SKILL.md",
      installed: false,
      differs: false,
    },
    agentCalls: [
      {
        at: isoIn(-3 * hour),
        command: "status --json",
        cwd: "~/projects/ledger",
        allowed: true,
      },
      {
        at: isoIn(-2 * hour),
        command: "use PR",
        cwd: "~/projects/ledger",
        allowed: false,
      },
      {
        at: isoIn(-1 * hour),
        command: "status --json --refresh",
        cwd: "~/projects/acme-kb",
        allowed: true,
      },
    ],
    warnings: [{ kind: "soon", id: labId, daysLeft: 4 }],
    pending: null,
    secretsWarning: null,
    lastPollAt: isoIn(-12000),
    error: null,
    sessions: {
      running: 9,
      busy: 2,
      tied: 1,
      tiedBusy: 1,
      waiting: [
        {
          session: "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34",
          label: "acme-kb-dd",
          project: "acme-kb",
          since: isoIn(-4 * 60000),
        },
      ],
      unavailable: null,
      warning: null,
    },
    money: {
      currency: "EUR",
      rate: { value: 0.9048, date: "2026-11-13", own: false },
      lookUp: true,
      modelsWithPrice: 4,
      pricesCheckedAt: isoIn(-2 * 3600000),
      lastChange: {
        at: isoIn(-6 * 86400000),
        added: ["openai/gpt-6"],
        changed: ["anthropic/claude-haiku-5-5"],
        removed: [],
      },
      lastError: null,
    },
    priceList: [
      {
        key: "anthropic/claude-opus-5-5",
        price: { input: 4, output: 20, cacheWrite: 5, cacheRead: 0.2, tiered: false },
        own: false,
        tiered: false,
        since: isoIn(-30 * 86400000),
      },
      {
        key: "anthropic/claude-haiku-5-5",
        price: { input: 0.1, output: 0.5, cacheWrite: 0.125, cacheRead: 0.01, tiered: true },
        own: false,
        tiered: true,
        since: isoIn(-30 * 86400000),
      },
      {
        key: "openai/gpt-6",
        price: { input: 2, output: 10, cacheWrite: 2.5, cacheRead: 0.2, tiered: false },
        own: false,
        tiered: false,
        since: isoIn(-12 * 86400000),
      },
      {
        key: "ollama-cloud/deepseek-v4.1-flash",
        price: { input: 0.3, output: 1.2, cacheWrite: 0, cacheRead: 0, tiered: false },
        own: false,
        tiered: false,
        since: isoIn(-5 * 86400000),
      },
    ],
    // The Projects group (spec §3.6): what the session records named, and the
    // log the page appends to. `fixtureResolveProjects` applies the log.
    projects: [
      {
        key: { kind: "repo", id: "github.com/acme/acme-kb" },
        name: "acme-kb",
        repo: "github.com/acme/acme-kb",
        org: "acme",
        folders: { atlas: ["~/projects/acme-kb"] },
        sessions: 212,
      },
      {
        key: { kind: "repo", id: "github.com/acme/acme-notes" },
        name: "acme-notes",
        repo: "github.com/acme/acme-notes",
        org: "acme",
        folders: { atlas: ["~/projects/acme-notes"] },
        sessions: 41,
      },
      {
        key: { kind: "repo", id: "github.com/sam/atlas" },
        name: "atlas",
        repo: "github.com/sam/atlas",
        org: "sam",
        folders: {
          atlas: ["~/projects/atlas"],
          vega: ["~/src/atlas"],
        },
        sessions: 268,
      },
      {
        key: { kind: "repo", id: "github.com/sam/ledger" },
        name: "ledger",
        repo: "github.com/sam/ledger",
        org: "sam",
        folders: {
          atlas: ["~/projects/ledger"],
          vega: ["~/ledger"],
        },
        sessions: 97,
      },
      {
        key: { kind: "repo", id: "github.com/sam/seatswap" },
        name: "seatswap",
        repo: "github.com/sam/seatswap",
        org: "sam",
        folders: { atlas: ["~/projects/seatswap"] },
        sessions: 63,
      },
      {
        key: { kind: "folder", id: "~" },
        name: "~ (home folder)",
        repo: null,
        org: null,
        folders: { atlas: ["~"], vega: ["~"] },
        sessions: 22,
      },
    ],
    projectLog: [],
    settings: Object.assign({}, DEFAULT_SETTINGS, {
      autoBurn: true,
      suggestSwitches: true,
    }),
  };
}

// The money view with the fixture's current settings, as the Rust thread would
// compute it from the stored files.
function fixtureMoneyView(fixture) {
  const view = Object.assign({}, clone(fixture.money));
  const currency =
    typeof fixture.settings.currency === "string" ? fixture.settings.currency : null;
  view.currency = currency;
  view.lookUp = Boolean(fixture.settings.lookUpPrices);
  view.modelsWithPrice = Array.isArray(fixture.priceList) ? fixture.priceList.length : 0;
  if (!currency) {
    view.rate = null;
  } else if (currency === "USD") {
    view.rate = { value: 1, date: null, own: false };
  } else if (typeof fixture.settings.usdRate === "number") {
    view.rate = { value: fixture.settings.usdRate, date: null, own: true };
  } else if (currency !== "EUR") {
    // The fixture's stored looked-up rate is in EUR.
    view.rate = null;
  }
  return view;
}

function fixtureUniqueAbbr(fixture, label) {
  const used = new Set(fixture.views.map((view) => view.abbr));
  const clean = String(label || "").toUpperCase().replace(/[^A-Z0-9]/g, "");
  const candidates = [];
  if (clean.length >= 2) candidates.push(clean.slice(0, 2));
  if (clean.length >= 1) candidates.push(clean[0]);
  if (clean.length >= 2) candidates.push(clean[0] + clean[clean.length - 1]);
  for (const candidate of candidates) {
    if (candidate && candidate.length <= 2 && !used.has(candidate)) return candidate;
  }
  for (const character of "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789") {
    if (!used.has(character)) return character;
  }
  return "X";
}

function fixtureCreateFromLive(fixture, labelValue) {
  const label = String(labelValue || "").trim().slice(0, 40);
  if (!label) throw new Error("invalid label: use 1 to 40 characters");
  const color = PALETTE.find((candidate) => !fixture.views.some((view) => view.color === candidate));
  const account = {
    id: fixtureId(),
    label,
    abbr: fixtureUniqueAbbr(fixture, label),
    color: color || PALETTE[0],
    email: fixture.live.email || null,
    plan: "Ultra",
    price: null,
    autoBurn: true,
    isActive: false,
    loginState: "ok",
    loginEndsAtMs: Date.now() + 30 * 86400000,
    loginDaysLeft: 30,
    usage: null,
    fetchedAt: null,
    lastError: null,
  };
  fixture.views.push(account);
  for (const view of fixture.views) view.isActive = view.id === account.id;
  fixture.activeId = account.id;
  fixture.live = { kind: "known", accountId: account.id };
  if (fixture.burn) fixture.burn.suggestion = null;
  fixture.lastPollAt = new Date().toISOString();
  return account;
}

function fixtureIcon(abbr, color, size) {
  const pixels = new Uint8ClampedArray(size * size * 4);
  const clean = String(abbr || "?")
    .toUpperCase()
    .replace(/[^A-Z0-9]/g, "")
    .slice(0, 2) || "?";
  const disc = hexToRgb(color) || { r: 59, g: 130, b: 246 };
  const textColor = contrastText(color) === "#0f1115"
    ? { r: 15, g: 17, b: 21 }
    : { r: 255, g: 255, b: 255 };
  const centre = (size - 1) / 2;
  const radius = size * 0.46;
  const gap = Math.max(1, Math.round(size / 22));
  let unit = Math.max(1, Math.floor(size / 9));
  const textWidth = (count) => count * 3 * unit + (count - 1) * gap;
  while (unit > 1 && textWidth(clean.length) > radius * 1.35) {
    unit -= 1;
  }
  const totalWidth = textWidth(clean.length);
  const totalHeight = 5 * unit;
  const startX = centre - (totalWidth - 1) / 2;
  const startY = centre - (totalHeight - 1) / 2;

  const glyphHit = (x, y) => {
    for (let index = 0; index < clean.length; index += 1) {
      const offsetX = index * (3 * unit + gap);
      const localX = x - offsetX;
      if (localX >= 0 && localX < 3 * unit) {
        const column = Math.floor(localX / unit);
        const row = Math.floor(y / unit);
        if (row >= 0 && row < 5) {
          const rows = GLYPHS[clean[index]];
          if (rows && rows[row] && rows[row][column] === "1") return true;
        }
      }
    }
    return false;
  };

  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      let hits = 0;
      let red = 0;
      let green = 0;
      let blue = 0;
      for (let sy = 0; sy < 2; sy += 1) {
        for (let sx = 0; sx < 2; sx += 1) {
          const px = x + (sx + 0.5) / 2;
          const py = y + (sy + 0.5) / 2;
          const dx = px - centre;
          const dy = py - centre;
          if (dx * dx + dy * dy > radius * radius) continue;
          const localX = px - startX;
          const localY = py - startY;
          const onGlyph =
            localX >= 0 &&
            localY >= 0 &&
            localX < totalWidth &&
            localY < totalHeight &&
            glyphHit(localX, localY);
          const colorAt = onGlyph ? textColor : disc;
          hits += 1;
          red += colorAt.r;
          green += colorAt.g;
          blue += colorAt.b;
        }
      }
      if (hits === 0) continue;
      const offset = (y * size + x) * 4;
      pixels[offset] = Math.round(red / hits);
      pixels[offset + 1] = Math.round(green / hits);
      pixels[offset + 2] = Math.round(blue / hits);
      pixels[offset + 3] = Math.round((hits / 4) * 255);
    }
  }
  return pixels;
}

function validateFixtureSettings(settings) {
  const merged = Object.assign({}, DEFAULT_SETTINGS, settings);
  if (!["percent", "percentReset", "off"].includes(merged.trayText)) {
    throw new Error(`invalid trayText ${JSON.stringify(merged.trayText)}`);
  }
  if (!["binding", "session", "week"].includes(merged.iconLimit)) {
    throw new Error(`invalid iconLimit ${JSON.stringify(merged.iconLimit)}`);
  }
  if (!["system", "light", "dark"].includes(merged.theme)) {
    throw new Error(`invalid theme ${JSON.stringify(merged.theme)}`);
  }
  if (!["thisHost", "allHosts"].includes(merged.notifyWaitingHosts)) {
    throw new Error(
      `invalid notifyWaitingHosts ${JSON.stringify(merged.notifyWaitingHosts)}`
    );
  }
  if (!Number.isInteger(merged.pollActiveSecs) || merged.pollActiveSecs < 30) {
    throw new Error(`pollActiveSecs must be at least 30 s, got ${merged.pollActiveSecs}`);
  }
  if (!Number.isInteger(merged.pollInactiveSecs) || merged.pollInactiveSecs < 30) {
    throw new Error(`pollInactiveSecs must be at least 30 s, got ${merged.pollInactiveSecs}`);
  }
  if (
    merged.claudeConfigDir !== null &&
    merged.claudeConfigDir !== undefined &&
    !String(merged.claudeConfigDir).startsWith("/")
  ) {
    throw new Error(
      `claudeConfigDir must be an absolute path, got ${merged.claudeConfigDir}`
    );
  }
  if (
    merged.historyDir !== null &&
    merged.historyDir !== undefined &&
    !String(merged.historyDir).startsWith("/")
  ) {
    throw new Error(`historyDir must be an absolute path, got ${merged.historyDir}`);
  }
  if (merged.hostName !== null && merged.hostName !== undefined) {
    const name = String(merged.hostName);
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$/.test(name)) {
      throw new Error(
        `host name ${JSON.stringify(name)} must be 1 to 63 characters of A-Z, a-z, 0-9, '.', '_' or '-' and must start with a letter or a digit; set hostName in the settings`
      );
    }
  }
  if (
    merged.currency !== null &&
    merged.currency !== undefined &&
    !/^[A-Z]{3}$/.test(String(merged.currency))
  ) {
    throw new Error(
      `currency must be three capital letters, got ${JSON.stringify(merged.currency)}`
    );
  }
  if (
    merged.usdRate !== null &&
    merged.usdRate !== undefined &&
    !(Number.isFinite(merged.usdRate) && merged.usdRate > 0)
  ) {
    throw new Error(`usdRate must be above 0, got ${merged.usdRate}`);
  }
  if (typeof merged.lookUpPrices !== "boolean") {
    throw new Error(`lookUpPrices must be true or false, got ${merged.lookUpPrices}`);
  }
  return merged;
}

// ---------------------------------------------------------------------------
// Projects and organizations (spec §3.6) in the fixture. This is the page's
// stand-in for core::projects, over the fixture's few rows: the same rules —
// the log in order, a later record wins, a join follows the chain and drops
// the earlier record of a cycle, an organization is its identity with a shown
// name. It is only for checking the page in a browser; the app resolves in
// Rust.
// ---------------------------------------------------------------------------

function fixtureKeyText(key) {
  if (!isObject(key)) return "";
  return `${key.kind}:${key.id}`;
}

function fixtureKeyFromText(text) {
  const mark = text.indexOf(":");
  if (mark < 0) return { kind: "folder", id: text };
  return { kind: text.slice(0, mark), id: text.slice(mark + 1) };
}

function fixtureKeyName(key) {
  if (!isObject(key)) return "?";
  if (key.kind === "repo") {
    const parts = String(key.id).split("/").filter((part) => part !== "");
    return parts.length > 0 ? parts[parts.length - 1] : String(key.id);
  }
  // The home folder's name is `~ (home folder)`, as the core shows it.
  if (key.id === "~") return "~ (home folder)";
  return String(key.id);
}

function fixtureRefText(ref) {
  return `${ref && ref.acc}\u0000${ref && ref.org}`;
}

// The reference the fixture's history uses for an account row. Only the
// fixture builds it; the app reads the real one from `projects_view`.
function fixtureAccountRef(view) {
  return { acc: view.id, org: `org:${view.orgName || view.id}` };
}

function fixtureProjectRoot(joins, key) {
  let current = key;
  const seen = new Set();
  for (;;) {
    const edge = joins.get(current);
    if (!edge) return current;
    if (seen.has(current)) return current;
    seen.add(current);
    current = edge.into;
  }
}

function fixtureBreakJoinCycle(joins, key, into) {
  for (;;) {
    const edges = [];
    const seen = new Set();
    let node = into;
    let closes = node === key;
    while (!closes) {
      const edge = joins.get(node);
      if (!edge) break;
      if (seen.has(node)) return;
      seen.add(node);
      edges.push({ node, order: edge.order });
      node = edge.into;
      closes = node === key;
    }
    if (!closes || edges.length === 0) return;
    edges.sort((a, b) => a.order - b.order);
    joins.delete(edges[0].node);
  }
}

function fixtureOrgRoot(orgJoin, org) {
  let current = org;
  const seen = new Set();
  for (;;) {
    const edge = orgJoin.get(current);
    if (!edge) return current;
    if (seen.has(current)) return current;
    seen.add(current);
    current = edge.into;
  }
}

function fixtureResolveProjects(fixture) {
  const rows = new Map();
  const keyOf = new Map();
  for (const row of Array.isArray(fixture.projects) ? fixture.projects : []) {
    rows.set(fixtureKeyText(row.key), clone(row));
    keyOf.set(fixtureKeyText(row.key), row.key);
  }
  const names = new Map();
  const assigned = new Map();
  const joins = new Map();
  const orgJoin = new Map();
  const orgNames = new Map();
  // The account lists in the order they apply, after every `OrgJoin`: one rule
  // per account.
  const orgAccountLists = [];
  const log = Array.isArray(fixture.projectLog) ? fixture.projectLog : [];
  log.forEach((record, order) => {
    const edit = isObject(record.edit) ? record.edit : {};
    switch (edit.edit) {
      case "rename":
        names.set(fixtureKeyText(edit.key), String(edit.name));
        break;
      case "join": {
        const key = fixtureKeyText(edit.key);
        const into = fixtureKeyText(edit.into);
        keyOf.set(key, edit.key);
        keyOf.set(into, edit.into);
        if (key === into) break;
        fixtureBreakJoinCycle(joins, key, into);
        joins.set(key, { into, order });
        break;
      }
      case "unjoin":
        joins.delete(fixtureKeyText(edit.key));
        break;
      case "assign":
        assigned.set(fixtureKeyText(edit.key), isObject(edit) && "org" in edit ? edit.org : null);
        keyOf.set(fixtureKeyText(edit.key), edit.key);
        break;
      case "orgRename":
        orgNames.set(String(edit.org), String(edit.name));
        break;
      case "orgJoin": {
        const org = String(edit.org);
        const into = String(edit.into);
        if (org === into) break;
        fixtureBreakJoinCycle(orgJoin, org, into);
        orgJoin.set(org, { into, order });
        break;
      }
      case "orgAccounts":
        orgAccountLists.push({
          org: String(edit.org),
          accounts: Array.isArray(edit.accounts) ? clone(edit.accounts) : [],
          order,
        });
        break;
      default:
        break;
    }
  });

  const keys = new Set(rows.keys());
  for (const [key, edge] of joins) {
    keys.add(key);
    keys.add(edge.into);
  }
  const groups = new Map();
  for (const key of keys) {
    const root = fixtureProjectRoot(joins, key);
    if (!groups.has(root)) groups.set(root, []);
    groups.get(root).push(key);
  }

  const projects = [];
  for (const [root, members] of groups) {
    const memberKeys = members.filter((member) => rows.has(member));
    if (memberKeys.length === 0) continue;
    const folders = {};
    let sessions = 0;
    for (const member of memberKeys) {
      const row = rows.get(member);
      for (const [host, list] of Object.entries(row.folders || {})) {
        const target = folders[host] || (folders[host] = []);
        for (const cwd of list) if (!target.includes(cwd)) target.push(cwd);
      }
      sessions += Number(row.sessions) || 0;
    }
    for (const list of Object.values(folders)) list.sort();
    const home = rows.get(root);
    const key = keyOf.get(root) || fixtureKeyFromText(root);
    const joined = memberKeys
      .filter((member) => member !== root)
      .sort()
      .map((member) => {
        const joinedKey = keyOf.get(member) || fixtureKeyFromText(member);
        return { key: joinedKey, name: fixtureKeyName(joinedKey) };
      });
    projects.push({
      key,
      name: names.has(root) ? names.get(root) : home ? home.name : fixtureKeyName(key),
      repo: home ? home.repo : key.kind === "repo" ? key.id : null,
      org: assigned.has(root) ? assigned.get(root) : home ? home.org : null,
      joined,
      folders,
      sessions,
    });
  }

  const identities = new Set();
  for (const project of projects) if (project.org) identities.add(project.org);
  for (const view of Array.isArray(fixture.views) ? fixture.views : []) {
    if (typeof view.orgName === "string" && view.orgName !== "") identities.add(view.orgName);
  }
  for (const name of orgNames.keys()) identities.add(name);
  for (const name of orgJoin.keys()) identities.add(name);
  for (const edge of orgJoin.values()) identities.add(edge.into);
  for (const record of orgAccountLists) identities.add(record.org);
  for (const project of projects) {
    if (project.org) project.org = fixtureOrgRoot(orgJoin, project.org);
  }

  const byRoot = new Map();
  for (const identity of identities) {
    const root = fixtureOrgRoot(orgJoin, identity);
    if (!byRoot.has(root)) byRoot.set(root, []);
    byRoot.get(root).push(identity);
  }

  // One rule per account: it starts in the root of its `orgName`; then each
  // `OrgAccounts` record, in log order, takes every account it names to its
  // root and drops every account in that root that it does not name. An account
  // no record ever named keeps its default.
  const accountOrg = new Map();
  for (const view of Array.isArray(fixture.views) ? fixture.views : []) {
    if (typeof view.orgName !== "string" || view.orgName.trim() === "") continue;
    const ref = fixtureAccountRef(view);
    accountOrg.set(fixtureRefText(ref), { ref, root: fixtureOrgRoot(orgJoin, view.orgName) });
  }
  for (const record of orgAccountLists) {
    const root = fixtureOrgRoot(orgJoin, record.org);
    const named = new Map();
    for (const ref of record.accounts) named.set(fixtureRefText(ref), ref);
    for (const [text, entry] of [...accountOrg]) {
      if (entry.root === root && !named.has(text)) accountOrg.delete(text);
    }
    for (const [text, ref] of named) accountOrg.set(text, { ref, root });
  }
  const accountsIn = new Map();
  for (const entry of accountOrg.values()) {
    if (!accountsIn.has(entry.root)) accountsIn.set(entry.root, []);
    accountsIn.get(entry.root).push(entry.ref);
  }
  for (const list of accountsIn.values()) {
    list.sort(
      (a, b) => String(a.acc).localeCompare(String(b.acc)) || String(a.org).localeCompare(String(b.org))
    );
  }

  const orgs = [];
  for (const [root] of byRoot) {
    orgs.push({
      id: root,
      name: orgNames.has(root) ? orgNames.get(root) : root,
      accounts: accountsIn.get(root) || [],
      projects: projects.filter((project) => project.org === root).length,
    });
  }
  orgs.sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));

  const orgLabel = (id) => {
    if (!id) return "";
    const org = orgs.find((entry) => entry.id === id);
    return org ? org.name : id;
  };
  projects.sort(
    (a, b) =>
      Number(a.org === null) - Number(b.org === null) ||
      orgLabel(a.org).localeCompare(orgLabel(b.org)) ||
      a.name.localeCompare(b.name) ||
      fixtureKeyText(a.key).localeCompare(fixtureKeyText(b.key))
  );

  const accountRefs = (Array.isArray(fixture.views) ? fixture.views : []).map((view) => ({
    id: view.id,
    account: fixtureAccountRef(view),
  }));
  return { projects, orgs, accountRefs };
}

function fixtureCheckName(what, value) {
  const text = String(value === undefined || value === null ? "" : value);
  if (text.trim() === "") throw new Error(`a ${what} name must not be only spaces`);
  const length = [...text].length;
  if (length > 40) {
    throw new Error(`a ${what} name must be 1 to 40 characters, not ${length}`);
  }
  return text;
}

// The same refusals as `ProjectEdit::validate`, and nothing is appended when
// one throws.
function fixtureEditProject(fixture, edit) {
  if (!isObject(edit)) throw new Error("projects_edit needs an edit");
  switch (String(edit.edit || "")) {
    case "rename":
      fixtureCheckName("project", edit.name);
      break;
    case "orgRename":
      fixtureCheckName("organization", edit.name);
      break;
    case "join":
      if (fixtureKeyText(edit.key) === fixtureKeyText(edit.into)) {
        throw new Error("a project cannot be joined into itself");
      }
      break;
    case "assign":
      if (edit.org !== null && edit.org !== undefined) {
        fixtureCheckName("organization", edit.org);
      }
      break;
    case "orgJoin":
      if (String(edit.org || "").trim() === "") {
        throw new Error("an organization name must not be empty");
      }
      if (String(edit.org) === String(edit.into)) {
        throw new Error("an organization cannot be joined into itself");
      }
      fixtureCheckName("organization", edit.into);
      break;
    case "orgAccounts":
      fixtureCheckName("organization", edit.org);
      break;
    case "unjoin":
      break;
    default:
      throw new Error(`unknown project edit ${String(edit.edit)}`);
  }
  fixture.projectLog.push({ at: new Date().toISOString(), edit: clone(edit) });
}

export async function fixtureInvoke(command, args) {
  console.log(`[fixture] ${command}`, args);
  const fixture = currentFixture();
  if (!fixture && command !== "fit_panel" && command !== "hide_panel") {
    throw new Error("fixture is not ready");
  }

  switch (command) {
    case "get_snapshot": {
      const snapshot = clone(fixture);
      snapshot.accounts = fixture.views.map((view) => clone(view));
      delete snapshot.projects;
      delete snapshot.projectLog;
      return snapshot;
    }

    case "refresh_now":
      fixture.lastPollAt = new Date().toISOString();
      for (const view of fixture.views) {
        if (view.usage) view.fetchedAt = fixture.lastPollAt;
      }
      return null;

    case "switch_account": {
      const key = String(args.key || "");
      const unsaved = args.unsaved || "refuse";
      const label = args.label === undefined || args.label === null ? null : String(args.label);
      const target = fixture.views.find(
        (view) => view.id === key || String(view.abbr).toLowerCase() === key.toLowerCase()
      );
      if (!target) throw new Error(`unknown account ${key}`);
      if (target.loginState === "loggedOut") {
        throw new Error(`${target.label} is logged out; log in again first`);
      }
      if (fixture.live.kind === "unknown" && unsaved === "refuse") {
        fixture.pending = {
          targetId: target.id,
          targetLabel: target.label,
          email: fixture.live.email || null,
          orgName: fixture.live.orgName || null,
        };
        throw new Error("not saved");
      }
      if (fixture.live.kind === "unknown" && unsaved === "save") {
        fixtureCreateFromLive(fixture, label);
      }
      fixture.pending = null;
      for (const view of fixture.views) view.isActive = view.id === target.id;
      fixture.activeId = target.id;
      fixture.live = { kind: "known", accountId: target.id };
      if (fixture.burn) fixture.burn.suggestion = null;
      fixture.lastPollAt = new Date().toISOString();
      return `Switched to ${target.abbr} — ${target.label}`;
    }

    case "cancel_pending":
      fixture.pending = null;
      return null;

    case "save_current": {
      if (fixture.live.kind === "unknown") {
        fixtureCreateFromLive(fixture, args.label);
        fixture.pending = null;
        return null;
      }
      if (fixture.live.kind === "known") {
        const account = fixture.views.find((view) => view.id === fixture.live.accountId);
        if (!account) throw new Error("the live account is not in the list");
        account.loginState = "ok";
        fixture.lastPollAt = new Date().toISOString();
        return null;
      }
      throw new Error("there is no live login to save");
    }

    case "edit_account": {
      const key = String(args.key || "");
      const account = fixture.views.find(
        (view) => view.id === key || String(view.abbr).toLowerCase() === key.toLowerCase()
      );
      if (!account) throw new Error(`unknown account ${key}`);
      const label = args.label === undefined || args.label === null
        ? account.label
        : String(args.label).trim();
      if (!label || label.length > 40) {
        throw new Error(`invalid label ${JSON.stringify(label)}: use 1 to 40 characters`);
      }
      const abbr = (args.abbr === undefined || args.abbr === null
        ? account.abbr
        : String(args.abbr)
      )
        .trim()
        .toUpperCase();
      if (!/^[A-Z0-9]{1,2}$/.test(abbr)) {
        throw new Error(
          `invalid abbreviation ${JSON.stringify(abbr)}: use 1-2 letters A-Z or digits`
        );
      }
      if (fixture.views.some((view) => view.id !== account.id && view.abbr === abbr)) {
        throw new Error(`abbreviation ${JSON.stringify(abbr)} is already used by another account`);
      }
      const color = (args.color === undefined || args.color === null
        ? account.color
        : String(args.color)
      )
        .trim()
        .toLowerCase();
      if (!/^#[0-9a-f]{6}$/.test(color)) {
        throw new Error(`invalid colour ${JSON.stringify(color)}: use #rrggbb`);
      }
      const autoBurn =
        args.autoBurn === undefined || args.autoBurn === null
          ? account.autoBurn
          : args.autoBurn;
      if (typeof autoBurn !== "boolean") {
        throw new Error("autoBurn must be true or false");
      }
      let price = account.price === undefined ? null : account.price;
      if (Object.prototype.hasOwnProperty.call(args, "price")) {
        if (args.price === null || args.price === "") {
          price = null;
        } else {
          const amount = Number(args.price);
          if (!Number.isFinite(amount) || amount <= 0) {
            throw new Error(
              `invalid price ${String(args.price)}: use a number above 0 per month`
            );
          }
          price = amount;
        }
      }
      account.label = label;
      account.abbr = abbr;
      account.color = color;
      account.autoBurn = autoBurn;
      account.price = price;
      return clone(account);
    }

    case "remove_account": {
      const key = String(args.key || "");
      const index = fixture.views.findIndex(
        (view) => view.id === key || String(view.abbr).toLowerCase() === key.toLowerCase()
      );
      if (index < 0) throw new Error(`unknown account ${key}`);
      const removed = fixture.views[index];
      fixture.views.splice(index, 1);
      if (fixture.live.kind === "known" && fixture.live.accountId === removed.id) {
        fixture.live = {
          kind: "unknown",
          email: removed.email || null,
          orgName: null,
        };
      }
      if (fixture.activeId === removed.id) fixture.activeId = null;
      for (const view of fixture.views) view.isActive = view.id === fixture.activeId;
      if (fixture.burn) fixture.burn.suggestion = null;
      if (Array.isArray(fixture.warnings)) {
        fixture.warnings = fixture.warnings.filter((warning) => warning.id !== removed.id);
      }
      return clone(removed);
    }

    case "set_auto_burn": {
      if (typeof args.on !== "boolean") {
        throw new Error("set_auto_burn needs a boolean on");
      }
      if (!fixture.burn) throw new Error("auto-burn is not loaded");
      fixture.settings = Object.assign({}, fixture.settings, { autoBurn: args.on });
      fixture.burn.on = args.on;
      if (args.on) fixture.burn.suggestion = null;
      fixture.lastPollAt = new Date().toISOString();
      return null;
    }

    case "pause_burn": {
      if (!fixture.burn) throw new Error("auto-burn is not loaded");
      const hours = args.hours;
      if (hours === null || hours === undefined) {
        fixture.burn.pausedUntil = null;
      } else {
        if (!Number.isInteger(hours) || hours <= 0) {
          throw new Error(`pause_burn needs a positive whole number of hours, got ${hours}`);
        }
        fixture.burn.pausedUntil = isoIn(hours * 3600000);
      }
      fixture.lastPollAt = new Date().toISOString();
      return null;
    }

    case "save_settings":
      fixture.settings = validateFixtureSettings(args.settings || {});
      if (fixture.burn) fixture.burn.on = fixture.settings.autoBurn;
      if (fixture.burn && fixture.settings.autoBurn) fixture.burn.suggestion = null;
      fixture.lastPollAt = fixture.lastPollAt || new Date().toISOString();
      return null;

    case "open_window":
      return null;

    case "agent_calls": {
      const limit = Number(args.limit);
      if (!Number.isInteger(limit) || limit < 0) {
        throw new Error(`agent_calls needs a limit of 0 or more, got ${args.limit}`);
      }
      return clone(
        fixture.agentCalls
          .slice()
          .sort((a, b) => Date.parse(b.at) - Date.parse(a.at))
          .slice(0, limit)
      );
    }

    case "money_view":
      return fixtureMoneyView(fixture);

    case "price_list":
      return clone(fixture.priceList);

    case "set_price_override": {
      const key = String(args.key || "");
      const row = fixture.priceList.find((entry) => entry.key === key);
      if (!row) throw new Error(`unknown model ${key}`);
      if (args.price === null || args.price === undefined) {
        const original = fixture.priceLookedUp && fixture.priceLookedUp[key];
        if (!original) throw new Error(`no looked-up price for ${key}`);
        row.price = clone(original);
        row.own = false;
        row.tiered = Boolean(original.tiered);
      } else {
        const source = args.price;
        const price = {
          input: Number(source.input),
          output: Number(source.output),
          cacheWrite: Number(source.cacheWrite),
          cacheRead: Number(source.cacheRead),
          tiered: Boolean(source.tiered),
        };
        for (const [name, value] of Object.entries(price)) {
          if (name === "tiered") continue;
          if (!Number.isFinite(value) || value < 0) {
            throw new Error(`the ${name} price must be a number of 0 or more`);
          }
        }
        row.price = price;
        row.own = true;
        row.tiered = price.tiered;
      }
      fixture.money.modelsWithPrice = fixture.priceList.length;
      return null;
    }

    case "lookup_now":
      fixture.money.pricesCheckedAt = new Date().toISOString();
      return fixtureMoneyView(fixture);

    case "skill_status":
      return clone(fixture.skill);

    case "skill_install": {
      if (typeof args.force !== "boolean") {
        throw new Error("skill_install needs force true or false");
      }
      if (fixture.skill.installed && fixture.skill.differs && !args.force) {
        throw new Error(
          `${fixture.skill.path} already exists and differs from seatswap's bundled skill; use --force to replace it`
        );
      }
      fixture.skill = Object.assign({}, fixture.skill, {
        installed: true,
        differs: false,
      });
      return clone(fixture.skill);
    }

    case "icon_preview": {
      const abbr = String(args.abbr || "").toUpperCase();
      const color = String(args.color || "").toLowerCase();
      if (!/^[A-Z0-9]{1,2}$/.test(abbr)) throw new Error("invalid abbreviation");
      if (!/^#[0-9a-f]{6}$/.test(color)) throw new Error("invalid colour");
      return [16, 22, 32].map((size) => ({
        size,
        rgba: Array.from(fixtureIcon(abbr, color, size)),
      }));
    }

    // Only a stand-in so the page can be looked at in a browser: the real
    // texts come from `format.rs`, which a browser cannot run.
    case "time_texts":
      return (args.times || []).map((time) => {
        if (time.style === "age") return "a moment ago";
        const date = new Date(time.at);
        if (time.style === "day" || time.style === "date") {
          const months = [
            "Jan",
            "Feb",
            "Mar",
            "Apr",
            "May",
            "Jun",
            "Jul",
            "Aug",
            "Sep",
            "Oct",
            "Nov",
            "Dec",
          ];
          // `date` is a calendar date: the day as written, without moving it
          // into the browser's time zone. `day` is a moment in local time.
          if (time.style === "date") {
            return `${date.getUTCDate()} ${months[date.getUTCMonth()]}`;
          }
          return `${date.getDate()} ${months[date.getMonth()]}`;
        }
        return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
      });

    case "projects_view":
      return clone(fixtureResolveProjects(fixture));

    case "projects_edit": {
      fixtureEditProject(fixture, args.edit);
      return clone(fixtureResolveProjects(fixture));
    }

    case "fit_panel":
      return null;

    case "hide_panel":
      return null;

    default:
      throw new Error(`unknown command ${command}`);
  }
}
