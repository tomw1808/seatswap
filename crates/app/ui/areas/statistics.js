// The Statistics area of the large window (spec §3): the range, the accounts,
// the host, the seven page tabs, and the pages under areas/statistics/. The
// area only asks its commands and shows what they answer; a page module gets
// `query`, `call`, the snapshot's `accounts`, `open`, and the currency the
// snapshot's settings name. Plain ES2020, no framework.

import {
  banner,
  clear,
  contrastText,
  dateText,
  errorMessage,
  h,
  isColor,
  isObject,
  readChoice,
  writeChoice,
} from "../common.js";
import { hideTip } from "../charts.js";

const RANGE_KEY = "seatswap.stats.range";
const ACCOUNTS_KEY = "seatswap.stats.accounts";
const HOST_KEY = "seatswap.stats.host";
const TAB_KEY = "seatswap.stats.tab";

const RANGES = [
  { id: "thisWeek", label: "This week" },
  { id: "lastWeek", label: "Last week" },
  { id: "days30", label: "30d" },
  { id: "days90", label: "90d" },
  { id: "months12", label: "12 mo" },
  { id: "all", label: "All" },
];

const PAGES = [
  { id: "overview", title: "Overview" },
  { id: "weeks", title: "Weeks" },
  { id: "projects", title: "Projects" },
  { id: "subscriptions", title: "Subscriptions" },
  { id: "forecast", title: "Forecast" },
  { id: "rhythm", title: "Rhythm" },
  { id: "activity", title: "Activity" },
];
const PAGE_IDS = PAGES.map((page) => page.id);

const state = {
  active: false,
  container: null,
  context: null,
  snapshot: null,
  root: null,
  head: null,
  tabrow: null,
  errorHost: null,
  pageHost: null,
  accounts: [],
  hosts: null,
  historyStart: null,
  historyBytes: null,
  hostError: null,
  choiceError: null,
  range: "thisWeek",
  excluded: new Set(),
  host: "*",
  page: "overview",
  query: null,
  accountKey: "",
  loadToken: 0,
};

// The page's empty text: exactly the text the plan gives, with the stats pages'
// date ("Mon 9 Nov"), or without a date when the history has no first record.
function emptyRangeText(recordingSince) {
  if (typeof recordingSince !== "string" || recordingSince === "") return "Nothing recorded yet.";
  if (Number.isNaN(Date.parse(recordingSince))) return "Nothing recorded yet.";
  return `Nothing recorded yet. seatswap has been recording since ${dateText(recordingSince)}.`;
}

class EmptyRange extends Error {
  constructor(recordingSince) {
    super(emptyRangeText(recordingSince));
    this.name = "EmptyRange";
    this.recordingSince =
      typeof recordingSince === "string" && recordingSince !== "" ? recordingSince : null;
  }
}

function accountsFrom(snapshot) {
  if (!isObject(snapshot) || !Array.isArray(snapshot.views)) return [];
  return snapshot.views.filter((view) => isObject(view) && typeof view.id === "string");
}

function pageFromSub(sub) {
  if (typeof sub !== "string" || sub === "") return null;
  if (PAGE_IDS.includes(sub)) {
    writeChoice(TAB_KEY, sub);
    return sub;
  }
  console.warn(`[statistics] unknown tab ${JSON.stringify(sub)} in the address; keeping the chosen tab`);
  return null;
}

// The query's account list: empty means every account, also those no longer
// stored whose history still counts; only a chip taken out names the chosen ids.
function chosenIds() {
  if (state.excluded.size === 0) return [];
  return state.accounts.filter((view) => !state.excluded.has(view.id)).map((view) => view.id);
}

function chosenAccounts() {
  return state.accounts.filter((view) => !state.excluded.has(view.id));
}

function buildQuery() {
  return {
    range: state.range,
    accounts: chosenIds(),
    host: state.host === "*" ? null : state.host,
  };
}

function writeExcluded() {
  const ordered = state.accounts
    .map((view) => view.id)
    .filter((id) => state.excluded.has(id));
  writeChoice(ACCOUNTS_KEY, ordered.join(","));
}

function reconcileExcluded(excluded, accounts, keepAtLeastOne) {
  state.choiceError = null;
  const known = new Set(accounts.map((view) => view.id));
  const next = new Set();
  for (const id of excluded) {
    if (known.has(id)) next.add(id);
    else console.warn(`[statistics] dropped the kept account ${JSON.stringify(id)}: it is no longer stored`);
  }
  if (keepAtLeastOne && accounts.length > 0 && next.size >= accounts.length) {
    // The stored choice would leave no account at all. It is reset, and the
    // area shows that in its error banner: never a silent fallback.
    state.choiceError =
      "the kept account choice leaves no account; every account is shown again";
    next.clear();
  }
  return next;
}

function readExcluded(accounts) {
  const raw = readChoice(ACCOUNTS_KEY, (value) => typeof value === "string", "");
  const ids = raw
    .split(",")
    .map((id) => id.trim())
    .filter((id) => id !== "");
  return reconcileExcluded(new Set(ids), accounts, true);
}

// ---------------------------------------------------------------------------
// the frame
// ---------------------------------------------------------------------------

function accountChip(view) {
  const chosen = !state.excluded.has(view.id);
  const color = isColor(view.color) ? view.color : "var(--muted)";
  const onlyChosen = chosen && chosenIds().length === 1;
  const label = String(view.abbr || view.label || view.id);
  const node = h("button", {
    type: "button",
    class: `stats-account${chosen ? "" : " stats-account-off"}`,
    "aria-label": `Show ${label}`,
    "aria-pressed": chosen ? "true" : "false",
    disabled: onlyChosen,
    title: onlyChosen
      ? "at least one account"
      : `${label}${view.label ? ` · ${view.label}` : ""} — click to ${
          chosen ? "take it out" : "bring it back"
        }`,
  });
  node.textContent = label;
  node.style.borderColor = color;
  if (chosen) {
    node.style.background = color;
    node.style.color = contrastText(color);
  } else {
    node.style.background = "transparent";
    node.style.color = "var(--text)";
  }
  node.addEventListener("click", () => toggleAccount(view.id));
  return node;
}

function renderChips() {
  const host = h("div", { class: "stats-chips" });
  for (const view of state.accounts) host.appendChild(accountChip(view));
  return host;
}

function renderHostSelect() {
  if (!Array.isArray(state.hosts) || state.hosts.length <= 1) return null;
  const select = h("select", {
    class: "stats-host",
    "aria-label": "Host",
    onchange: (event) => selectHost(event.target.value),
  });
  const all = h("option", { value: "*", text: "All hosts" });
  if (state.host === "*") all.selected = true;
  select.appendChild(all);
  for (const name of state.hosts) {
    const option = h("option", { value: name, text: name });
    if (state.host === name) option.selected = true;
    select.appendChild(option);
  }
  return select;
}

function renderRangeTabs() {
  const tabs = h("div", { class: "stats-tabs", role: "group", "aria-label": "Range" });
  for (const range of RANGES) {
    tabs.appendChild(
      h(
        "button",
        {
          type: "button",
          class: `stats-tab${state.range === range.id ? " stats-tab-on" : ""}`,
          "aria-pressed": state.range === range.id ? "true" : "false",
          onclick: () => selectRange(range.id),
        },
        range.label
      )
    );
  }
  return tabs;
}

function renderPageTabs() {
  const tabs = h("div", { class: "stats-tabs", role: "tablist", "aria-label": "Page" });
  for (const page of PAGES) {
    tabs.appendChild(
      h(
        "button",
        {
          type: "button",
          role: "tab",
          class: `stats-tab${state.page === page.id ? " stats-tab-on" : ""}`,
          "aria-selected": state.page === page.id ? "true" : "false",
          onclick: () => selectTab(page.id),
        },
        page.title
      )
    );
  }
  return tabs;
}

// "4.6 MB": the history's size as the frame writes it, one decimal and one
// megabyte = 1 048 576 bytes.
function historySizeText(bytes) {
  if (typeof bytes !== "number" || !Number.isSafeInteger(bytes) || bytes < 0) {
    throw new Error(`invalid history size ${String(bytes)}: use a whole number of bytes`);
  }
  return `${(bytes / 1048576).toFixed(1)} MB`;
}

function renderFrame() {
  if (!state.head || !state.tabrow) return;
  clear(state.head);
  clear(state.tabrow);

  const accounts = state.accounts.length;
  const hosts = Array.isArray(state.hosts) ? String(state.hosts.length) : "—";
  const meta = [`${accounts} accounts`, `${hosts} hosts`];
  if (typeof state.historyStart === "string") {
    meta.push(`since ${dateText(state.historyStart)} · now`);
  }
  state.head.appendChild(
    h("p", {
      class: "stats-meta",
      text: meta.join(" · "),
      title:
        state.hostError === null
          ? undefined
          : `the host list could not be read: ${state.hostError}`,
    })
  );

  const controls = h("div", { class: "stats-controls" });
  controls.appendChild(renderChips());
  const hostSelect = renderHostSelect();
  if (hostSelect) controls.appendChild(hostSelect);
  controls.appendChild(renderRangeTabs());
  state.head.appendChild(controls);

  state.tabrow.appendChild(renderPageTabs());
  const spacer = h("span", { class: "sp" });
  state.tabrow.appendChild(spacer);
  if (state.historyBytes !== null) {
    state.tabrow.appendChild(
      h("span", {
        class: "stats-history",
        text: `history: ${historySizeText(state.historyBytes)}`,
      })
    );
  }
  state.tabrow.appendChild(
    h(
      "button",
      {
        type: "button",
        class: "hbtn xs stats-export",
        title: "The export lives in Settings → History and hosts",
        onclick: () => {
          location.hash = "settings/history";
        },
      },
      "Export"
    )
  );
}

// ---------------------------------------------------------------------------
// the choices
// ---------------------------------------------------------------------------

function selectRange(id) {
  if (!RANGES.some((range) => range.id === id)) throw new Error(`unknown range ${JSON.stringify(id)}`);
  state.range = id;
  writeChoice(RANGE_KEY, id);
  state.query = buildQuery();
  renderFrame();
  void loadPage();
}

function toggleAccount(id) {
  if (!state.accounts.some((view) => view.id === id)) return;
  state.choiceError = null;
  const chosen = chosenIds();
  if (chosen.includes(id) && chosen.length === 1) return;
  if (state.excluded.has(id)) state.excluded.delete(id);
  else state.excluded.add(id);
  writeExcluded();
  state.query = buildQuery();
  renderFrame();
  void loadPage();
}

function selectHost(value) {
  state.host = value;
  writeChoice(HOST_KEY, value);
  state.query = buildQuery();
  renderFrame();
  void loadPage();
}

function selectTab(id) {
  if (!PAGE_IDS.includes(id)) throw new Error(`unknown tab ${JSON.stringify(id)}`);
  state.page = id;
  writeChoice(TAB_KEY, id);
  const target = `statistics/${id}`;
  if (location.hash.slice(1) !== target) location.hash = target;
  // The address change asks the window to render the area; load here too so the
  // page is shown even when this module's render sees the tab already chosen.
  renderFrame();
  void loadPage();
}

function open(tab) {
  selectTab(tab);
}

// ---------------------------------------------------------------------------
// the commands
// ---------------------------------------------------------------------------

async function call(command, extra) {
  const args = { query: state.query };
  if (isObject(extra)) Object.assign(args, extra);
  const reply = await state.context.invoke(command, args);
  if (!isObject(reply)) throw new Error(`${command} returned an unexpected reply`);
  if (Object.prototype.hasOwnProperty.call(reply, "empty")) {
    const empty = isObject(reply.empty) ? reply.empty : {};
    throw new EmptyRange(empty.recordingSince === undefined ? null : empty.recordingSince);
  }
  if (!Object.prototype.hasOwnProperty.call(reply, "data")) {
    throw new Error(`${command} returned an unexpected reply`);
  }
  return reply.data;
}

function pageCurrency() {
  const settings = isObject(state.snapshot) ? state.snapshot.settings : null;
  return isObject(settings) && typeof settings.currency === "string" ? settings.currency : null;
}

async function loadFrame() {
  try {
    const reply = await state.context.invoke("stats_frame", {});
    if (!isObject(reply)) throw new Error("stats_frame returned an unexpected reply");
    const names = reply.hosts;
    if (!Array.isArray(names) || !names.every((name) => typeof name === "string")) {
      throw new Error("stats_frame returned an unexpected host list");
    }
    const start = reply.historyStart;
    if (
      start !== null &&
      (typeof start !== "string" || start === "" || Number.isNaN(Date.parse(start)))
    ) {
      throw new Error("stats_frame returned an unexpected history start");
    }
    const bytes = reply.historyBytes;
    if (typeof bytes !== "number" || !Number.isSafeInteger(bytes) || bytes < 0) {
      throw new Error("stats_frame returned an unexpected history size");
    }
    state.hosts = names;
    state.historyStart = start;
    state.historyBytes = bytes;
    state.hostError = null;
  } catch (error) {
    state.hosts = null;
    state.historyStart = null;
    state.historyBytes = null;
    state.hostError = errorMessage(error);
  }
}

function showHostError() {
  if (state.hostError === null) return;
  state.errorHost.appendChild(banner("error", "⚠", state.hostError));
}

function showChoiceError() {
  if (state.choiceError === null) return;
  state.errorHost.appendChild(banner("error", "⚠", state.choiceError));
}

async function loadPage() {
  if (!state.active || !state.pageHost) return;
  hideTip();
  const token = ++state.loadToken;
  clear(state.errorHost);
  state.query = buildQuery();
  showHostError();
  showChoiceError();

  let module;
  try {
    module = await import(`./statistics/${state.page}.js`);
  } catch (error) {
    if (token !== state.loadToken) return;
    console.warn(`[statistics] cannot load statistics/${state.page}.js`, error);
    clear(state.pageHost);
    state.pageHost.appendChild(
      banner("error", "⚠", `Cannot show ${state.page}: ${errorMessage(error)}`)
    );
    return;
  }
  if (token !== state.loadToken || !state.active) return;
  const env = {
    query: state.query,
    call,
    accounts: chosenAccounts(),
    open,
    currency: pageCurrency(),
    historyStart: typeof state.historyStart === "string" ? state.historyStart : null,
  };
  // A page draws into its own stage: a page that loses the race to a newer load
  // writes into a stage that is never shown, so it cannot leave half a page.
  const stage = h("div", { class: "stats-page-stage" });
  try {
    await module.show(stage, env);
  } catch (error) {
    if (token !== state.loadToken || !state.active) return;
    clear(state.pageHost);
    if (error && error.name === "EmptyRange") {
      state.pageHost.appendChild(h("p", { class: "stats-empty", text: errorMessage(error) }));
    } else {
      state.pageHost.appendChild(banner("error", "⚠", errorMessage(error)));
    }
    return;
  }
  if (token !== state.loadToken || !state.active) return;
  clear(state.pageHost);
  state.pageHost.appendChild(stage);
}

// ---------------------------------------------------------------------------
// enter, leave, update
// ---------------------------------------------------------------------------

function buildRoot(container) {
  clear(container);
  const root = h("div", { class: "stats" });
  state.head = h("div", { class: "stats-head" });
  state.tabrow = h("div", { class: "stats-tabrow" });
  state.errorHost = h("div", { class: "stats-errors" });
  state.pageHost = h("div", { class: "stats-page" });
  root.appendChild(state.head);
  root.appendChild(state.tabrow);
  root.appendChild(state.errorHost);
  root.appendChild(state.pageHost);
  container.appendChild(root);
  state.root = root;
}

export async function render(container, context) {
  if (!isObject(context)) throw new Error("the Statistics area needs its context");

  if (state.active && state.container === container) {
    state.context = context;
    const snapshot = isObject(context.snapshot) ? context.snapshot : state.snapshot;
    const nextAccounts = accountsFrom(snapshot);
    const nextKey = nextAccounts.map((view) => view.id).join("\u0000");
    const subPage = pageFromSub(context.sub);
    state.snapshot = snapshot;
    if (nextKey !== state.accountKey) {
      state.accountKey = nextKey;
      state.accounts = nextAccounts;
      state.excluded = reconcileExcluded(state.excluded, state.accounts, true);
      writeExcluded();
      state.query = buildQuery();
      if (subPage !== null) state.page = subPage;
      renderFrame();
      await loadPage();
      return;
    }
    if (subPage !== null && subPage !== state.page) {
      state.page = subPage;
      renderFrame();
      await loadPage();
      return;
    }
    renderFrame();
    return;
  }

  if (state.active) unmount();
  state.active = true;
  state.container = container;
  state.context = context;
  state.snapshot = isObject(context.snapshot) ? context.snapshot : null;
  state.accounts = accountsFrom(state.snapshot);
  state.accountKey = state.accounts.map((view) => view.id).join("\u0000");
  state.excluded = readExcluded(state.accounts);
  writeExcluded();
  state.range = readChoice(RANGE_KEY, RANGES.map((range) => range.id), "thisWeek");
  state.host = readChoice(HOST_KEY, (value) => typeof value === "string" && value !== "", "*");
  state.page = pageFromSub(context.sub) || readChoice(TAB_KEY, PAGE_IDS, "overview");
  state.query = buildQuery();
  state.historyStart = null;
  state.historyBytes = null;
  state.hostError = null;
  state.loadToken += 1;

  buildRoot(container);
  await loadFrame();
  if (!state.active) return;
  if (state.hosts && state.host !== "*" && !state.hosts.includes(state.host)) {
    console.warn(`[statistics] dropped the kept host ${JSON.stringify(state.host)}: it is not in the history`);
    state.host = "*";
    writeChoice(HOST_KEY, "*");
  }
  state.query = buildQuery();
  renderFrame();
  await loadPage();
}

export function update(snapshot) {
  if (!state.active || !isObject(snapshot)) return;
  state.snapshot = snapshot;
  const nextAccounts = accountsFrom(snapshot);
  const nextKey = nextAccounts.map((view) => view.id).join("\u0000");
  if (nextKey !== state.accountKey) {
    state.accountKey = nextKey;
    state.accounts = nextAccounts;
    state.excluded = reconcileExcluded(state.excluded, state.accounts, true);
    writeExcluded();
    state.query = buildQuery();
    renderFrame();
    void loadPage();
    return;
  }
  state.accounts = nextAccounts;
  renderFrame();
}

export function unmount() {
  state.active = false;
  state.loadToken += 1;
  state.container = null;
  state.context = null;
  state.snapshot = null;
  state.root = null;
  state.head = null;
  state.tabrow = null;
  state.errorHost = null;
  state.pageHost = null;
  state.accounts = [];
  state.hosts = null;
  state.historyStart = null;
  state.historyBytes = null;
  state.hostError = null;
  state.choiceError = null;
  state.excluded = new Set();
  state.query = null;
  state.accountKey = "";
  hideTip();
}
