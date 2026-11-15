// The Settings area of the large window (spec §3.7): seven groups on the left,
// the chosen group's card on the right. Every control saves at once through
// `save_settings`; a refused save shows its error under the row and puts the
// control back to the saved value. Plain ES2020, no framework.

import {
  abbrLetterBoxes,
  applyTheme,
  clear,
  clone,
  colorSwatches,
  contrastText,
  errorMessage,
  h,
  hexField,
  iconPreview,
  isColor,
  isObject,
  selectInput,
  shortPercent,
  switchButton,
  textInput,
} from "../common.js";

const GROUPS = [
  { id: "general", label: "General" },
  { id: "accounts", label: "Accounts" },
  { id: "auto-burn", label: "Auto-burn" },
  { id: "notifications", label: "Notifications" },
  { id: "projects", label: "Projects" },
  { id: "agents", label: "Agents" },
  { id: "history", label: "History and hosts" },
];

const ALIASES = {
  general: "general",
  gen: "general",
  accounts: "accounts",
  acc: "accounts",
  "auto-burn": "auto-burn",
  autoburn: "auto-burn",
  burn: "auto-burn",
  notifications: "notifications",
  noti: "notifications",
  projects: "projects",
  proj: "projects",
  agents: "agents",
  history: "history",
  hist: "history",
};

const NOT_YET = "Comes with a later version.";
const MISSING = "not measured yet: it needs a few days of work";

const state = {
  snapshot: null,
  settings: null,
  context: null,
  container: null,
  group: "general",
  sub: null,
  // The account whose chip editor is open, and the row the target marks.
  openEditorId: null,
  selectedId: null,
  confirmRemoveId: null,
  right: null,
  errors: Object.create(null),
  skillToken: 0,
  callsToken: 0,
  // Saves run one after another, each built from the settings as the one
  // before left them, so a quick second change never carries a stale copy.
  saving: Promise.resolve(),
  pendingSaves: 0,
};

// A snapshot read before a save landed must not undo it: while saves are
// pending, the page keeps its own settings.
function takeSettings(snapshot) {
  if (state.pendingSaves > 0) return;
  state.settings = isObject(snapshot.settings) ? snapshot.settings : null;
}

function groupFrom(sub) {
  if (typeof sub !== "string" || sub === "") return { group: "general", item: null };
  const mark = sub.indexOf("?");
  const name = (mark < 0 ? sub : sub.slice(0, mark)).toLowerCase();
  const item = mark < 0 ? null : sub.slice(mark + 1);
  return { group: ALIASES[name] || null, item };
}

function labelOf(id) {
  const group = GROUPS.find((entry) => entry.id === id);
  return group ? group.label : "Settings";
}

// ---------------------------------------------------------------------------
// the page and its seven groups
// ---------------------------------------------------------------------------

export function render(container, context) {
  const snapshot = isObject(context.snapshot) ? context.snapshot : {};
  const parsed = groupFrom(context.sub);
  const group = parsed.group;
  const sameTarget = state.sub === context.sub;
  state.sub = context.sub;
  if (parsed.item && !sameTarget) {
    state.selectedId = parsed.item;
    state.openEditorId = parsed.item;
  }
  const active = document.activeElement;
  const typing =
    active && active.tagName === "INPUT" && container.contains(active);
  if (typing && group === state.group && state.container === container) {
    // A snapshot arrived while a number or text field was being typed in: keep
    // the control (and what was typed) until it is saved on change.
    state.snapshot = snapshot;
    takeSettings(snapshot);
    state.context = context;
    return;
  }
  state.snapshot = snapshot;
  takeSettings(snapshot);
  state.context = context;
  state.container = container;
  state.group = group;

  clear(container);
  const right = h("div", { class: "col settings-col" });
  const grid = h("div", { class: "g settings-area" }, [
    h("div", { class: "hcard" }, [
      h(
        "div",
        { class: "snav", role: "tablist", "aria-label": "Setting group" },
        GROUPS.map((group) => groupButton(group, right))
      ),
    ]),
    right,
  ]);
  container.appendChild(grid);
  renderGroup(right, state.group);
}

function groupButton(group, right) {
  const current = group.id === state.group;
  return h(
    "button",
    {
      type: "button",
      class: `snb${current ? " ison" : ""}`,
      role: "tab",
      "aria-selected": current ? "true" : "false",
      onclick: () => selectGroup(group.id, right),
    },
    group.label
  );
}

function selectGroup(id, right) {
  state.openEditorId = null;
  state.selectedId = null;
  state.confirmRemoveId = null;
  const target = `settings/${id}`;
  if (location.hash.slice(1) === target) {
    state.group = id;
    renderGroup(right, id);
    return;
  }
  location.hash = target;
}

function renderGroup(right, id) {
  clear(right);
  state.group = id;
  state.right = right;
  if (!GROUPS.some((group) => group.id === id)) {
    right.appendChild(panel("Settings", "", [message("Unknown setting group.")]));
    return;
  }
  if (!["general", "accounts", "auto-burn", "notifications", "agents"].includes(id)) {
    right.appendChild(panel(labelOf(id), "", [message(NOT_YET)]));
    return;
  }
  if (id === "general") right.appendChild(generalCard());
  else if (id === "accounts") right.appendChild(accountsCard());
  else if (id === "auto-burn") right.appendChild(autoBurnCard());
  else if (id === "notifications") right.appendChild(notificationsCard());
  else right.appendChild(agentsCard());
}

function panel(title, sub, children, foot) {
  return h("div", { class: "hcard settings-card" }, [
    h("div", { class: "ch" }, [
      h("span", { class: "t", text: title }),
      sub ? h("span", { class: "s", text: sub }) : null,
    ]),
    h("div", { class: "settings-body" }, children),
    foot ? h("div", { class: "cfoot" }, foot) : null,
  ]);
}

function message(text) {
  return h("p", { class: "area-message", text });
}

function section(text) {
  return h("div", { class: "sh", text });
}

function setting(label, control, note, field) {
  const error = h("div", { class: "row-error", hidden: true });
  const wrapper = h(
    "div",
    { class: "set-row", dataset: field ? { field } : {} },
    [
      h("div", { class: "set" }, [
        h("span", {}, label),
        h("span", {}, [control, note ? h("span", { class: "mf" }, note) : null]),
      ]),
      error,
    ]
  );
  return wrapper;
}

function note(children) {
  return h("div", { class: "note" }, children);
}

// ---------------------------------------------------------------------------
// saving: one control, the whole settings object, no Save button
// ---------------------------------------------------------------------------

function fieldError(field, text) {
  state.errors[field] = text || null;
  if (!state.container) return;
  const nodes = state.container.querySelectorAll(`[data-field="${field}"] .row-error`);
  for (const node of nodes) {
    node.textContent = text || "";
    node.hidden = !text;
  }
}

// Every save (a setting or an account field) runs through one queue, one after
// another, so a quick second change keeps the first.
function queuedSave(run) {
  state.pendingSaves += 1;
  const task = state.saving.then(run);
  state.saving = task.catch(() => false).finally(() => {
    state.pendingSaves -= 1;
  });
  return task;
}

function saveField(field, value) {
  return queuedSave(() => saveNow(field, value));
}

// An account field saves through the same `edit_account` the CLI uses. The
// refused save's error stays under that account's row and the control goes
// back to the stored value at the caller.
function saveAccount(account, patch, field) {
  return queuedSave(async () => {
    try {
      await state.context.invoke("edit_account", { key: account.id, ...patch });
      fieldError(field, null);
      return true;
    } catch (error) {
      fieldError(field, errorMessage(error));
      return false;
    }
  });
}

async function saveNow(field, value) {
  if (!state.settings) {
    fieldError(field, "The settings could not be read; nothing is saved.");
    return false;
  }
  const settings = clone(state.settings);
  settings[field] = value;
  try {
    await state.context.invoke("save_settings", { settings });
    state.settings = settings;
    if (state.snapshot) state.snapshot.settings = settings;
    fieldError(field, null);
    return true;
  } catch (error) {
    const text = errorMessage(error);
    if (text.startsWith("settings saved; start at login could not be set:")) {
      // The settings themselves are saved; only the autostart entry failed.
      state.settings = settings;
      if (state.snapshot) state.snapshot.settings = settings;
      fieldError(field, text);
      return true;
    }
    fieldError(field, text);
    return false;
  }
}

function savedValue(field) {
  return state.settings ? state.settings[field] : undefined;
}

function themeControl(value) {
  const select = selectInput(
    [
      ["system", "System"],
      ["light", "Light"],
      ["dark", "Dark"],
    ],
    value,
    async (next) => {
      applyTheme(next);
      select.disabled = true;
      const ok = await saveField("theme", next);
      if (!ok) applyTheme(savedValue("theme"));
      select.value = String(savedValue("theme"));
      select.disabled = false;
    }
  );
  select.setAttribute("aria-label", "Theme");
  return select;
}

function selectSetting(field, options, label) {
  const select = selectInput(options, savedValue(field), async (next) => {
    select.disabled = true;
    await saveField(field, next);
    select.value = String(savedValue(field));
    select.disabled = false;
  });
  if (label) select.setAttribute("aria-label", label);
  return select;
}

function toggle(field, label) {
  const control = switchButton(Boolean(savedValue(field)), label, async (next) => {
    control.disabled = true;
    const ok = await saveField(field, next);
    if (!ok) {
      control.setAttribute("aria-checked", savedValue(field) ? "true" : "false");
    }
    control.disabled = false;
  });
  return control;
}

function numberField(field, label, options) {
  const value = savedValue(field);
  const input = h("input", {
    type: "number",
    class: "inp num",
    value: value === null || value === undefined ? "" : String(value),
    min: String(options.min),
    step: String(options.step || 1),
    "aria-label": label,
  });
  const save = async () => {
    const raw = input.value.trim();
    const number = Number(raw);
    if (raw === "" || !Number.isInteger(number) || number < options.min) {
      fieldError(field, `${label} must be a whole number of at least ${options.min}.`);
      input.value = String(savedValue(field));
      return;
    }
    await saveField(field, number);
    input.value = String(savedValue(field));
  };
  input.addEventListener("change", save);
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      input.blur();
    }
  });
  return h("span", { class: "number-control" }, [
    input,
    h("span", { class: "mf", text: options.unit || "" }),
  ]);
}

function textField(field, label, placeholder) {
  const value = savedValue(field);
  const input = textInput({
    value: value === null || value === undefined ? "" : String(value),
    placeholder,
    "aria-label": label,
  });
  input.className = "inp tall";
  input.addEventListener("change", async () => {
    const raw = input.value.trim();
    const next = raw === "" ? null : raw;
    const ok = await saveField(field, next);
    if (!ok) input.value = savedValue(field) || "";
    else input.value = next || "";
  });
  return input;
}

// ---------------------------------------------------------------------------
// General
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Accounts (spec §3.7): a row per account, a chip editor, and the price.
// ---------------------------------------------------------------------------

function accountRows() {
  const snapshot = state.snapshot || {};
  if (Array.isArray(snapshot.accounts)) return snapshot.accounts;
  return Array.isArray(snapshot.views) ? snapshot.views : [];
}

// Task 4 adds `settings.currency`; until then a missing code is said as such
// and no made-up code is put beside a price.
function currencyCode() {
  const value = state.settings && state.settings.currency;
  return typeof value === "string" && /^[A-Z]{3}$/.test(value) ? value : null;
}

function accountPlan(row) {
  return typeof row.plan === "string" && row.plan !== "" ? row.plan : "—";
}

function accountEmail(row) {
  return typeof row.email === "string" && row.email !== "" ? row.email : "—";
}

function loginEndsText(row) {
  let days = row.loginDaysLeft;
  if (typeof days !== "number" && typeof row.loginEndsAtMs === "number") {
    days = Math.floor((row.loginEndsAtMs - Date.now()) / 86400000);
  }
  if (typeof days !== "number" || !Number.isFinite(days)) return "—";
  if (days < 0) return "ended";
  if (days === 0) return "today";
  return `in ${Math.round(days)} days`;
}

function accountsCard() {
  const card = panel(
    "Accounts",
    "the coloured field holds the abbreviation: click it to change the letters and the colour",
    [],
    [h("span", { class: "mf", text: "The ✎ in the pop-up opens this group." })]
  );
  const body = card.querySelector(".settings-body");
  body.appendChild(
    h("div", { class: "lr hd acc-grid" }, [
      h("span", { text: "Abbr." }),
      h("span", { text: "Label" }),
      h("span", { text: "Account" }),
      h("span", { text: "Plan" }),
      h("span", { text: "Price / month" }),
      h("span", { text: "Auto-burn may use it" }),
      h("span", { text: "Organization" }),
      h("span", { text: "Login ends" }),
    ])
  );
  const rows = accountRows();
  if (rows.length === 0) {
    body.appendChild(h("div", { class: "lr", text: "No accounts yet." }));
  }
  for (const row of rows) body.appendChild(accountRow(row));
  body.appendChild(
    h("div", { class: "lr acc-add" }, [
      h("span", {
        class: "mf",
        text: "+ Add account: log in with /login in Claude Code, then save it here.",
      }),
    ])
  );
  body.appendChild(
    note([
      "Account and plan come from the login and cannot be changed here. Price is what you pay per month, in the currency set under General. It turns percent into money, so accounts can be added up. Auto-burn may use it: switch it off for an account that should be left alone.",
    ])
  );
  return card;
}

function accountRow(row) {
  const field = `account:${row.id}`;
  let accent = isColor(row.color) ? row.color.toLowerCase() : "#555b66";
  let abbr = row.abbr || "?";

  const wrap = h("div", {
    class: `acc-wrap${state.selectedId === row.id ? " selected" : ""}`,
    dataset: { field },
  });
  const chip = h("button", {
    type: "button",
    class: `acc-chip${state.openEditorId === row.id ? " open" : ""}`,
    title: "Change the abbreviation and the colour",
    "aria-label": `Edit account ${abbr}`,
    "aria-expanded": state.openEditorId === row.id ? "true" : "false",
  });
  chip.appendChild(h("span", { text: abbr }));
  chip.appendChild(h("i", { "aria-hidden": "true", text: "\u270E" }));
  const paintChip = () => {
    chip.firstChild.textContent = abbr;
    chip.style.background = accent;
    chip.style.color = contrastText(accent);
    chip.setAttribute("aria-label", `Edit account ${abbr}`);
  };
  paintChip();
  chip.addEventListener("click", () => {
    state.openEditorId = state.openEditorId === row.id ? null : row.id;
    state.selectedId = row.id;
    state.confirmRemoveId = null;
    renderGroup(state.right, "accounts");
  });

  const labelInput = textInput({
    class: "inp tall acc-label",
    value: row.label || "",
    maxlength: "40",
    "aria-label": "Label",
  });
  labelInput.addEventListener("change", async () => {
    const next = labelInput.value.trim();
    if (next === (row.label || "")) {
      labelInput.value = row.label || "";
      return;
    }
    const ok = await saveAccount(row, { label: next }, field);
    if (ok) row.label = next;
    else labelInput.value = row.label || "";
  });

  const priceInput = textInput({
    class: "inp tall acc-price",
    value: row.price === null || row.price === undefined ? "" : String(row.price),
    inputmode: "decimal",
    "aria-label": "Price per month",
  });
  const storedPrice = () =>
    row.price === null || row.price === undefined ? "" : String(row.price);
  priceInput.addEventListener("change", async () => {
    const raw = priceInput.value.trim();
    let amount = null;
    if (raw !== "") {
      amount = Number(raw);
      if (!Number.isFinite(amount) || amount <= 0) {
        fieldError(field, "The price must be a number above 0 per month.");
        priceInput.value = storedPrice();
        return;
      }
    }
    if (amount === row.price) {
      priceInput.value = storedPrice();
      return;
    }
    const ok = await saveAccount(row, { price: amount }, field);
    if (ok) {
      row.price = amount;
      priceInput.value = storedPrice();
    } else {
      priceInput.value = storedPrice();
    }
  });

  const code = currencyCode();
  const priceCell = h("div", { class: "price-cell" }, [
    priceInput,
    code
      ? h("span", { class: "x11 mf", text: code })
      : h("span", {
          class: "x11 mf currency-hint",
          text: "currency: set it under General",
        }),
  ]);

  const auto = switchButton(
    row.autoBurn === true,
    `Auto-burn may use ${abbr}`,
    async (next) => {
      const ok = await saveAccount(row, { autoBurn: next }, field);
      if (ok) row.autoBurn = next;
      else auto.setAttribute("aria-checked", row.autoBurn ? "true" : "false");
    }
  );

  wrap.appendChild(
    h("div", { class: "lr acc-grid" }, [
      chip,
      labelInput,
      h("span", { class: "acc-text", title: accountEmail(row), text: accountEmail(row) }),
      h("span", { class: "acc-text", title: accountPlan(row), text: accountPlan(row) }),
      priceCell,
      h("div", { class: "acc-switch" }, [auto]),
      h("span", {
        class: "acc-text",
        text: "—",
        title: "The organization list comes with the Projects group",
      }),
      h("span", { class: "acc-text", text: loginEndsText(row) }),
    ])
  );
  wrap.appendChild(
    h("div", {
      class: "row-error",
      hidden: !state.errors[field],
      text: state.errors[field] || "",
    })
  );

  if (state.openEditorId === row.id) {
    wrap.appendChild(
      chipEditor(row, field, (next) => {
        if (typeof next.abbr === "string") abbr = next.abbr;
        if (typeof next.color === "string") accent = next.color;
        paintChip();
      })
    );
  }
  return wrap;
}

function chipEditor(row, field, changed) {
  let color = isColor(row.color) ? row.color.toLowerCase() : "";

  const preview = iconPreview();
  const schedule = () => {
    preview.schedule(letters.value(), color, (error) => fieldError(field, error));
  };

  const letters = abbrLetterBoxes(row.abbr || "", {
    onInput: schedule,
    onChange: async (abbr) => {
      if (!/^[A-Z0-9]{1,2}$/.test(abbr)) {
        fieldError(field, "The abbreviation must be one or two letters A-Z or digits.");
        letters.setValue(row.abbr || "");
        return;
      }
      if (abbr === row.abbr) {
        letters.setValue(row.abbr || "");
        return;
      }
      const ok = await saveAccount(row, { abbr }, field);
      if (ok) {
        row.abbr = abbr;
        changed({ abbr });
      } else {
        letters.setValue(row.abbr || "");
      }
    },
  });

  const swatches = colorSwatches(color, async (next) => {
    hex.setValue(next);
    color = next;
    schedule();
    await saveColor(next);
  });

  async function saveColor(next) {
    if (next === row.color) return;
    if (!isColor(next)) {
      fieldError(field, "The colour must be #rrggbb.");
      hex.setValue(row.color || "");
      swatches.select(row.color);
      color = row.color || "";
      schedule();
      return;
    }
    const ok = await saveAccount(row, { color: next }, field);
    if (ok) {
      row.color = next;
      swatches.select(next);
      changed({ color: next });
    } else {
      hex.setValue(row.color || "");
      swatches.select(row.color);
      color = row.color || "";
      schedule();
    }
  }

  const hex = hexField(color, {
    onInput: (next) => {
      color = next;
      schedule();
    },
    onChange: saveColor,
  });

  const editor = h("div", { class: "acc-editor" }, [
    h("div", { class: "editor-row" }, [
      h("div", { class: "field" }, [
        h("span", { class: "field-label", text: "Letters" }),
        letters.node,
      ]),
      h("div", { class: "field" }, [
        h("span", { class: "field-label", text: "Colour" }),
        h("div", { class: "color-line" }, [swatches.node, hex.node]),
      ]),
      h("div", { class: "field" }, [
        h("span", { class: "field-label", text: "Tray icon" }),
        preview.node,
      ]),
    ]),
    h("div", { class: "editor-actions" }, [
      h("button", {
        type: "button",
        class: "hbtn xs danger",
        text: "Remove",
        "aria-label": `Remove account ${row.abbr || "?"}`,
        onclick: () => {
          state.confirmRemoveId = row.id;
          renderGroup(state.right, "accounts");
        },
      }),
    ]),
  ]);
  if (state.confirmRemoveId === row.id) editor.appendChild(confirmRemove(row, field));
  preview.draw(letters.value(), color).then((error) => {
    if (error) fieldError(field, error);
  });
  return editor;
}

function confirmRemove(row, field) {
  const name = `${row.abbr || "?"} — ${row.label || "this account"}`;
  return h("div", { class: "confirm" }, [
    h("div", {
      class: "confirm-text",
      text: `Remove ${name}? The live login is not touched.`,
    }),
    h("div", { class: "confirm-actions" }, [
      h("button", {
        type: "button",
        class: "hbtn xs danger",
        text: "Remove",
        onclick: () => removeAccount(row, field),
      }),
      h("button", {
        type: "button",
        class: "hbtn xs ghost",
        text: "Keep it",
        onclick: () => {
          state.confirmRemoveId = null;
          renderGroup(state.right, "accounts");
        },
      }),
    ]),
  ]);
}

async function removeAccount(row, field) {
  try {
    await state.context.invoke("remove_account", { key: row.id });
    state.confirmRemoveId = null;
    state.openEditorId = null;
    state.selectedId = null;
    delete state.errors[field];
    const snapshot = state.snapshot;
    if (snapshot) {
      if (Array.isArray(snapshot.accounts)) {
        snapshot.accounts = snapshot.accounts.filter((entry) => entry.id !== row.id);
      }
      if (Array.isArray(snapshot.views)) {
        snapshot.views = snapshot.views.filter((entry) => entry.id !== row.id);
      }
    }
    renderGroup(state.right, "accounts");
  } catch (error) {
    fieldError(field, errorMessage(error));
  }
}

function generalCard() {
  return panel(
    "General",
    "",
    [
      section("Look"),
      setting("Theme", themeControl(savedValue("theme") || "system"), "", "theme"),
      setting(
        "Tray text",
        selectSetting(
          "trayText",
          [
            ["percent", "Percent"],
            ["percentReset", "Percent and reset"],
            ["off", "Off"],
          ],
          "Tray text"
        ),
        "",
        "trayText"
      ),
      setting(
        "The tray ring shows",
        selectSetting(
          "iconLimit",
          [
            ["binding", "The binding limit"],
            ["session", "The 5 h session"],
            ["week", "The week"],
          ],
          "The tray ring shows"
        ),
        "",
        "iconLimit"
      ),
      section("Start and polling"),
      setting(
        "Start at login",
        toggle("startAtLogin", "Start at login"),
        "",
        "startAtLogin"
      ),
      setting(
        "Read the active account every",
        numberField("pollActiveSecs", "Read the active account every", {
          min: 30,
          step: 30,
          unit: "s",
        }),
        "every 30 s from 90 %",
        "pollActiveSecs"
      ),
      setting(
        "Read inactive accounts every",
        numberField("pollInactiveSecs", "Read inactive accounts every", {
          min: 30,
          step: 30,
          unit: "s",
        }),
        "",
        "pollInactiveSecs"
      ),
      section("Claude Code"),
      setting(
        "Claude Code's config folder",
        textField("claudeConfigDir", "Claude Code's config folder", "/home/you/.claude"),
        "",
        "claudeConfigDir"
      ),
      setting(
        "Read Claude Code's session files",
        toggle("readSessions", "Read Claude Code's session files"),
        "model, busy or idle, tokens per folder · never message text",
        "readSessions"
      ),
      setting(
        "Copy the live login at once",
        toggle("mirrorLive", "Copy the live login at once"),
        "",
        "mirrorLive"
      ),
      setting(
        "Refresh inactive logins",
        toggle("refreshInactive", "Refresh inactive logins"),
        "needed to read their usage",
        "refreshInactive"
      ),
    ]
  );
}

// ---------------------------------------------------------------------------
// Auto-burn
// ---------------------------------------------------------------------------

function burn() {
  return isObject(state.snapshot.burn) ? state.snapshot.burn : null;
}

function percentText(value) {
  return shortPercent(value);
}

// The window share is the range over the accounts auto-burn may use; one value
// when they agree.
function measuredText(measured) {
  if (!isObject(measured)) return MISSING;
  const values = Object.values(measured.windowPercentOfWeek || {}).filter((value) =>
    Number.isFinite(value)
  );
  const parts = [];
  if (values.length === 0) {
    parts.push(`a full 5 h window is ${MISSING}`);
  } else {
    const low = Math.min(...values);
    const high = Math.max(...values);
    const share =
      low === high ? percentText(low) : `${percentText(low)}–${percentText(high)}`;
    parts.push(`a full 5 h window is about ${share} % of a week`);
  }
  const cost = measured.switchCostPercentOfWindow;
  if (Number.isFinite(cost)) {
    parts.push(`a switch costs about ${percentText(cost)} % of a window`);
  } else {
    parts.push(`a switch cost is ${MISSING}`);
  }
  return parts.join(" · ");
}

function switchesText(view) {
  if (!view) return MISSING;
  if (!view.early) return "switching only when full";
  return "switching before 100 % pays";
}

function switchesNote(view) {
  if (!view) return "";
  if (!view.early) return "early switching is off";
  const value = view.measured ? view.measured.switchesPerDay : null;
  if (!Number.isFinite(value)) return MISSING;
  return `about ${percentText(value)} switches a day`;
}

function autoBurnCard() {
  const view = burn();
  return panel(
    "Auto-burn",
    "switches accounts so that you are blocked as little as possible",
    [
      section("Auto-burn"),
      setting(
        "Auto-burn",
        toggle("autoBurn", "Auto-burn"),
        "switches by itself, as well as it can",
        "autoBurn"
      ),
      setting(
        "Suggest switches",
        toggle("suggestSwitches", "Suggest switches"),
        "while auto-burn is off: what it would do now, and why",
        "suggestSwitches"
      ),
      section("What it decides by itself"),
      setting(
        "How early it switches",
        h("span", { text: view && view.why ? view.why : MISSING }),
        "the size of your 5 h windows and what a switch costs"
      ),
      setting("Measured so far", h("span", { text: measuredText(view && view.measured) })),
      setting(
        "So right now",
        h("span", { class: "ok", text: switchesText(view) }),
        switchesNote(view)
      ),
      setting(
        "After a switch by you",
        h("span", { text: "it waits 4 hours, or until that account is full" })
      ),
      setting(
        "While a session would lose its thinking",
        h("span", { text: "a planned switch waits" }),
        "never longer than the wanted account can spare"
      ),
      note([
        h("b", { text: "There is nothing to tune." }),
        " A session on Fable 5.2, Mythos 5.2, Sonnet 5.5 or Haiku 5.5 cannot read its earlier thinking on another account, so a planned switch waits while such a session is busy. Sessions on Opus 5.5 keep their thinking. For long runs that should survive switches, Opus 5.5 is the better choice.",
      ]),
    ]
  );
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

function notificationsCard() {
  return panel(
    "Notifications",
    "",
    [
      section("Sessions"),
      setting(
        "A session is waiting for you",
        toggle("notifyWaiting", "A session is waiting for you"),
        "when it has waited 15 seconds · stays until you click it · a click opens Sessions",
        "notifyWaiting"
      ),
      setting(
        "For sessions on",
        selectSetting(
          "notifyWaitingHosts",
          [
            ["thisHost", "this host"],
            ["allHosts", "all hosts"],
          ],
          "For sessions on"
        ),
        "other hosts report with the delay of the sync",
        "notifyWaitingHosts"
      ),
      section("Accounts"),
      setting(
        "An account is usable again",
        toggle("notifyUsableAgain", "An account is usable again"),
        "",
        "notifyUsableAgain"
      ),
      setting(
        "All accounts are full",
        h("span", { class: "mf", text: "always" }),
        "with the time the next one is free"
      ),
      setting(
        "Every automatic switch",
        toggle("notifyOnSwitch", "Every automatic switch"),
        "",
        "notifyOnSwitch"
      ),
      setting(
        "A switch by an agent",
        h("span", { class: "mf", text: "always" })
      ),
      setting(
        "Suggested switches",
        h(
          "button",
          {
            type: "button",
            class: "link",
            onclick: () => {
              const right = state.container.querySelector(".settings-col");
              if (right) selectGroup("auto-burn", right);
            },
          },
          "see Auto-burn"
        )
      ),
      setting(
        "A login ends",
        toggle("notifyLoginEnds", "A login ends"),
        "5 days and 1 day before",
        "notifyLoginEnds"
      ),
    ]
  );
}

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

function agentsCard() {
  const skillControl = h("span", { class: "row" }, [
    h("span", { class: "mf", text: "loading…" }),
  ]);
  const skillNote = h("span", { class: "mf" });
  const skillRow = setting("Agent skill", skillControl, skillNote, "skill");
  const callRows = h("div", { class: "col calls" });
  const calls = h("div", { class: "col" }, [
    h("div", {
      class: "lr hd",
      style: { gridTemplateColumns: "96px minmax(0, 1fr) 170px 110px" },
    }, [
      h("span", { text: "When" }),
      h("span", { text: "Command" }),
      h("span", { text: "From" }),
      h("span", { text: "Result" }),
    ]),
    callRows,
  ]);

  const card = panel(
    "Agents",
    "what agents may do with seatswap",
    [
      section("What agents may do"),
      setting(
        "Agents may read usage",
        toggle("agentsRead", "Agents may read usage"),
        "status, list",
        "agentsRead"
      ),
      setting(
        "Agents may switch accounts",
        toggle("agentsSwitch", "Agents may switch accounts"),
        "use, next · every switch is notified",
        "agentsSwitch"
      ),
      note([
        "These two switches work through a marker that Claude Code puts on every command it runs. An agent runs as you. It can remove the marker, or change seatswap's files directly. The switches stop agents that follow the rules. They are not a lock. To block the command itself, add a deny rule for ",
        h("code", { text: "seatswap" }),
        " to Claude Code's permissions.",
      ]),
      section("Telling agents about seatswap"),
      skillRow,
      section("Last calls by agents"),
      calls,
      note([
        "A call counts as an agent's when ",
        h("code", { text: "CLAUDECODE=1" }),
        " or ",
        h("code", { text: "AI_AGENT" }),
        " is set.",
      ]),
    ],
    h("span", {
      class: "mf",
      text: "The skill goes to <claude config folder>/skills/seatswap/SKILL.md.",
    })
  );

  // Fill the two rows that need the app, after the card is in the page.
  loadSkill(skillControl, skillNote);
  loadCalls(callRows);
  return card;
}

function skillButton(label, force, control, noteNode) {
  const button = h("button", {
    type: "button",
    class: "hbtn xs pri",
    onclick: async () => {
      button.disabled = true;
      try {
        const status = await state.context.invoke("skill_install", { force });
        fieldError("skill", null);
        showSkill(control, noteNode, status);
      } catch (error) {
        fieldError("skill", errorMessage(error));
      }
      button.disabled = false;
    },
  }, label);
  return button;
}

function showSkill(control, noteNode, status) {
  clear(control);
  if (!status.installed) {
    control.appendChild(skillButton("Install", false, control, noteNode));
    noteNode.textContent = `not installed · goes to ${status.path}`;
    return;
  }
  control.appendChild(
    h("span", { class: status.differs ? "sw warn" : "sw ok", text: status.differs ? "Differs" : "Installed" })
  );
  if (status.differs) {
    control.appendChild(skillButton("Replace", true, control, noteNode));
    noteNode.textContent = `differs from the bundled skill · ${status.path}`;
  } else {
    noteNode.textContent = status.path;
  }
}

async function loadSkill(control, noteNode) {
  const token = ++state.skillToken;
  try {
    const status = await state.context.invoke("skill_status", {});
    if (token !== state.skillToken) return;
    showSkill(control, noteNode, status);
  } catch (error) {
    if (token !== state.skillToken) return;
    clear(control);
    control.appendChild(h("span", { class: "sw bad", text: "could not read" }));
    fieldError("skill", errorMessage(error));
  }
}

async function loadCalls(container) {
  const token = ++state.callsToken;
  try {
    const rows = await state.context.invoke("agent_calls", { limit: 20 });
    if (token !== state.callsToken) return;
    renderCalls(container, rows);
  } catch (error) {
    if (token !== state.callsToken) return;
    clear(container);
    container.appendChild(message(errorMessage(error)));
  }
}

async function renderCalls(container, rows) {
  clear(container);
  if (!Array.isArray(rows) || rows.length === 0) {
    container.appendChild(h("div", { class: "lr" }, [h("span", { text: "No calls yet." })]));
    return;
  }
  let whens = rows.map(() => "?");
  try {
    whens = await state.context.invoke("time_texts", {
      times: rows.map((row) => ({ at: row.at, style: "clock" })),
    });
    if (!Array.isArray(whens) || whens.length !== rows.length) {
      throw new Error("time_texts returned an unexpected reply");
    }
  } catch (error) {
    console.error(`agent call times: ${errorMessage(error)}`);
  }
  for (let index = 0; index < rows.length; index += 1) {
    const row = rows[index];
    container.appendChild(
      h(
        "div",
        {
          class: "lr",
          style: { gridTemplateColumns: "96px minmax(0, 1fr) 170px 110px" },
        },
        [
          h("span", { class: "mf", text: whens[index] }),
          h("span", { class: "mono x11", text: `seatswap ${row.command}` }),
          h("span", { class: "mf", text: row.cwd || "—" }),
          h("span", {}, [
            h("span", {
              class: row.allowed ? "sw ok" : "sw bad",
              text: row.allowed ? "answered" : "refused",
            }),
          ]),
        ]
      )
    );
  }
}
