// The Settings area of the large window (spec §3.7): eight groups on the left,
// the chosen group's card on the right. Every control saves one field at once
// through `save_setting`; a refused save shows its error under the row and puts
// the control back to the saved value. A snapshot updates the open group in
// place from the stored views; it never re-reads them. Plain ES2020, no
// framework.

import {
  abbrLetterBoxes,
  applyTheme,
  clear,
  colorSwatches,
  contrastText,
  errorMessage,
  focusInside,
  h,
  hexField,
  iconPreview,
  isColor,
  isObject,
  message,
  panel,
  selectInput,
  shortPercent,
  switchControl,
  textInput,
} from "../common.js";

const GROUPS = [
  { id: "general", label: "General" },
  { id: "accounts", label: "Accounts" },
  { id: "auto-burn", label: "Auto-burn" },
  { id: "notifications", label: "Notifications" },
  { id: "projects", label: "Projects" },
  { id: "agents", label: "Agents" },
  { id: "providers", label: "Providers" },
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
  providers: "providers",
  prov: "providers",
  history: "history",
  hist: "history",
};

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
  // The account ids the last snapshot held: only a change in this list makes
  // the Accounts group read the projects again.
  accountIds: null,
  // The agents' skill and calls, read when the Agents group opens.
  skill: null,
  skillError: null,
  skillLoaded: false,
  skillToken: 0,
  calls: null,
  callsError: null,
  callsLoaded: false,
  callsToken: 0,
  // The money view from the look-up thread, the price list under API prices,
  // and the request tokens that keep a slow reply from overwriting a newer one.
  money: null,
  moneyError: null,
  moneyLoaded: false,
  moneyToken: 0,
  moneyListening: false,
  moneyOpen: false,
  priceRows: null,
  priceError: null,
  priceLoaded: false,
  priceToken: 0,
  // Saves run one after another, each built from the settings as the one
  // before left them, so a quick second change never carries a stale copy.
  saving: Promise.resolve(),
  pendingSaves: 0,
  // The resolved projects and organizations (spec §3.6), and the request token
  // that keeps a slow reply from overwriting a newer one.
  projects: null,
  projectsError: null,
  projectsLoaded: false,
  projectsToken: 0,
  // The host folders of the history (spec §1.5), the view loaded when the
  // group is opened, and what the export and import rows are holding.
  history: null,
  historyError: null,
  historyLoaded: false,
  historyToken: 0,
  exportPath: "",
  exportResult: "",
  importFolder: "",
  importResult: "",
  importConflicts: [],
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

// ---------------------------------------------------------------------------
// the page and its eight groups
// ---------------------------------------------------------------------------

// The `accounts` list of the real snapshot, which always has it. A snapshot
// without it is an error the page shows; it is never read as an empty list.
function accountsOf(snapshot) {
  if (!Array.isArray(snapshot.accounts)) {
    throw new Error("the snapshot has no accounts list");
  }
  return snapshot.accounts;
}

// The account ids a snapshot holds, as one comparable text: only a changed
// list makes the Accounts group read the projects again.
function accountIdsOf(snapshot) {
  return accountsOf(snapshot)
    .map((row) => (isObject(row) && typeof row.id === "string" ? row.id : ""))
    .filter((id) => id !== "")
    .sort()
    .join("\u0000");
}

// Open a group: replace the old content only once the views it needs are
// stored, so the old group stays visible while it loads.
export async function render(container, context) {
  const snapshot = isObject(context.snapshot) ? context.snapshot : {};
  const parsed = groupFrom(context.sub);
  const group = parsed.group;
  const groupChanged = group !== state.group;
  const sameTarget = state.sub === context.sub;
  const openRequest = context.fromOpen === true;
  state.sub = context.sub;
  if (parsed.item && (!sameTarget || openRequest)) {
    state.selectedId = parsed.item;
    state.openEditorId = parsed.item;
  }
  if (focusInside(state.container) && group === state.group && state.container === container && !openRequest) {
    // Keep the control (and what was typed) until it is saved on change.
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
  state.accountIds = accountIdsOf(snapshot);
  listenMoney(context);
  if (group === "general" && (groupChanged || !state.moneyLoaded)) {
    await loadMoney(context);
  }
  if (group === "general" && state.moneyOpen && !state.priceLoaded) {
    await loadPriceList();
  }
  if ((group === "accounts" || group === "projects") && (groupChanged || !state.projectsLoaded)) {
    await loadProjects();
  }
  // The hosts table is read when the group opens; a snapshot in between
  // redraws it from the stored view.
  if (group === "history" && (groupChanged || !state.historyLoaded)) await loadHistory();
  if (group === "agents") {
    if (groupChanged || !state.skillLoaded) await loadSkillView();
    if (groupChanged || !state.callsLoaded) await loadCallsView();
  }

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

// A snapshot: store it, then redraw the open group from the stored views. No
// command is sent for a redraw. While a field has focus nothing is redrawn;
// the next snapshot after it lost focus draws. The only view read here is the
// projects view, and only when the snapshot's account list changed.
export async function update(snapshot) {
  const next = isObject(snapshot) ? snapshot : {};
  state.snapshot = next;
  takeSettings(next);
  if (!state.container || !state.right) return;
  if (focusInside(state.container)) return;
  const group = state.group;
  const right = state.right;
  const ids = accountIdsOf(next);
  const accountsChanged = state.accountIds !== null && ids !== state.accountIds;
  state.accountIds = ids;
  if (accountsChanged && group === "accounts") await loadProjects();
  if (state.right !== right || state.group !== group || focusInside(state.container)) return;
  renderGroup(right, group);
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
  if (id === "general") right.appendChild(generalCard());
  else if (id === "accounts") right.appendChild(accountsCard());
  else if (id === "auto-burn") right.appendChild(autoBurnCard());
  else if (id === "notifications") right.appendChild(notificationsCard());
  else if (id === "projects") right.appendChild(projectsCard());
  else if (id === "providers") right.appendChild(providersCard());
  else if (id === "history") right.appendChild(historyCard());
  else right.appendChild(agentsCard());
}

function section(text) {
  return h("div", { class: "sh", text });
}

function setting(label, control, note, field) {
  const text = field ? state.errors[field] || "" : "";
  const error = h("div", { class: "row-error", hidden: text === "", text });
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
// saving: one control, one field, no Save button
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
  try {
    const saved = await state.context.invoke("save_setting", { field, value });
    if (!isObject(saved) || !isObject(saved.settings)) {
      throw new Error("save_setting returned an unexpected reply");
    }
    state.settings = saved.settings;
    if (state.snapshot) state.snapshot.settings = saved.settings;
    // A warning is not a refused save: the settings are saved. It stays under
    // the row like an error, and the next successful save of the field clears
    // it.
    const warning =
      typeof saved.warning === "string" && saved.warning !== "" ? saved.warning : null;
    fieldError(field, warning);
    return true;
  } catch (error) {
    fieldError(field, errorMessage(error));
    return false;
  }
}

function savedValue(field) {
  return state.settings ? state.settings[field] : undefined;
}

// The mock-up gives every select the same 170 px width; the two organization
// selects and "For sessions on" are 130 px, and the currency is 110 px.
function selectWidth(select, width = 170) {
  select.style.width = `${width}px`;
  select.style.flex = "0 0 auto";
  return select;
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
  return selectWidth(select);
}

function selectSetting(field, options, label, width) {
  const select = selectInput(options, savedValue(field), async (next) => {
    select.disabled = true;
    await saveField(field, next);
    select.value = String(savedValue(field));
    select.disabled = false;
  });
  if (label) select.setAttribute("aria-label", label);
  return selectWidth(select, width);
}

function toggle(field, label, words = true) {
  const control = switchControl(
    Boolean(savedValue(field)),
    label,
    async (next) => {
      control.button.disabled = true;
      const ok = await saveField(field, next);
      if (!ok) control.set(Boolean(savedValue(field)));
      control.button.disabled = false;
    },
    { words }
  );
  return control.node;
}

// What a number field holds, for the Rust side to check: its number when the
// text is a number, the text itself when it is not, and null when it is empty.
function typedNumber(raw) {
  const text = String(raw).trim();
  if (text === "") return null;
  const value = Number(text);
  return Number.isFinite(value) ? value : text;
}

function numberField(field, label, options) {
  const value = savedValue(field);
  const input = h("input", {
    type: "number",
    class: "inp num",
    value: value === null || value === undefined ? "" : String(value),
    step: String(options.step || 1),
    "aria-label": label,
  });
  const save = async () => {
    await saveField(field, typedNumber(input.value));
    const saved = savedValue(field);
    input.value = saved === null || saved === undefined ? "" : String(saved);
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

const RESTART_NOTE = "takes effect after the tray app restarts";

// The one text-setting field. `options` holds `placeholder`, `note` (shown
// under the row), `restart` (true when the saved value differs from the one in
// effect: the restart note then replaces `note`) and `width`. Every text
// setting saves the same way; a history field reads its view again afterwards.
function textField(field, label, options) {
  const settings = options || {};
  const value = savedValue(field);
  const input = textInput({
    value: value === null || value === undefined ? "" : String(value),
    placeholder: settings.placeholder,
    "aria-label": label,
  });
  input.className = "inp tall";
  if (settings.width) input.style.width = `${settings.width}px`;
  const noteNode = h("span", { class: "mf" });
  const showNote = () => {
    noteNode.textContent = settings.restart === true ? RESTART_NOTE : settings.note || "";
  };
  input.addEventListener("change", async () => {
    const raw = input.value.trim();
    const next = raw === "" ? null : raw;
    const ok = await saveField(field, next);
    if (!ok) {
      input.value = savedValue(field) || "";
      showNote();
      return;
    }
    input.value = next || "";
    if (settings.restart !== undefined) {
      // The saved value now differs from the one in effect; read the view again
      // so the note and the placeholders come from the command, not the page.
      await loadHistory();
      if (state.group === "history" && state.right) renderGroup(state.right, "history");
    }
  });
  showNote();
  return setting(label, input, noteNode, field);
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
  if (token === state.moneyToken) state.moneyLoaded = true;
}

function listenMoney(context) {
  if (state.moneyListening) return;
  state.moneyListening = true;
  Promise.resolve(
    context.listen("money", (view) => {
      if (!isObject(view)) return;
      state.money = view;
      state.moneyError = null;
      state.moneyLoaded = true;
      const draw = async () => {
        if (state.moneyOpen) {
          state.priceLoaded = false;
          await loadPriceList();
        }
        renderMoney();
      };
      draw().catch((error) => console.error(`money event: ${errorMessage(error)}`));
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
  if (focusInside(state.container)) return;
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
  if (token !== state.moneyToken) return;
  state.moneyLoaded = true;
  if (state.moneyOpen) {
    state.priceLoaded = false;
    await loadPriceList();
  }
  if (state.group === "general" && state.right && !focusInside(state.container)) {
    renderGroup(state.right, "general");
  }
}

// After an own price: read the view and the price list again, but do not ask
// the internet.
async function reloadMoney() {
  if (!state.context) return;
  await loadMoney(state.context);
  if (state.moneyOpen) {
    state.priceLoaded = false;
    await loadPriceList();
  }
  if (state.group === "general" && state.right && !focusInside(state.container)) {
    renderGroup(state.right, "general");
  }
}

// The page's times all come from `time_texts`; the Rust side formats them.
// A failed `time_texts` is shown once in the open group, not once for every
// node that got its "?".
function showTimeError(error) {
  const text = `The times could not be read: ${errorMessage(error)}`;
  console.warn(`[settings] ${text}`);
  const right = state.right;
  if (!right || right.querySelector("[data-time-error]")) return;
  const node = message(text, "error");
  node.dataset.timeError = "true";
  right.appendChild(node);
}

// The page's times all come from `time_texts`; the Rust side formats them. A
// failed request puts "?" into each node and says why once in the group.
async function fillTimeNodes(requests) {
  if (requests.length === 0 || !state.context) return;
  let texts = null;
  let error = null;
  try {
    texts = await state.context.invoke("time_texts", {
      times: requests.map((request) => ({ at: request.at, style: request.style })),
    });
    if (!Array.isArray(texts) || texts.length !== requests.length) {
      throw new Error("time_texts returned an unexpected reply");
    }
  } catch (problem) {
    error = problem;
  }
  for (let index = 0; index < requests.length; index += 1) {
    const request = requests[index];
    const value = error ? "?" : texts[index] || "";
    const text = `${request.prefix || ""}${value}${request.suffix || ""}`;
    if (request.attribute) request.node.setAttribute(request.attribute, text);
    else request.node.textContent = text;
  }
  if (error) showTimeError(error);
}

// The note rows lay their parts out as flex items; a bare " · " text node loses
// its outer spaces there. A span with `white-space: pre` keeps them.
function middotSeparator() {
  return h("span", { class: "middot-sep", text: " · " });
}

function addNotePart(host, node) {
  if (host.childNodes.length > 0) host.appendChild(middotSeparator());
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
  selectWidth(select, 110);

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
  const control = switchControl(
    Boolean(savedValue("lookUpPrices")),
    "Look up prices and the dollar rate",
    async (next) => {
      control.button.disabled = true;
      const ok = await saveField("lookUpPrices", next);
      if (!ok) {
        control.set(Boolean(savedValue("lookUpPrices")));
        control.button.disabled = false;
        return;
      }
      await refreshMoney();
      control.button.disabled = false;
    }
  );
  return setting(
    "Look up prices and the dollar rate",
    control.node,
    "prices hourly from models.dev · rate daily from the European Central Bank · nothing about you is sent",
    "lookUpPrices"
  );
}

function rateText(view, code) {
  if (!code) return "set a currency first";
  if (!view || !isObject(view.rate)) return "no rate yet: API values are shown in USD";
  const value = Number(view.rate.value);
  if (!Number.isFinite(value)) return "no rate yet: API values are shown in USD";
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
      });
    } else if (code === "USD") {
      note.textContent = "USD itself";
    }
  }
  const row = setting("1 US dollar is", control, note, "rate");
  const problem =
    view && typeof view.rateError === "string" && view.rateError !== ""
      ? view.rateError
      : null;
  if (problem) {
    const error = row.querySelector(".row-error");
    error.hidden = false;
    error.textContent = problem;
  }
  return row;
}

function ownRateRow() {
  const code = currencyCode();
  const stored = savedValue("usdRate");
  const own = isObject(stored) ? stored : null;
  const ownValue =
    own && typeof own.currency === "string" && own.currency === code
      ? own.value
      : null;
  const input = h("input", {
    type: "number",
    class: "inp num",
    value: typeof ownValue === "number" ? String(ownValue) : "",
    step: "0.0001",
    "aria-label": "Use my own rate",
    disabled: !code || code === "USD",
  });
  const reset = () => {
    const value = savedValue("usdRate");
    const current =
      isObject(value) &&
      typeof value.currency === "string" &&
      value.currency === code &&
      typeof value.value === "number"
        ? value.value
        : null;
    input.value = current === null ? "" : String(current);
  };
  input.addEventListener("change", async () => {
    if (!code || code === "USD") {
      reset();
      return;
    }
    const value = typedNumber(input.value);
    const next = value === null ? null : { currency: code, value };
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
  let note = "empty: the looked-up rate is used";
  if (!code) note = "set a currency first";
  else if (code === "USD") note = "USD needs no rate";
  else if (own && own.currency !== code) {
    note = `your rate ${own.value} was for ${own.currency} and is not used for ${code}`;
  }
  return setting("Use my own rate", control, note, "usdRate");
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
    class: "hbtn xs",
    text: state.moneyOpen ? "Hide" : "Show…",
    "aria-expanded": state.moneyOpen ? "true" : "false",
  });
  show.addEventListener("click", async () => {
    state.moneyOpen = !state.moneyOpen;
    if (state.moneyOpen && !state.priceLoaded) {
      // Show the old card at once, then replace the list when it is read.
      renderGroup(state.right, "general");
      await loadPriceList();
    }
    if (state.group === "general" && state.right) renderGroup(state.right, "general");
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
      });
    } else {
      error.appendChild(document.createTextNode(" · nothing is stored yet"));
    }
  }
  const leftOut =
    view && Array.isArray(view.leftOut)
      ? view.leftOut.filter((entry) => typeof entry === "string" && entry !== "")
      : [];
  if (leftOut.length > 0) {
    const line = h("div", {
      class: "lr mf price-left-out",
      text: `${countText(leftOut.length, "model", "models")} of models.dev's list ${leftOut.length === 1 ? "was" : "were"} left out`,
      title: leftOut.join("\n"),
    });
    row.appendChild(line);
  }
  return row;
}

// Read the price list once per opening or change; the card draws it from the
// stored rows, so a snapshot never asks for it again.
async function loadPriceList() {
  if (!state.context) return;
  const token = ++state.priceToken;
  try {
    const rows = await state.context.invoke("price_list", {});
    if (token !== state.priceToken) return;
    if (!Array.isArray(rows)) throw new Error("price_list returned an unexpected reply");
    state.priceRows = rows;
    state.priceError = null;
  } catch (error) {
    if (token !== state.priceToken) return;
    state.priceRows = null;
    state.priceError = errorMessage(error);
  }
  if (token === state.priceToken) state.priceLoaded = true;
}

function drawPriceList(container) {
  clear(container);
  if (state.priceError) {
    container.appendChild(
      h("div", { class: "row-error price-list-error", text: state.priceError })
    );
  } else if (!state.priceLoaded || state.priceRows === null) {
    container.appendChild(h("div", { class: "lr mf", text: "loading the price list…" }));
    return;
  } else {
    const rows = state.priceRows;
    if (rows.length === 0) {
      container.appendChild(
        h("div", {
          class: "lr mf",
          text: "no prices yet: run a look-up with the switch on, or add a model below",
        })
      );
    } else {
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
      fillTimeNodes(times);
    }
  }
  container.appendChild(addModelRow());
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

async function savePrice(row, inputs) {
  const field = `price:${row.key}`;
  const price = { tiered: Boolean(row.tiered) };
  for (const [property, input] of Object.entries(inputs)) {
    price[property] = typedNumber(input.value);
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
      step: "any",
      "aria-label": `${label} price for ${row.key}`,
    });
    input.addEventListener("change", () => savePrice(row, inputs));
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

// An own price for any model (spec §1.4.1), including one models.dev does not
// list: the key is `<provider>/<model id>`. The providers come from the money
// view (`money::PRICE_PROVIDERS`); the server refuses a bad key or price.
function addModelRow() {
  const field = "price:new";
  const money = isObject(state.money) ? state.money : null;
  const providers =
    money && Array.isArray(money.providers)
      ? money.providers.filter((name) => typeof name === "string" && name !== "")
      : [];
  const provider = selectInput(
    providers.map((name) => [name, name]),
    providers[0],
    () => {}
  );
  provider.setAttribute("aria-label", "Model provider");
  selectWidth(provider);
  const model = textInput({
    class: "inp tall price-add-model",
    placeholder: "claude-opus-5-5",
    "aria-label": "Model id",
  });
  const grid = h("div", { class: "lr price-grid price-add-grid" }, [
    h("span", { class: "mf", text: "add a model" }),
  ]);
  const labels = {
    input: "input",
    output: "output",
    cacheWrite: "cache write",
    cacheRead: "cache read",
  };
  const inputs = {};
  for (const [property, table] of Object.entries(labels)) {
    const input = h("input", {
      type: "number",
      class: "inp num price-field",
      value: "0",
      step: "any",
      "aria-label": `${table} price for the new model`,
    });
    inputs[property] = input;
    grid.appendChild(h("span", { class: "price-value" }, [input]));
  }
  const add = h("button", { type: "button", class: "hbtn xs", text: "Add" });
  grid.appendChild(h("span", { class: "price-marks" }, [add]));
  add.addEventListener("click", async () => {
    const modelId = model.value.trim();
    const price = { tiered: false };
    for (const [property, input] of Object.entries(inputs)) {
      price[property] = typedNumber(input.value);
    }
    const key = `${provider.value}/${modelId}`;
    add.disabled = true;
    try {
      await state.context.invoke("set_price_override", { key, price });
      fieldError(field, null);
      model.value = "";
      for (const input of Object.values(inputs)) input.value = "0";
      await reloadMoney();
    } catch (error) {
      fieldError(field, errorMessage(error));
    } finally {
      add.disabled = false;
    }
  });
  return h("div", { class: "price-add", dataset: { field } }, [
    h("div", { class: "price-add-key" }, [
      h("span", { class: "mf", text: "own price for" }),
      provider,
      h("span", { class: "mf", text: "/" }),
      model,
    ]),
    grid,
    h("div", { class: "row-error", hidden: true }),
  ]);
}

// ---------------------------------------------------------------------------
// General
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Accounts (spec §3.7): a row per account, a chip editor, and the price.
// ---------------------------------------------------------------------------

function accountRows() {
  return accountsOf(state.snapshot || {});
}

// The currency code saved under General: three capital letters, or null when
// none is set. Without one, a price says its currency is unknown and
// `amountText` writes "—" instead of a made-up code.
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
      h("b", { text: "Account and plan" }),
      " come from the login and cannot be changed here. ",
      h("b", { text: "Price" }),
      " is what you pay per month, in the currency set under General. It turns percent into money, so accounts can be added up. ",
      h("b", { text: "Auto-burn may use it:" }),
      " switch it off for an account that should be left alone.",
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

  const code = currencyCode();
  const priceCurrency = () =>
    typeof row.priceCurrency === "string" && row.priceCurrency !== ""
      ? row.priceCurrency
      : null;
  const currencyLabel = h("span", { class: "x11 mf" });
  const currencyMark = h("span", {
    class: "sw warn price-currency-mark",
    hidden: true,
  });
  const paintPriceCurrency = () => {
    currencyMark.hidden = true;
    currencyMark.title = "";
    currencyMark.setAttribute("aria-label", "");
    if (row.price === null || row.price === undefined) {
      if (code) {
        currencyLabel.textContent = code;
        currencyLabel.className = "x11 mf";
      } else {
        currencyLabel.textContent = "currency: set it under General";
        currencyLabel.className = "x11 mf currency-hint";
      }
      return;
    }
    const own = priceCurrency();
    if (own === null) {
      currencyLabel.textContent = "no currency";
      currencyLabel.className = "x11 mf";
      currencyMark.hidden = false;
      currencyMark.title =
        "the price was entered before a currency was set; enter it again";
      currencyMark.setAttribute("aria-label", currencyMark.title);
      return;
    }
    currencyLabel.textContent = own;
    currencyLabel.className = "x11 mf";
    if (code && own !== code) {
      currencyMark.hidden = false;
      currencyMark.title = `in ${own}: enter it again for ${code}`;
      currencyMark.setAttribute("aria-label", currencyMark.title);
    }
  };

  const priceInput = textInput({
    class: "inp tall acc-price",
    value: row.price === null || row.price === undefined ? "" : String(row.price),
    inputmode: "decimal",
    "aria-label": "Price per month",
  });
  const storedPrice = () =>
    row.price === null || row.price === undefined ? "" : String(row.price);
  priceInput.addEventListener("change", async () => {
    const amount = typedNumber(priceInput.value);
    const sameCurrency =
      amount === null || (code !== null && priceCurrency() === code);
    if (amount === row.price && sameCurrency) {
      priceInput.value = storedPrice();
      return;
    }
    const ok = await saveAccount(row, { price: amount }, field);
    if (ok) {
      row.price = amount;
      row.priceCurrency = amount === null ? null : code;
      priceInput.value = storedPrice();
      paintPriceCurrency();
    } else {
      priceInput.value = storedPrice();
    }
  });

  paintPriceCurrency();
  const priceCell = h("div", { class: "price-cell" }, [
    priceInput,
    currencyLabel,
    currencyMark,
  ]);

  const auto = switchControl(
    row.autoBurn === true,
    `Auto-burn may use ${abbr}`,
    async (next) => {
      const ok = await saveAccount(row, { autoBurn: next }, field);
      if (ok) row.autoBurn = next;
      else auto.set(row.autoBurn === true);
    },
    { words: false }
  );

  wrap.appendChild(
    h("div", { class: "lr acc-grid" }, [
      chip,
      labelInput,
      h("span", { class: "acc-text", title: accountEmail(row), text: accountEmail(row) }),
      h("span", { class: "acc-text", title: accountPlan(row), text: accountPlan(row) }),
      priceCell,
      h("div", { class: "acc-switch" }, [auto.node]),
      accountOrgCell(row, field),
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

// ---------------------------------------------------------------------------
// Projects and organizations (spec §3.6): names, joins, organizations — the
// same on every host. A change appends one line to the shared project log.
// ---------------------------------------------------------------------------

// Every replacement of state.projects bumps the token, so a `loadProjects`
// reply that started before it cannot overwrite the newer view.
function setProjects(view, error) {
  state.projectsToken += 1;
  state.projects = view;
  state.projectsError = error;
}

async function loadProjects() {
  if (!state.context) return;
  const token = ++state.projectsToken;
  try {
    const view = await state.context.invoke("projects_view", {});
    if (token !== state.projectsToken) return;
    if (!isObject(view)) throw new Error("projects_view returned an unexpected reply");
    setProjects(view, null);
  } catch (error) {
    if (token !== state.projectsToken) return;
    setProjects(null, errorMessage(error));
  }
  if (token === state.projectsToken) state.projectsLoaded = true;
}

function projectKeyText(key) {
  if (!isObject(key) || typeof key.id !== "string") return "?";
  return typeof key.host === "string" && key.host !== ""
    ? `${key.kind}:${key.id}@${key.host}`
    : `${key.kind}:${key.id}`;
}

// A project's name in a list. A folder project's id has a host, so the same
// folder on two hosts can be told apart: "~ (home folder) · atlas".
function projectLabel(name, key) {
  const text = typeof name === "string" && name !== "" ? name : "?";
  const host =
    isObject(key) && typeof key.host === "string" && key.host !== "" ? key.host : null;
  return host ? `${text} · ${host}` : text;
}

function sameRef(first, second) {
  return Boolean(first && second && first.acc === second.acc && first.org === second.org);
}

function accountRefOf(projects, id) {
  if (!projects || !Array.isArray(projects.accountRefs)) return null;
  const entry = projects.accountRefs.find((item) => item.id === id);
  return entry ? entry.account : null;
}

function orgOfAccount(projects, id) {
  const ref = accountRefOf(projects, id);
  if (!ref) return null;
  const orgs = Array.isArray(projects.orgs) ? projects.orgs : [];
  return orgs.find((org) => (org.accounts || []).some((account) => sameRef(account, ref))) || null;
}

// One project edit through the window's command, queued with the settings
// saves so two quick changes keep their order. The reply is the whole new view.
function saveProject(edit, field) {
  return queuedSave(async () => {
    try {
      const view = await state.context.invoke("projects_edit", { edit });
      if (!isObject(view)) throw new Error("projects_edit returned an unexpected reply");
      setProjects(view, null);
      fieldError(field, null);
      if (state.right && (state.group === "projects" || state.group === "accounts")) {
        renderGroup(state.right, state.group);
      }
      return true;
    } catch (error) {
      fieldError(field, errorMessage(error));
      return false;
    }
  });
}

// The Accounts group's organization column: the organizations plus "none".
// One change is one edit for this account: it moves only this account.
function accountOrgCell(row, field) {
  const projects = isObject(state.projects) ? state.projects : null;
  if (!projects) {
    return h("span", {
      class: "acc-text",
      text: "not read",
      title: state.projectsError || "The organizations are read from the history.",
    });
  }
  const current = orgOfAccount(projects, row.id);
  const options = [["", "none"], ...projects.orgs.map((org) => [org.id, org.name])];
  const select = selectInput(options, current ? current.id : "", async (next) => {
    select.disabled = true;
    const ok = await saveAccountOrg(row, next, field);
    if (ok) {
      renderGroup(state.right, "accounts");
      return;
    }
    select.disabled = false;
    const back = orgOfAccount(state.projects, row.id);
    select.value = back ? back.id : "";
  });
  select.setAttribute("aria-label", `Organization of ${row.abbr || "the account"}`);
  return selectWidth(select, 130);
}

function saveAccountOrg(row, orgId, field) {
  return queuedSave(async () => {
    const projects = isObject(state.projects) ? state.projects : null;
    if (!projects) {
      fieldError(field, "The organizations could not be read; nothing is saved.");
      return false;
    }
    const ref = accountRefOf(projects, row.id);
    if (!ref) {
      fieldError(field, "The account is not in the stored list; nothing is saved.");
      return false;
    }
    const current = orgOfAccount(projects, row.id);
    const to = orgId === "" ? null : projects.orgs.find((org) => org.id === orgId) || null;
    if (orgId !== "" && !to) {
      fieldError(field, `unknown organization ${orgId}`);
      return false;
    }
    if ((current ? current.id : "") === (to ? to.id : "")) {
      fieldError(field, null);
      return true;
    }
    // One edit names this one account, so no other account moves.
    const edit = { edit: "accountOrg", account: ref, org: to ? to.id : null };
    try {
      const view = await state.context.invoke("projects_edit", { edit });
      if (!isObject(view)) throw new Error("projects_edit returned an unexpected reply");
      setProjects(view, null);
      fieldError(field, null);
      return true;
    } catch (error) {
      fieldError(field, errorMessage(error));
      await loadProjects();
      return false;
    }
  });
}

function foldersText(folders) {
  if (!isObject(folders)) return "—";
  const parts = [];
  for (const host of Object.keys(folders).sort()) {
    const list = Array.isArray(folders[host]) ? folders[host] : [];
    parts.push(`${host}: ${list.join(", ")}`);
  }
  return parts.length > 0 ? parts.join(" · ") : "—";
}

function projectsCard() {
  const card = panel(
    "Projects",
    "a project is a repository; an organization is several projects · the same on all your hosts",
    [],
    [
      h("span", {
        class: "mf",
        text: "Changes apply to the past too: the history stores the repository, not the name.",
      }),
    ]
  );
  const body = card.querySelector(".settings-body");
  const projects = isObject(state.projects) ? state.projects : null;
  if (!projects) {
    body.appendChild(message(state.projectsError || "The projects could not be read."));
    return card;
  }
  body.appendChild(
    h("div", { class: "lr hd proj-grid" }, [
      h("span", { text: "Project" }),
      h("span", { text: "Repository" }),
      h("span", { text: "Folders, per host" }),
      h("span", { text: "Organization" }),
      h("span", { class: "n", text: "Sessions" }),
    ])
  );
  if (projects.projects.length === 0) {
    body.appendChild(
      h("div", { class: "lr mf", text: "No sessions recorded yet, so no project is known." })
    );
  }
  for (const project of projects.projects) body.appendChild(projectRow(project, projects));

  body.appendChild(section("Join projects"));
  body.appendChild(joinRow(projects));

  body.appendChild(section("Organizations"));
  if (projects.orgs.length === 0) {
    body.appendChild(h("div", { class: "lr mf", text: "No organization yet." }));
  }
  for (const org of projects.orgs) body.appendChild(orgRow(org, projects));
  body.appendChild(
    note([
      h("b", { text: "Found by the repository." }),
      " The address of a repository (here its GitHub name) is the same on every host, so the same project needs no set-up anywhere. Its owner becomes the organization. What you rename, join or assign here is shared with your other hosts through the history folder.",
    ])
  );
  return card;
}

function projectRow(project, projects) {
  const field = `project:${projectKeyText(project.key)}`;
  const wrap = h("div", { class: "proj-wrap", dataset: { field } });

  const nameInput = textInput({
    class: "inp tall proj-name",
    value: project.name || "",
    "aria-label": "Project name",
  });
  nameInput.addEventListener("change", async () => {
    const next = nameInput.value.trim();
    if (next === project.name) {
      nameInput.value = project.name || "";
      return;
    }
    const ok = await saveProject({ edit: "rename", key: project.key, name: next }, field);
    if (!ok) nameInput.value = project.name || "";
  });

  const repo = project.repo
    ? h("span", { class: "mono x11", title: project.repo, text: project.repo })
    : h("span", { class: "mono x11 mf", text: "no repository" });
  const folders = foldersText(project.folders);
  wrap.appendChild(
    h("div", { class: "lr tall proj-grid" }, [
      nameInput,
      repo,
      h("span", { class: "mono x11 mf", title: folders, text: folders }),
      projectOrgCell(project, projects, field),
      h("span", { class: "n", text: String(project.sessions) }),
    ])
  );
  if (Array.isArray(project.joined) && project.joined.length > 0) {
    wrap.appendChild(joinedLine(project, field));
  }
  wrap.appendChild(
    h("div", {
      class: "row-error",
      hidden: !state.errors[field],
      text: state.errors[field] || "",
    })
  );
  return wrap;
}

function projectOrgCell(project, projects, field) {
  const NEW_ORG = "\u0000new-organization";
  const select = selectInput(
    [
      ["", "none"],
      ...projects.orgs.map((org) => [org.id, org.name]),
      [NEW_ORG, "new…"],
    ],
    project.org || "",
    async (next) => {
      if (next === NEW_ORG) {
        newWrap.hidden = false;
        newInput.focus();
        select.value = project.org || "";
        return;
      }
      const ok = await saveProject(
        { edit: "assign", key: project.key, org: next === "" ? null : next },
        field
      );
      if (!ok) select.value = project.org || "";
    }
  );
  select.setAttribute("aria-label", `Organization of ${project.name || "the project"}`);
  selectWidth(select, 130);

  const newInput = textInput({
    class: "inp tall proj-new-input",
    placeholder: "organization name",
    "aria-label": "New organization name",
  });
  const use = h("button", { type: "button", class: "hbtn xs", text: "Use" });
  const saveNew = async () => {
    const next = newInput.value.trim();
    if (next === "") {
      fieldError(field, "An organization name must not be empty.");
      newInput.focus();
      return;
    }
    use.disabled = true;
    const ok = await saveProject({ edit: "assign", key: project.key, org: next }, field);
    use.disabled = false;
    if (!ok) {
      newInput.focus();
      return;
    }
    newWrap.hidden = true;
    newInput.value = "";
  };
  use.addEventListener("click", saveNew);
  newInput.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      saveNew();
    }
  });
  const newWrap = h("span", { class: "proj-new-org", hidden: true }, [newInput, use]);
  return h("span", { class: "proj-org" }, [select, newWrap]);
}

// A joined project has no row of its own: the core sends its key and the
// default name of that key, shown under the project it was joined into, with
// an undo that writes `Unjoin`.
function joinedLine(project, field) {
  const line = h("div", { class: "proj-joined" }, [h("span", { class: "mf", text: "joined: " })]);
  project.joined.forEach((joined, index) => {
    if (index > 0) line.appendChild(middotSeparator());
    line.appendChild(
      h("span", { class: "mono x11", text: projectLabel(joined.name, joined.key) })
    );
    const undo = h("button", {
      type: "button",
      class: "hbtn xs ghost",
      text: "undo",
      "aria-label": `Unjoin ${projectLabel(joined.name, joined.key)}`,
    });
    undo.addEventListener("click", async () => {
      undo.disabled = true;
      const ok = await saveProject({ edit: "unjoin", key: joined.key }, field);
      if (!ok) undo.disabled = false;
    });
    line.appendChild(document.createTextNode(" "));
    line.appendChild(undo);
  });
  return line;
}

function joinRow(projects) {
  const field = "project:join";
  const rows = projects.projects;
  const options = rows.map((project, index) => [
    String(index),
    projectLabel(project.name, project.key),
  ]);
  const first = selectInput(options, "0", () => {});
  const second = selectInput(options, rows.length > 1 ? "1" : "0", () => {});
  first.setAttribute("aria-label", "Join this project");
  second.setAttribute("aria-label", "Into this project");
  selectWidth(first);
  selectWidth(second);
  const join = h("button", { type: "button", class: "hbtn", text: "Join projects" });
  join.disabled = rows.length < 2;
  join.addEventListener("click", async () => {
    const from = rows[Number(first.value)];
    const into = rows[Number(second.value)];
    if (!from || !into) return;
    join.disabled = true;
    const ok = await saveProject({ edit: "join", key: from.key, into: into.key }, field);
    if (!ok) join.disabled = false;
  });
  const control = h("span", { class: "proj-join" }, [
    h("span", { class: "mf", text: "join" }),
    first,
    h("span", { class: "mf", text: "into" }),
    second,
    join,
  ]);
  return setting(
    "Join projects",
    control,
    rows.length < 2
      ? "there must be two projects"
      : "the first project's sessions count for the second",
    field
  );
}

function accountChip(ref, projects) {
  const entry = (projects.accountRefs || []).find((item) => sameRef(item.account, ref));
  const row = entry ? accountRows().find((account) => account.id === entry.id) : null;
  const abbr = row && row.abbr ? row.abbr : "?";
  const color = row && isColor(row.color) ? row.color : "#555b66";
  return h("span", {
    class: "acc-chip org-chip",
    style: { background: color, color: contrastText(color) },
    title: row ? `${row.label || abbr}${row.email ? ` · ${row.email}` : ""}` : ref.acc,
    text: abbr,
  });
}

function orgRow(org, projects) {
  const field = `org:${org.id}`;
  const nameInput = textInput({
    class: "inp tall org-name",
    value: org.name || "",
    "aria-label": "Organization name",
  });
  nameInput.addEventListener("change", async () => {
    const next = nameInput.value.trim();
    if (next === org.name) {
      nameInput.value = org.name || "";
      return;
    }
    const ok = await saveProject({ edit: "orgRename", org: org.id, name: next }, field);
    if (!ok) nameInput.value = org.name || "";
  });
  const chips = (org.accounts || []).map((ref) => accountChip(ref, projects));
  const accounts =
    chips.length > 0
      ? h("span", { class: "org-chips" }, chips)
      : h("span", { class: "mf", text: "no account" });
  const counts = `${countText(org.accounts.length, "account", "accounts")} · ${countText(
    org.projects,
    "project",
    "projects"
  )}`;
  return setting(
    nameInput,
    accounts,
    `from the owner of the repositories · ${counts}`,
    field
  );
}

// The mock-up shows the folder in effect as the empty field's placeholder. The
// snapshot fills it from the ClaudePaths the app runs with; when the snapshot
// has no folder, the field says so instead of inventing one.
function configFolderPlaceholder() {
  const folder =
    state.snapshot && typeof state.snapshot.claudeConfigDir === "string"
      ? state.snapshot.claudeConfigDir
      : "";
  return folder !== "" ? folder : "the config folder in effect is not known";
}

function generalCard() {
  const card = panel("General", "", [], [
    h("span", { class: "mf" }, [
      "To open the pop-up with a key, bind ",
      h("code", { text: "seatswap panel" }),
      " to a key in your desktop's keyboard settings.",
    ]),
  ]);
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
    drawPriceList(list);
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
        step: 30,
        unit: "s",
      }),
      "",
      "pollInactiveSecs"
    )
  );
  body.appendChild(section("Claude Code"));
  body.appendChild(
    textField("claudeConfigDir", "Claude Code's config folder", {
      placeholder: configFolderPlaceholder(),
      note: "empty: $CLAUDE_CONFIG_DIR or Claude Code's default folder · takes effect after restarting seatswap",
    })
  );
  body.appendChild(
    setting(
      "Read Claude Code's session files",
      toggle("readSessions", "Read Claude Code's session files"),
      "model, busy or idle, tokens per folder · never message text · takes effect after restarting seatswap",
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
      low === high ? shortPercent(low) : `${shortPercent(low)}–${shortPercent(high)}`;
    parts.push(`a full 5 h window is about ${share} % of a week`);
  }
  const cost = measured.switchCostPercentOfWindow;
  if (Number.isFinite(cost)) {
    parts.push(`a switch costs about ${shortPercent(cost)} % of a window`);
  } else {
    parts.push(`a switch cost is ${MISSING}`);
  }
  return parts.join(" · ");
}

function switchesNote(view) {
  if (!view) return "";
  if (!view.early) return "early switching is off";
  const value = view.measured ? view.measured.switchesPerDay : null;
  if (!Number.isFinite(value)) return MISSING;
  return `about ${shortPercent(value)} switches a day`;
}

function autoBurnCard() {
  const view = burn();
  const holdH =
    view && Number.isInteger(view.manualHoldH) ? view.manualHoldH : null;
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
        h("span", { text: "from what it has measured" }),
        "the size of your 5 h windows and what a switch costs"
      ),
      setting("Measured so far", h("span", { text: measuredText(view && view.measured) })),
      setting(
        "So right now",
        h("span", {
          class: view && view.early ? "ok" : "",
          text: view && typeof view.why === "string" ? view.why : MISSING,
        }),
        switchesNote(view)
      ),
      setting(
        "After a switch by you",
        h("span", {
          text:
            holdH === null
              ? "the hold after a switch by you is not known"
              : `it waits ${holdH} hour${holdH === 1 ? "" : "s"}, or until that account is full`,
        })
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
  // The mock-up draws "All accounts are full" as a switch, but the settings
  // description says it is always on: there is no setting to save. The switch
  // shows that state and says so, instead of offering a click that would do
  // nothing.
  const allFull = switchControl(true, "All accounts are full", () => {});
  allFull.button.disabled = true;
  allFull.button.title = "always on: it is sent whenever every account is full";
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
          "For sessions on",
          130
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
        allFull.node,
        "with the time the next one is free"
      ),
      setting(
        "Every automatic switch",
        toggle("notifyOnSwitch", "Every automatic switch"),
        "",
        "notifyOnSwitch"
      ),
      setting("A switch by an agent", h("span", { class: "mf", text: "always" })),
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
  const skillControl = h("span", { class: "row" });
  const skillNote = h("span", { class: "mf" });
  if (isObject(state.skill)) {
    showSkill(skillControl, skillNote, state.skill);
  } else if (state.skillError) {
    skillControl.appendChild(
      h("span", { class: "sw bad", text: "could not read" })
    );
    skillNote.textContent = state.skillError;
  } else {
    skillControl.appendChild(h("span", { class: "mf", text: "loading…" }));
  }
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
        h("b", { text: "These switches are not a lock." }),
        " They work through a marker that Claude Code puts on every command it runs. An agent runs as you: it can remove the marker, or change seatswap's files directly. The switches stop agents that follow the rules. To block the command itself, add a deny rule for ",
        h("span", { class: "mono", text: "seatswap" }),
        " to Claude Code's permissions.",
      ]),
      section("Telling agents about seatswap"),
      skillRow,
      section("Last calls by agents"),
      calls,
    ],
    h("span", { class: "mf" }, [
      "A call counts as an agent's when ",
      h("span", { class: "mono", text: "CLAUDECODE=1" }),
      " or ",
      h("span", { class: "mono", text: "AI_AGENT" }),
      " is set.",
    ])
  );

  // Draw the two rows that need the app from the stored views.
  renderCalls(callRows);
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
        state.skill = status;
        state.skillError = null;
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

async function loadSkillView() {
  if (!state.context) return;
  const token = ++state.skillToken;
  try {
    const status = await state.context.invoke("skill_status", {});
    if (token !== state.skillToken) return;
    if (!isObject(status)) throw new Error("skill_status returned an unexpected reply");
    state.skill = status;
    state.skillError = null;
  } catch (error) {
    if (token !== state.skillToken) return;
    state.skill = null;
    state.skillError = errorMessage(error);
  }
  if (token === state.skillToken) state.skillLoaded = true;
}

async function loadCallsView() {
  if (!state.context) return;
  const token = ++state.callsToken;
  try {
    const rows = await state.context.invoke("agent_calls", { limit: 20 });
    if (token !== state.callsToken) return;
    if (!Array.isArray(rows)) throw new Error("agent_calls returned an unexpected reply");
    // The clock text is part of the stored row, so a redraw needs no command.
    let whens = rows.map(() => "?");
    try {
      whens = await state.context.invoke("time_texts", {
        times: rows.map((row) => ({ at: row.at, style: "dayClock" })),
      });
      if (!Array.isArray(whens) || whens.length !== rows.length) {
        throw new Error("time_texts returned an unexpected reply");
      }
    } catch (error) {
      console.error(`agent call times: ${errorMessage(error)}`);
      whens = rows.map(() => "?");
    }
    if (token !== state.callsToken) return;
    state.calls = rows.map((row, index) => Object.assign({}, row, { when: whens[index] }));
    state.callsError = null;
  } catch (error) {
    if (token !== state.callsToken) return;
    state.calls = null;
    state.callsError = errorMessage(error);
  }
  if (token === state.callsToken) state.callsLoaded = true;
}

function renderCalls(container) {
  clear(container);
  if (state.callsError) {
    container.appendChild(message(state.callsError));
    return;
  }
  const rows = Array.isArray(state.calls) ? state.calls : null;
  if (rows === null) {
    container.appendChild(h("div", { class: "lr mf", text: "loading…" }));
    return;
  }
  if (rows.length === 0) {
    container.appendChild(h("div", { class: "lr" }, [h("span", { text: "No calls yet." })]));
    return;
  }
  for (const row of rows) {
    container.appendChild(
      h(
        "div",
        {
          class: "lr",
          style: { gridTemplateColumns: "96px minmax(0, 1fr) 170px 110px" },
        },
        [
          h("span", { class: "mf", text: row.when || "?" }),
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

// ---------------------------------------------------------------------------
// Providers (spec §7, step D task 5)
// ---------------------------------------------------------------------------

function providersCard() {
  return panel(
    "Providers",
    "read-only cards for Codex and Ollama's cloud plan",
    [
      section("Codex"),
      setting(
        "Show a Codex card",
        toggle("codexCard", "Show a Codex card"),
        "read only · every 5 minutes",
        "codexCard"
      ),
      note(
        "seatswap reads Codex's login file and asks chatgpt.com for your usage every 5 minutes. It never changes Codex's files."
      ),
      textField("codexHome", "Codex folder", {
        note: "empty: $CODEX_HOME or ~/.codex",
      }),
      section("Ollama"),
      setting(
        "Show an Ollama card",
        toggle("ollamaCard", "Show an Ollama card"),
        "asks the Ollama server on this machine for your cloud balance every 5 minutes",
        "ollamaCard"
      ),
      textField("ollamaUrl", "Ollama server"),
    ]
  );
}

// ---------------------------------------------------------------------------
// History and hosts (spec §1.5, §3.7; step C1 task 7)
// ---------------------------------------------------------------------------

// The mock-up's line: a host whose last record is older than this is amber.
const HOST_STALE_MS = 15 * 60000;

function historyCard() {
  const view = isObject(state.history) ? state.history : null;
  const card = panel("History and hosts", "what seatswap has recorded, and where", []);
  const body = card.querySelector(".settings-body");

  body.appendChild(section("This host"));
  body.appendChild(
    textField("hostName", "Name of this host", {
      placeholder: view && typeof view.host === "string" ? view.host : "",
      note: "must be different on every host",
      restart: Boolean(view && typeof view.savedHost === "string"),
    })
  );
  body.appendChild(
    textField("historyDir", "History folder", {
      placeholder: view && typeof view.dir === "string" ? view.dir : "",
      note: "a synced folder shares it with your other hosts",
      restart: Boolean(view && typeof view.savedDir === "string"),
      width: 280,
    })
  );

  body.appendChild(section("Hosts found in the folder"));
  const hosts = h("div", { class: "col hist-hosts" });
  body.appendChild(hosts);
  renderHosts(hosts, view, state.historyError);

  body.appendChild(
    note([
      h("b", { text: "Each host writes only its own subfolder," }),
      " so two hosts can never overwrite each other. ",
      h("b", { text: "Logins are never put here:" }),
      " a login copied to another host would log this one out.",
    ])
  );

  body.appendChild(section("Export and import"));
  body.appendChild(exportRow());
  body.appendChild(importRow());
  return card;
}

// Read the host folders when the group opens, after an import and after a
// saved host name or folder. A snapshot render in between uses the stored view.
async function loadHistory() {
  if (!state.context) return;
  const token = ++state.historyToken;
  try {
    const view = await state.context.invoke("history_view", {});
    if (token !== state.historyToken) return;
    if (!isObject(view)) throw new Error("history_view returned an unexpected reply");
    state.history = view;
    state.historyError = null;
  } catch (error) {
    if (token !== state.historyToken) return;
    state.historyError = errorMessage(error);
  }
  if (token === state.historyToken) state.historyLoaded = true;
}

// The hosts table: Host · Last record · Since · Size, 28 px rows as the mock-up.
function renderHosts(container, view, error) {
  clear(container);
  const rows = isObject(view) && Array.isArray(view.hosts) ? view.hosts : [];
  if (error && rows.length === 0) {
    container.appendChild(message(error));
    return;
  }
  container.appendChild(
    h("div", { class: "lr hd hist-grid" }, [
      h("span", { text: "Host" }),
      h("span", { text: "Last record" }),
      h("span", { text: "Since" }),
      h("span", { class: "hist-size", text: "Size" }),
    ])
  );
  const times = [];
  const now = Date.now();
  for (const info of rows) {
    let last = null;
    if (typeof info.lastRecord === "string" && info.lastRecord !== "") {
      const age = Date.parse(info.lastRecord);
      const stale = !Number.isFinite(age) || now - age > HOST_STALE_MS;
      const text = h("span");
      last = h("span", { class: stale ? "sw warn" : "sw ok" }, [text]);
      times.push({ node: text, style: "age", at: info.lastRecord });
    } else {
      last = h("span", { class: "mf", text: "no record yet" });
    }

    let since;
    if (typeof info.firstRecord === "string" && info.firstRecord !== "") {
      const date = h("span");
      times.push({
        node: date,
        style: "dayYear",
        at: info.firstRecord,
      });
      since = h("span", {}, [date]);
      if (info.thisHost) since.appendChild(document.createTextNode(" · this host"));
    } else {
      since = h("span", { class: "mf", text: "—" });
    }

    container.appendChild(
      h("div", { class: "lr hist-grid" }, [
        h("b", { text: info.host }),
        h("span", {}, [last]),
        since,
        h("span", { class: "n hist-size", text: bytesText(info.bytes) }),
      ])
    );
  }
  if (error) container.appendChild(message(error));
  fillTimeNodes(times);
}

// A folder's size as the mock-up shows it: "2.9 MB".
function bytesText(bytes) {
  const count = Number(bytes);
  if (!Number.isFinite(count) || count < 0) return "—";
  if (count >= 1024 * 1024) return `${(count / (1024 * 1024)).toFixed(1)} MB`;
  if (count >= 1024) return `${(count / 1024).toFixed(1)} kB`;
  return `${Math.round(count)} B`;
}

function exportRow() {
  const input = textInput({
    value: state.exportPath,
    placeholder: "~/seatswap-history.csv",
    "aria-label": "save to",
  });
  input.className = "inp";
  input.addEventListener("input", () => {
    state.exportPath = input.value;
  });
  const result = h("span", { class: "mf" });
  const written = (text) => {
    state.exportResult = text;
    result.textContent = text;
  };
  const run = (format) => async () => {
    const to = String(state.exportPath || "").trim();
    if (to === "") {
      written("");
      fieldError("historyExport", "enter a file path");
      return;
    }
    fieldError("historyExport", null);
    try {
      await state.context.invoke("history_export", {
        format,
        to,
        from: null,
        until: null,
      });
      written(`written: ${to}`);
    } catch (error) {
      written("");
      fieldError("historyExport", errorMessage(error));
    }
  };
  const buttons = h("span", { class: "hist-buttons" }, [
    h("button", { type: "button", class: "hbtn xs", onclick: run("csv") }, "CSV"),
    h("button", { type: "button", class: "hbtn xs", onclick: run("json") }, "JSON"),
  ]);
  written(state.exportResult);
  return setting(
    "Export",
    h("span", { class: "hist-export" }, [input, buttons]),
    result,
    "historyExport"
  );
}

function importRow() {
  const input = textInput({
    value: state.importFolder,
    placeholder: "~/Sync/seatswap-history",
    "aria-label": "import from",
  });
  input.className = "inp";
  input.addEventListener("input", () => {
    state.importFolder = input.value;
  });
  const noteLines = [];
  if (state.importResult !== "") noteLines.push(state.importResult);
  noteLines.push(...state.importConflicts);
  const noteChildren = [];
  for (const line of noteLines) {
    if (noteChildren.length > 0) noteChildren.push(h("br"));
    noteChildren.push(line);
  }
  const result = h("span", { class: "mf" }, noteChildren);
  const sub = h("span");
  const note = h("span", { class: "mf" }, [result, sub]);
  const paintSub = () => {
    sub.textContent = result.hasChildNodes()
      ? " · for hosts without a synced folder"
      : "for hosts without a synced folder";
  };
  paintSub();
  const run = async () => {
    const from = String(state.importFolder || "").trim();
    if (from === "") {
      state.importResult = "";
      state.importConflicts = [];
      result.textContent = "";
      paintSub();
      fieldError("historyImport", "enter a folder path");
      return;
    }
    fieldError("historyImport", null);
    try {
      const reply = await state.context.invoke("history_import", { from });
      if (
        !isObject(reply) ||
        !Array.isArray(reply.copied) ||
        !Array.isArray(reply.conflicts) ||
        !isObject(reply.view)
      ) {
        throw new Error("history_import returned an unexpected reply");
      }
      for (const conflict of reply.conflicts) {
        if (
          !isObject(conflict) ||
          typeof conflict.host !== "string" ||
          typeof conflict.reason !== "string"
        ) {
          throw new Error("history_import returned an unexpected reply");
        }
      }
      state.history = reply.view;
      state.importResult =
        reply.copied.length === 0 ? "nothing new" : `copied: ${reply.copied.join(", ")}`;
      state.importConflicts = reply.conflicts.map(
        (conflict) => `conflict ${conflict.host}: ${conflict.reason}`
      );
    } catch (error) {
      state.importResult = "";
      state.importConflicts = [];
      result.textContent = "";
      paintSub();
      fieldError("historyImport", errorMessage(error));
      return;
    }
    // The table shows the folder as it is now; the field keeps what was typed.
    if (state.group === "history" && state.right) renderGroup(state.right, "history");
  };
  return setting(
    "Import another host's folder",
    h("span", { class: "hist-import" }, [
      input,
      h("button", { type: "button", class: "hbtn xs", onclick: run }, "Import"),
    ]),
    note,
    "historyImport"
  );
}
