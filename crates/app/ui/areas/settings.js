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
  // The money view from the look-up thread, the price list under API prices,
  // and the request tokens that keep a slow reply from overwriting a newer one.
  money: null,
  moneyError: null,
  moneyToken: 0,
  moneyListening: false,
  moneyOpen: false,
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

export async function render(container, context) {
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
  listenMoney(context);
  await loadMoney(context);

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
  const noteNode = note
    ? typeof note === "string"
      ? h("span", { class: "mf" }, note)
      : note
    : null;
  const wrapper = h(
    "div",
    { class: "set-row", dataset: field ? { field } : {} },
    [
      h("div", { class: "set" }, [
        h("span", {}, label),
        h("span", {}, [control, noteNode]),
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
// Money (spec §1.4, §1.4.1; lead decisions 3, 4 and 6)
// ---------------------------------------------------------------------------

async function loadMoney(context) {
  const token = ++state.moneyToken;
  try {
    const view = await context.invoke("money_view", {});
    if (token !== state.moneyToken) return;
    if (isObject(view)) {
      state.money = view;
      state.moneyError = null;
    } else {
      state.moneyError = "money_view returned an unexpected reply";
    }
  } catch (error) {
    if (token !== state.moneyToken) return;
    state.moneyError = errorMessage(error);
  }
}

function listenMoney(context) {
  if (state.moneyListening) return;
  state.moneyListening = true;
  Promise.resolve(
    context.listen("money", (view) => {
      if (!isObject(view)) return;
      state.money = view;
      state.moneyError = null;
      renderMoney();
    })
  ).catch((error) => {
    state.moneyListening = false;
    console.error(`money event: ${errorMessage(error)}`);
  });
}

// A change of the look-up thread's view redraws the General card, unless the
// person is typing in a field right now.
function renderMoney() {
  if (state.group !== "general" || !state.right) return;
  const active = document.activeElement;
  if (active && (active.tagName === "INPUT" || active.tagName === "SELECT")) return;
  renderGroup(state.right, "general");
}

// After a save of currency, usdRate or lookUpPrices: one round now, so the rate
// and the list are fresh at once.
async function refreshMoney() {
  if (!state.context) return;
  const token = ++state.moneyToken;
  try {
    const view = await state.context.invoke("lookup_now", {});
    if (token !== state.moneyToken) return;
    if (isObject(view)) {
      state.money = view;
      state.moneyError = null;
    } else {
      state.moneyError = "lookup_now returned an unexpected reply";
    }
  } catch (error) {
    if (token !== state.moneyToken) return;
    state.moneyError = errorMessage(error);
  }
  if (state.group === "general" && state.right) renderGroup(state.right, "general");
}

// After an own price: read the view again, but do not ask the internet.
async function reloadMoney() {
  if (!state.context) return;
  await loadMoney(state.context);
  if (state.group === "general" && state.right) renderGroup(state.right, "general");
}

// The page's times all come from `time_texts`; the Rust side formats them.
async function fillTimeNodes(requests) {
  if (requests.length === 0 || !state.context) return;
  let texts;
  try {
    texts = await state.context.invoke("time_texts", {
      times: requests.map((request) => ({ at: request.at, style: request.style })),
    });
    if (!Array.isArray(texts) || texts.length !== requests.length) {
      throw new Error("time_texts returned an unexpected reply");
    }
  } catch (error) {
    console.error(`money times: ${errorMessage(error)}`);
    texts = requests.map((request) => request.fallback || "");
  }
  for (let index = 0; index < requests.length; index += 1) {
    const request = requests[index];
    const text = `${request.prefix || ""}${texts[index] || ""}${request.suffix || ""}`;
    if (request.attribute) request.node.setAttribute(request.attribute, text);
    else request.node.textContent = text;
  }
}

function addNotePart(host, node) {
  if (host.childNodes.length > 0) host.appendChild(document.createTextNode(" · "));
  host.appendChild(node);
}

const STANDARD_CURRENCIES = ["EUR", "USD", "CHF", "GBP"];

function currencyRow() {
  const stored = savedValue("currency");
  const code = typeof stored === "string" && stored !== "" ? stored : null;
  const known = code !== null && STANDARD_CURRENCIES.includes(code);
  let custom;
  let customWrap;

  const reset = () => {
    const current = savedValue("currency");
    const currentCode =
      typeof current === "string" && current !== "" ? current : null;
    select.value = STANDARD_CURRENCIES.includes(currentCode)
      ? currentCode
      : currentCode
        ? "other"
        : "";
    customWrap.hidden = Boolean(currentCode) && STANDARD_CURRENCIES.includes(currentCode);
    custom.value = currentCode && !STANDARD_CURRENCIES.includes(currentCode) ? currentCode : "";
  };

  const select = selectInput(
    [
      ["", "choose…"],
      ...STANDARD_CURRENCIES.map((currency) => [currency, currency]),
      ["other", "other…"],
    ],
    known ? code : code ? "other" : "",
    async (next) => {
      if (next === "other") {
        customWrap.hidden = false;
        custom.focus();
        return;
      }
      select.disabled = true;
      const ok = await saveField("currency", next === "" ? null : next);
      select.disabled = false;
      if (ok) {
        await refreshMoney();
      } else {
        reset();
      }
    }
  );
  select.setAttribute("aria-label", "Currency");

  custom = textInput({
    value: code && !known ? code : "",
    placeholder: "EUR",
    maxlength: "3",
    class: "inp tall money-other-input",
    "aria-label": "Currency code",
  });
  custom.addEventListener("change", async () => {
    const raw = custom.value.trim();
    const next = raw === "" ? null : raw;
    const ok = await saveField("currency", next);
    if (!ok) {
      custom.value = code || "";
      return;
    }
    await refreshMoney();
  });
  customWrap = h("span", { class: "money-other", hidden: known || !code }, [custom]);
  return setting(
    "Currency",
    h("span", { class: "money-currency" }, [select, customWrap]),
    "for prices and for the value of tokens",
    "currency"
  );
}

function lookUpRow() {
  const control = switchButton(
    Boolean(savedValue("lookUpPrices")),
    "Look up prices and the dollar rate",
    async (next) => {
      control.disabled = true;
      const ok = await saveField("lookUpPrices", next);
      if (!ok) {
        control.setAttribute(
          "aria-checked",
          savedValue("lookUpPrices") ? "true" : "false"
        );
        control.disabled = false;
        return;
      }
      await refreshMoney();
      control.disabled = false;
    }
  );
  return setting(
    "Look up prices and the dollar rate",
    control,
    "prices hourly from models.dev · rate daily from the European Central Bank · nothing about you is sent",
    "lookUpPrices"
  );
}

function rateText(view, code) {
  if (!code) return "set a currency first";
  if (!view || !isObject(view.rate)) return "not looked up yet";
  const value = Number(view.rate.value);
  if (!Number.isFinite(value)) return "not looked up yet";
  return `${value.toFixed(4)} ${code}`;
}

function rateRow(times) {
  const code = currencyCode();
  const view = isObject(state.money) ? state.money : null;
  const control = h("span", { class: "money-rate", text: rateText(view, code) });
  let note = h("span", { class: "mf" });
  if (code && view && isObject(view.rate)) {
    if (view.rate.own) {
      note.textContent = "your own rate";
    } else if (typeof view.rate.date === "string" && view.rate.date !== "") {
      const node = h("span");
      note.appendChild(node);
      times.push({
        node,
        style: "date",
        at: `${view.rate.date}T00:00:00Z`,
        prefix: "looked up ",
        fallback: view.rate.date,
      });
    }
  }
  return setting("1 US dollar is", control, note, "rate");
}

function ownRateRow() {
  const code = currencyCode();
  const stored = savedValue("usdRate");
  const input = h("input", {
    type: "number",
    class: "inp num",
    value: typeof stored === "number" ? String(stored) : "",
    min: "0",
    step: "0.0001",
    "aria-label": "Use my own rate",
  });
  const reset = () => {
    const value = savedValue("usdRate");
    input.value = typeof value === "number" ? String(value) : "";
  };
  input.addEventListener("change", async () => {
    const raw = input.value.trim();
    let next = null;
    if (raw !== "") {
      next = Number(raw);
      if (!Number.isFinite(next) || next <= 0) {
        fieldError("usdRate", "The rate must be a number above 0.");
        reset();
        return;
      }
    }
    const ok = await saveField("usdRate", next);
    if (!ok) {
      reset();
      return;
    }
    await refreshMoney();
  });
  const control = h("span", { class: "number-control" }, [
    input,
    code ? h("span", { class: "mf", text: `${code} per USD` }) : null,
  ]);
  return setting(
    "Use my own rate",
    control,
    "empty: the looked-up rate is used",
    "usdRate"
  );
}

function countText(count, singular, plural) {
  return `${count} ${count === 1 ? singular : plural}`;
}

function apiPricesRow(times) {
  const view = isObject(state.money) ? state.money : null;
  const count =
    view && Number.isFinite(view.modelsWithPrice)
      ? Math.max(0, Math.round(view.modelsWithPrice))
      : null;
  const text =
    count === null
      ? state.moneyError
        ? "prices: not read"
        : "no prices yet"
      : `${count} models with a price`;
  const show = h("button", {
    type: "button",
    class: "hbtn xs ghost",
    text: state.moneyOpen ? "Hide" : "Show…",
    "aria-expanded": state.moneyOpen ? "true" : "false",
  });
  show.addEventListener("click", () => {
    state.moneyOpen = !state.moneyOpen;
    renderGroup(state.right, "general");
  });
  const control = h("span", { class: "money-prices-head" }, [
    h("span", { text }),
    show,
  ]);

  const note = h("span", { class: "mf money-api-note" });
  if (view) {
    if (view.pricesCheckedAt) {
      const node = h("span");
      addNotePart(note, node);
      times.push({
        node,
        style: "clock",
        at: view.pricesCheckedAt,
        prefix: "checked ",
        fallback: "—",
      });
    }
    if (isObject(view.lastChange) && view.lastChange.at) {
      const added = Array.isArray(view.lastChange.added) ? view.lastChange.added.length : 0;
      const changed = Array.isArray(view.lastChange.changed)
        ? view.lastChange.changed.length
        : 0;
      const parts = [];
      if (added > 0) parts.push(countText(added, "new model", "new models"));
      if (changed > 0) parts.push(countText(changed, "price", "prices"));
      const node = h("span");
      addNotePart(note, node);
      times.push({
        node,
        style: "day",
        at: view.lastChange.at,
        prefix: "last change ",
        suffix: parts.length > 0 ? `: ${parts.join(", ")}` : "",
        fallback: "—",
      });
    }
  }
  if (note.childNodes.length === 0) note.textContent = "not looked up yet";

  const row = setting("API prices", control, note, "prices");
  const error = row.querySelector(".row-error");
  const problem =
    view && typeof view.lastError === "string" && view.lastError !== ""
      ? view.lastError
      : state.moneyError;
  if (problem) {
    error.hidden = false;
    error.appendChild(document.createTextNode(problem));
    if (view && view.pricesCheckedAt) {
      error.appendChild(document.createTextNode(" · the stored list is from "));
      const day = h("span");
      error.appendChild(day);
      times.push({
        node: day,
        style: "day",
        at: view.pricesCheckedAt,
        fallback: "—",
      });
    } else {
      error.appendChild(document.createTextNode(" · nothing is stored yet"));
    }
  }
  return row;
}

async function loadPriceList(container) {
  clear(container);
  container.appendChild(h("div", { class: "lr mf", text: "loading the price list…" }));
  let rows;
  try {
    rows = await state.context.invoke("price_list", {});
  } catch (error) {
    clear(container);
    container.appendChild(
      h("div", { class: "row-error price-list-error", text: errorMessage(error) })
    );
    return;
  }
  clear(container);
  if (!Array.isArray(rows) || rows.length === 0) {
    container.appendChild(
      h("div", {
        class: "lr mf",
        text: "no prices yet: run a look-up with the switch on, or enter an own price below",
      })
    );
    return;
  }
  container.appendChild(
    h("div", { class: "lr hd price-grid" }, [
      h("span", { text: "Model" }),
      h("span", { text: "Input" }),
      h("span", { text: "Output" }),
      h("span", { text: "Cache write" }),
      h("span", { text: "Cache read" }),
      h("span", { text: "" }),
    ])
  );
  const times = [];
  for (const row of rows) container.appendChild(priceRow(row, times));
  container.appendChild(
    h("div", {
      class: "note price-list-note",
      text: "USD per million tokens. A field saves that model's own price; a tiered model uses its base price.",
    })
  );
  await fillTimeNodes(times);
}

function priceInputValue(row, property) {
  const value = row.price && Number(row.price[property]);
  return Number.isFinite(value) ? String(value) : "0";
}

function resetPriceInputs(row, inputs) {
  for (const [property, input] of Object.entries(inputs)) {
    input.value = priceInputValue(row, property);
  }
}

async function savePrice(row, inputs, labels) {
  const field = `price:${row.key}`;
  const price = { tiered: Boolean(row.tiered) };
  for (const [property, input] of Object.entries(inputs)) {
    const raw = input.value.trim();
    const value = Number(raw);
    if (raw === "" || !Number.isFinite(value) || value < 0) {
      fieldError(
        field,
        `The ${labels[property]} price must be a number of 0 or more.`
      );
      resetPriceInputs(row, inputs);
      return;
    }
    price[property] = value;
  }
  try {
    await state.context.invoke("set_price_override", { key: row.key, price });
    row.price = price;
    row.own = true;
    await reloadMoney();
  } catch (error) {
    fieldError(field, errorMessage(error));
    resetPriceInputs(row, inputs);
  }
}

function priceRow(row, times) {
  const field = `price:${row.key}`;
  const wrap = h("div", { class: "price-wrap", dataset: { field } });
  if (!row.own && typeof row.since === "string" && row.since !== "") {
    times.push({
      node: wrap,
      style: "day",
      at: row.since,
      prefix: "since ",
      attribute: "title",
      fallback: row.since,
    });
  }
  const grid = h("div", { class: "lr price-grid" });
  grid.appendChild(
    h("span", { class: "mono x11", title: row.key, text: row.key })
  );
  const inputs = {};
  const labels = {
    input: "input",
    output: "output",
    cacheWrite: "cache write",
    cacheRead: "cache read",
  };
  for (const [property, label] of Object.entries(labels)) {
    const input = h("input", {
      type: "number",
      class: "inp num price-field",
      value: priceInputValue(row, property),
      min: "0",
      step: "any",
      "aria-label": `${label} price for ${row.key}`,
    });
    input.addEventListener("change", () => savePrice(row, inputs, labels));
    inputs[property] = input;
    grid.appendChild(h("span", { class: "price-value" }, [input]));
  }
  const marks = h("span", { class: "price-marks" });
  if (row.tiered) marks.appendChild(h("span", { class: "sw warn", text: "tiered" }));
  if (row.own) marks.appendChild(h("span", { class: "sw ok", text: "own" }));
  grid.appendChild(marks);
  wrap.appendChild(grid);
  if (row.own) {
    const back = h("button", {
      type: "button",
      class: "hbtn xs ghost",
      text: "back to the looked-up price",
    });
    back.addEventListener("click", async () => {
      back.disabled = true;
      try {
        await state.context.invoke("set_price_override", {
          key: row.key,
          price: null,
        });
        await reloadMoney();
      } catch (error) {
        fieldError(field, errorMessage(error));
        back.disabled = false;
      }
    });
    wrap.appendChild(h("div", { class: "price-own-line" }, [back]));
  }
  wrap.appendChild(h("div", { class: "row-error", hidden: true }));
  return wrap;
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
  const card = panel("General", "", []);
  const body = card.querySelector(".settings-body");
  const times = [];

  // Money is the first section, as in the mock-up.
  body.appendChild(section("Money"));
  body.appendChild(currencyRow());
  body.appendChild(lookUpRow());
  body.appendChild(rateRow(times));
  body.appendChild(ownRateRow());
  body.appendChild(apiPricesRow(times));
  if (state.moneyOpen) {
    const list = h("div", { class: "price-list" });
    body.appendChild(list);
    loadPriceList(list);
  }

  body.appendChild(section("Look"));
  body.appendChild(setting("Theme", themeControl(savedValue("theme") || "system"), "", "theme"));
  body.appendChild(
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
    )
  );
  body.appendChild(
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
    )
  );
  body.appendChild(section("Start and polling"));
  body.appendChild(
    setting("Start at login", toggle("startAtLogin", "Start at login"), "", "startAtLogin")
  );
  body.appendChild(
    setting(
      "Read the active account every",
      numberField("pollActiveSecs", "Read the active account every", {
        min: 30,
        step: 30,
        unit: "s",
      }),
      "every 30 s from 90 %",
      "pollActiveSecs"
    )
  );
  body.appendChild(
    setting(
      "Read inactive accounts every",
      numberField("pollInactiveSecs", "Read inactive accounts every", {
        min: 30,
        step: 30,
        unit: "s",
      }),
      "",
      "pollInactiveSecs"
    )
  );
  body.appendChild(section("Claude Code"));
  body.appendChild(
    setting(
      "Claude Code's config folder",
      textField("claudeConfigDir", "Claude Code's config folder", "/home/you/.claude"),
      "",
      "claudeConfigDir"
    )
  );
  body.appendChild(
    setting(
      "Read Claude Code's session files",
      toggle("readSessions", "Read Claude Code's session files"),
      "model, busy or idle, tokens per folder · never message text",
      "readSessions"
    )
  );
  body.appendChild(
    setting(
      "Copy the live login at once",
      toggle("mirrorLive", "Copy the live login at once"),
      "",
      "mirrorLive"
    )
  );
  body.appendChild(
    setting(
      "Refresh inactive logins",
      toggle("refreshInactive", "Refresh inactive logins"),
      "needed to read their usage",
      "refreshInactive"
    )
  );

  fillTimeNodes(times);
  return card;
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
