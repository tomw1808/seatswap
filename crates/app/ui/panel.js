// seatswap pop-up panel.
// Plain ES2020, no build step. Talks to the tray app through the Tauri 2 global API;
// without it, runs on a built-in fixture snapshot so the page can be checked in a browser.

import {
  HAS_TAURI,
  LEVEL_COLORS,
  TRAY_GREY,
  applyTheme,
  banner,
  clamp,
  clear,
  contrastText,
  errorMessage,
  followTheme,
  h,
  isColor,
  isActiveView,
  isObject,
  invoke,
  limitLevel,
  listen,
  mixHex,
  rgba,
  shortPercent,
  switchButton,
  textInput,
} from "./common.js";
import { currentFixture, ensureFixture } from "./fixture.js";

const state = {
  snapshot: null,
  view: "main",
  // The account the keyboard cursor (↑/↓, Enter) is on; null = no cursor shown.
  // Cleared every time the pop-up opens, so it never outlives one use.
  cursorId: null,
  // The account a switch waits for the person to confirm; only a switch with
  // sessions on a model that loses its thinking asks (spec §2.12).
  confirmSwitchId: null,
  pendingKey: null,
  pendingLabel: "",
  unknownKey: null,
  unknownLabel: "",
  refreshing: false,
  lastPercents: new Map(),
  lastCardOrder: [],
  errors: {
    banner: null,
    burn: null,
    accounts: {},
    add: null,
  },
};

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
  return "system";
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

// ---------------------------------------------------------------------------
// snapshot field readers (the shape is docs/implementation.md, serde camelCase)
// ---------------------------------------------------------------------------

function allViews() {
  if (!state.snapshot || !Array.isArray(state.snapshot.views)) return [];
  return state.snapshot.views;
}

function viewIsActive(view) {
  return isActiveView(view, state.snapshot);
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
// rendering: main view
// ---------------------------------------------------------------------------

function renderMain() {
  applyTheme(currentThemeSetting());
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
    // A warning is not a refused save: the setting is saved, the warning shown.
    const warning = await invoke("set_auto_burn", { on });
    afterCommand();
    if (typeof warning === "string" && warning !== "") {
      state.errors.burn = warning;
      renderMain();
    }
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
  // The badge and the pencil open the large window on this account's row.
  const edit = (event) => {
    event.stopPropagation();
    invoke("open_window", { target: `settings/accounts?${view.id}` }).catch((error) => {
      state.errors.accounts[view.id] = errorMessage(error);
      renderMain();
    });
  };
  badge.addEventListener("click", edit);
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
// commands
// ---------------------------------------------------------------------------

function afterCommand() {
  const fixture = currentFixture();
  if (HAS_TAURI || !fixture) return;
  state.snapshot = fixture;
  applyTheme(currentThemeSetting());
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
  if (raw === "add") return raw;
  return "main";
}

// The pop-up has no settings view (spec §3.7): its gear and the comma open the
// large window at Settings instead.
async function openSettings() {
  try {
    await invoke("open_window", { target: "settings" });
  } catch (error) {
    state.errors.banner = errorMessage(error);
    if (state.view === "main") renderMain();
  }
}

function setView(view, options) {
  const wanted = view === "add" ? view : "main";
  state.view = wanted;

  const main = document.getElementById("view-main");
  const add = document.getElementById("view-add");
  if (main) main.hidden = wanted !== "main";
  if (add) add.hidden = wanted !== "add";

  if (wanted === "main") {
    renderMain();
  } else {
    state.errors.add = null;
    renderAdd();
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
    openSettings();
  }
}


// ---------------------------------------------------------------------------
// boot
// ---------------------------------------------------------------------------

function wireStatic() {
  const refresh = document.getElementById("refresh-btn");
  if (refresh) refresh.addEventListener("click", refreshNow);
  const settings = document.getElementById("settings-btn");
  if (settings) settings.addEventListener("click", openSettings);
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
  applyTheme(currentThemeSetting());

  if (state.view === "main") {
    renderMain();
  } else if (state.view === "add") {
    const active = document.activeElement;
    const typing =
      active && typeof active.closest === "function"
        ? active.closest("input, textarea, select")
        : null;
    if (!typing) renderAdd();
  }
  updateAge();
}

async function boot() {
  wireStatic();
  followTheme(currentThemeSetting);
  if (!HAS_TAURI) ensureFixture();
  applyTheme(currentThemeSetting());
  setView(viewFromHash(), { noHash: true });

  await listen("snapshot", (payload) => handleSnapshot(payload));
  await listen("navigate", (payload) => {
    const view = typeof payload === "string" ? payload : payload && payload.view;
    // Sent each time the pop-up opens: start without a keyboard cursor.
    state.cursorId = null;
    if (view === "main" || view === "add") setView(view);
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
