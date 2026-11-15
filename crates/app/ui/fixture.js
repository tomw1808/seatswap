// The built-in fixture both pages fall back to in a browser without Tauri:
// what the pop-up and the large window show without the tray app.
// Moved out of panel.js unchanged; its state lives here, not in either page.

import { PALETTE, clone, contrastText, hexToRgb, isObject } from "./common.js";
import { fixtureStats } from "./fixture-stats.js";

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
    // The folder the app runs with (the Snapshot's field); the General group
    // shows it as the empty config-folder field's placeholder.
    claudeConfigDir: "~/.claude",
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
        priceCurrency: "USD",
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
        priceCurrency: null,
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
        priceCurrency: "EUR",
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
      manualHoldH: 4,
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
        at: isoIn(-30 * 60000),
        command: "status --json",
        cwd: "~/projects/ledger",
        allowed: true,
      },
      {
        at: isoIn(-26 * hour),
        command: "use PR",
        cwd: "~/projects/ledger",
        allowed: false,
      },
      {
        at: isoIn(-50 * hour),
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
          host: null,
        },
      ],
      unavailable: null,
      warning: null,
    },
    money: {
      currency: "EUR",
      rate: { value: 0.9048, date: "2026-11-13", own: false },
      lookUp: true,
      providers: ["anthropic", "openai", "ollama-cloud"],
      modelsWithPrice: 4,
      pricesCheckedAt: isoIn(-2 * 3600000),
      lastChange: {
        at: isoIn(-6 * 86400000),
        added: ["openai/gpt-6"],
        changed: ["anthropic/claude-haiku-5-5"],
        removed: [],
      },
      leftOut: ["anthropic/claude-x-9: the input cost is not a number"],
      rateError: null,
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
    // The Projects group (spec §3.6): a fixed view the two project commands
    // read and update. The real app resolves the shared log in Rust.
    projectView: {
      projects: [
        {
          key: { kind: "repo", id: "github.com/acme/acme-kb" },
          name: "acme-kb",
          repo: "github.com/acme/acme-kb",
          org: "acme",
          joined: [],
          folders: { atlas: ["~/projects/acme-kb"] },
          sessions: 212,
        },
        {
          key: { kind: "repo", id: "github.com/acme/acme-notes" },
          name: "acme-notes",
          repo: "github.com/acme/acme-notes",
          org: "acme",
          joined: [],
          folders: { atlas: ["~/projects/acme-notes"] },
          sessions: 41,
        },
        {
          key: { kind: "repo", id: "github.com/sam/atlas" },
          name: "atlas",
          repo: "github.com/sam/atlas",
          org: "sam",
          joined: [],
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
          joined: [],
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
          joined: [],
          folders: { atlas: ["~/projects/seatswap"] },
          sessions: 63,
        },
        {
          key: { kind: "folder", id: "~", host: "atlas" },
          name: "~ (home folder)",
          repo: null,
          org: null,
          joined: [],
          folders: { atlas: ["~"] },
          sessions: 12,
        },
        {
          key: { kind: "folder", id: "~", host: "vega" },
          name: "~ (home folder)",
          repo: null,
          org: null,
          joined: [],
          folders: { vega: ["~"] },
          sessions: 10,
        },
      ],
      orgs: [
        {
          id: "acme",
          name: "acme",
          accounts: [{ acc: acmeId, org: "org:acme" }],
          projects: 2,
        },
        {
          id: "sam",
          name: "sam",
          accounts: [
            { acc: privateId, org: "org:sam" },
            { acc: labId, org: "org:sam" },
          ],
          projects: 3,
        },
      ],
    },
    // The History and hosts group (spec §1.5, §3.7): the host folders of the
    // history, and the files an export in this browser has "written".
    history: {
      host: "atlas",
      dir: "~/Sync/seatswap-history",
      hosts: [
        {
          host: "atlas",
          thisHost: true,
          firstRecord: isoIn(-100 * day),
          lastRecord: isoIn(-2 * 60000),
          bytes: 3040870,
        },
        {
          host: "vega",
          thisHost: false,
          firstRecord: isoIn(-72 * day),
          lastRecord: isoIn(-3 * hour),
          bytes: 1258291,
        },
      ],
      bytes: 4299161,
    },
    exports: [],
    settings: Object.assign({}, DEFAULT_SETTINGS, {
      autoBurn: true,
      suggestSwitches: true,
    }),
  };
}

// The history view as the app opened it, plus the saved settings' values when
// they differ: a restart would use those, as the real command reports.
function fixtureHistoryView(fixture) {
  const view = clone(fixture.history);
  const settings = fixture.settings || {};
  const host =
    typeof settings.hostName === "string" ? settings.hostName.trim() : "";
  view.savedHost = host !== "" && host !== view.host ? host : null;
  const dir =
    typeof settings.historyDir === "string" ? settings.historyDir.trim() : "";
  view.savedDir = dir !== "" && dir !== view.dir ? dir : null;
  view.bytes = view.hosts.reduce((total, host) => total + (Number(host.bytes) || 0), 0);
  return view;
}

// The command's `expand_tilde`: `~/…` is the fixture's home folder.
function fixtureExpand(path) {
  if (path === "~") return "/home/sam";
  if (path.startsWith("~/")) return `/home/sam/${path.slice(2)}`;
  return path;
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
  const own = isObject(fixture.settings.usdRate) ? fixture.settings.usdRate : null;
  if (!currency) {
    view.rate = null;
  } else if (currency === "USD") {
    view.rate = { value: 1, date: null, own: false };
  } else if (own && own.currency === currency) {
    view.rate = { value: own.value, date: null, own: true };
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
    priceCurrency: null,
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
  if (merged.usdRate !== null && merged.usdRate !== undefined) {
    const rate = merged.usdRate;
    if (
      !isObject(rate) ||
      !/^[A-Z]{3}$/.test(String(rate.currency)) ||
      !(Number.isFinite(rate.value) && rate.value > 0)
    ) {
      throw new Error(
        `usdRate must be {currency: three capital letters, value: above 0}, got ${JSON.stringify(rate)}`
      );
    }
  }
  if (typeof merged.lookUpPrices !== "boolean") {
    throw new Error(`lookUpPrices must be true or false, got ${merged.lookUpPrices}`);
  }
  return merged;
}

// One field replaced, the way the worker's `merge_setting` does it: the field
// must be a settings key, the two path settings expand a typed `~/`, and the
// result is validated as a whole.
function fixtureSaveSetting(fixture, field, value) {
  const base = Object.assign({}, DEFAULT_SETTINGS, fixture.settings || {});
  if (!Object.prototype.hasOwnProperty.call(base, field)) {
    throw new Error(`unknown setting ${field}`);
  }
  const next = clone(base);
  next[field] = value === undefined ? null : value;
  if (
    (field === "claudeConfigDir" || field === "historyDir") &&
    typeof next[field] === "string" &&
    (next[field] === "~" || next[field].startsWith("~/"))
  ) {
    next[field] = fixtureExpand(next[field]);
  }
  fixture.settings = validateFixtureSettings(next);
  if (fixture.burn) {
    fixture.burn.on = fixture.settings.autoBurn;
    if (fixture.settings.autoBurn) fixture.burn.suggestion = null;
  }
  fixture.lastPollAt = fixture.lastPollAt || new Date().toISOString();
  return { settings: clone(fixture.settings), warning: null };
}

// ---------------------------------------------------------------------------
// Projects and organizations (spec §3.6) in the fixture: a fixed view that
// `projects_view` returns and `projects_edit` changes in place. The real app
// resolves the shared log in core::projects; this is only for checking the
// page in a browser.
// ---------------------------------------------------------------------------

function fixtureAccountRef(view) {
  return { acc: view.id, org: `org:${view.orgName || view.id}` };
}

function fixtureIdText(id) {
  if (!isObject(id) || typeof id.id !== "string") return "";
  return typeof id.host === "string" && id.host !== ""
    ? `${id.kind}:${id.id}\u0000${id.host}`
    : `${id.kind}:${id.id}`;
}

function fixtureSameRef(first, second) {
  return Boolean(first && second && first.acc === second.acc && first.org === second.org);
}

function fixtureProjectView(fixture) {
  return {
    projects: clone(fixture.projectView.projects),
    orgs: clone(fixture.projectView.orgs),
    accountRefs: fixture.views.map((view) => ({
      id: view.id,
      account: fixtureAccountRef(view),
    })),
  };
}

function fixtureFindProject(projects, id) {
  return projects.find((project) => fixtureIdText(project.key) === fixtureIdText(id)) || null;
}

function fixtureFindOrg(orgs, id) {
  return orgs.find((org) => org.id === id) || null;
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

function fixtureCheckIdentity(value) {
  const text = String(value === undefined || value === null ? "" : value);
  if (text.trim() === "") throw new Error("an organization name must not be empty");
  return text;
}

// The same refusals as `ProjectEdit::validate`, then the fixed view is changed
// directly; nothing is appended when a check throws.
function fixtureEditProject(fixture, edit) {
  if (!isObject(edit)) throw new Error("projects_edit needs an edit");
  const view = fixture.projectView;
  switch (String(edit.edit || "")) {
    case "rename": {
      const project = fixtureFindProject(view.projects, edit.key);
      if (!project) throw new Error("unknown project");
      project.name = fixtureCheckName("project", edit.name);
      break;
    }
    case "orgRename": {
      const org = fixtureFindOrg(view.orgs, String(edit.org));
      if (!org) throw new Error(`unknown organization ${String(edit.org)}`);
      org.name = fixtureCheckName("organization", edit.name);
      break;
    }
    case "join": {
      if (fixtureIdText(edit.key) === fixtureIdText(edit.into)) {
        throw new Error("a project cannot be joined into itself");
      }
      const from = fixtureFindProject(view.projects, edit.key);
      const into = fixtureFindProject(view.projects, edit.into);
      if (!from || !into) throw new Error("unknown project");
      for (const [host, list] of Object.entries(from.folders || {})) {
        const folders = into.folders[host] || (into.folders[host] = []);
        for (const cwd of list) if (!folders.includes(cwd)) folders.push(cwd);
      }
      into.sessions = (Number(into.sessions) || 0) + (Number(from.sessions) || 0);
      into.joined = [...(into.joined || []), { key: clone(from.key), name: from.name }];
      view.projects = view.projects.filter((project) => project !== from);
      break;
    }
    case "unjoin": {
      for (const project of view.projects) {
        const joined = Array.isArray(project.joined) ? project.joined : [];
        const found = joined.find(
          (entry) => fixtureIdText(entry.key) === fixtureIdText(edit.key)
        );
        if (!found) continue;
        project.joined = joined.filter((entry) => entry !== found);
        view.projects.push({
          key: clone(found.key),
          name: found.name,
          repo: found.key.kind === "repo" ? found.key.id : null,
          org: null,
          joined: [],
          folders: {},
          sessions: 0,
        });
        break;
      }
      break;
    }
    case "assign": {
      const project = fixtureFindProject(view.projects, edit.key);
      if (!project) throw new Error("unknown project");
      const org =
        edit.org === null || edit.org === undefined ? null : fixtureCheckIdentity(edit.org);
      if (org !== null && !fixtureFindOrg(view.orgs, org)) {
        view.orgs.push({ id: org, name: org, accounts: [], projects: 0 });
      }
      project.org = org;
      break;
    }
    case "orgJoin": {
      const org = fixtureCheckIdentity(edit.org);
      const into = fixtureCheckIdentity(edit.into);
      if (org === into) throw new Error("an organization cannot be joined into itself");
      const source = fixtureFindOrg(view.orgs, org);
      const target = fixtureFindOrg(view.orgs, into);
      if (!source || !target) throw new Error("unknown organization");
      for (const account of source.accounts || []) {
        if (!target.accounts.some((entry) => fixtureSameRef(entry, account))) {
          target.accounts.push(account);
        }
      }
      for (const project of view.projects) if (project.org === org) project.org = into;
      view.orgs = view.orgs.filter((entry) => entry !== source);
      break;
    }
    case "accountOrg": {
      const ref =
        fixture.views
          .map((view) => fixtureAccountRef(view))
          .find((entry) => fixtureSameRef(entry, edit.account)) || null;
      if (!ref) throw new Error("unknown account");
      for (const org of view.orgs) {
        org.accounts = (org.accounts || []).filter((entry) => !fixtureSameRef(entry, ref));
      }
      if (edit.org !== null && edit.org !== undefined) {
        const org = fixtureCheckIdentity(edit.org);
        const target = fixtureFindOrg(view.orgs, org);
        if (!target) throw new Error(`unknown organization ${org}`);
        target.accounts.push(ref);
      }
      break;
    }
    default:
      throw new Error(`unknown project edit ${String(edit.edit)}`);
  }
  for (const org of view.orgs) {
    org.projects = view.projects.filter((project) => project.org === org.id).length;
  }
}

// ---------------------------------------------------------------------------
// the Sessions area's made-up view (spec §3.8), shaped by gui/window.rs
// ---------------------------------------------------------------------------

const SESSION_SUBV = 1.4696153846153845; // EUR per million output tokens
const SESSION_APIV = 20.357142857142858; // EUR at API prices
const SESSION_PROJECTS = {
  atlas: "github.com/sam/atlas",
  "acme-kb": "github.com/acme/acme-kb",
  ledger: "github.com/sam/ledger",
  seatswap: "github.com/sam/seatswap",
};

function fixtureTokenCounts(million) {
  return { input: 0, out: Math.round(million * 1000000), cw: 0, cr: 0 };
}

function fixtureSessionValues(million, currency = "EUR") {
  if (!(million > 0)) {
    return {
      api: null,
      apiCurrency: null,
      apiMissing: "no tokens yet",
      subscription: null,
      currency,
      subscriptionMissing: "no tokens yet",
    };
  }
  return {
    api: Number((million * SESSION_APIV).toFixed(2)),
    apiCurrency: currency,
    apiMissing: null,
    subscription: Number((million * SESSION_SUBV).toFixed(2)),
    currency,
    subscriptionMissing: null,
  };
}

function fixtureTodayAt(hours, minutes) {
  const date = new Date();
  date.setHours(hours, minutes, 0, 0);
  if (date.getTime() > Date.now()) date.setTime(Date.now() - (hours * 60 + minutes) * 60000);
  return date.toISOString();
}

function fixtureDaysAgoAt(days, hours, minutes) {
  const date = new Date();
  date.setDate(date.getDate() - days);
  date.setHours(hours, minutes, 0, 0);
  return date.toISOString();
}

function fixtureAgent(session, key, parent, kind, task, model, running, first, last, million) {
  return {
    agent: `${session}-${key}`,
    parent: parent ? `${session}-${parent}` : null,
    kind,
    task,
    model,
    running,
    first,
    last,
    tokens: fixtureTokenCounts(million),
    values: fixtureSessionValues(million),
  };
}

function fixtureFinishedAgents(session, host, count, tokensFor) {
  const agents = [];
  for (let index = 0; index < count; index += 1) {
    const last = new Date(Date.now() - (index + 1) * 7 * 60000).toISOString();
    agents.push(
      fixtureAgent(
        session,
        `done-${index}`,
        null,
        index % 3 === 0 ? "general-purpose" : "Explore",
        host === "atlas" ? `finished task ${index + 1}` : null,
        index % 2 === 0 ? "claude-sonnet-5-5" : "claude-opus-5-5",
        false,
        new Date(Date.parse(last) - 5 * 60000).toISOString(),
        last,
        tokensFor(index)
      )
    );
  }
  return agents;
}

// `?fixture=agents400` in the address lists one very large session: 400 agents,
// 380 completed and 20 running, five of the running ones with agents of their
// own. It exercises the tree's depth and its many-agent case.
function fixtureWantsAgents400() {
  if (typeof location === "undefined" || typeof URLSearchParams !== "function") return false;
  try {
    return new URLSearchParams(location.search).get("fixture") === "agents400";
  } catch {
    return false;
  }
}

function fixtureAgents400(session, host) {
  const agents = [];
  const task = (index) =>
    host === "atlas" ? `many-agent task ${index + 1}` : null;
  for (let index = 0; index < 10; index += 1) {
    agents.push(
      fixtureAgent(
        session,
        `run-${index}`,
        null,
        index % 3 === 0 ? "Plan" : index % 2 === 0 ? "Explore" : "general-purpose",
        task(index),
        index % 2 === 0 ? "claude-sonnet-5-5" : "claude-opus-5-5",
        true,
        isoIn(-(index + 1) * 60000),
        null,
        0.05
      )
    );
    if (index < 5) {
      for (let child = 0; child < 2; child += 1) {
        agents.push(
          fixtureAgent(
            session,
            `run-${index}-child-${child}`,
            `run-${index}`,
            "Explore",
            task(10 + index * 2 + child),
            "claude-sonnet-5-5",
            true,
            isoIn(-(index + 2) * 60000),
            null,
            0.05
          )
        );
      }
    }
  }
  agents.push(...fixtureFinishedAgents(session, host, 380, () => 0.01));
  return agents;
}

function fixtureSessionRows() {
  const atlasDone = fixtureFinishedAgents("atlas-a6", "atlas", 38, (index) => {
    if (index === 0) return 0.3;
    if (index === 1) return 0.9;
    return Number(((9.6 - 1.2) / 36).toFixed(4));
  });
  atlasDone[0].task = "find the config loader";
  atlasDone[0].last = fixtureTodayAt(9, 2);
  atlasDone[0].first = new Date(Date.parse(atlasDone[0].last) - 5 * 60000).toISOString();
  atlasDone[1].task = "write the migration";
  atlasDone[1].last = fixtureTodayAt(8, 41);
  atlasDone[1].first = new Date(Date.parse(atlasDone[1].last) - 5 * 60000).toISOString();
  for (const index of [36, 37]) {
    atlasDone[index].last = isoIn(-26 * 3600000);
    atlasDone[index].first = isoIn(-27 * 3600000);
  }

  const a6Children = [
    fixtureAgent(
      "atlas-a6",
      "gp-explore",
      "gp",
      "Explore",
      "find the callers of retry()",
      "claude-sonnet-5-5",
      true,
      isoIn(-2 * 60000),
      null,
      0.1
    ),
  ];
  const a6Agents = [
    fixtureAgent(
      "atlas-a6",
      "gp",
      null,
      "general-purpose",
      "fix the failing tests",
      "claude-opus-5-5",
      true,
      isoIn(-12 * 60000),
      null,
      1.1
    ),
    fixtureAgent(
      "atlas-a6",
      "plan",
      null,
      "Plan",
      "design the retry logic",
      "claude-opus-5-5",
      true,
      isoIn(-60000),
      null,
      0.2
    ),
    fixtureAgent(
      "atlas-a6",
      "explore",
      null,
      "Explore",
      "map the order engine",
      "claude-sonnet-5-5",
      true,
      isoIn(-3 * 60000),
      null,
      0.4
    ),
    ...a6Children,
    ...atlasDone,
  ];

  const s56Agents = fixtureFinishedAgents("atlas-56", "atlas", 12, () => 0.1);
  s56Agents[0].task = "read the pricing module";

  const ddAgents = [
    fixtureAgent(
      "acme-kb-dd",
      "explore",
      null,
      "Explore",
      "list the ingest sources",
      "claude-sonnet-5-5",
      true,
      isoIn(-2 * 60000),
      null,
      0.1
    ),
    ...fixtureFinishedAgents("acme-kb-dd", "atlas", 6, () => 0.2166667),
  ];

  const b3Agents = fixtureFinishedAgents("atlas-b3", "vega", 4, () => 0.1);
  b3Agents[0].task = null;
  const staleFableAgents = fixtureFinishedAgents("acme-kb-91", "ledger", 5, () => 0.1);
  for (const agent of staleFableAgents.slice(0, 2)) {
    agent.running = true;
    agent.last = null;
  }
  const staleSonnetAgents = fixtureFinishedAgents("acme-kb-a2", "ledger", 3, () => 0.08);
  staleSonnetAgents[0].running = true;
  staleSonnetAgents[0].last = null;
  const ledgerAgents = fixtureFinishedAgents("ledger-02", "vega", 1, () => 0.05);
  const s47Agents = fixtureFinishedAgents("seatswap-47", "atlas", 2, (index) =>
    index === 0 ? 0.6 : 0.5
  );
  s47Agents[0].task = "research a usage endpoint";
  s47Agents[1].task = "research a second provider";

  const make = (name, project, host, status, model, since, million, lastHour, running, done, agents, options = {}) => ({
    session: name,
    host,
    label: name,
    projectId: { kind: "repo", id: SESSION_PROJECTS[project] },
    project,
    org: null,
    cwd: `/home/sam/projects/${project}`,
    provider: options.provider || "Claude Code",
    model,
    tied: options.tied === true,
    status,
    since,
    started: since,
    agentsRunning: running,
    agentsDone: done,
    agents,
    tokens: fixtureTokenCounts(million),
    lastHour: fixtureTokenCounts(lastHour),
    values: fixtureSessionValues(million, options.currency || "EUR"),
  });

  const rows = [
    make("atlas-a6", "atlas", "atlas", "busy", "claude-opus-5-5", isoIn(-(2 * 60 + 31) * 60000), 12.4, 1.9, 3, 38, a6Agents),
    make("atlas-56", "atlas", "atlas", "idle", "claude-opus-5-5", isoIn(-6 * 3600000), 8.1, 0, 0, 12, s56Agents),
    make("atlas-11", "atlas", "atlas", "closed", "claude-opus-5-5", fixtureTodayAt(9, 12), 7.9, 0, 0, 17, []),
    make("atlas-b3", "atlas", "vega", "idle", "claude-fable-5-2", isoIn(-100 * 60000), 3.9, 0.5, 0, 4, b3Agents, { tied: true, currency: "USD" }),
    make("acme-kb-dd", "acme-kb", "atlas", "waiting", "claude-opus-5-5", isoIn(-4 * 60000), 2.2, 0.8, 1, 6, ddAgents),
    make("acme-kb-3c", "acme-kb", "atlas", "closed", "claude-opus-5-5", fixtureTodayAt(8, 40), 4.4, 0, 0, 9, []),
    make("atlas-c4", "atlas", "atlas", "closed", "claude-mythos-5-2", fixtureTodayAt(7, 30), 1.1, 0, 0, 5, [], { tied: true }),
    make("acme-kb-74", "acme-kb", "atlas", "idle", "claude-sonnet-5-5", isoIn(-10 * 3600000), 0.3, 0, 0, 0, [], { tied: true }),
    make("acme-kb-91", "acme-kb", "ledger", { other: "no report" }, "claude-fable-5-2", isoIn(-3 * 3600000), 1.2, 0, 2, 3, staleFableAgents, { tied: true }),
    make("acme-kb-a2", "acme-kb", "ledger", { other: "no report" }, "claude-sonnet-5-5", isoIn(-3 * 3600000), 0.9, 0, 1, 2, staleSonnetAgents),
    make("ledger-02", "ledger", "vega", "idle", "claude-opus-5-5", isoIn(-17 * 3600000), 0.1, 0, 0, 1, ledgerAgents),
    make("ledger-9e", "ledger", "vega", "closed", "claude-opus-5-5", fixtureDaysAgoAt(3, 22, 5), 1.6, 0, 0, 3, []),
    make("ledger-c1", "ledger", "atlas", "idle", "gpt-6-astra", isoIn(-35 * 60000), 0.4, 0, 0, 0, [], {
      provider: "Codex",
    }),
    make("seatswap-47", "seatswap", "atlas", "busy", "claude-fable-5-2", isoIn(-6 * 60000), 5.3, 1.2, 0, 2, s47Agents, { tied: true }),
    make("seatswap-b9", "seatswap", "atlas", "idle", null, isoIn(-19 * 3600000), 0, 0, 0, 0, []),
  ];
  if (fixtureWantsAgents400()) {
    rows.push(
      make(
        "atlas-7c",
        "atlas",
        "atlas",
        "busy",
        "claude-opus-5-5",
        isoIn(-60 * 60000),
        4.8,
        0.8,
        20,
        380,
        fixtureAgents400("atlas-7c", "atlas")
      )
    );
  }
  return rows;
}

function fixtureAddTokens(one, other) {
  return {
    input: one.input + other.input,
    out: one.out + other.out,
    cw: one.cw + other.cw,
    cr: one.cr + other.cr,
  };
}

function fixtureSums(rows) {
  const groups = new Map();
  const group = (of) => {
    const key = JSON.stringify(of);
    if (!groups.has(key)) {
      groups.set(key, {
        of,
        sessions: 0,
        tokens: fixtureTokenCounts(0),
        lastHour: fixtureTokenCounts(0),
      });
    }
    return groups.get(key);
  };
  group({ kind: "all" });
  for (const row of rows) {
    const keys = [
      { kind: "all" },
      { kind: "provider", provider: row.provider },
      { kind: "project", project: row.projectId },
      { kind: "projectModel", project: row.projectId, model: row.model },
      { kind: "model", model: row.model },
    ];
    for (const key of keys) {
      const found = group(key);
      found.sessions += 1;
      found.tokens = fixtureAddTokens(found.tokens, row.tokens);
      found.lastHour = fixtureAddTokens(found.lastHour, row.lastHour);
    }
  }
  return Array.from(groups.values()).map((found) => ({
    ...found,
    values: fixtureSessionValues(found.tokens.out / 1000000),
  }));
}

function fixtureSessionsView(args) {
  const options = isObject(args) && isObject(args.options) ? args.options : {};
  const closed = typeof options.closed === "string" ? options.closed : "none";
  if (!["none", "day", "week", "month"].includes(closed)) {
    throw new Error(`unknown closed filter ${JSON.stringify(options.closed)}`);
  }
  const host = typeof options.host === "string" && options.host !== "" ? options.host : null;
  const now = Date.now();
  const days = closed === "day" ? 1 : closed === "week" ? 7 : closed === "month" ? 30 : 0;
  let rows = fixtureSessionRows();
  if (closed === "none") {
    rows = rows.filter((row) => row.status !== "closed");
  } else {
    rows = rows.filter(
      (row) => row.status !== "closed" || Date.parse(row.since) >= now - days * 86400000
    );
  }
  if (host !== null) rows = rows.filter((row) => row.host === host);
  return {
    at: new Date(now).toISOString(),
    thisHost: "atlas",
    hosts: [
      { host: "atlas", at: new Date(now).toISOString(), stale: false, this: true },
      { host: "vega", at: new Date(now - 2 * 60000).toISOString(), stale: false, this: false },
      {
        host: "ledger",
        at: new Date(now - 3 * 3600000).toISOString(),
        stale: true,
        this: false,
      },
    ],
    sessions: rows,
    sums: fixtureSums(rows),
    switchEffect: {
      losesThinking: 3,
      losesThinkingBusy: 1,
      losesThinkingModels: ["claude-fable-5-2", "claude-sonnet-5-5"],
      resend: 2,
      free: 4,
    },
    currency: "EUR",
    unavailable: null,
  };
}

export async function fixtureInvoke(command, args) {
  console.log(`[fixture] ${command}`, args);
  if (String(command).startsWith("stats_")) return fixtureStats(command, args);
  const fixture = currentFixture();
  if (!fixture && command !== "fit_panel" && command !== "hide_panel") {
    throw new Error("fixture is not ready");
  }

  switch (command) {
    case "get_snapshot": {
      const snapshot = clone(fixture);
      snapshot.accounts = fixture.views.map((view) => clone(view));
      delete snapshot.projectView;
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
      let priceCurrency = account.priceCurrency || null;
      if (Object.prototype.hasOwnProperty.call(args, "price")) {
        if (args.price === null || args.price === "") {
          price = null;
          priceCurrency = null;
        } else {
          const code =
            typeof fixture.settings.currency === "string"
              ? fixture.settings.currency
              : null;
          if (!code) {
            throw new Error("set a currency in Settings → General before a price");
          }
          const amount = Number(args.price);
          if (!Number.isFinite(amount) || amount <= 0) {
            throw new Error(
              `invalid price ${String(args.price)}: use a number above 0 per month`
            );
          }
          price = amount;
          priceCurrency = code;
        }
      }
      account.label = label;
      account.abbr = abbr;
      account.color = color;
      account.autoBurn = autoBurn;
      account.price = price;
      account.priceCurrency = priceCurrency;
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
      fixtureSaveSetting(fixture, "autoBurn", args.on);
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

    case "save_setting":
      return fixtureSaveSetting(fixture, String(args.field || ""), args.value);

    case "sessions_view":
      return clone(fixtureSessionsView(args));

    case "sessions_subscribe":
      return null;

    case "sessions_unsubscribe":
      return null;

    case "open_panel":
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
      const PROVIDERS = ["anthropic", "openai", "ollama-cloud"];
      const mark = key.indexOf("/");
      const provider = mark < 0 ? "" : key.slice(0, mark);
      const modelId = mark < 0 ? "" : key.slice(mark + 1);
      if (!PROVIDERS.includes(provider) || modelId === "") {
        throw new Error(
          "a model key is provider/model with a provider from anthropic, openai, ollama-cloud and a model id, e.g. anthropic/claude-opus-5-5"
        );
      }
      let row = fixture.priceList.find((entry) => entry.key === key);
      if (args.price === null || args.price === undefined) {
        if (!row) throw new Error(`no own price for ${key}`);
        const original = fixture.priceLookedUp && fixture.priceLookedUp[key];
        if (!original) {
          fixture.priceList = fixture.priceList.filter((entry) => entry.key !== key);
        } else {
          row.price = clone(original);
          row.own = false;
          row.tiered = Boolean(original.tiered);
        }
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
        if (!row) {
          row = {
            key,
            price,
            own: true,
            tiered: price.tiered,
            since: null,
          };
          fixture.priceList.push(row);
        } else {
          row.price = price;
          row.own = true;
          row.tiered = price.tiered;
        }
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
    case "time_texts": {
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
      const weekdays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
      const clock = (date) =>
        `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
      return (args.times || []).map((time) => {
        if (time.style === "age") return "a moment ago";
        const date = new Date(time.at);
        if (time.style === "day" || time.style === "date") {
          // `date` is a calendar date: the day as written, without moving it
          // into the browser's time zone. `day` is a moment in local time.
          if (time.style === "date") {
            return `${date.getUTCDate()} ${months[date.getUTCMonth()]}`;
          }
          return `${date.getDate()} ${months[date.getMonth()]}`;
        }
        if (time.style === "dayClock") {
          const now = new Date();
          const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
          const day = new Date(date.getFullYear(), date.getMonth(), date.getDate());
          const days = Math.round((today - day) / 86400000);
          // Today keeps the clock; the last six days name the weekday; older
          // calls keep the day and month in front, as `TimeStyle::DayClock`.
          if (days === 0) return clock(date);
          if (days > 0 && days <= 6) return `${weekdays[date.getDay()]} ${clock(date)}`;
          return `${date.getDate()} ${months[date.getMonth()]} ${clock(date)}`;
        }
        return clock(date);
      });
    }

    case "projects_view":
      return clone(fixtureProjectView(fixture));

    case "projects_edit": {
      fixtureEditProject(fixture, args.edit);
      return clone(fixtureProjectView(fixture));
    }

    case "history_view":
      return fixtureHistoryView(fixture);

    case "history_import": {
      const from = fixtureExpand(String(args.from || "").trim());
      if (from === "") throw new Error("enter a folder path");
      if (!from.startsWith("/")) {
        throw new Error(`the folder must be an absolute path, got ${from}`);
      }
      const folders = from.replace(/\/+$/, "").split("/");
      const name = String(folders[folders.length - 1] || "").toLowerCase();
      // The history folder itself, as the group shows it ("~" is the home folder).
      const own = fixtureHistoryView(fixture).dir.replace(/^~/, "/home/sam");
      if (from.replace(/\/+$/, "") === own || name === fixture.history.host) {
        throw new Error(`${from}: that is this history folder`);
      }
      if (!/^[a-z0-9][a-z0-9._-]*$/.test(name)) {
        throw new Error(`no host folder found in ${from}`);
      }
      const known = fixture.history.hosts.find((host) => host.host === name);
      const copied = [];
      if (!known) {
        fixture.history.hosts.push({
          host: name,
          thisHost: false,
          firstRecord: isoIn(-30 * 86400000),
          lastRecord: isoIn(-60000),
          bytes: 734003,
        });
        copied.push(name);
      } else {
        // One more file arrived from that host.
        known.bytes += 262144;
        known.lastRecord = isoIn(-30000);
        copied.push(name);
      }
      fixture.history.hosts.sort((one, other) => (one.host < other.host ? -1 : 1));
      return { copied, conflicts: [], view: fixtureHistoryView(fixture) };
    }

    case "history_export": {
      const format = String(args.format || "");
      if (format !== "csv" && format !== "json") {
        throw new Error(`unknown export format ${JSON.stringify(format)}: use "csv" or "json"`);
      }
      const to = fixtureExpand(String(args.to || "").trim());
      if (to === "") throw new Error("enter a file path");
      if (!to.startsWith("/")) throw new Error(`the export path must be an absolute path, got ${to}`);
      const from = args.from ? Date.parse(String(args.from)) : null;
      const until = args.until ? Date.parse(String(args.until)) : null;
      if (from !== null && until !== null && !(from < until)) {
        throw new Error(`from ${args.from} is not before until ${args.until}`);
      }
      if (fixture.exports.includes(to)) throw new Error(`${to} exists; choose another name`);
      fixture.exports.push(to);
      return null;
    }

    case "fit_panel":
      return null;

    case "hide_panel":
      return null;

    default:
      throw new Error(`unknown command ${command}`);
  }
}
