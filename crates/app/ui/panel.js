// seatswap pop-up panel.
// Plain ES2020, no build step. Talks to the tray app through the Tauri 2 global API;
// without it, runs on a built-in fixture snapshot so the page can be checked in a browser.

// ---------------------------------------------------------------------------
// constants and helpers
// ---------------------------------------------------------------------------

const PALETTE = [
  "#3b82f6", "#f59e0b", "#10b981", "#ef4444", "#8b5cf6",
  "#ec4899", "#14b8a6", "#f97316", "#6366f1", "#84cc16",
];

// The tray's grey for an unknown or invalid account colour (icon.rs GREY).
const TRAY_GREY = "#555b66";

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
  agentsRead: true,
  agentsSwitch: false,
  statuslineFeed: true,
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

const LEVEL_COLORS = {
  fine: "#22c55e",
  close: "#f59e0b",
  critical: "#ef4444",
  locked: "#ef4444",
};


const TAURI_GLOBAL = typeof window !== "undefined" ? window.__TAURI__ : undefined;
const HAS_TAURI = Boolean(
  TAURI_GLOBAL && TAURI_GLOBAL.core && typeof TAURI_GLOBAL.core.invoke === "function"
);

const state = {
  snapshot: null,
  view: "main",
  // The account the keyboard cursor (↑/↓, Enter) is on; null = no cursor shown.
  // Cleared every time the pop-up opens, so it never outlives one use.
  cursorId: null,
  // The account a switch waits for the person to confirm; only a switch with
  // sessions on a model that loses its thinking asks (spec §2.12).
  confirmSwitchId: null,
  editingId: null,
  editor: null,
  pendingKey: null,
  pendingLabel: "",
  unknownKey: null,
  unknownLabel: "",
  settingsDraft: null,
  // { text, title } for the settings header; null until loadVersion() has answered.
  version: null,
  refreshing: false,
  lastPercents: new Map(),
  lastCardOrder: [],
  errors: {
    banner: null,
    burn: null,
    accounts: {},
    editor: null,
    editorColor: null,
    preview: null,
    add: null,
    settings: null,
    settingsConfigDir: null,
    settingsHostName: null,
    settingsHistoryDir: null,
    settingsSaved: false,
  },
  fixture: null,
};

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function clamp(value, low, high) {
  return Math.min(high, Math.max(low, value));
}

function clone(value) {
  return value === undefined || value === null ? value : JSON.parse(JSON.stringify(value));
}

function errorMessage(error) {
  if (typeof error === "string") return error;
  if (error && typeof error.message === "string") return error.message;
  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}

function isColor(value) {
  return typeof value === "string" && /^#[0-9a-fA-F]{6}$/.test(value);
}

function hexToRgb(value) {
  if (!isColor(value)) return null;
  const number = parseInt(value.slice(1), 16);
  return {
    r: (number >> 16) & 0xff,
    g: (number >> 8) & 0xff,
    b: number & 0xff,
  };
}

function rgba(value, alpha) {
  const rgb = hexToRgb(value);
  if (!rgb) return `rgba(139, 147, 167, ${alpha})`;
  return `rgba(${rgb.r}, ${rgb.g}, ${rgb.b}, ${alpha})`;
}

function rgbToHex(rgb) {
  const part = (number) => clamp(Math.round(number), 0, 255).toString(16).padStart(2, "0");
  return `#${part(rgb.r)}${part(rgb.g)}${part(rgb.b)}`;
}

function mixHex(first, second, ratio) {
  const a = hexToRgb(first);
  const b = hexToRgb(second);
  if (!a || !b) return first;
  const mix = (x, y) => x + (y - x) * ratio;
  return rgbToHex({ r: mix(a.r, b.r), g: mix(a.g, b.g), b: mix(a.b, b.b) });
}

function relativeLuminance(value) {
  const rgb = hexToRgb(value);
  if (!rgb) return 0;
  const channel = (part) => {
    const scaled = part / 255;
    return scaled <= 0.03928 ? scaled / 12.92 : Math.pow((scaled + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * channel(rgb.r) + 0.7152 * channel(rgb.g) + 0.0722 * channel(rgb.b);
}

function contrastText(background) {
  const luminance = relativeLuminance(background);
  const whiteRatio = 1.05 / (luminance + 0.05);
  const darkRatio = (luminance + 0.05) / (relativeLuminance("#0f1115") + 0.05);
  return darkRatio >= whiteRatio ? "#0f1115" : "#ffffff";
}

function limitLevel(limit) {
  const percent = Number(limit && limit.percent);
  if (!Number.isFinite(percent)) return "fine";
  if (percent >= 100) return "locked";
  if (percent >= 90 || limit.severity === "critical") return "critical";
  if (percent >= 70) return "close";
  return "fine";
}

function shortPercent(value) {
  const number = Number(value);
  if (!Number.isFinite(number)) return "–";
  const rounded = Math.round(number * 10) / 10;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

function fmtPercent(value) {
  return `${shortPercent(value)} %`;
}

function prefersReducedMotion() {
  return Boolean(
    typeof window !== "undefined" &&
      window.matchMedia &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

function currentThemeSetting() {
  if (state.snapshot && isObject(state.snapshot.settings) && state.snapshot.settings.theme) {
    return state.snapshot.settings.theme;
  }
  if (state.settingsDraft && state.settingsDraft.theme) return state.settingsDraft.theme;
  return "system";
}

function applyTheme(setting) {
  const wanted = setting || currentThemeSetting();
  let resolved = wanted;
  if (wanted !== "light" && wanted !== "dark") {
    const light =
      typeof window !== "undefined" &&
      window.matchMedia &&
      window.matchMedia("(prefers-color-scheme: light)").matches;
    resolved = light ? "light" : "dark";
  }
  document.documentElement.dataset.theme = resolved;
}

// ---------------------------------------------------------------------------
// bridge: invoke() and listen() over the Tauri 2 global
// ---------------------------------------------------------------------------

async function invoke(command, args) {
  if (HAS_TAURI) {
    return TAURI_GLOBAL.core.invoke(command, args || {});
  }
  return fixtureInvoke(command, args || {});
}

async function listen(eventName, handler) {
  if (HAS_TAURI && TAURI_GLOBAL.event && typeof TAURI_GLOBAL.event.listen === "function") {
    const unlisten = await TAURI_GLOBAL.event.listen(eventName, (event) => {
      handler(event && Object.prototype.hasOwnProperty.call(event, "payload") ? event.payload : event);
    });
    return unlisten;
  }
  return () => {};
}

// ---------------------------------------------------------------------------
// DOM helpers (all data goes through text nodes or attributes)
// ---------------------------------------------------------------------------

function clear(node) {
  while (node && node.firstChild) node.removeChild(node.firstChild);
}

function setStyles(node, styles) {
  for (const [key, value] of Object.entries(styles)) {
    if (value === undefined || value === null) continue;
    if (key.startsWith("--")) node.style.setProperty(key, String(value));
    else node.style[key] = value;
  }
}

function applyProps(node, props) {
  for (const [key, value] of Object.entries(props || {})) {
    if (key === "hidden") {
      node.hidden = Boolean(value);
      continue;
    }
    if (key === "checked") {
      node.checked = Boolean(value);
      continue;
    }
    if (key === "disabled") {
      node.disabled = Boolean(value);
      continue;
    }
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") node.className = value;
    else if (key === "text") node.textContent = String(value);
    else if (key === "style") setStyles(node, value);
    else if (key === "dataset") {
      for (const [dataKey, dataValue] of Object.entries(value)) {
        if (dataValue !== undefined && dataValue !== null) node.dataset[dataKey] = String(dataValue);
      }
    } else if (key === "value") {
      node.value = value;
    } else if (key.startsWith("on") && typeof value === "function") {
      node.addEventListener(key.slice(2), value);
    } else if (value === true) {
      node.setAttribute(key, "");
    } else {
      node.setAttribute(key, String(value));
    }
  }
}

function appendChildren(node, children) {
  const list = Array.isArray(children) ? children : [children];
  for (const child of list) {
    if (child === undefined || child === null || child === false) continue;
    if (typeof child === "string" || typeof child === "number") {
      node.appendChild(document.createTextNode(String(child)));
    } else {
      node.appendChild(child);
    }
  }
}

function h(tag, props, children) {
  const node = document.createElement(tag);
  applyProps(node, props || {});
  if (children !== undefined) appendChildren(node, children);
  return node;
}

function button(label, variant, onclick, props) {
  const extra = props || {};
  const className = ["btn", variant || "", extra.class || ""].filter(Boolean).join(" ");
  const merged = Object.assign({ type: "button" }, extra, {
    class: className,
    onclick,
  });
  return h("button", merged, label);
}

function textInput(props) {
  return h("input", Object.assign({ type: "text", spellcheck: "false" }, props || {}));
}

function selectInput(options, value, onchange) {
  const select = h("select", {
    onchange: (event) => onchange(event.target.value),
  });
  for (const [optionValue, optionLabel] of options) {
    const option = h("option", { value: optionValue, text: optionLabel });
    if (optionValue === value) option.selected = true;
    select.appendChild(option);
  }
  return select;
}

// ---------------------------------------------------------------------------
// snapshot field readers (the shape is docs/implementation.md, serde camelCase)
// ---------------------------------------------------------------------------

function allViews() {
  if (!state.snapshot || !Array.isArray(state.snapshot.views)) return [];
  return state.snapshot.views;
}

function viewIsActive(view) {
  if (!view) return false;
  if (view.isActive) return true;
  return Boolean(
    state.snapshot && state.snapshot.activeId && view.id === state.snapshot.activeId
  );
}

// The active account is the bottom card; the others keep the list order.
function orderedViews(views) {
  return (Array.isArray(views) ? views.slice() : []).sort(
    (a, b) => Number(viewIsActive(a)) - Number(viewIsActive(b))
  );
}

function findView(id) {
  if (!id) return null;
  return allViews().find((view) => view.id === id) || null;
}

function accountLimits(view) {
  if (!view || !isObject(view.usage) || !Array.isArray(view.usage.limits)) return [];
  return view.usage.limits;
}

function limitKindName(limit) {
  if (!isObject(limit)) return "";
  const kind = limit.kind;
  if (typeof kind === "string") return kind;
  // serde serializes LimitKind::Other("daily_tokens") as {"other":"daily_tokens"}.
  if (isObject(kind) && typeof kind.other === "string") return kind.other;
  return "";
}

function findLimit(view, kind) {
  return accountLimits(view).find((limit) => limitKindName(limit) === kind) || null;
}

function scopedLimits(view) {
  return accountLimits(view).filter((limit) => limitKindName(limit) === "weekly_scoped");
}

function otherLimits(view) {
  const known = new Set(["session", "weekly_all", "weekly_scoped"]);
  return accountLimits(view)
    .map((limit) => ({ name: limitKindName(limit), limit }))
    .filter((entry) => entry.name && !known.has(entry.name));
}

// The age of an account's usage, shown once it is older than two minutes.
function usageAge(view) {
  if (!view || !view.usage || !view.fetchedAt) return null;
  const time = Date.parse(view.fetchedAt);
  if (!Number.isFinite(time)) return null;
  if (Date.now() - time < 120000) return null;
  return h("span", {}, ["usage ", timeNode(view.fetchedAt, "age")]);
}

function accountLabel(view) {
  if (!view) return "?";
  return view.abbr || view.label || "?";
}

function loginWarningText(warning) {
  if (!isObject(warning)) return "";
  const account = findView(warning.id);
  const name = account ? account.label : "An account";
  if (warning.kind === "soon") {
    const days = Number(warning.daysLeft);
    return `${name} — login ends in ${Number.isFinite(days) ? days : "?"} d`;
  }
  if (warning.kind === "tomorrow") return `${name} — login ends tomorrow`;
  if (warning.kind === "ended") return `${name} — login ended, log in again`;
  return "";
}

// ---------------------------------------------------------------------------
// theme
// ---------------------------------------------------------------------------

function wireTheme() {
  if (!window.matchMedia) return;
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  if (typeof media.addEventListener === "function") {
    media.addEventListener("change", () => {
      if (currentThemeSetting() === "system") applyTheme("system");
    });
  }
}

// ---------------------------------------------------------------------------
// rendering: main view
// ---------------------------------------------------------------------------

function renderMain() {
  applyTheme();
  const cards = document.getElementById("cards");
  const banners = document.getElementById("banners");
  if (!cards || !banners) return;

  const activeElement = document.activeElement;
  const focusField =
    activeElement && activeElement.dataset && activeElement.dataset.field
      ? activeElement.dataset.field
      : null;
  const focusSelection =
    focusField && activeElement && typeof activeElement.selectionStart === "number"
      ? activeElement.selectionStart
      : null;

  const views = orderedViews(state.snapshot ? state.snapshot.views : []);
  if (state.cursorId && !views.some((view) => view.id === state.cursorId)) {
    state.cursorId = null;
  }

  if (state.editingId && !findView(state.editingId)) {
    state.editingId = null;
    state.editor = null;
    state.errors.editor = null;
    state.errors.editorColor = null;
  }
  if (state.confirmSwitchId && !views.some((view) => view.id === state.confirmSwitchId)) {
    state.confirmSwitchId = null;
  }

  clear(banners);
  renderBanners(banners);

  const oldStrip = document.getElementById("burn-strip");
  if (oldStrip) oldStrip.remove();
  if (state.snapshot && isObject(state.snapshot.burn)) {
    const strip = renderBurnStrip();
    if (strip && cards.parentNode) cards.parentNode.insertBefore(strip, cards);
  } else {
    state.errors.burn = null;
  }

  const previousPositions = captureCardPositions();
  clear(cards);
  if (state.snapshot) {
    views.forEach((view, index) => {
      cards.appendChild(renderCard(view, index));
    });
  }

  const nextOrder = views.map((view) => view.id);
  const reordered =
    state.lastCardOrder.length === nextOrder.length &&
    state.lastCardOrder.some((id, index) => id !== nextOrder[index]);
  state.lastCardOrder = nextOrder;
  if (reordered) animateCardMoves(cards, previousPositions);

  renderFooter();
  updateAge();
  fitPanel();

  if (focusField) {
    const next = document.querySelector(`[data-field="${focusField}"]`);
    if (next && typeof next.focus === "function") {
      next.focus();
      if (focusSelection !== null && typeof next.setSelectionRange === "function") {
        try {
          next.setSelectionRange(focusSelection, focusSelection);
        } catch {
          // not a text field; ignore
        }
      }
    }
  }
}

// The running sessions and the number that wait for you (spec §4.1). The strip
// carries the same line while auto-burn has a view; the footer keeps it
// otherwise.
function sessionsLineNode(sessions, className) {
  if (sessions.unavailable != null) {
    return h("span", { class: className, text: String(sessions.unavailable) });
  }
  const waiting = Array.isArray(sessions.waiting) ? sessions.waiting.length : 0;
  const line = h("span", { class: className }, [
    `${sessions.running} sessions · ${sessions.busy} busy`,
  ]);
  if (waiting > 0) {
    line.appendChild(
      h("span", { class: "footer-waiting", text: ` · ${waiting} waiting for you` })
    );
  }
  return line;
}

// The footer line beside "+ Add account": the running sessions, and a warning
// mark whose tooltip is the sessions thread's warning (spec §1.6).
function renderFooter() {
  const footer = document.querySelector(".footer");
  if (!footer) return;
  for (const old of footer.querySelectorAll(".footer-sessions, .footer-warning")) {
    old.remove();
  }
  const snapshot = state.snapshot;
  if (!snapshot || snapshot.sessions == null) return;
  const sessions = snapshot.sessions;
  // The strip above the cards shows this line whenever auto-burn has a view.
  if (!isObject(snapshot.burn)) {
    footer.appendChild(sessionsLineNode(sessions, "footer-sessions"));
  }
  if (sessions.warning != null) {
    footer.appendChild(
      h("span", {
        class: "footer-warning",
        title: String(sessions.warning),
        text: "⚠",
      })
    );
  }
}

// The strip above the cards: the auto-burn switch, Pause/Resume, the plan's
// line, a waiting switch and the running sessions (spec §2.4, §2.12).
function renderBurnStrip() {
  const burn = state.snapshot.burn;
  const strip = h("div", {
    class: "burn-strip",
    id: "burn-strip",
    role: "group",
    "aria-label": "Auto-burn",
  });
  const errors = [];

  if (typeof burn.on !== "boolean") {
    errors.push("The auto-burn state is missing.");
  }
  const on = burn.on === true;
  const row = h("div", { class: "burn-row" }, [
    h("b", { class: "burn-title", text: "Auto-burn" }),
    typeof burn.on === "boolean"
      ? switchButton(on, "Auto-burn", setAutoBurn)
      : null,
    h("span", { class: "burn-state", text: on ? "On" : "Off" }),
    h("span", { class: "burn-spacer" }),
  ]);

  let pausedAt = NaN;
  if (burn.pausedUntil != null) {
    if (typeof burn.pausedUntil === "string") {
      pausedAt = Date.parse(burn.pausedUntil);
    }
    if (!Number.isFinite(pausedAt)) {
      errors.push("The auto-burn pause time is not a time.");
      pausedAt = NaN;
    }
  }
  if (pausedAt > Date.now()) {
    const paused = h("span", { class: "burn-paused" }, [
      "Auto-burn paused until ",
      timeNode(burn.pausedUntil, "clock"),
      " · ",
    ]);
    paused.appendChild(button("Resume", "ghost small", () => pauseBurn(null)));
    row.appendChild(paused);
  } else {
    row.appendChild(button("Pause 4 h", "ghost small", () => pauseBurn(4)));
  }
  strip.appendChild(row);

  if (burn.line != null) {
    strip.appendChild(h("p", { class: "burn-line", text: String(burn.line) }));
  }

  if (burn.waiting != null) {
    const waiting = isObject(burn.waiting) ? burn.waiting : null;
    const target = waiting ? findView(waiting.to) : null;
    const tiedBusy = waiting ? Number(waiting.tiedBusy) : NaN;
    if (!waiting) {
      errors.push("The waiting switch is incomplete.");
    } else if (!target) {
      errors.push("The waiting switch names an account that is not in the list.");
    } else if (!Number.isInteger(tiedBusy) || tiedBusy < 0) {
      errors.push("The waiting switch has no valid number of busy sessions.");
    } else if (typeof waiting.latest !== "string") {
      errors.push("The waiting switch has no latest time.");
    } else {
      strip.appendChild(
        h("p", { class: "burn-waiting" }, [
          `A switch to ${accountLabel(target)} is waiting: ${tiedBusy} ${
            tiedBusy === 1 ? "session" : "sessions"
          } on a model that would lose its thinking ${
            tiedBusy === 1 ? "is" : "are"
          } busy. At the latest `,
          timeNode(waiting.latest, "clock"),
          ".",
        ])
      );
    }
  }

  if (state.snapshot.sessions != null) {
    strip.appendChild(sessionsLineNode(state.snapshot.sessions, "burn-sessions"));
  }

  if (state.errors.burn) errors.unshift(state.errors.burn);
  for (const text of errors) {
    strip.appendChild(h("div", { class: "inline-error burn-error", text }));
  }
  return strip;
}

async function setAutoBurn(on) {
  state.errors.burn = null;
  try {
    await invoke("set_auto_burn", { on });
    afterCommand();
  } catch (error) {
    state.errors.burn = errorMessage(error);
    renderMain();
  }
}

async function pauseBurn(hours) {
  state.errors.burn = null;
  try {
    await invoke("pause_burn", { hours });
    afterCommand();
  } catch (error) {
    state.errors.burn = errorMessage(error);
    renderMain();
  }
}

function captureCardPositions() {
  const positions = new Map();
  const cards = document.getElementById("cards");
  if (!cards) return positions;
  for (const child of cards.children) {
    if (child.dataset && child.dataset.accountId) {
      positions.set(child.dataset.accountId, child.getBoundingClientRect().top);
    }
  }
  return positions;
}

function animateCardMoves(cards, previousPositions) {
  if (prefersReducedMotion()) return;
  for (const child of cards.children) {
    const id = child.dataset ? child.dataset.accountId : null;
    if (!id || !previousPositions.has(id)) continue;
    const delta = previousPositions.get(id) - child.getBoundingClientRect().top;
    if (Math.abs(delta) < 1) continue;
    child.style.transition = "none";
    child.style.transform = `translateY(${delta}px)`;
    void child.getBoundingClientRect();
    requestAnimationFrame(() => {
      child.style.transition = "transform 250ms cubic-bezier(0.34, 1.3, 0.64, 1)";
      child.style.transform = "translateY(0)";
      setTimeout(() => {
        child.style.transition = "";
        child.style.transform = "";
      }, 280);
    });
  }
}

function renderBanners(container) {
  const snapshot = state.snapshot;
  if (!snapshot) return;

  if (state.errors.banner) {
    container.appendChild(
      banner("error", "⚠", state.errors.banner, [
        button("Dismiss", "ghost", () => {
          state.errors.banner = null;
          renderMain();
        }, { class: "small" }),
      ])
    );
  }

  if (snapshot.error) {
    container.appendChild(banner("error", "⚠", String(snapshot.error)));
  }

  if (snapshot.secretsWarning) {
    container.appendChild(banner("warn", "⚠", String(snapshot.secretsWarning)));
  }

  if (snapshot.pending) {
    container.appendChild(renderPendingBanner(snapshot.pending));
  } else {
    state.pendingKey = null;
    if (snapshot.live && snapshot.live.kind === "unknown") {
      container.appendChild(renderUnknownLoginBanner(snapshot.live));
    } else {
      state.unknownKey = null;
    }
  }

  if (isObject(snapshot.burn) && isObject(snapshot.burn.suggestion)) {
    container.appendChild(renderBurnSuggestion(snapshot.burn.suggestion));
  }
  const confirmation = renderSwitchConfirmation();
  if (confirmation) container.appendChild(confirmation);

  if (Array.isArray(snapshot.warnings)) {
    for (const warning of snapshot.warnings) {
      const text = loginWarningText(warning);
      if (!text) continue;
      const kind = warning.kind === "ended" ? "error" : "warn";
      container.appendChild(banner(kind, "⏳", text));
    }
  }
}

function banner(kind, icon, text, extraChildren) {
  const body = h("div", { class: "banner-body" }, [
    h("div", { class: "banner-text" }, Array.isArray(text) ? text : String(text)),
  ]);
  if (extraChildren) {
    if (Array.isArray(extraChildren)) {
      for (const child of extraChildren) {
        if (child) body.appendChild(child);
      }
    } else {
      body.appendChild(extraChildren);
    }
  }
  return h(
    "div",
    {
      class: `banner ${kind}`,
      role: kind === "error" ? "alert" : "status",
    },
    [h("span", { class: "banner-icon", "aria-hidden": "true", text: icon }), body]
  );
}

function pendingLabel(pending) {
  const key = `${pending.targetId || ""}|${pending.email || ""}|${pending.orgName || ""}`;
  if (state.pendingKey !== key) {
    state.pendingKey = key;
    state.pendingLabel = String(pending.orgName || pending.email || "New account").slice(0, 40);
  }
  return state.pendingLabel;
}

function renderPendingBanner(pending) {
  const label = pendingLabel(pending);
  const input = textInput({
    value: label,
    maxlength: "40",
    "aria-label": "Label for the saved account",
    class: "banner-label-input",
    dataset: { field: "pending-label" },
  });
  input.addEventListener("input", () => {
    state.pendingLabel = input.value;
  });

  const actions = h("div", { class: "banner-actions" }, [
    button("Save as new account", "primary small", () =>
      switchWithPolicy(pending.targetId, "save", state.pendingLabel)
    ),
    button("Discard it", "danger small", () =>
      switchWithPolicy(pending.targetId, "discard", null)
    ),
    button("Cancel", "ghost small", cancelPending),
  ]);

  const email = pending.email || pending.orgName || "unknown login";
  const target = pending.targetLabel ? ` the switch to ${pending.targetLabel}` : " the switch";
  const body = h("div", { class: "banner-body" }, [
    h("div", {
      class: "banner-text",
      text: `The current login (${email}) is not saved. Save it as a new account, discard it, or cancel${target}.`,
    }),
    h("div", { class: "banner-label" }, [
      h("span", { class: "field-label", text: "Label" }),
      input,
    ]),
    actions,
  ]);

  return h(
    "div",
    { class: "banner info", role: "status" },
    [h("span", { class: "banner-icon", "aria-hidden": "true", text: "ⓘ" }), body]
  );
}

function unknownLabel(live) {
  const key = `${live.email || ""}|${live.orgName || ""}`;
  if (state.unknownKey !== key) {
    state.unknownKey = key;
    state.unknownLabel = String(live.orgName || live.email || "New account").slice(0, 40);
  }
  return state.unknownLabel;
}

function renderUnknownLoginBanner(live) {
  const label = unknownLabel(live);
  const input = textInput({
    value: label,
    maxlength: "40",
    "aria-label": "Label for the new account",
    class: "banner-label-input",
    dataset: { field: "unknown-label" },
  });
  input.addEventListener("input", () => {
    state.unknownLabel = input.value;
  });

  const body = h("div", { class: "banner-body" }, [
    h("div", {
      class: "banner-text",
      text: `New login found: ${live.email || live.orgName || "unknown account"}`,
    }),
    h("div", { class: "banner-label" }, [
      h("span", { class: "field-label", text: "Label" }),
      input,
    ]),
    h("div", { class: "banner-actions" }, [
      button("Save as account", "primary small", () => saveCurrent(state.unknownLabel)),
    ]),
  ]);

  return h(
    "div",
    { class: "banner info", role: "status" },
    [h("span", { class: "banner-icon", "aria-hidden": "true", text: "＋" }), body]
  );
}

// The suggestion auto-burn would carry out while it is off (spec §2.5).
function renderBurnSuggestion(suggestion) {
  const to = typeof suggestion.to === "string" ? suggestion.to : "";
  const text = typeof suggestion.text === "string" ? suggestion.text : "";
  if (!to || !text) {
    return banner("error", "⚠", "The switch suggestion is incomplete.");
  }
  return banner("info", "⚠", text, [
    h("div", { class: "banner-actions" }, [
      button("Switch", "primary small", () => switchTo(to)),
    ]),
  ]);
}

// A manual switch while sessions hold thinking the new account cannot keep
// (spec §2.12): the person decides, so this asks once.
function renderSwitchConfirmation() {
  const key = state.confirmSwitchId;
  if (!key) return null;
  const sessions = state.snapshot ? state.snapshot.sessions : null;
  if (!sessions || !(Number(sessions.tied) > 0)) {
    state.confirmSwitchId = null;
    return null;
  }
  if (!findView(key)) {
    state.confirmSwitchId = null;
    return null;
  }
  const text = `${sessions.running} sessions · ${sessions.busy} busy · ${sessions.tied} on a model that will work without its earlier thinking until you switch back.`;
  return banner("warn", "⚠", text, [
    h("div", { class: "banner-actions" }, [
      button("Switch", "primary small", confirmSwitch),
      button("Cancel", "ghost small", () => {
        state.confirmSwitchId = null;
        renderMain();
      }),
    ]),
  ]);
}

function loginDaysLeftOf(view) {
  if (view && typeof view.loginDaysLeft === "number") return view.loginDaysLeft;
  if (view && typeof view.loginEndsAtMs === "number") {
    return Math.floor((view.loginEndsAtMs - Date.now()) / 86400000);
  }
  return null;
}

// "next · about 12:00" or, when a switch waits for a quiet moment,
// "waiting · latest 12:40" (spec §3.7.1). Only while auto-burn is on.
function renderCardTag(view) {
  const burn = state.snapshot && isObject(state.snapshot.burn) ? state.snapshot.burn : null;
  if (!burn || burn.on !== true || viewIsActive(view) || view.loginState === "loggedOut") {
    return null;
  }
  if (isObject(burn.waiting) && burn.waiting.to === view.id) {
    if (typeof burn.waiting.latest === "string") {
      return h("span", { class: "card-tag waiting" }, [
        "waiting · latest ",
        timeNode(burn.waiting.latest, "clock"),
      ]);
    }
    return h("span", { class: "card-tag error", text: "waiting · latest time missing" });
  }
  const planned = isObject(burn.planned) ? burn.planned[view.id] : null;
  if (typeof planned === "string") {
    return h("span", { class: "card-tag next" }, [
      "next · about ",
      timeNode(planned, "clock"),
    ]);
  }
  return null;
}

function renderCard(view, index) {
  // An invalid colour is the tray's grey here too, never a palette stand-in.
  const accent = isColor(view.color) ? view.color : TRAY_GREY;
  const card = h("div", {
    class:
      "card" +
      (viewIsActive(view) ? " active" : "") +
      (view.id === state.cursorId ? " focused" : ""),
    role: "listitem",
    tabindex: "-1",
    dataset: { index: String(index), accountId: view.id },
    style: { "--accent": accent, "--accent-border": rgba(accent, 0.55) },
  });

  // The gauge is a second, larger target for the card's Switch button.
  const switchable = view.loginState !== "loggedOut" && !viewIsActive(view);
  const switchHere = (event) => {
    event.stopPropagation();
    switchTo(view.id);
  };
  const gauge = renderGauge(view);
  if (switchable) {
    gauge.classList.add("switchable");
    const tip = document.createElementNS("http://www.w3.org/2000/svg", "title");
    tip.textContent = `Switch to ${view.label || "this account"}`;
    gauge.prepend(tip);
    gauge.addEventListener("click", switchHere);
  }
  card.appendChild(gauge);

  const main = h("div", { class: "card-main" });

  const badge = h(
    "button",
    {
      type: "button",
      class: "badge",
      title: `Edit ${view.label || "account"}`,
      "aria-label": `Edit account ${view.abbr || ""}`,
    },
    [view.abbr || "?"]
  );
  badge.style.background = accent;
  badge.style.color = contrastText(accent);
  const edit = (event) => {
    event.stopPropagation();
    openEditor(view.id);
  };
  badge.addEventListener("click", edit);
  // The badge opens the editor too; the pencil is the visible hint for it.
  const pencil = h(
    "button",
    {
      type: "button",
      class: "icon-btn edit-btn",
      title: `Edit ${view.label || "account"}`,
      "aria-label": `Edit account ${view.abbr || ""}`,
    },
    [h("span", { "aria-hidden": "true", text: "\u270E" })]
  );
  pencil.addEventListener("click", edit);

  const status = h("div", { class: "card-status" });
  if (view.loginState === "loggedOut") {
    status.appendChild(h("span", { class: "card-note", text: "Logged out" }));
  } else if (viewIsActive(view)) {
    status.appendChild(h("span", { class: "pill", text: "Active" }));
  } else {
    status.appendChild(button("Switch", "small", switchHere));
  }

  main.appendChild(
    h("div", { class: "card-top" }, [
      badge,
      h("span", {
        class: "card-label",
        text: view.label || "Unnamed account",
        title: view.label || "",
      }),
      pencil,
      status,
    ])
  );

  const metaParts = [];
  if (view.email) metaParts.push(view.email);
  if (view.plan) metaParts.push(view.plan);
  const metaText = metaParts.length ? metaParts.join(" · ") : "no login details";
  const age = usageAge(view);
  const metaRow = h("div", { class: "card-meta-row" }, [
    h("div", { class: "card-meta", title: metaText }, age ? [metaText, " · ", age] : metaText),
  ]);
  const tag = renderCardTag(view);
  if (tag) metaRow.appendChild(tag);
  main.appendChild(metaRow);

  if (view.loginState === "loggedOut") {
    main.appendChild(renderLoggedOut());
  } else {
    const rows = [];
    const week = findLimit(view, "weekly_all");
    if (week) rows.push(renderUsageRow("Week", week, true));
    const session = findLimit(view, "session");
    if (session) rows.push(renderUsageRow("Session", session, true));
    for (const scoped of scopedLimits(view)) {
      rows.push(renderUsageRow(scoped.model || "Scoped", scoped, false));
    }
    for (const other of otherLimits(view)) {
      rows.push(renderUsageRow(other.name, other.limit, Boolean(other.limit.resetsAt)));
    }
    if (rows.length > 0) {
      main.appendChild(h("div", { class: "usage-rows" }, rows));
    } else if (!view.usage) {
      main.appendChild(h("div", { class: "card-note", text: "Usage not fetched yet." }));
    } else {
      main.appendChild(h("div", { class: "card-note", text: "No usage limits in the reply." }));
    }
  }

  if (view.usage && view.usage.extraUsageEnabled) {
    main.appendChild(h("div", { class: "card-note", text: "Extra usage is enabled." }));
  }

  const daysLeft = loginDaysLeftOf(view);
  if (typeof daysLeft === "number" && view.loginState !== "loggedOut" && daysLeft <= 5) {
    let text;
    if (daysLeft < 0) text = "login ended — log in again";
    else if (daysLeft === 0) text = "login ends today";
    else text = `login ends in ${daysLeft} d`;
    main.appendChild(h("div", { class: "login-ends", text }));
  }

  if (view.lastError) {
    main.appendChild(
      h("div", { class: "card-note", text: String(view.lastError), title: String(view.lastError) })
    );
  }

  const actionError = state.errors.accounts[view.id];
  if (actionError) {
    main.appendChild(h("div", { class: "card-note error", text: actionError }));
  }

  card.appendChild(main);

  if (state.editingId === view.id && state.editor) {
    card.appendChild(renderEditor(view));
  }

  return card;
}

function renderLoggedOut() {
  return h("div", { class: "logged-out" }, [
    h("div", { class: "logged-out-title", text: "Logged out — log in again" }),
    h("ol", {}, [
      h("li", {}, [
        "In a terminal run ",
        h("code", { text: "claude" }),
        ".",
      ]),
      h("li", {}, [
        "Run ",
        h("code", { text: "/login" }),
        " and sign in with this account.",
      ]),
      h("li", { text: "seatswap sees the new login — save it here." }),
    ]),
  ]);
}

function renderUsageRow(name, limit, showReset) {
  const level = limitLevel(limit);
  const color = LEVEL_COLORS[level];
  const percent = Number(limit.percent);
  const width = Number.isFinite(percent) ? clamp(percent, 0, 100) : 0;

  const fill = h("span", { class: "usage-fill" });
  fill.style.width = `${width}%`;
  fill.style.backgroundColor = color;

  const percentNode = h("span", { class: `usage-percent lv-${level}` }, [
    h("span", { text: fmtPercent(percent) }),
  ]);
  if (level === "locked") {
    percentNode.appendChild(
      h("span", { class: "lock", text: "🔒", title: "Limit reached" })
    );
  }

  const reset =
    showReset && limit.resetsAt
      ? timeNode(limit.resetsAt, "countdown", "reset-time")
      : h("span", { class: "reset-time" });

  return h("div", { class: "usage-row" }, [
    h("span", { class: "usage-name", text: name, title: name }),
    h("span", { class: "usage-track" }, [fill]),
    percentNode,
    reset,
  ]);
}

// ---------------------------------------------------------------------------
// ring gauges
// ---------------------------------------------------------------------------

let gaugeSequence = 0;

function renderGauge(view) {
  const template = document.getElementById("gauge-template");
  const gauge = template.content.firstElementChild.cloneNode(true);
  const session = findLimit(view, "session");
  const week = findLimit(view, "weekly_all");

  // Outer ring: the week (the limit that matters most). Inner ring: the 5 h
  // session, left out entirely on plans that have no session limit.
  setupArc(
    gauge,
    gauge.querySelector(".ring-arc.ring-outer"),
    gauge.querySelector(".grad-week"),
    week,
    view,
    "week"
  );
  setupArc(
    gauge,
    gauge.querySelector(".ring-arc.ring-inner"),
    gauge.querySelector(".grad-session"),
    session,
    view,
    "session"
  );

  const weekText = gauge.querySelector(".gauge-week");
  setGaugeText(weekText, week);
  setGaugeText(gauge.querySelector(".gauge-session"), session);
  if (!session) {
    gauge.querySelector(".ring-track.ring-inner").remove();
    // Alone in the middle: centre the week's figure vertically.
    weekText.setAttribute("y", "36");
  }
  return gauge;
}

function setGaugeText(node, limit) {
  if (!node) return;
  if (!limit) {
    node.textContent = "";
    return;
  }
  node.textContent = shortPercent(limit.percent);
  node.classList.add(`lv-${limitLevel(limit)}`);
}

function setupArc(gauge, arc, gradient, limit, view, slot) {
  if (!arc) return;
  if (!limit) {
    arc.style.display = "none";
    return;
  }

  const radius = Number(arc.getAttribute("r")) || 1;
  const circumference = 2 * Math.PI * radius;
  const level = limitLevel(limit);
  const base = LEVEL_COLORS[level];

  if (gradient) {
    const id = `gauge-gradient-${++gaugeSequence}`;
    gradient.id = id;
    const start = gradient.querySelector(".grad-start");
    const end = gradient.querySelector(".grad-end");
    if (start) start.setAttribute("stop-color", base);
    if (end) {
      end.setAttribute(
        "stop-color",
        mixHex(base, level === "critical" || level === "locked" ? "#fecaca" : "#ffffff", 0.3)
      );
    }
    arc.setAttribute("stroke", `url(#${id})`);
  } else {
    arc.setAttribute("stroke", base);
  }

  if (level === "critical" || level === "locked") arc.classList.add("glow");

  const target = clamp(Number(limit.percent) || 0, 0, 100);
  const key = `${view.id}:${slot}`;
  const startPercent = state.lastPercents.has(key) ? state.lastPercents.get(key) : 0;

  arc.style.strokeDasharray = String(circumference);
  arc.style.strokeDashoffset = String(circumference * (1 - clamp(startPercent, 0, 100) / 100));
  arc.style.transition = "none";
  arc.style.opacity = target <= 0 ? "0" : "1";

  if (prefersReducedMotion()) {
    arc.style.strokeDashoffset = String(circumference * (1 - target / 100));
  } else {
    void arc.getBoundingClientRect();
    requestAnimationFrame(() => {
      arc.style.transition = "stroke-dashoffset 400ms cubic-bezier(0.22, 1, 0.36, 1)";
      arc.style.strokeDashoffset = String(circumference * (1 - target / 100));
    });
  }

  state.lastPercents.set(key, target);
}

// ---------------------------------------------------------------------------
// inline account editor
// ---------------------------------------------------------------------------

function openEditor(id) {
  const account = findView(id);
  if (!account) return;
  const characters = String(account.abbr || "").split("");
  const validColor = isColor(account.color);
  state.editingId = id;
  state.editor = {
    label: account.label || "",
    abbr1: characters[0] || "",
    abbr2: characters[1] || "",
    // An invalid saved colour leaves the field empty; it is never replaced by
    // a palette colour that would silently be saved over the real value.
    color: validColor ? account.color.toLowerCase() : "",
    // Missing is not "off": Save refuses until the view carries the real value.
    autoBurn:
      typeof account.autoBurn === "boolean" ? account.autoBurn : null,
    confirmRemove: false,
  };
  state.errors.editor = null;
  state.errors.editorColor = validColor
    ? null
    : `The saved colour ${JSON.stringify(account.color)} is not #rrggbb. Choose a colour.`;
  state.errors.preview = null;
  if (typeof state.editor.autoBurn !== "boolean") {
    state.errors.editor = "Auto-burn may use this account: the account's value is missing.";
  }
  renderMain();
}

function closeEditor() {
  state.editingId = null;
  state.editor = null;
  state.errors.editor = null;
  state.errors.editorColor = null;
  state.errors.preview = null;
  renderMain();
}

function sanitizeAbbrChar(value) {
  const cleaned = String(value || "").toUpperCase().replace(/[^A-Z0-9]/g, "");
  return cleaned.slice(0, 1);
}

function renderEditor(account) {
  const editor = state.editor;
  const wrapper = h("div", { class: "editor" });

  const labelInput = textInput({
    value: editor.label,
    maxlength: "40",
    "aria-label": "Label",
    dataset: { field: "label" },
  });
  labelInput.addEventListener("input", () => {
    editor.label = labelInput.value;
  });

  const firstBox = textInput({
    value: editor.abbr1,
    maxlength: "1",
    class: "abbr-box",
    "aria-label": "First abbreviation character",
    autocapitalize: "characters",
    autocomplete: "off",
    dataset: { field: "abbr1" },
  });
  const secondBox = textInput({
    value: editor.abbr2,
    maxlength: "1",
    class: "abbr-box",
    "aria-label": "Second abbreviation character",
    autocapitalize: "characters",
    autocomplete: "off",
    dataset: { field: "abbr2" },
  });

  firstBox.addEventListener("input", () => {
    const value = sanitizeAbbrChar(firstBox.value);
    firstBox.value = value;
    editor.abbr1 = value;
    if (value && !editor.abbr2 && secondBox) {
      secondBox.focus();
      secondBox.select();
    }
    schedulePreview();
  });
  firstBox.addEventListener("keydown", (event) => {
    if (event.key === "ArrowRight" || event.key === "Tab") {
      if (event.key === "Tab" && event.shiftKey) return;
      if (secondBox) {
        event.preventDefault();
        secondBox.focus();
        secondBox.select();
      }
    }
  });

  secondBox.addEventListener("input", () => {
    const value = sanitizeAbbrChar(secondBox.value);
    secondBox.value = value;
    editor.abbr2 = value;
    if (!value) {
      // keep the first box as typed; focus stays for correction
    }
    schedulePreview();
  });
  secondBox.addEventListener("keydown", (event) => {
    if (event.key === "Backspace" && !secondBox.value) {
      event.preventDefault();
      firstBox.focus();
      firstBox.select();
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      firstBox.focus();
      firstBox.select();
    }
  });

  const pasteHandler = (event) => {
    const pasted = (event.clipboardData && event.clipboardData.getData("text")) || "";
    const cleaned = String(pasted).toUpperCase().replace(/[^A-Z0-9]/g, "").slice(0, 2);
    if (!cleaned) return;
    event.preventDefault();
    editor.abbr1 = cleaned[0] || "";
    editor.abbr2 = cleaned[1] || "";
    firstBox.value = editor.abbr1;
    secondBox.value = editor.abbr2;
    (editor.abbr2 ? secondBox : firstBox).focus();
    schedulePreview();
  };
  firstBox.addEventListener("paste", pasteHandler);
  secondBox.addEventListener("paste", pasteHandler);

  const abbrBoxes = h("div", { class: "abbr-boxes" }, [firstBox, secondBox]);
  const abbrField = h("label", { class: "field" }, [
    h("span", { class: "field-label", text: "Abbreviation" }),
    abbrBoxes,
  ]);

  const hexInput = textInput({
    value: editor.color,
    maxlength: "7",
    class: "hex-input",
    "aria-label": "Colour hex value",
    spellcheck: "false",
    dataset: { field: "hex" },
  });

  const swatches = h("div", { class: "swatches" });
  const swatchButtons = [];
  for (const color of PALETTE) {
    const swatch = h("button", {
      type: "button",
      class: "swatch" + (sameColor(color, editor.color) ? " selected" : ""),
      title: color,
      "aria-label": `Colour ${color}`,
    });
    swatch.style.backgroundColor = color;
    swatch.addEventListener("click", () => {
      editor.color = color;
      hexInput.value = color;
      editorColorError(null);
      updateSwatchSelection(swatchButtons, editor.color);
      schedulePreview();
    });
    swatchButtons.push({ button: swatch, color });
    swatches.appendChild(swatch);
  }

  hexInput.addEventListener("input", () => {
    let value = hexInput.value.trim().toLowerCase();
    if (value && !value.startsWith("#")) value = `#${value}`;
    editor.color = value;
    editorColorError(null);
    updateSwatchSelection(swatchButtons, editor.color);
    schedulePreview();
  });
  hexInput.addEventListener("blur", () => {
    if (isColor(editor.color)) {
      editor.color = editor.color.toLowerCase();
      hexInput.value = editor.color;
      schedulePreview();
    }
  });

  const colorField = h("div", { class: "field grow" }, [
    h("span", { class: "field-label", text: "Colour" }),
    h("div", { class: "editor-row" }, [swatches, hexInput]),
    h("div", {
      class: "inline-error",
      id: "editor-color-error",
      hidden: !state.errors.editorColor,
      text: state.errors.editorColor || "",
    }),
  ]);

  const previews = h("div", { class: "previews" });
  for (const size of [16, 22, 32]) {
    const canvas = h("canvas", {
      width: String(size),
      height: String(size),
      dataset: { size: String(size) },
      "aria-label": `Tray icon preview at ${size} pixels`,
    });
    previews.appendChild(
      h("div", { class: "preview-cell" }, [
        canvas,
        h("span", { class: "preview-size", text: `${size} px` }),
      ])
    );
  }

  const previewError = h("div", {
    class: "inline-error",
    id: "preview-error",
    hidden: !state.errors.preview,
    text: state.errors.preview || "",
  });

  const actions = h("div", { class: "editor-actions" }, [
    button("Save", "primary", saveEditor),
    button("Cancel", "ghost", closeEditor),
    h("span", { class: "editor-spacer" }),
    button("Remove account", "danger", () => {
      editor.confirmRemove = true;
      state.errors.editor = null;
      renderMain();
    }),
  ]);

  const errorNode = h("div", {
    class: "inline-error",
    id: "editor-error",
    hidden: !state.errors.editor,
    text: state.errors.editor || "",
  });

  wrapper.appendChild(
    h("div", { class: "editor-row" }, [
      h("label", { class: "field grow" }, [
        h("span", { class: "field-label", text: "Label" }),
        labelInput,
      ]),
      abbrField,
    ])
  );
  wrapper.appendChild(colorField);
  wrapper.appendChild(previews);
  wrapper.appendChild(previewError);

  wrapper.appendChild(
    h("div", { class: "setting editor-setting" }, [
      h("div", { class: "setting-label" }, [
        h("span", { text: "Auto-burn may use this account" }),
        h("span", {
          class: "sub",
          text:
            "off: auto-burn never switches to it; it still leaves it when it is full",
        }),
      ]),
      h("div", { class: "setting-control" }, [
        switchButton(
          editor.autoBurn === true,
          "Auto-burn may use this account",
          (value) => {
            editor.autoBurn = value;
            if (state.errors.editor) editorError(null);
          }
        ),
      ]),
    ])
  );

  if (editor.confirmRemove) {
    const name = `${account.abbr || "?"} — ${account.label || "this account"}`;
    wrapper.appendChild(
      h("div", { class: "confirm" }, [
        h("div", {
          class: "confirm-text",
          text: `Remove ${name}? The live login is not touched.`,
        }),
        h("div", { class: "confirm-actions" }, [
          button("Remove", "danger", () => removeAccount(account.id)),
          button("Keep it", "ghost", () => {
            editor.confirmRemove = false;
            renderMain();
          }),
        ]),
      ])
    );
  }

  wrapper.appendChild(errorNode);
  wrapper.appendChild(actions);

  schedulePreview();
  return wrapper;
}

function sameColor(first, second) {
  return String(first || "").toLowerCase() === String(second || "").toLowerCase();
}

function updateSwatchSelection(swatchButtons, color) {
  for (const entry of swatchButtons) {
    entry.button.classList.toggle("selected", sameColor(entry.color, color));
  }
}

let previewTimer = null;
let previewToken = 0;

function schedulePreview() {
  if (previewTimer !== null) clearTimeout(previewTimer);
  previewTimer = setTimeout(() => {
    previewTimer = null;
    drawPreviews();
  }, 110);
}

function clearPreviews() {
  for (const canvas of document.querySelectorAll(".previews canvas")) {
    const context = canvas.getContext("2d");
    if (context) context.clearRect(0, 0, canvas.width, canvas.height);
  }
}

function bytesOf(value) {
  if (Array.isArray(value)) return value;
  if (value instanceof Uint8Array || value instanceof Uint8ClampedArray) return value;
  if (typeof value === "string") {
    try {
      const binary = atob(value);
      const bytes = new Uint8Array(binary.length);
      for (let index = 0; index < binary.length; index += 1) {
        bytes[index] = binary.charCodeAt(index);
      }
      return bytes;
    } catch {
      return [];
    }
  }
  if (value && Array.isArray(value.data)) return value.data;
  return [];
}

function applyPreviewReply(reply) {
  if (!Array.isArray(reply)) {
    throw new Error("icon preview returned an unexpected reply");
  }
  for (const item of reply) {
    if (!isObject(item)) continue;
    const size = Number(item.size);
    if (!Number.isFinite(size) || size <= 0) continue;
    const canvas = document.querySelector(`.previews canvas[data-size="${String(size)}"]`);
    if (!canvas) continue;
    canvas.width = size;
    canvas.height = size;
    const context = canvas.getContext("2d");
    if (!context) continue;
    const source = bytesOf(item.rgba);
    const expected = size * size * 4;
    if (source.length < expected) {
      throw new Error(`icon preview for ${size} px is too short`);
    }
    const image = context.createImageData(size, size);
    for (let index = 0; index < expected; index += 1) {
      image.data[index] = source[index] & 0xff;
    }
    context.putImageData(image, 0, 0);
  }
}

async function drawPreviews() {
  const editor = state.editor;
  if (!editor) return;
  const abbr = `${editor.abbr1 || ""}${editor.abbr2 || ""}`.trim().toUpperCase();
  const color = String(editor.color || "").toLowerCase();
  if (!/^[A-Z0-9]{1,2}$/.test(abbr) || !isColor(color)) {
    clearPreviews();
    return;
  }
  const token = ++previewToken;
  try {
    const reply = await invoke("icon_preview", { abbr, color });
    if (token !== previewToken || !state.editor) return;
    applyPreviewReply(reply);
    if (state.errors.preview) {
      state.errors.preview = null;
      const node = document.getElementById("preview-error");
      if (node) {
        node.textContent = "";
        node.hidden = true;
      }
      fitPanel();
    }
  } catch (error) {
    if (token !== previewToken || !state.editor) return;
    state.errors.preview = errorMessage(error);
    const node = document.getElementById("preview-error");
    if (node) {
      node.textContent = state.errors.preview;
      node.hidden = false;
    }
    fitPanel();
  }
}

function editorAbbr() {
  if (!state.editor) return "";
  return `${state.editor.abbr1 || ""}${state.editor.abbr2 || ""}`.trim().toUpperCase();
}

function editorError(text) {
  state.errors.editor = text;
  const node = document.getElementById("editor-error");
  if (node) {
    node.textContent = text || "";
    node.hidden = !text;
  }
  fitPanel();
}

function editorColorError(text) {
  state.errors.editorColor = text;
  const node = document.getElementById("editor-color-error");
  if (node) {
    node.textContent = text || "";
    node.hidden = !text;
  }
  fitPanel();
}

async function saveEditor() {
  const editor = state.editor;
  if (!editor || !state.editingId) return;
  const account = findView(state.editingId);
  if (!account) {
    closeEditor();
    return;
  }
  const label = String(editor.label || "").trim();
  const abbr = editorAbbr();
  const color = String(editor.color || "").trim().toLowerCase();

  if (!label || label.length > 40) {
    editorError("Enter a label of 1 to 40 characters.");
    return;
  }
  if (!/^[A-Z0-9]{1,2}$/.test(abbr)) {
    editorError("Use 1 or 2 letters A–Z or digits for the abbreviation.");
    return;
  }
  if (!isColor(color)) {
    editorColorError("Use a colour of the form #rrggbb.");
    return;
  }
  editorColorError(null);
  if (typeof editor.autoBurn !== "boolean") {
    editorError("Auto-burn may use this account: the account's value is missing; re-open the panel.");
    return;
  }

  try {
    await invoke("edit_account", {
      key: account.id,
      label,
      abbr,
      color,
      autoBurn: editor.autoBurn,
    });
    state.errors.editor = null;
    closeEditor();
    afterCommand();
  } catch (error) {
    editorError(errorMessage(error));
  }
}

async function removeAccount(id) {
  try {
    await invoke("remove_account", { key: id });
    state.errors.editor = null;
    closeEditor();
    afterCommand();
  } catch (error) {
    editorError(errorMessage(error));
    renderMain();
  }
}

// ---------------------------------------------------------------------------
// add view (spec §6.1)
// ---------------------------------------------------------------------------

function renderAdd() {
  const root = document.getElementById("view-add");
  if (!root) return;
  clear(root);

  root.appendChild(
    h("div", { class: "view-head" }, [
      button("‹ Back", "ghost", () => setView("main")),
      h("span", { class: "view-heading", text: "Add an account" }),
    ])
  );

  const live = state.snapshot && isObject(state.snapshot.live) ? state.snapshot.live : { kind: "noLogin" };
  const steps = h("ol", { class: "steps" });
  steps.appendChild(renderAddStepOne(live));
  steps.appendChild(
    h("li", {}, [
      h("div", { class: "step-title", text: "Log in with the other account" }),
      h("div", { class: "step-body" }, [
        "In a terminal run ",
        h("code", { text: "claude" }),
        ", then ",
        h("code", { text: "/login" }),
        " with the other account. A ",
        h("code", { text: "/logout" }),
        " first is usually not needed.",
      ]),
    ])
  );
  steps.appendChild(
    h("li", {}, [
      h("div", { class: "step-title", text: "Save the new login" }),
      h("div", {
        class: "step-body",
        text: "seatswap notices the new login and offers “Save as new account” here.",
      }),
    ])
  );
  root.appendChild(steps);

  if (state.errors.add) {
    root.appendChild(h("div", { class: "inline-error", text: state.errors.add }));
  }

  root.appendChild(
    h("div", { class: "settings-actions" }, [
      button("Done", "ghost", () => setView("main")),
    ])
  );

  fitPanel();
}

function renderAddStepOne(live) {
  const item = h("li", {});
  item.appendChild(h("div", { class: "step-title", text: "Save the current login" }));

  if (live.kind === "known") {
    const account = findView(live.accountId);
    const name = account ? `${account.abbr} — ${account.label}` : "the current account";
    item.appendChild(
      h("div", { class: "step-body", text: `Your current account (${name}) is saved.` })
    );
    return item;
  }

  if (live.kind === "unknown") {
    const email = live.email || live.orgName || "unknown login";
    item.appendChild(
      h("div", { class: "step-body", text: `The live login (${email}) is not saved yet.` })
    );
    const label = unknownLabel(live);
    const input = textInput({
      value: label,
      maxlength: "40",
      "aria-label": "Label for the new account",
    });
    input.addEventListener("input", () => {
      state.unknownLabel = input.value;
    });
    item.appendChild(
      h("div", { class: "inline-form" }, [
        h("label", { class: "field" }, [
          h("span", { class: "field-label", text: "Label" }),
          input,
        ]),
        h("div", { class: "banner-actions" }, [
          button("Save", "primary small", () => saveCurrent(state.unknownLabel)),
        ]),
      ])
    );
    return item;
  }

  if (live.kind === "ended") {
    item.appendChild(
      h("div", {
        class: "step-body",
        text: "The live login has ended. Log in again in Claude Code first.",
      })
    );
    return item;
  }

  item.appendChild(
    h("div", {
      class: "step-body",
      text: "No live login found. Run claude once and sign in first.",
    })
  );
  return item;
}

async function saveCurrent(labelValue) {
  const label = String(labelValue || "").trim();
  if (!label || label.length > 40) {
    state.errors.add = "Enter a label of 1 to 40 characters.";
    if (state.view === "add") {
      renderAdd();
    } else {
      state.errors.banner = state.errors.add;
      renderMain();
    }
    return;
  }
  try {
    await invoke("save_current", { label });
    state.errors.add = null;
    afterCommand();
  } catch (error) {
    state.errors.add = errorMessage(error);
    if (state.view === "add") renderAdd();
    else {
      state.errors.banner = state.errors.add;
      renderMain();
    }
  }
}

// ---------------------------------------------------------------------------
// settings view (spec §10)
// ---------------------------------------------------------------------------

function renderSettings() {
  const root = document.getElementById("view-settings");
  if (!root) return;
  clear(root);

  root.appendChild(
    h("div", { class: "view-head" }, [
      button("‹ Back", "ghost", () => setView("main")),
      h("span", { class: "view-heading", text: "Settings" }),
      state.version
        ? h("span", { class: "view-version", text: state.version.text, title: state.version.title })
        : null,
    ])
  );

  if (!state.snapshot) {
    root.appendChild(h("div", { class: "setting-note", text: "Loading settings…" }));
    fitPanel();
    return;
  }

  if (!isObject(state.snapshot.settings)) {
    // accounts.json could not be read: never show defaults that Save would
    // write over the real settings.
    const error = h("div", {
      class: "inline-error",
      id: "settings-error",
      text:
        state.snapshot.error ||
        "The settings could not be read from accounts.json; Save is disabled.",
    });
    root.appendChild(error);
    const disabled = button("Save", "primary", saveSettings);
    disabled.disabled = true;
    root.appendChild(h("div", { class: "settings-actions" }, [disabled]));
    fitPanel();
    return;
  }

  if (!state.settingsDraft) {
    state.settingsDraft = Object.assign({}, state.snapshot.settings);
  }
  const draft = state.settingsDraft;
  const settings = h("div", { class: "settings" });

  settings.appendChild(
    settingRow(
      "Tray text",
      selectInput(
        [
          ["percent", "Percent"],
          ["percentReset", "Percent + reset"],
          ["off", "Off"],
        ],
        draft.trayText,
        (value) => {
          draft.trayText = value;
        }
      ),
      "What the tray label shows."
    )
  );

  settings.appendChild(
    settingRow(
      "Tray ring",
      selectInput(
        [
          ["binding", "Binding limit"],
          ["session", "Session"],
          ["week", "Week"],
        ],
        draft.iconLimit,
        (value) => {
          draft.iconLimit = value;
        }
      ),
      "Which limit the tray icon ring shows."
    )
  );

  settings.appendChild(
    settingRow(
      "Poll active account",
      numberControl(draft, "pollActiveSecs", { min: "30", step: "30" }, "s"),
      "Seconds between usage polls of the live account."
    )
  );

  settings.appendChild(
    settingRow(
      "Poll other accounts",
      numberControl(draft, "pollInactiveSecs", { min: "30", step: "30" }, "s"),
      "Seconds between usage polls of accounts that are not live."
    )
  );

  settings.appendChild(settingRow("Notify on switch", checkboxControl(draft, "notifyOnSwitch")));
  settings.appendChild(
    settingRow(
      "Mirror live login",
      checkboxControl(draft, "mirrorLive"),
      "Keep the stored copy in step when Claude Code's login changes."
    )
  );
  settings.appendChild(
    settingRow(
      "Refresh inactive logins",
      checkboxControl(draft, "refreshInactive"),
      "Refresh the saved logins of accounts that are not live. Off: their last known usage is shown with its age."
    )
  );
  settings.appendChild(settingRow("Start at login", checkboxControl(draft, "startAtLogin")));
  settings.appendChild(
    settingRow(
      "Theme",
      selectInput(
        [
          ["system", "System"],
          ["light", "Light"],
          ["dark", "Dark"],
        ],
        draft.theme,
        (value) => {
          draft.theme = value;
          applyTheme(value);
        }
      )
    )
  );

  const hotkeyInput = textInput({
    value: draft.hotkey || "",
    readonly: true,
    "aria-label": "Hotkey (read only)",
  });
  settings.appendChild(settingRow("Hotkey", hotkeyInput));
  settings.appendChild(
    h("div", { class: "hotkey-note" }, [
      "Bind ",
      h("code", { text: "seatswap panel" }),
      " to a key in your desktop's keyboard settings.",
    ])
  );

  const configInput = textInput({
    value: draft.claudeConfigDir || "",
    placeholder: "/home/you/.claude",
    "aria-label": "Claude config directory",
  });
  configInput.addEventListener("input", () => {
    draft.claudeConfigDir = configInput.value;
    settingsFieldError("claudeConfigDir", null);
  });
  const configField = h("div", { class: "field" }, [
    configInput,
    h("div", {
      class: "inline-error",
      id: "claude-config-error",
      hidden: !state.errors.settingsConfigDir,
      text: state.errors.settingsConfigDir || "",
    }),
  ]);
  settings.appendChild(
    settingRow(
      "Claude config folder",
      configField,
      "Empty: $CLAUDE_CONFIG_DIR or Claude Code's default folder."
    )
  );
  settings.appendChild(
    h("div", {
      class: "setting-note",
      text: "Takes effect after restarting seatswap.",
    })
  );

  settings.appendChild(settingGroup("Auto-burn"));
  settings.appendChild(
    settingRow("Auto-burn", switchControl(draft, "autoBurn", "Auto-burn"))
  );
  settings.appendChild(
    settingRow(
      "Suggest switches",
      switchControl(draft, "suggestSwitches", "Suggest switches"),
      "while auto-burn is off"
    )
  );

  settings.appendChild(settingGroup("History"));

  const hostInput = textInput({
    value: draft.hostName || "",
    placeholder: "the machine's name",
    "aria-label": "Name of this host",
  });
  hostInput.addEventListener("input", () => {
    draft.hostName = hostInput.value;
    settingsFieldError("hostName", null);
  });
  settings.appendChild(
    settingRow(
      "Name of this host",
      h("div", { class: "field" }, [
        hostInput,
        h("div", {
          class: "inline-error",
          id: "host-name-error",
          hidden: !state.errors.settingsHostName,
          text: state.errors.settingsHostName || "",
        }),
      ]),
      "Empty: the machine's name."
    )
  );

  const historyInput = textInput({
    value: draft.historyDir || "",
    placeholder: "seatswap's own folder",
    "aria-label": "History folder",
  });
  historyInput.addEventListener("input", () => {
    draft.historyDir = historyInput.value;
    settingsFieldError("historyDir", null);
  });
  settings.appendChild(
    settingRow(
      "History folder",
      h("div", { class: "field" }, [
        historyInput,
        h("div", {
          class: "inline-error",
          id: "history-dir-error",
          hidden: !state.errors.settingsHistoryDir,
          text: state.errors.settingsHistoryDir || "",
        }),
      ]),
      "Empty: seatswap's own folder."
    )
  );

  settings.appendChild(
    settingRow(
      "Read Claude Code's session files",
      checkboxControl(draft, "readSessions"),
      "model, busy or idle, tokens per folder · never message text"
    )
  );
  settings.appendChild(
    h("div", {
      class: "setting-note",
      text: "Takes effect after restarting seatswap.",
    })
  );

  settings.appendChild(settingGroup("Notifications"));
  settings.appendChild(
    settingRow("An account is usable again", checkboxControl(draft, "notifyUsableAgain"))
  );
  settings.appendChild(
    settingRow("A session is waiting for you", checkboxControl(draft, "notifyWaiting"))
  );

  settings.appendChild(settingGroup("Agents"));
  settings.appendChild(
    settingRow("Agents may read usage", checkboxControl(draft, "agentsRead"))
  );
  settings.appendChild(
    settingRow("Agents may switch accounts", checkboxControl(draft, "agentsSwitch"))
  );
  settings.appendChild(
    h("div", { class: "setting-note" }, [
      "These two switches work through a marker that Claude Code puts on every command it runs. An agent runs as you. It can remove the marker, or change seatswap's files directly. The switches stop agents that follow the rules. They are not a lock. To block the command itself, add a deny rule for ",
      h("code", { text: "seatswap" }),
      " to Claude Code's permissions.",
    ])
  );

  root.appendChild(settings);

  const errorNode = h("div", {
    class: "inline-error",
    id: "settings-error",
    hidden: !state.errors.settings,
    text: state.errors.settings || "",
  });
  root.appendChild(errorNode);

  const saved = h("span", {
    class: "inline-ok",
    id: "settings-saved",
    hidden: !state.errors.settingsSaved,
    text: "Saved.",
  });

  root.appendChild(
    h("div", { class: "settings-actions" }, [
      button("Save", "primary", saveSettings),
      saved,
    ])
  );

  fitPanel();
}

function settingGroup(title) {
  return h("div", {
    class: "setting-group",
    role: "heading",
    "aria-level": "3",
    text: title,
  });
}

function settingRow(labelText, control, subText) {
  const label = h("div", { class: "setting-label" }, [
    h("span", { text: labelText }),
  ]);
  if (subText) label.appendChild(h("span", { class: "sub", text: subText }));
  if (
    control &&
    typeof control.matches === "function" &&
    control.matches("input, select, textarea") &&
    !control.getAttribute("aria-label")
  ) {
    control.setAttribute("aria-label", labelText);
  }
  return h("div", { class: "setting" }, [
    label,
    h("div", { class: "setting-control" }, [control]),
  ]);
}

function numberControl(draft, key, attrs, unit) {
  const input = h(
    "input",
    Object.assign(
      {
        type: "number",
        value: draft[key] === null || draft[key] === undefined ? "" : String(draft[key]),
        "aria-label": key,
      },
      attrs
    )
  );
  input.addEventListener("input", () => {
    draft[key] = input.value === "" ? "" : Number(input.value);
  });
  return h("span", { class: "number-control" }, [
    input,
    h("span", { class: "unit", text: unit }),
  ]);
}

function checkboxControl(draft, key) {
  const input = h("input", { type: "checkbox", checked: Boolean(draft[key]), "aria-label": key });
  input.addEventListener("change", () => {
    draft[key] = input.checked;
  });
  return input;
}

// The switch used for auto-burn and for the two per-account switches.
function switchButton(checked, label, onchange) {
  const control = h("button", {
    type: "button",
    class: "switch",
    role: "switch",
    "aria-checked": checked ? "true" : "false",
    "aria-label": label,
  });
  control.addEventListener("click", () => {
    const next = control.getAttribute("aria-checked") !== "true";
    control.setAttribute("aria-checked", next ? "true" : "false");
    onchange(next);
  });
  return control;
}

function switchControl(draft, key, label) {
  return switchButton(Boolean(draft[key]), label || key, (value) => {
    draft[key] = value;
  });
}

function settingsError(text) {
  state.errors.settings = text || null;
  const node = document.getElementById("settings-error");
  if (node) {
    node.textContent = text || "";
    node.hidden = !text;
  }
  const saved = document.getElementById("settings-saved");
  if (saved) saved.hidden = Boolean(text) || !state.errors.settingsSaved;
  fitPanel();
}

const SETTINGS_FIELD_ERRORS = {
  claudeConfigDir: ["settingsConfigDir", "claude-config-error"],
  hostName: ["settingsHostName", "host-name-error"],
  historyDir: ["settingsHistoryDir", "history-dir-error"],
};

function settingsFieldError(field, text) {
  const target = SETTINGS_FIELD_ERRORS[field];
  if (!target) return;
  state.errors[target[0]] = text || null;
  const node = document.getElementById(target[1]);
  if (node) {
    node.textContent = text || "";
    node.hidden = !text;
  }
  fitPanel();
}

// Which row an error from the core belongs to. A relative path is not always
// claudeConfigDir's fault: historyDir and hostName have their own rules.
function settingsErrorField(message) {
  if (message.includes("historyDir")) return "historyDir";
  if (
    message.includes("hostName") ||
    /host name .*must be 1 to 63 characters/i.test(message)
  ) {
    return "hostName";
  }
  if (message.includes("claudeConfigDir")) return "claudeConfigDir";
  return null;
}

function routeSettingsError(message) {
  settingsFieldError("hostName", null);
  settingsFieldError("historyDir", null);
  settingsFieldError("claudeConfigDir", null);
  const field = settingsErrorField(message);
  if (field) {
    settingsFieldError(field, message);
    settingsError(null);
  } else {
    settingsError(message);
  }
}

async function saveSettings() {
  if (!state.snapshot || !isObject(state.snapshot.settings)) {
    settingsError("The settings could not be read from accounts.json; re-open the panel.");
    return;
  }
  if (!state.settingsDraft) return;
  const draft = state.settingsDraft;

  if (!Number.isInteger(draft.pollActiveSecs) || draft.pollActiveSecs < 30) {
    settingsError("Poll active account must be a whole number of at least 30 seconds.");
    return;
  }
  if (!Number.isInteger(draft.pollInactiveSecs) || draft.pollInactiveSecs < 30) {
    settingsError("Poll other accounts must be a whole number of at least 30 seconds.");
    return;
  }

  const configDir = String(draft.claudeConfigDir || "").trim();
  const hostName = String(draft.hostName || "").trim();
  const historyDir = String(draft.historyDir || "").trim();
  // Keep every field the pop-up does not edit, so saving here never resets one
  // of them to its default.
  const payload = Object.assign({}, draft);
  Object.assign(payload, {
    trayText: draft.trayText,
    iconLimit: draft.iconLimit,
    pollActiveSecs: draft.pollActiveSecs,
    pollInactiveSecs: draft.pollInactiveSecs,
    autoBurn: Boolean(draft.autoBurn),
    suggestSwitches: Boolean(draft.suggestSwitches),
    notifyOnSwitch: Boolean(draft.notifyOnSwitch),
    mirrorLive: Boolean(draft.mirrorLive),
    refreshInactive: Boolean(draft.refreshInactive),
    hotkey: draft.hotkey,
    startAtLogin: Boolean(draft.startAtLogin),
    theme: draft.theme,
    claudeConfigDir: configDir === "" ? null : configDir,
    hostName: hostName === "" ? null : hostName,
    historyDir: historyDir === "" ? null : historyDir,
    readSessions: Boolean(draft.readSessions),
    notifyUsableAgain: Boolean(draft.notifyUsableAgain),
    notifyWaiting: Boolean(draft.notifyWaiting),
    agentsRead: Boolean(draft.agentsRead),
    agentsSwitch: Boolean(draft.agentsSwitch),
  });

  try {
    await invoke("save_settings", { settings: payload });
    state.errors.settings = null;
    state.errors.settingsConfigDir = null;
    state.errors.settingsHostName = null;
    state.errors.settingsHistoryDir = null;
    state.errors.settingsSaved = true;
    Object.assign(draft, payload);
    state.settingsDraft = draft;
    if (state.snapshot) state.snapshot.settings = Object.assign({}, payload);
    applyTheme(payload.theme);
    settingsFieldError("hostName", null);
    settingsFieldError("historyDir", null);
    settingsFieldError("claudeConfigDir", null);
    settingsError(null);
    afterCommand();
  } catch (error) {
    const message = errorMessage(error);
    state.errors.settingsSaved = false;
    if (message.startsWith("settings saved; start at login could not be set:")) {
      // The settings themselves are saved; drop the draft and show only the
      // autostart failure.
      if (state.snapshot) state.snapshot.settings = Object.assign({}, payload);
      state.settingsDraft = null;
      renderSettings();
    }
    routeSettingsError(message);
  }
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

function afterCommand() {
  if (HAS_TAURI || !state.fixture) return;
  state.snapshot = state.fixture;
  applyTheme();
  if (state.view === "main") renderMain();
  else if (state.view === "add") renderAdd();
  updateAge();
}

async function switchTo(key) {
  const sessions = state.snapshot ? state.snapshot.sessions : null;
  if (sessions && Number(sessions.tied) > 0) {
    state.confirmSwitchId = key;
    state.errors.banner = null;
    renderMain();
    return;
  }
  state.confirmSwitchId = null;
  return switchWithPolicy(key, "refuse", null);
}

function confirmSwitch() {
  const key = state.confirmSwitchId;
  state.confirmSwitchId = null;
  if (!key) {
    renderMain();
    return;
  }
  return switchWithPolicy(key, "refuse", null);
}

async function switchWithPolicy(key, unsaved, label) {
  state.confirmSwitchId = null;
  const cleanLabel = label === undefined || label === null ? null : String(label).trim();
  if (unsaved === "save" && !cleanLabel) {
    state.errors.banner = "Enter a label of 1 to 40 characters for the new account.";
    renderMain();
    return;
  }
  state.errors.banner = null;
  delete state.errors.accounts[key];
  try {
    await invoke("switch_account", {
      key,
      unsaved,
      label: cleanLabel,
    });
    delete state.errors.accounts[key];
    afterCommand();
  } catch (error) {
    const message = errorMessage(error);
    if (message === "not saved" || message.indexOf("not saved") !== -1) {
      // The next snapshot carries `pending`; leave the question to that banner.
      afterCommand();
      return;
    }
    if (findView(key)) state.errors.accounts[key] = message;
    else state.errors.banner = message;
    renderMain();
  }
}

async function cancelPending() {
  try {
    await invoke("cancel_pending", {});
    afterCommand();
  } catch (error) {
    state.errors.banner = errorMessage(error);
    renderMain();
  }
}

async function refreshNow() {
  if (state.refreshing) return;
  state.refreshing = true;
  updateAge();
  try {
    await invoke("refresh_now", {});
  } catch (error) {
    state.errors.banner = errorMessage(error);
    if (state.view === "main") renderMain();
  } finally {
    if (!HAS_TAURI) {
      state.refreshing = false;
      afterCommand();
      updateAge();
    } else {
      setTimeout(() => {
        if (!state.refreshing) return;
        state.refreshing = false;
        updateAge();
      }, 1500);
    }
  }
}

async function hidePanel() {
  try {
    await invoke("hide_panel", {});
  } catch {
    // the window is going away; nothing useful to show
  }
}

let fitTimer = null;

function fitPanel() {
  const height = document.body.scrollHeight;
  invoke("fit_panel", { height }).catch(() => {});
  if (fitTimer !== null) clearTimeout(fitTimer);
  fitTimer = setTimeout(() => {
    fitTimer = null;
    invoke("fit_panel", { height: document.body.scrollHeight }).catch(() => {});
  }, 450);
}

// ---------------------------------------------------------------------------
// header age and live countdowns
// ---------------------------------------------------------------------------

// The header's poll age, then every time text on the page (`updateTimes`).
// Runs every second and after each state change.
function updateAge() {
  const node = document.getElementById("poll-age");
  if (node) {
    const refresh = document.getElementById("refresh-btn");
    if (refresh) refresh.classList.toggle("refreshing", state.refreshing);
    const polled = state.snapshot && state.snapshot.lastPollAt;
    const status = state.refreshing
      ? "refreshing…"
      : !state.snapshot
        ? "loading…"
        : !polled || !Number.isFinite(Date.parse(polled))
          ? "never"
          : null;
    if (status) {
      node.removeAttribute("data-time");
      node.textContent = status;
    } else {
      node.setAttribute("data-time", polled);
      node.setAttribute("data-time-style", "age");
    }
  }
  updateTimes();
}

// A time written by `updateTimes`. `style` is `countdown` (`35m`, `1h05m`,
// `1d18h`, `Sun 02:00`), `clock` (`14:40`, `Sun 02:00`) or `age` (`12 s ago`,
// `5 min ago`).
function timeNode(iso, style, className) {
  return h("span", {
    class: className || "",
    "data-time": String(iso),
    "data-time-style": style,
  });
}

// Every time text on the page, written by the Rust side (`time_texts`), so the
// panel, the tray menu, the tooltip and the CLI share one rule (`format.rs`).
async function updateTimes() {
  const nodes = Array.from(document.querySelectorAll("[data-time]"));
  if (nodes.length === 0) return;
  const times = nodes.map((node) => ({
    at: node.getAttribute("data-time"),
    style: node.getAttribute("data-time-style"),
  }));
  try {
    const texts = await invoke("time_texts", { times });
    if (!Array.isArray(texts) || texts.length !== nodes.length) {
      throw new Error("time_texts returned an unexpected reply");
    }
    nodes.forEach((node, index) => {
      // The node may have changed meaning while the request ran (the header
      // switching to "refreshing…"); only write the text it asked for.
      if (node.getAttribute("data-time") !== times[index].at) return;
      if (node.textContent !== texts[index]) node.textContent = texts[index];
      node.removeAttribute("title");
    });
  } catch (error) {
    const message = errorMessage(error);
    console.error(`time texts: ${message}`);
    for (const node of nodes) {
      node.textContent = "?";
      node.title = message;
    }
  }
}

// ---------------------------------------------------------------------------
// views, keyboard
// ---------------------------------------------------------------------------

function viewFromHash() {
  const raw = String(location.hash || "").replace(/^#/, "").toLowerCase();
  if (raw === "add" || raw === "settings") return raw;
  return "main";
}

function setView(view, options) {
  const wanted = view === "add" || view === "settings" ? view : "main";
  state.view = wanted;

  const main = document.getElementById("view-main");
  const add = document.getElementById("view-add");
  const settingsView = document.getElementById("view-settings");
  if (main) main.hidden = wanted !== "main";
  if (add) add.hidden = wanted !== "add";
  if (settingsView) settingsView.hidden = wanted !== "settings";
  // The gear leads to settings; inside settings it would lead nowhere.
  const settingsBtn = document.getElementById("settings-btn");
  if (settingsBtn) settingsBtn.hidden = wanted === "settings";

  if (wanted === "main") {
    renderMain();
  } else if (wanted === "add") {
    state.errors.add = null;
    renderAdd();
  } else {
    state.settingsDraft = null;
    state.errors.settings = null;
    state.errors.settingsConfigDir = null;
    state.errors.settingsHostName = null;
    state.errors.settingsHistoryDir = null;
    state.errors.settingsSaved = false;
    renderSettings();
  }

  if (!options || !options.noHash) {
    try {
      history.replaceState(null, "", `#${wanted}`);
    } catch {
      // some webviews refuse history changes; the view still switched
    }
  }
  fitPanel();
}

// Move the keyboard cursor by `step` cards. Without a cursor it starts from the
// account in use, so the first ↑ lands on the card above it.
function moveCursor(step) {
  const views = orderedViews(state.snapshot ? state.snapshot.views : []);
  if (views.length === 0) return;
  let from = views.findIndex((view) => view.id === state.cursorId);
  if (from < 0) from = views.findIndex((view) => viewIsActive(view));
  if (from < 0) from = step > 0 ? -1 : views.length;
  const index = (((from + step) % views.length) + views.length) % views.length;
  state.cursorId = views[index].id;
  const cards = document.getElementById("cards");
  if (!cards) return;
  for (let child = 0; child < cards.children.length; child += 1) {
    cards.children[child].classList.toggle("focused", child === index);
  }
  const focused = cards.children[index];
  if (focused) {
    if (typeof focused.focus === "function") {
      try {
        focused.focus({ preventScroll: true });
      } catch {
        focused.focus();
      }
    }
    if (typeof focused.scrollIntoView === "function") {
      focused.scrollIntoView({ block: "nearest" });
    }
  }
}

function typingTarget(target) {
  if (!target || !(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  return Boolean(target.closest("input, textarea, select, [contenteditable='true']"));
}

function onKeyDown(event) {
  if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
  const target = event.target;

  if (event.key === "Escape") {
    event.preventDefault();
    if (state.editingId) {
      closeEditor();
      return;
    }
    if (state.view !== "main") {
      setView("main");
      return;
    }
    hidePanel();
    return;
  }

  if (typingTarget(target)) return;
  if (state.view !== "main") return;

  const views = orderedViews(state.snapshot ? state.snapshot.views : []);
  const onControl =
    target instanceof HTMLElement && target.closest("button, a, [role='button']");

  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    if (views.length === 0) return;
    event.preventDefault();
    moveCursor(event.key === "ArrowDown" ? 1 : -1);
    return;
  }

  if (event.key === "Enter") {
    if (!state.cursorId || onControl) return;
    event.preventDefault();
    switchTo(state.cursorId);
    return;
  }

  if (/^[1-9]$/.test(event.key)) {
    const view = views[Number(event.key) - 1];
    if (!view) return;
    event.preventDefault();
    switchTo(view.id);
    return;
  }

  if (event.key === ",") {
    event.preventDefault();
    setView("settings");
  }
}

// ---------------------------------------------------------------------------
// built-in fixture: what a browser shows without the tray app
// ---------------------------------------------------------------------------

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
        plan: "Ultra",
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
        plan: "Max",
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
        plan: "Team 5x",
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
      early: false,
      why: "switching before 100 % does not pay",
      learning: [],
      line: "On AC until its 5 h window is full, about 2h. Then LB.",
      planned: { [labId]: isoIn(2 * hour) },
      waiting: { to: privateId, latest: isoIn(40 * 60000), tiedBusy: 1 },
      suggestion: null,
    },
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
    settings: Object.assign({}, DEFAULT_SETTINGS, {
      autoBurn: true,
      suggestSwitches: true,
    }),
  };
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
  return merged;
}

async function fixtureInvoke(command, args) {
  console.log(`[fixture] ${command}`, args);
  const fixture = state.fixture;
  if (!fixture && command !== "fit_panel" && command !== "hide_panel") {
    throw new Error("fixture is not ready");
  }

  switch (command) {
    case "get_snapshot":
      return clone(fixture);

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
      account.label = label;
      account.abbr = abbr;
      account.color = color;
      account.autoBurn = autoBurn;
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
        return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
      });

    case "fit_panel":
      return null;

    case "hide_panel":
      return null;

    default:
      throw new Error(`unknown command ${command}`);
  }
}

// ---------------------------------------------------------------------------
// boot
// ---------------------------------------------------------------------------

function wireStatic() {
  const refresh = document.getElementById("refresh-btn");
  if (refresh) refresh.addEventListener("click", refreshNow);
  const settings = document.getElementById("settings-btn");
  if (settings) settings.addEventListener("click", () => setView("settings"));
  const close = document.getElementById("close-btn");
  if (close) close.addEventListener("click", hidePanel);
  const add = document.getElementById("add-btn");
  if (add) add.addEventListener("click", () => setView("add"));

  document.addEventListener("keydown", onKeyDown);
  window.addEventListener("hashchange", () => {
    const wanted = viewFromHash();
    if (wanted !== state.view) setView(wanted, { noHash: true });
  });
}

function handleSnapshot(snapshot) {
  if (!isObject(snapshot)) return;
  state.snapshot = snapshot;
  state.refreshing = false;
  applyTheme();

  if (state.view === "main") {
    renderMain();
  } else if (state.view === "add") {
    const active = document.activeElement;
    const typing =
      active && typeof active.closest === "function"
        ? active.closest("input, textarea, select")
        : null;
    if (!typing) renderAdd();
  } else if (state.view === "settings") {
    if (!isObject(snapshot.settings)) state.settingsDraft = null;
    if (!state.settingsDraft) renderSettings();
  }
  updateAge();
}

// The app's version (the workspace version in Cargo.toml) for the settings header.
async function loadVersion() {
  if (!HAS_TAURI) {
    state.version = { text: "fixture", title: "No Tauri: built-in test data" };
  } else {
    try {
      const version = await TAURI_GLOBAL.app.getVersion();
      state.version = { text: `v${version}`, title: `seatswap ${version}` };
    } catch (error) {
      state.version = { text: "v?", title: `Version unknown: ${errorMessage(error)}` };
    }
  }
  if (state.view === "settings") renderSettings();
}

async function boot() {
  wireStatic();
  wireTheme();
  if (!HAS_TAURI) {
    state.fixture = makeFixture();
    console.info("[fixture] no window.__TAURI__; using the built-in snapshot");
  }
  applyTheme();
  setView(viewFromHash(), { noHash: true });
  loadVersion();

  await listen("snapshot", (payload) => handleSnapshot(payload));
  await listen("navigate", (payload) => {
    const view = typeof payload === "string" ? payload : payload && payload.view;
    // Sent each time the pop-up opens: start without a keyboard cursor.
    state.cursorId = null;
    if (view === "main" || view === "add" || view === "settings") setView(view);
  });

  try {
    const snapshot = await invoke("get_snapshot", {});
    handleSnapshot(snapshot);
  } catch (error) {
    state.errors.banner = errorMessage(error);
    if (state.view === "main") renderMain();
  }

  setInterval(updateAge, 1000);
  updateAge();
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot, { once: true });
} else {
  boot();
}
