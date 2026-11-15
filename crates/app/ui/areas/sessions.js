// The Sessions area of the large window (spec §3.8): every running session of
// this host and the user's other hosts, their agents, tokens and values, and
// the closed ones on request. Plain ES2020, no framework.
//
// The view carries every number the page shows: the page looks a group's sum
// up in `sums` and never adds values itself. Times and ages come from the Rust
// `time_texts` command; durations are counted here from the view's times.

import {
  accountName,
  clear,
  errorMessage,
  h,
  isActiveView,
  isObject,
  kpi,
  modelName,
  readChoice,
  tokens,
  valueSummary,
  valueText,
  writeChoice,
} from "../common.js";

const CLOSED_KEY = "seatswap.sessions.closed";
const HOST_KEY = "seatswap.sessions.host";
const GROUPING_KEY = "seatswap.sessions.grouping";
const ALL_HOSTS = "*";

const CLOSED_CHOICES = [
  { id: "none", label: "None" },
  { id: "day", label: "24 h" },
  { id: "week", label: "7 d" },
  { id: "month", label: "30 d" },
];
const GROUPINGS = [
  { id: "project", label: "By project" },
  { id: "model", label: "By model" },
];

const DEPARTED_FOR_MS = 60000;
const MARK_FOR_MS = 3000;

const state = {
  active: false,
  container: null,
  context: null,
  snapshot: null,
  view: null,
  shell: null,
  sideCards: null,
  sideKeys: null,
  accountKey: null,
  accountValue: null,
  treeScroll: null,
  treeNodes: new Map(),
  kpiHost: null,
  kpis: null,
  liveHost: null,
  liveKey: null,
  winHead: null,
  controlsNode: null,
  sessHead: null,
  unavailableHost: null,
  errorHost: null,
  linkHost: null,
  controls: null,
  valueHead: null,
  apiHead: null,
  sums: new Map(),
  rowNodes: new Map(),
  timeRequests: [],
  renderToken: 0,
  startToken: 0,
  choices: { closed: "none", host: null, grouping: "project" },
  hostKey: null,
  unlisten: null,
  subscribed: false,
  subscribeRequested: false,
  subscribeInFlight: null,
  subscribeSeq: 0,
  subscribeError: null,
  error: null,
  linkMessage: null,
  openSessions: new Set(),
  openAgents: new Set(),
  departed: new Map(),
  marked: null,
  markTimer: null,
  minuteTimer: null,
  departTimer: null,
  choiceToken: 0,
  ignoreDepartures: false,
  sub: null,
  pendingLink: null,
};

// ---------------------------------------------------------------------------
// the three kept choices (the one reader and writer live in common.js)
// ---------------------------------------------------------------------------

function readChoices() {
  state.choices.closed = readChoice(
    CLOSED_KEY,
    CLOSED_CHOICES.map((choice) => choice.id),
    "none"
  );
  state.choices.grouping = readChoice(
    GROUPING_KEY,
    GROUPINGS.map((group) => group.id),
    "project"
  );
  let host = null;
  try {
    const value = localStorage.getItem(HOST_KEY);
    if (value !== null && value !== "") {
      host = value === ALL_HOSTS ? null : value;
    } else if (value === "") {
      localStorage.removeItem(HOST_KEY);
    }
  } catch {
    host = null;
  }
  state.choices.host = host;
}

function options() {
  return { closed: state.choices.closed, host: state.choices.host };
}

// ---------------------------------------------------------------------------
// small shared helpers (all presentation, never a rule of the view)
// ---------------------------------------------------------------------------

function plural(count, one, many) {
  return `${count} ${count === 1 ? one : many}`;
}

// "A and B"; three or more: "A, B and C".
function nameList(names) {
  if (names.length === 0) return "";
  if (names.length === 1) return names[0];
  if (names.length === 2) return `${names[0]} and ${names[1]}`;
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

function projectKey(id) {
  if (!isObject(id)) return "";
  return `${id.kind || ""}\u0000${id.id || ""}\u0000${id.host || ""}`;
}

function modelKey(model) {
  return model === null || model === undefined ? "\u0000none" : `\u0001${model}`;
}

function sumKey(of) {
  if (!isObject(of)) return "";
  switch (of.kind) {
    case "all":
      return "all";
    case "provider":
      return `provider\u0000${of.provider || ""}`;
    case "project":
      return `project\u0000${projectKey(of.project)}`;
    case "projectModel":
      return `projectModel\u0000${projectKey(of.project)}\u0000${modelKey(of.model)}`;
    case "model":
      return `model\u0000${modelKey(of.model)}`;
    default:
      return `\uffff${JSON.stringify(of)}`;
  }
}

function indexSums(view) {
  const sums = new Map();
  for (const sum of view.sums) {
    if (
      !isObject(sum) ||
      !isObject(sum.of) ||
      !Number.isFinite(sum.sessions) ||
      !isObject(sum.tokens) ||
      !isObject(sum.lastHour) ||
      !isObject(sum.values)
    ) {
      throw new Error("a sum in the view is not a sum");
    }
    sums.set(sumKey(sum.of), sum);
  }
  return sums;
}

function validEffect(effect) {
  return (
    isObject(effect) &&
    ["losesThinking", "losesThinkingBusy", "resend", "free"].every((key) =>
      Number.isFinite(effect[key])
    ) &&
    Array.isArray(effect.losesThinkingModels) &&
    effect.losesThinkingModels.every((model) => typeof model === "string")
  );
}

function sumFor(kind, value) {
  if (kind === "all") return state.sums.get("all") || null;
  if (kind === "provider") return state.sums.get(`provider\u0000${value}`) || null;
  if (kind === "project") return state.sums.get(`project\u0000${projectKey(value)}`) || null;
  if (kind === "projectModel") {
    return (
      state.sums.get(
        `projectModel\u0000${projectKey(value.project)}\u0000${modelKey(value.model)}`
      ) || null
    );
  }
  if (kind === "model") return state.sums.get(`model\u0000${modelKey(value)}`) || null;
  return null;
}

function groupsOf(rows, keyOf) {
  const groups = new Map();
  for (const row of rows) {
    const key = keyOf(row);
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(row);
  }
  return Array.from(groups.values());
}

// Providers in the mock-up's order (Claude Code first, then Codex), models in
// the order their first session appears.
function providerGroups(rows) {
  return groupsOf(rows, (row) => row.provider || "").sort((a, b) =>
    String(a[0].provider || "").localeCompare(String(b[0].provider || ""))
  );
}

function modelGroups(rows) {
  return groupsOf(rows, (row) => modelKey(row.model));
}

function statusName(row) {
  if (typeof row.status === "string") return row.status;
  if (isObject(row.status) && typeof row.status.other === "string") return "other";
  throw new Error(`unknown session status ${JSON.stringify(row.status)}`);
}

function isClosed(row) {
  return statusName(row) === "closed";
}

// A row whose status the view knows is not a live one: a stale host's report
// says `{other: "no report"}`. The page reads that status; it never judges the
// report's age itself.
function isReported(row) {
  return statusName(row) !== "other";
}

// A session that counts as running: listed, not closed, and with a live status.
function isRunning(row) {
  return !isClosed(row) && isReported(row);
}

function otherHostStale(row) {
  const view = state.view;
  if (!view || !Array.isArray(view.hosts)) return null;
  if (row.host && row.host === view.thisHost) return null;
  return view.hosts.find((host) => host.host === row.host) || null;
}

function valueNode(values, kind, viewCurrency, className) {
  const value = valueText(values, kind, viewCurrency);
  return h(
    "span",
    { class: className || "n", title: value.title || null },
    value.text
  );
}

function tokenNode(count, className) {
  if (count === null || count === undefined) {
    return h("span", { class: className || "n" }, "");
  }
  return h("span", { class: className || "n" }, tokens(count));
}

function lastHourTokenNode(count) {
  return count === null || count === undefined || count === 0
    ? h("span", { class: "n" }, "")
    : tokenNode(count, "n pri");
}

function durationText(since, now) {
  if (typeof since !== "string") return "";
  const start = Date.parse(since);
  if (!Number.isFinite(start)) return "";
  const minutes = Math.max(0, Math.floor((now - start) / 60000));
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  if (hours < 24) return rest ? `${hours} h ${rest} min` : `${hours} h`;
  const days = Math.floor(hours / 24);
  const left = hours % 24;
  return left ? `${days} d ${left} h` : `${days} d`;
}

function isToday(iso) {
  if (typeof iso !== "string") return false;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return false;
  const now = new Date();
  return (
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate()
  );
}

function timeNode(at, style, className) {
  const node = h("span", { class: className || "" });
  state.timeRequests.push({ node, at, style });
  return node;
}

function appendText(host, text) {
  host.appendChild(document.createTextNode(text));
}

// ---------------------------------------------------------------------------
// the page: enter, leave, render
// ---------------------------------------------------------------------------

export async function render(container, context) {
  if (!isObject(context)) throw new Error("the Sessions area needs its context");
  if (state.active && state.container === container) {
    state.context = context;
    state.snapshot = isObject(context.snapshot) ? context.snapshot : state.snapshot;
    const nextSub = typeof context.sub === "string" && context.sub ? context.sub : null;
    if (nextSub === null) {
      state.sub = null;
      state.pendingLink = null;
      state.linkMessage = null;
      renderHeadLines();
      return;
    }
    const changed = state.sub !== nextSub;
    state.sub = nextSub;
    if (context.fromOpen === true || changed) {
      if (state.view) applyLink(nextSub);
      else state.pendingLink = nextSub;
    }
    return;
  }
  if (state.active) unmount();

  const start = ++state.startToken;
  state.active = true;
  state.container = container;
  state.context = context;
  state.snapshot = isObject(context.snapshot) ? context.snapshot : null;
  state.view = null;
  state.error = null;
  state.subscribeError = null;
  state.subscribeInFlight = null;
  state.linkMessage = null;
  state.sub = typeof context.sub === "string" ? context.sub : null;
  state.pendingLink = state.sub;
  state.openSessions.clear();
  state.openAgents.clear();
  state.departed.clear();
  state.treeNodes = new Map();
  state.rowNodes = new Map();
  state.sums = new Map();
  state.marked = null;
  readChoices();
  buildShell(container);

  const unlisten = await context.listen("sessions-view", (payload) => {
    if (!isObject(payload) || start !== state.startToken) return;
    try {
      applyView(payload);
    } catch (error) {
      setError(`sessions-view: ${errorMessage(error)}`);
    }
  });
  // The area may have been left while the listener was being set up. Nothing
  // of this start may survive a leave: the listener goes, the start stops.
  // It is kept in a local until then, so a later start's listener is never
  // overwritten or dropped.
  if (!state.active || start !== state.startToken) {
    if (unlisten) {
      try {
        unlisten();
      } catch (error) {
        console.warn("[sessions] cannot remove the view listener", error);
      }
    }
    return;
  }
  state.unlisten = unlisten;
  state.minuteTimer = setInterval(() => {
    if (state.active && state.view) renderView();
  }, 60000);
  await firstView(start);
}

// The sessions area has its own event; a snapshot never redraws it. The
// account chip is the one part that comes from the snapshot.
export function update(snapshot) {
  if (!isObject(snapshot)) return;
  state.snapshot = snapshot;
  if (state.active && state.view) renderKpis();
}

export function unmount() {
  if (!state.active) return;
  // A start that is still in flight must stop where it is.
  state.startToken += 1;
  state.active = false;
  const context = state.context;
  if (state.unlisten) {
    try {
      state.unlisten();
    } catch (error) {
      console.warn("[sessions] cannot remove the view listener", error);
    }
    state.unlisten = null;
  }
  if (state.markTimer) clearTimeout(state.markTimer);
  if (state.minuteTimer) clearInterval(state.minuteTimer);
  if (state.departTimer) clearTimeout(state.departTimer);
  state.markTimer = null;
  state.minuteTimer = null;
  state.departTimer = null;
  unmountControls();
  state.container = null;
  state.view = null;
  state.shell = null;
  state.sideCards = null;
  state.sideKeys = null;
  state.accountKey = null;
  state.accountValue = null;
  state.treeScroll = null;
  state.treeNodes = new Map();
  state.kpiHost = null;
  state.kpis = null;
  state.liveHost = null;
  state.liveKey = null;
  state.unavailableHost = null;
  state.errorHost = null;
  state.linkHost = null;
  state.controls = null;
  state.valueHead = null;
  state.apiHead = null;
  state.rowNodes = new Map();
  state.sums = new Map();
  state.openSessions.clear();
  state.openAgents.clear();
  state.departed.clear();
  state.sub = null;
  state.pendingLink = null;
  const wasRequested = state.subscribeRequested;
  state.subscribed = false;
  state.subscribeRequested = false;
  state.subscribeError = null;
  // A subscribe that is still in flight sends its own unsubscribe when it
  // answers; sending one now could overtake it.
  if (wasRequested && context && !state.subscribeInFlight) {
    context
      .invoke("sessions_unsubscribe", {})
      .catch((error) => console.error(`[sessions] cannot unsubscribe: ${errorMessage(error)}`));
  }
  // The context is kept until the unsubscribe call above was made.
  state.context = null;
}

async function firstView(start) {
  state.error = null;
  const context = state.context;
  try {
    const view = await invokeView();
    if (!state.active || start !== state.startToken) return;
    applyView(view);
  } catch (error) {
    if (!state.active || start !== state.startToken) return;
    setError(`sessions_view: ${errorMessage(error)}`);
    renderHeadLines();
  }
  if (!state.active || start !== state.startToken) return;
  const subscribeId = ++state.subscribeSeq;
  state.subscribeInFlight = subscribeId;
  try {
    state.subscribeRequested = true;
    await context.invoke("sessions_subscribe", { options: options() });
    // Left meanwhile: undo this subscribe, unless a later start has asked for
    // its own already (the thread keeps one subscription, in the order the
    // commands came; an unsubscribe now would end the later one).
    if (!state.active || start !== state.startToken) {
      if (!state.active || !state.subscribeRequested) {
        context
          .invoke("sessions_unsubscribe", {})
          .catch((error) => console.error(`[sessions] cannot unsubscribe: ${errorMessage(error)}`));
      }
      return;
    }
    state.subscribed = true;
    clearSubscribeError();
  } catch (error) {
    if (!state.active || start !== state.startToken) return;
    state.subscribed = false;
    setSubscribeError(`sessions_subscribe: ${errorMessage(error)}`);
  } finally {
    if (state.subscribeInFlight === subscribeId) state.subscribeInFlight = null;
  }
}

async function invokeView() {
  const view = await state.context.invoke("sessions_view", { options: options() });
  if (
    !isObject(view) ||
    !Array.isArray(view.sessions) ||
    !Array.isArray(view.hosts) ||
    !Array.isArray(view.sums) ||
    !validEffect(view.switchEffect)
  ) {
    throw new Error("sessions_view returned an unexpected reply");
  }
  return view;
}

function setError(text) {
  state.error = text;
  renderHeadLines();
}

// A failed subscribe is not a view error: `applyView` clears the latter, while
// this one stays until a subscribe succeeds.
function setSubscribeError(text) {
  state.subscribeError = text;
  renderHeadLines();
}

function clearSubscribeError() {
  if (state.subscribeError === null) return;
  state.subscribeError = null;
  renderHeadLines();
}

function applyView(view) {
  if (
    !isObject(view) ||
    !Array.isArray(view.sessions) ||
    !Array.isArray(view.hosts) ||
    !Array.isArray(view.sums) ||
    !validEffect(view.switchEffect)
  ) {
    setError("sessions_view returned an unexpected reply");
    return;
  }
  // A stored host that this view does not know is forgotten, as the brief says.
  if (
    state.choices.host !== null &&
    !view.hosts.some((host) => isObject(host) && host.host === state.choices.host)
  ) {
    state.choices.host = null;
    try {
      localStorage.removeItem(HOST_KEY);
    } catch {
      // See writeChoice.
    }
  }

  const previous = state.ignoreDepartures ? null : state.view;
  state.ignoreDepartures = false;
  if (previous && state.choices.closed === "none") {
    const nextIds = new Set(view.sessions.map((row) => row.session));
    for (const row of previous.sessions) {
      if (nextIds.has(row.session) || isClosed(row) || state.departed.has(row.session)) continue;
      state.departed.set(row.session, {
        row,
        until: Date.now() + DEPARTED_FOR_MS,
        place: departurePlace(previous.sessions, row),
      });
    }
    for (const id of Array.from(state.departed.keys())) {
      if (nextIds.has(id)) state.departed.delete(id);
    }
  } else {
    state.departed.clear();
  }
  if (state.choices.closed !== "none") state.departed.clear();

  state.view = view;
  state.sums = indexSums(view);
  state.error = null;
  renderView();

  if (state.pendingLink) {
    const id = state.pendingLink;
    state.pendingLink = null;
    applyLink(id);
  }
  scheduleDeparted();
}

function scheduleDeparted() {
  if (state.departTimer) clearTimeout(state.departTimer);
  state.departTimer = null;
  let soonest = null;
  for (const departed of state.departed.values()) {
    if (soonest === null || departed.until < soonest) soonest = departed.until;
  }
  if (soonest === null) return;
  state.departTimer = setTimeout(
    () => {
      state.departTimer = null;
      if (!state.active) return;
      const now = Date.now();
      for (const [id, departed] of Array.from(state.departed.entries())) {
        if (departed.until <= now) state.departed.delete(id);
      }
      renderView();
      scheduleDeparted();
    },
    Math.max(0, soonest - Date.now()) + 50
  );
}

function applyLink(id) {
  if (!state.active) return;
  if (!state.view) {
    state.pendingLink = id;
    return;
  }
  state.sub = id;
  const found = state.view.sessions.some((row) => row.session === id);
  if (!found) {
    state.linkMessage = "that session is no longer listed";
    renderHeadLines();
    return;
  }
  state.linkMessage = null;
  state.openSessions.add(id);
  if (state.marked && state.marked.id === id) {
    const old = state.rowNodes.get(id);
    if (old) old.classList.remove("sess-marked");
  }
  state.marked = { id, until: Date.now() + MARK_FOR_MS };
  if (state.markTimer) clearTimeout(state.markTimer);
  state.markTimer = setTimeout(() => {
    const marked = state.rowNodes.get(id);
    if (marked) marked.classList.remove("sess-marked");
    state.marked = null;
    state.markTimer = null;
    if (state.active && state.view) renderView();
  }, MARK_FOR_MS);
  renderView();
  const node = state.rowNodes.get(id);
  if (node) {
    node.classList.remove("sess-marked");
    void node.offsetWidth;
    node.classList.add("sess-marked");
    node.scrollIntoView({ block: "center" });
  }
}

// ---------------------------------------------------------------------------
// the shell: head controls, numbers, the tree and the cards
// ---------------------------------------------------------------------------

function buildShell(container) {
  clear(container);
  state.hostKey = null;
  state.liveKey = null;
  const root = h("div", { class: "sessions" });
  const head = h("div", { class: "sess-head" });
  state.liveHost = h("div", { class: "sess-live" });
  state.unavailableHost = h("p", { class: "sess-unavailable", hidden: true });
  state.errorHost = h("p", { class: "sess-error", hidden: true });
  state.linkHost = h("p", { class: "sess-link-message", hidden: true });
  const controls = h("div", { class: "sess-controls" });
  state.controls = {
    closed: new Map(),
    grouping: new Map(),
    host: h("select", { class: "sess-host-select", "aria-label": "Hosts" }),
  };
  const closedGroup = h("div", {
    class: "sess-seg",
    role: "group",
    "aria-label": "Closed sessions",
  });
  for (const choice of CLOSED_CHOICES) {
    const button = h(
      "button",
      {
        type: "button",
        class: "sess-segbtn",
        onclick: () => changeChoice({ closed: choice.id }),
      },
      choice.label
    );
    state.controls.closed.set(choice.id, button);
    closedGroup.appendChild(button);
  }
  const hostWrap = h("label", { class: "sess-host" }, [state.controls.host]);
  state.controls.host.addEventListener("change", () => {
    const value = state.controls.host.value;
    changeChoice({ host: value === ALL_HOSTS ? null : value });
  });
  const groupingGroup = h("div", {
    class: "sess-seg",
    role: "group",
    "aria-label": "Grouping",
  });
  for (const group of GROUPINGS) {
    const button = h(
      "button",
      {
        type: "button",
        class: "sess-segbtn",
        onclick: () => changeChoice({ grouping: group.id }),
      },
      group.label
    );
    state.controls.grouping.set(group.id, button);
    groupingGroup.appendChild(button);
  }
  controls.append(
    h("span", { class: "sess-choice-row" }, [
      h("span", { class: "sess-control-label" }, "Closed sessions"),
      closedGroup,
    ]),
    h("span", { class: "sess-choice-row" }, [hostWrap]),
    h("span", { class: "sess-choice-row" }, [groupingGroup])
  );
  head.append(state.liveHost, state.unavailableHost, state.errorHost, state.linkHost);
  state.sessHead = head;
  mountControls(controls);

  state.kpiHost = h("div", { class: "sess-kpis" });
  state.kpis = null;
  const body = h("div", { class: "sess-body" });
  const tree = h("div", { class: "hcard sess-tree" });
  state.valueHead = h("span", { class: "n sess-value-head" }, "value");
  state.apiHead = h("span", { class: "n sess-api-head" }, "API value");
  const treeHead = h("div", { class: "sess-cols sess-cols-head" }, [
    h("span", {}, "Project · provider and model · session · agent"),
    h("span", {}, "Model"),
    h("span", {}, "Status"),
    h("span", { class: "n" }, "Agents"),
    h("span", { class: "n" }, "Tokens"),
    h("span", { class: "n" }, "Last h"),
    state.valueHead,
    state.apiHead,
    h("span", {}, "Host"),
  ]);
  state.treeScroll = h("div", { class: "sess-scroll" });
  tree.append(treeHead, state.treeScroll);
  const side = h("div", { class: "sess-side" });
  state.sideCards = { model: null, switch: null, last: null };
  state.sideKeys = { model: null, switch: null, last: null };
  state.accountKey = null;
  state.accountValue = null;
  body.append(tree, side);
  root.append(head, state.kpiHost, body);
  state.shell = { root, tree, side, treeHead };
  container.appendChild(root);
  renderControls();
}

// The mock-up has the choices on the tabs' row at the right and the live line
// under the tabs. The tabs belong to the window; the areas module puts its
// choices beside them and hides the window's title only while Sessions is open.
function mountControls(controls) {
  const winHead = document.querySelector(".win-head");
  state.controlsNode = controls;
  if (!winHead) {
    if (state.sessHead) state.sessHead.appendChild(controls);
    return;
  }
  state.winHead = winHead;
  winHead.classList.add("sess-mode");
  winHead.appendChild(controls);
}

function unmountControls() {
  if (state.winHead) state.winHead.classList.remove("sess-mode");
  if (state.controlsNode && state.controlsNode.parentNode) {
    state.controlsNode.parentNode.removeChild(state.controlsNode);
  }
  state.winHead = null;
  state.controlsNode = null;
  state.sessHead = null;
}

function renderControls() {
  if (!state.controls) return;
  for (const [id, button] of state.controls.closed) {
    const on = id === state.choices.closed;
    button.classList.toggle("ison", on);
    button.setAttribute("aria-pressed", on ? "true" : "false");
  }
  for (const [id, button] of state.controls.grouping) {
    const on = id === state.choices.grouping;
    button.classList.toggle("ison", on);
    button.setAttribute("aria-pressed", on ? "true" : "false");
  }
  updateHostSelect();
}

function updateHostSelect() {
  if (!state.controls || !state.view) return;
  const select = state.controls.host;
  const hosts = state.view.hosts.map((host) => host.host);
  const key = hosts.join("\u0000");
  if (state.hostKey !== key) {
    clear(select);
    select.appendChild(h("option", { value: ALL_HOSTS }, "All hosts"));
    for (const host of hosts) select.appendChild(h("option", { value: host }, host));
    state.hostKey = key;
  }
  select.value = state.choices.host === null ? ALL_HOSTS : state.choices.host;
}

async function changeChoice(patch) {
  const context = state.context;
  const closedChanged = Object.prototype.hasOwnProperty.call(patch, "closed");
  // Another closed choice or host lists other rows: what leaves the list then
  // did not leave its host. A grouping change lists the same rows.
  if (closedChanged || Object.prototype.hasOwnProperty.call(patch, "host")) {
    state.departed.clear();
    state.ignoreDepartures = true;
  }
  Object.assign(state.choices, patch);
  if (Object.prototype.hasOwnProperty.call(patch, "closed")) {
    writeChoice(CLOSED_KEY, state.choices.closed);
    if (state.choices.closed !== "none") state.departed.clear();
  }
  if (Object.prototype.hasOwnProperty.call(patch, "grouping")) {
    writeChoice(GROUPING_KEY, state.choices.grouping);
  }
  if (Object.prototype.hasOwnProperty.call(patch, "host")) {
    writeChoice(HOST_KEY, state.choices.host === null ? ALL_HOSTS : state.choices.host);
  }
  state.error = null;
  state.linkMessage = null;
  renderControls();
  renderHeadLines();
  if (closedChanged || patch.grouping !== undefined) renderView();
  if (!state.active) return;
  const token = ++state.choiceToken;
  const subscribeId = ++state.subscribeSeq;
  state.subscribeInFlight = subscribeId;
  try {
    state.subscribeRequested = true;
    await context.invoke("sessions_subscribe", { options: options() });
    if (!state.active) {
      context
        .invoke("sessions_unsubscribe", {})
        .catch((error) => console.error(`[sessions] cannot unsubscribe: ${errorMessage(error)}`));
      return;
    }
    state.subscribed = true;
    clearSubscribeError();
  } catch (error) {
    state.subscribed = false;
    if (state.active && token === state.choiceToken) {
      setSubscribeError(`sessions_subscribe: ${errorMessage(error)}`);
    }
  } finally {
    if (state.subscribeInFlight === subscribeId) state.subscribeInFlight = null;
  }
  try {
    const view = await invokeView();
    if (token !== state.choiceToken || !state.active) return;
    applyView(view);
  } catch (error) {
    if (token === state.choiceToken) setError(`sessions_view: ${errorMessage(error)}`);
  }
}

// ---------------------------------------------------------------------------
// head, numbers
// ---------------------------------------------------------------------------

function renderHeadLines() {
  if (!state.liveHost) return;
  const view = state.view;
  if (view) {
    const key = `${view.thisHost || ""}|${view.hosts
      .map((host) => `${host.host}@${host.at}`)
      .join(",")}|${Math.floor(Date.now() / 60000)}`;
    if (state.liveKey !== key) {
      state.liveKey = key;
      clear(state.liveHost);
      state.liveHost.appendChild(h("span", { class: "sw ok" }, "live"));
      if (view.thisHost) {
        appendText(state.liveHost, ` · ${view.thisHost}, this host`);
      } else {
        appendText(state.liveHost, " · no history folder");
      }
      for (const host of view.hosts) {
        if (host.this) continue;
        if (host.stale) {
          appendText(state.liveHost, ` · ${host.host}: no report since `);
          state.liveHost.appendChild(timeNode(host.at, "dayClock"));
          continue;
        }
        appendText(state.liveHost, ` · ${host.host}: as of `);
        state.liveHost.appendChild(timeNode(host.at, "age"));
      }
    }
    if (state.valueHead) state.valueHead.textContent = view.currency || "value";
    if (state.apiHead) {
      state.apiHead.textContent = view.currency ? `API ${view.currency}` : "API value";
    }
    state.unavailableHost.hidden = !view.unavailable;
    state.unavailableHost.textContent = view.unavailable
      ? `unavailable: ${view.unavailable}`
      : "";
    updateHostSelect();
  }
  const errorText = [state.subscribeError, state.error].filter(Boolean).join(" · ");
  state.errorHost.hidden = !errorText;
  state.errorHost.textContent = errorText ? `error: ${errorText}` : "";
  state.linkHost.hidden = !state.linkMessage;
  state.linkHost.textContent = state.linkMessage || "";
}

// The account in use comes from the snapshot, by the pop-up's rule.
function accountInUse() {
  const snapshot = state.snapshot;
  if (!isObject(snapshot) || !Array.isArray(snapshot.views)) return null;
  return snapshot.views.find((view) => isActiveView(view, snapshot)) || null;
}

// The KPI's "In use" value: the shared account tag, the account's label and
// its weekly percent. The tag itself is common.js's `accountName`.
function inUseNode() {
  const account = accountInUse();
  if (!account) {
    if (state.accountKey === "none" && state.accountValue) return state.accountValue;
    state.accountKey = "none";
    state.accountValue = h("span", { class: "mf" }, "no account in use");
    return state.accountValue;
  }
  const key = JSON.stringify(account);
  if (state.accountKey === key && state.accountValue) return state.accountValue;
  state.accountKey = key;
  const views =
    isObject(state.snapshot) && Array.isArray(state.snapshot.views)
      ? state.snapshot.views
      : [account];
  const chip = accountName(views, account.id, "sess-acc");
  const label = account.label || account.abbr || account.id || "";
  const limits =
    account.usage && Array.isArray(account.usage.limits) ? account.usage.limits : [];
  const weekly = limits.find((limit) => isObject(limit) && limit.kind === "weekly_all");
  const percent =
    weekly && typeof weekly.percent === "number" && Number.isFinite(weekly.percent)
      ? ` · week ${weekly.percent} %`
      : "";
  state.accountValue = h("span", { class: "sess-inuse" }, [chip, ` ${label}${percent}`]);
  return state.accountValue;
}

function renderKpis() {
  if (!state.kpiHost || !state.view) return;
  const view = state.view;
  const all = sumFor("all");
  const running = view.sessions.filter(isRunning);
  const busy = running.filter((row) => statusName(row) === "busy");
  const waiting = running.filter((row) => statusName(row) === "waiting");
  const hosts = new Set(running.map((row) => row.host).filter(Boolean));
  const agentsRunning = running.reduce(
    (count, row) => count + (Number.isFinite(row.agentsRunning) ? row.agentsRunning : 0),
    0
  );
  let completedToday = 0;
  for (const row of view.sessions) {
    for (const agent of Array.isArray(row.agents) ? row.agents : []) {
      if (!agent.running && isToday(agent.last)) completedToday += 1;
    }
  }
  // "Thinking tied to the account" counts the listed running rows whose thinking
  // is tied, and of them the busy ones. It never asks a model or the switch
  // effect: `row.tied` is the view's answer.
  const tied = running.filter((row) => row.tied === true);
  const tiedBusy = tied.filter((row) => statusName(row) === "busy").length;
  const tiedSubText =
    tied.length === 0
      ? ""
      : tiedBusy > 0
        ? `${tied.length === 1 ? "session" : "sessions"} · ${tiedBusy} of them busy`
        : tied.length === 1
          ? "session"
          : "sessions";
  const closedCount = view.sessions.filter((row) => isClosed(row)).length;

  if (!state.kpis) {
    state.kpis = Array.from({ length: 7 }, () =>
      kpi("", "", null, null, null, { prefix: "sess" })
    );
    for (const entry of state.kpis) state.kpiHost.appendChild(entry);
  }
  const kpis = state.kpis;
  kpis[0].update(
    "Running",
    String(running.length),
    hosts.size === 1 ? "on 1 host" : `on ${hosts.size} hosts`
  );
  kpis[1].update(
    "Busy",
    String(busy.length),
    waiting.length ? `${waiting.length} waiting for you` : "",
    "",
    "pri",
    waiting.length ? "warn" : ""
  );
  kpis[2].update("Agents", `${agentsRunning} running`, `${completedToday} completed today`);
  kpis[3].update(
    `${view.sessions.length === 1 ? "Total of the 1 session" : `Total of the ${view.sessions.length} sessions`}${closedCount ? ` · ${closedCount} closed` : ""}`,
    all ? `${tokens(all.tokens.out)} tokens` : "no sum in the view",
    all ? valueSummary(all.values) : ""
  );
  kpis[4].update("Last hour", all ? tokens(all.lastHour.out) : "no sum in the view", "");
  kpis[5].update(
    "Thinking tied to the account",
    String(tied.length),
    tiedSubText,
    "",
    tied.length ? "warn" : ""
  );
  kpis[6].update("In use", inUseNode(), "");
}

// ---------------------------------------------------------------------------
// the tree
// ---------------------------------------------------------------------------

function groupSubText(rows) {
  const running = rows.filter(isRunning);
  const closed = rows.filter(isClosed).length;
  const unreported = rows.filter((row) => !isReported(row)).length;
  const busy = running.filter((row) => statusName(row) === "busy").length;
  const waiting = running.filter((row) => statusName(row) === "waiting").length;
  const agents = running.reduce(
    (count, row) => count + (Number.isFinite(row.agentsRunning) ? row.agentsRunning : 0),
    0
  );
  const parts = [];
  if (running.length) parts.push(plural(running.length, "session", "sessions"));
  if (closed) parts.push(`${closed} closed`);
  if (unreported) parts.push(`${unreported} without a report`);
  if (!running.length && !closed && !unreported) parts.push("0 sessions");
  if (busy) parts.push(`${busy} busy`);
  if (waiting) parts.push(`${waiting} waiting for you`);
  if (agents) parts.push(`${plural(agents, "agent", "agents")} running`);
  return parts.join(" · ");
}

// A model group's own row. Its counts are the listed rows only; when every row
// of the group has left, the grey rows still keep the group on screen and the
// text comes from them (with nothing counted). "Thinking tied" follows the rows'
// `tied` flag and nothing else.
function modelSubText(rows, model, sampleRows) {
  const rowsForText = rows.length ? rows : sampleRows || [];
  if (rowsForText.length === 0) {
    throw new Error(`the model group ${String(model)} has no row`);
  }
  const running = rows.filter(isRunning);
  const provider = rowsForText[0].provider;
  const parts =
    model === null
      ? ["no answers yet", plural(running.length, "session", "sessions")]
      : [provider, modelName(model), plural(running.length, "session", "sessions")];
  if (model !== null) {
    const tied = rowsForText.some((row) => isReported(row) && row.tied === true);
    parts.push(tied ? "thinking tied to the account" : "thinking survives a switch");
  }
  return parts.join(" · ");
}

function setValueText(node, values, kind, currency, className) {
  const value = valueText(values, kind, currency);
  node.className = className;
  if (value.title) node.title = value.title;
  else node.removeAttribute("title");
  if (node.textContent !== value.text) node.textContent = value.text;
}

function createGroupItem(item) {
  const nameNode = h("b", { class: "sess-group-name" }, item.name);
  const subNode = h("span", { class: "mf sess-group-sub" }, ` · ${item.sub}`);
  const valueNodes = [
    h("span", { class: "n b6" }),
    h("span", { class: "n b6" }),
  ];
  const tokensNode = h("span", { class: "n b6" });
  const node = h("div", { class: "sess-row sess-group" }, [
    h("span", { class: "sess-name", style: { paddingLeft: `${item.depth * 14}px` } }, [
      nameNode,
      subNode,
    ]),
    h("span", {}, ""),
    h("span", {}, ""),
    h("span", {}, ""),
    tokensNode,
    h("span", {}, ""),
    valueNodes[0],
    valueNodes[1],
    h("span", {}, ""),
  ]);
  const entry = {
    type: "group",
    node,
    nameNode,
    subNode,
    tokensNode,
    valueNodes,
    signature: "",
  };
  updateGroupItem(entry, item);
  return entry;
}

function updateGroupItem(entry, item) {
  const signature = JSON.stringify([item.name, item.sub, item.sum, item.depth, state.view.currency]);
  if (signature === entry.signature) return;
  entry.signature = signature;
  if (entry.nameNode.textContent !== item.name) entry.nameNode.textContent = item.name;
  const sub = ` · ${item.sub}`;
  if (entry.subNode.textContent !== sub) entry.subNode.textContent = sub;
  entry.node.firstChild.style.paddingLeft = `${item.depth * 14}px`;
  if (item.sum) {
    entry.tokensNode.className = "n b6";
    entry.tokensNode.removeAttribute("title");
    const text = tokens(item.sum.tokens.out);
    if (entry.tokensNode.textContent !== text) entry.tokensNode.textContent = text;
    setValueText(entry.valueNodes[0], item.sum.values, "subscription", state.view.currency, "n b6");
    setValueText(entry.valueNodes[1], item.sum.values, "api", state.view.currency, "n b6");
  } else {
    entry.tokensNode.className = "n";
    entry.tokensNode.title = "this group has no sum in the view";
    if (entry.tokensNode.textContent !== "") entry.tokensNode.textContent = "";
    for (const valueNode of entry.valueNodes) {
      valueNode.className = "n";
      valueNode.removeAttribute("title");
      if (valueNode.textContent !== "") valueNode.textContent = "";
    }
  }
}

function createSubItem(item) {
  const textNode = h("span", {
    class: "mf",
    style: { paddingLeft: `${Math.max(0, item.depth * 14 - 2)}px` },
  }, item.text);
  return {
    type: "sub",
    node: h("div", { class: "sess-row sess-subrow" }, textNode),
    textNode,
    text: item.text,
    depth: item.depth,
  };
}

function updateSubItem(entry, item) {
  if (entry.text === item.text && entry.depth === item.depth) return;
  entry.text = item.text;
  entry.depth = item.depth;
  entry.textNode.style.paddingLeft = `${Math.max(0, item.depth * 14 - 2)}px`;
  if (entry.textNode.textContent !== item.text) entry.textNode.textContent = item.text;
}

function agentsCell(running, done) {
  return h("span", { class: "sess-agents" }, [
    h(
      "span",
      { class: running > 0 ? "pri b6" : "mf", title: "Agents running" },
      String(Number.isFinite(running) ? running : 0)
    ),
    h("span", { class: "mf" }, " · "),
    h("span", { class: "mf", title: "Agents completed" }, String(Number.isFinite(done) ? done : 0)),
  ]);
}

function statusNode(row, now) {
  const status = statusName(row);
  if (status === "closed") {
    const span = h("span", { class: "sw" });
    appendText(span, "closed ");
    span.appendChild(timeNode(row.since, "dayClock"));
    return span;
  }
  if (status === "other") {
    const text =
      isObject(row.status) && typeof row.status.other === "string" ? row.status.other : "no report";
    return h("span", { class: "sw" }, text);
  }
  if (status === "busy") return h("span", { class: "sw pri" }, `busy ${durationText(row.since, now)}`);
  if (status === "idle") return h("span", { class: "sw" }, `idle ${durationText(row.since, now)}`);
  if (status === "waiting") {
    return h("span", { class: "sw warn" }, `waiting for you ${durationText(row.since, now)}`);
  }
  return h("span", { class: "sw" }, status);
}

function hostNode(row) {
  const view = state.view;
  if (!row.host || row.host === view.thisHost) {
    return h("span", { class: "mf" }, row.host || "this host");
  }
  const host = otherHostStale(row);
  if (host && host.stale) {
    const span = h("span", { class: "mf" });
    appendText(span, "no report since ");
    span.appendChild(timeNode(host.at, "dayClock"));
    return span;
  }
  const span = h("span", { class: "mf" });
  appendText(span, `${row.host} · `);
  span.appendChild(timeNode(host ? host.at : row.since, "age"));
  return span;
}

function tiedNode(row) {
  if (!row.tied) return null;
  return h(
    "span",
    {
      class: "sess-tag warn",
      title:
        "Thinking tied to the account | on another account this session works without its earlier reasoning",
    },
    "tied"
  );
}

function sessionCells(row, options, now) {
  const view = state.view;
  const currency = view.currency;
  const name = row.label || row.session;
  const car = h("span", { class: options.details ? "sess-car" : "sess-car0" });
  const nameParts = [car];
  if (options.wide) {
    nameParts.push(
      h("span", { class: "sess-session-name" }, [
        name,
        h("span", { class: "mf" }, ` · ${row.project}`),
      ])
    );
  } else {
    nameParts.push(h("span", { class: "sess-session-name" }, name));
  }
  const model = options.wide
    ? h("span", {}, "")
    : h("span", { class: "sess-model" }, [
        row.model ? modelName(row.model) : "—",
        tiedNode(row),
      ]);
  const lastHour =
    row.lastHour && typeof row.lastHour.out === "number" && row.lastHour.out > 0
      ? lastHourTokenNode(row.lastHour.out)
      : h("span", { class: "n" }, "");
  return [
    h(
      "span",
      { class: "sess-name", style: { paddingLeft: `${options.depth * 14}px` } },
      nameParts
    ),
    model,
    statusNode(row, now),
    agentsCell(row.agentsRunning, row.agentsDone),
    tokenNode(row.tokens ? row.tokens.out : 0),
    lastHour,
    valueNode(row.values, "subscription", currency),
    valueNode(row.values, "api", currency),
    hostNode(row),
  ];
}

function sessionHasAgents(row) {
  return Array.isArray(row.agents) && row.agents.length > 0;
}

function sessionStructure(row, options) {
  return `${sessionHasAgents(row) ? "d" : "r"}|${options.wide ? "w" : "n"}|${options.depth}`;
}

function sessionClasses(row, options) {
  const classes = ["sess-row", "sess-session"];
  if (options.departed) classes.push("sess-dim");
  if (isClosed(row)) classes.push("sess-closed");
  return classes.join(" ");
}

function sessionSignature(row, options, nowMinute) {
  return JSON.stringify([
    row.label,
    row.session,
    row.project,
    row.model,
    statusName(row),
    row.since,
    row.tied,
    row.agentsRunning,
    row.agentsDone,
    row.tokens ? row.tokens.out : null,
    row.lastHour ? row.lastHour.out : null,
    row.values,
    row.host,
    options.wide,
    options.depth,
    options.departed === true,
    nowMinute,
  ]);
}

// The agents of an open session are drawn again when one of them changed, and
// every minute while one runs (its duration is counted from `first`).
function agentSignature(row) {
  const agents = Array.isArray(row.agents) ? row.agents : [];
  const minute = agents.some((agent) => agent.running === true)
    ? String(Math.floor(Date.now() / 60000))
    : "";
  return minute + "\u0002" + agents
    .map((agent) =>
      JSON.stringify([
        agent.agent,
        agent.parent,
        agent.kind,
        agent.task,
        agent.model,
        agent.running === true,
        agent.first,
        agent.last,
        agent.tokens ? agent.tokens.out : null,
        agent.values,
      ])
    )
    .join("\u0001");
}

function createSessionItem(row, options) {
  const details = sessionHasAgents(row);
  const open = state.openSessions.has(row.session);
  const rowNode = h(details ? "summary" : "div", { class: sessionClasses(row, options) });
  for (const cell of sessionCells(row, { ...options, details }, Date.now())) {
    rowNode.appendChild(cell);
  }
  rowNode.dataset.session = row.session;
  state.rowNodes.set(row.session, rowNode);
  const entry = {
    type: "session",
    row,
    options,
    structure: sessionStructure(row, options),
    signature: sessionSignature(row, options, Math.floor(Date.now() / 60000)),
    agentsSignature: "",
    node: rowNode,
    rowNode,
    children: null,
    build: null,
  };
  if (!details) {
    entry.agentsSignature = agentSignature(row);
    return entry;
  }

  const container = h("details", { class: "sess-details", open });
  container.appendChild(rowNode);
  const children = h("div", { class: "sess-children" });
  container.appendChild(children);
  entry.node = container;
  entry.children = children;
  const build = () => {
    if (children.dataset.built === "1") return;
    renderAgentList(children, entry.row, null, 4, entry.row.session);
    children.dataset.built = "1";
    entry.agentsSignature = agentSignature(entry.row);
  };
  entry.build = build;
  if (open) build();
  container.addEventListener("toggle", () => {
    if (container.open) {
      state.openSessions.add(entry.row.session);
      build();
    } else {
      state.openSessions.delete(entry.row.session);
    }
  });
  return entry;
}

// The row keeps its DOM node; only a changed row has its cells written again.
// Its children are built only while the row is open.
function updateSessionItem(entry, row, options) {
  entry.row = row;
  entry.options = options;
  const signature = sessionSignature(row, options, Math.floor(Date.now() / 60000));
  if (signature !== entry.signature) {
    entry.signature = signature;
    entry.rowNode.className = sessionClasses(row, options);
    clear(entry.rowNode);
    for (const cell of sessionCells(row, { ...options, details: entry.children !== null }, Date.now())) {
      entry.rowNode.appendChild(cell);
    }
  }
  if (entry.children && entry.children.dataset.built === "1") {
    const signatureOfAgents = agentSignature(row);
    if (signatureOfAgents !== entry.agentsSignature) {
      entry.agentsSignature = signatureOfAgents;
      clear(entry.children);
      delete entry.children.dataset.built;
      entry.build();
    }
  }
}

function agentStatus(agent, now) {
  if (agent.running) {
    const since = agent.first || agent.last;
    const duration = since ? durationText(since, now) : "";
    return h("span", { class: "sw pri" }, duration ? `running ${duration}` : "running");
  }
  const span = h("span", { class: "sw ok" });
  appendText(span, "done ");
  if (agent.last) span.appendChild(timeNode(agent.last, "dayClock"));
  return span;
}

function agentCells(agent, children, depth) {
  const now = Date.now();
  const view = state.view;
  const name = h("span", { class: "sess-agent-name" }, [
    h("b", {}, agent.kind || "agent"),
  ]);
  if (typeof agent.task === "string" && agent.task !== "") {
    name.appendChild(h("span", { class: "mf" }, ` ${agent.task}`));
  }
  const running = children.filter((child) => child.running).length;
  const done = children.length - running;
  return [
    h(
      "span",
      { class: "sess-name", style: { paddingLeft: `${depth * 14}px` } },
      [h("span", { class: children.length ? "sess-car" : "sess-car0" }), name]
    ),
    h("span", {}, ""),
    agentStatus(agent, now),
    children.length ? agentsCell(running, done) : h("span", {}, ""),
    tokenNode(agent.tokens ? agent.tokens.out : 0),
    h("span", { class: "n" }, ""),
    valueNode(agent.values, "subscription", view.currency),
    valueNode(agent.values, "api", view.currency),
    h("span", {}, ""),
  ];
}

function agentNode(session, agent, children, depth) {
  const key = `${session.session}\u0000${agent.agent}`;
  const open = state.openAgents.has(key);
  const row = h("div", {
    class: "sess-row sess-agent",
  });
  for (const cell of agentCells(agent, children, depth)) row.appendChild(cell);
  row.dataset.agent = key;
  if (children.length === 0) return row;

  const container = h("details", { class: "sess-details sess-agent-details", open });
  container.appendChild(h("summary", { class: "sess-summary-wrap" }, row));
  const inner = h("div", { class: "sess-children" });
  container.appendChild(inner);
  const build = () => {
    if (inner.dataset.built === "1") return;
    renderAgentList(inner, session, agent.agent, depth + 1, key);
    inner.dataset.built = "1";
  };
  if (open) build();
  container.addEventListener("toggle", () => {
    if (container.open) {
      state.openAgents.add(key);
      build();
    } else {
      state.openAgents.delete(key);
    }
  });
  return container;
}

function completedNode(session, agents, depth, parentKey) {
  const key = `${parentKey}\u0000completed`;
  const open = state.openAgents.has(key);
  const total = agents.reduce(
    (count, agent) => count + (agent.tokens && Number.isFinite(agent.tokens.out) ? agent.tokens.out : 0),
    0
  );
  const row = h("div", { class: "sess-row sess-agent sess-completed" }, [
    h(
      "span",
      { class: "sess-name", style: { paddingLeft: `${depth * 14}px` } },
      [h("span", { class: open ? "sess-car" : "sess-car0" }), `${plural(agents.length, "completed", "completed")}`]
    ),
    h("span", {}, ""),
    h("span", {}, ""),
    h("span", {}, ""),
    tokenNode(total),
    h("span", { class: "n" }, ""),
    h("span", { class: "n" }, ""),
    h("span", { class: "n" }, ""),
    h("span", {}, ""),
  ]);
  const container = h("details", { class: "sess-details", open });
  container.appendChild(h("summary", { class: "sess-summary-wrap" }, row));
  const inner = h("div", { class: "sess-children" });
  container.appendChild(inner);
  const build = () => {
    if (inner.dataset.built === "1") return;
    for (const agent of agents) {
      inner.appendChild(
        agentNode(session, agent, childAgents(session, agent.agent), depth + 1)
      );
    }
    inner.dataset.built = "1";
  };
  if (open) build();
  container.addEventListener("toggle", () => {
    if (container.open) {
      state.openAgents.add(key);
      build();
    } else {
      state.openAgents.delete(key);
    }
  });
  return container;
}

function childAgents(session, parentId) {
  return session.agents.filter((agent) => agent.parent === parentId);
}

function renderAgentList(container, session, parentId, depth, parentKey) {
  const agents = Array.isArray(session.agents) ? session.agents : [];
  const ids = new Set(agents.map((agent) => agent.agent));
  const children = agents.filter((agent) =>
    parentId === null
      ? !agent.parent || !ids.has(agent.parent)
      : agent.parent === parentId
  );
  const running = children.filter((agent) => agent.running);
  const done = children.filter((agent) => !agent.running);
  for (const group of groupsOf(running, (agent) => modelKey(agent.model))) {
    const model = group[0].model;
    container.appendChild(
      createSubItem({
        text: `${session.provider || "Claude Code"} · ${modelName(model)} · ${plural(
          group.length,
          "agent",
          "agents"
        )}`,
        depth,
      }).node
    );
    for (const agent of group) {
      container.appendChild(
        agentNode(session, agent, childAgents(session, agent.agent), depth)
      );
    }
  }
  if (done.length) container.appendChild(completedNode(session, done, depth, parentKey));
}

// Where a row sat when it left: its group's order and its index in the group
// for each grouping, counted in the view it left.
function departurePlace(rows, row) {
  const index = rows.findIndex((candidate) => candidate.session === row.session);
  if (index < 0) throw new Error(`the row ${row.session} is not in the view`);
  const project = projectKey(row.projectId);
  const provider = row.provider || "";
  const projectOrder = [];
  let projectIndex = 0;
  let providerIndex = 0;
  for (const candidate of rows) {
    const key = projectKey(candidate.projectId);
    if (!projectOrder.includes(key)) projectOrder.push(key);
  }
  for (let at = 0; at < index; at += 1) {
    const candidate = rows[at];
    if (projectKey(candidate.projectId) === project) projectIndex += 1;
    if ((candidate.provider || "") === provider) providerIndex += 1;
  }
  return {
    project: {
      groupsBefore: projectOrder.slice(0, projectOrder.indexOf(project)),
      index: projectIndex,
    },
    provider,
    providerIndex,
  };
}

// A group that only a grey row keeps goes right after the last group that was
// before it when the row left and is still listed (the view's groups are
// numbered 0, 1, …; the grey group sits between two of them).
function departedGroupOrder(groupsBefore, viewGroups) {
  let after = -1;
  for (const key of groupsBefore) {
    const group = viewGroups.get(key);
    if (group && group.order > after) after = group.order;
  }
  return after + 0.5 + groupsBefore.length / 1e6;
}

function insertDeparted(entries, entry, index) {
  const at = Math.max(0, Math.min(index, entries.length));
  entries.splice(at, 0, entry);
}

// By project: project → "provider · model" → session. Only the project row has
// numbers; its sub-text counts the view's rows, never a grey one.
function projectTreeItems() {
  const now = Date.now();
  const groups = new Map();
  for (const row of state.view.sessions) {
    const key = projectKey(row.projectId);
    let group = groups.get(key);
    if (!group) {
      group = {
        key,
        order: groups.size,
        entries: [],
        projectId: row.projectId,
        name: row.project || "unassigned",
      };
      groups.set(key, group);
    }
    group.entries.push({ row, departed: false });
  }
  const viewGroups = new Map(groups);
  for (const departed of state.departed.values()) {
    if (departed.until <= now) continue;
    const key = projectKey(departed.row.projectId);
    let group = groups.get(key);
    if (!group) {
      group = {
        key,
        order: departedGroupOrder(departed.place.project.groupsBefore, viewGroups),
        entries: [],
        projectId: departed.row.projectId,
        name: departed.row.project || "unassigned",
      };
      groups.set(key, group);
    }
    insertDeparted(
      group.entries,
      { row: departed.row, departed: true },
      departed.place.project.index
    );
  }
  const ordered = Array.from(groups.values()).sort((one, other) => one.order - other.order);
  const items = [];
  for (const group of ordered) {
    const viewRows = group.entries.filter((entry) => !entry.departed).map((entry) => entry.row);
    items.push({
      type: "group",
      key: `project\u0000${group.key}`,
      name: group.name,
      sub: groupSubText(viewRows),
      sum: sumFor("project", group.projectId),
      depth: 0,
    });
    for (const providerRows of providerGroups(group.entries.map((entry) => entry.row))) {
      const provider = providerRows[0].provider || "";
      for (const modelRows of modelGroups(providerRows)) {
        const model = modelRows[0].model;
        items.push({
          type: "sub",
          key: `projectModel\u0000${group.key}\u0000${provider}\u0000${modelKey(model)}`,
          text:
            model === null ? "no answers yet" : `${provider} · ${modelName(model)}`,
          depth: 1,
        });
        for (const entry of group.entries) {
          if ((entry.row.provider || "") !== provider) continue;
          if (modelKey(entry.row.model) !== modelKey(model)) continue;
          items.push(sessionTreeItem(entry));
        }
      }
    }
  }
  return items;
}

// By model: provider → model → session, the project after the session's name.
function modelTreeItems() {
  const now = Date.now();
  const providers = new Map();
  for (const row of state.view.sessions) {
    const provider = row.provider || "";
    let group = providers.get(provider);
    if (!group) {
      group = { provider, entries: [] };
      providers.set(provider, group);
    }
    group.entries.push({ row, departed: false });
  }
  for (const departed of state.departed.values()) {
    if (departed.until <= now) continue;
    const provider = departed.row.provider || "";
    let group = providers.get(provider);
    if (!group) {
      group = { provider, entries: [] };
      providers.set(provider, group);
    }
    insertDeparted(
      group.entries,
      { row: departed.row, departed: true },
      departed.place.providerIndex
    );
  }
  const ordered = Array.from(providers.values()).sort((one, other) =>
    one.provider.localeCompare(other.provider)
  );
  const items = [];
  for (const group of ordered) {
    const viewRows = group.entries.filter((entry) => !entry.departed).map((entry) => entry.row);
    items.push({
      type: "group",
      key: `provider\u0000${group.provider}`,
      name: group.provider,
      sub: groupSubText(viewRows),
      sum: sumFor("provider", group.provider),
      depth: 0,
    });
    for (const modelRows of modelGroups(group.entries.map((entry) => entry.row))) {
      const model = modelRows[0].model;
      const leaf = group.entries.filter(
        (entry) => modelKey(entry.row.model) === modelKey(model)
      );
      const viewLeafRows = leaf.filter((entry) => !entry.departed).map((entry) => entry.row);
      items.push({
        type: "sub",
        key: `model\u0000${group.provider}\u0000${modelKey(model)}`,
        text: modelSubText(viewLeafRows, model, leaf.map((entry) => entry.row)),
        depth: 1,
      });
      for (const entry of leaf) items.push(sessionTreeItem(entry));
    }
  }
  return items;
}

function sessionTreeItem(entry) {
  return {
    type: "session",
    key: `session\u0000${entry.row.session}`,
    row: entry.row,
    options: {
      wide: state.choices.grouping === "model",
      depth: 2,
      departed: entry.departed,
    },
  };
}

function removeTreeEntry(key) {
  const entry = state.treeNodes.get(key);
  if (!entry) return;
  if (entry.node.parentNode === state.treeScroll) state.treeScroll.removeChild(entry.node);
  if (entry.type === "session") state.rowNodes.delete(entry.row.session);
  state.treeNodes.delete(key);
}

function needsReplacement(entry, item) {
  if (entry.type !== item.type) return true;
  return (
    item.type === "session" &&
    entry.structure !== sessionStructure(item.row, item.options)
  );
}

function entryForItem(item) {
  let entry = state.treeNodes.get(item.key);
  switch (item.type) {
    case "empty":
      if (!entry || entry.type !== "empty") {
        if (entry) removeTreeEntry(item.key);
        entry = {
          type: "empty",
          node: h("p", { class: "sess-empty mf" }, "No sessions are listed."),
        };
        state.treeNodes.set(item.key, entry);
      }
      return entry;
    case "group":
      if (!entry || entry.type !== "group") {
        if (entry) removeTreeEntry(item.key);
        entry = createGroupItem(item);
        state.treeNodes.set(item.key, entry);
      } else {
        updateGroupItem(entry, item);
      }
      return entry;
    case "sub":
      if (!entry || entry.type !== "sub") {
        if (entry) removeTreeEntry(item.key);
        entry = createSubItem(item);
        state.treeNodes.set(item.key, entry);
      } else {
        updateSubItem(entry, item);
      }
      return entry;
    case "session":
      if (!entry || entry.type !== "session") {
        if (entry) removeTreeEntry(item.key);
        entry = createSessionItem(item.row, item.options);
        state.treeNodes.set(item.key, entry);
      } else {
        updateSessionItem(entry, item.row, item.options);
      }
      return entry;
    default:
      throw new Error(`unknown tree item ${item.type}`);
  }
}

// Rows are matched by session id and moved, not rebuilt: an unchanged row keeps
// its node, a changed one has its texts written, and only a row that was not
// there before is inserted. The scroll stays where the user left it.
function renderTree() {
  if (!state.treeScroll || !state.view) return;
  let items =
    state.choices.grouping === "model" ? modelTreeItems() : projectTreeItems();
  if (items.length === 0) items = [{ type: "empty", key: "empty" }];
  // A row inserted above the visible part must not move what the user looks
  // at: the scroll is put back where it was.
  const scroll = state.treeScroll.scrollTop;
  const seen = new Set();
  let cursor = state.treeScroll.firstChild;
  for (const item of items) {
    seen.add(item.key);
    const old = state.treeNodes.get(item.key);
    if (old && needsReplacement(old, item)) {
      if (cursor === old.node) cursor = old.node.nextSibling;
      removeTreeEntry(item.key);
    }
    const entry = entryForItem(item);
    if (entry.node === cursor) cursor = cursor.nextSibling;
    else state.treeScroll.insertBefore(entry.node, cursor);
  }
  while (cursor) {
    const next = cursor.nextSibling;
    state.treeScroll.removeChild(cursor);
    cursor = next;
  }
  for (const key of Array.from(state.treeNodes.keys())) {
    if (seen.has(key)) continue;
    const entry = state.treeNodes.get(key);
    if (entry.node.parentNode === state.treeScroll) {
      state.treeScroll.removeChild(entry.node);
    }
    if (entry.type === "session") state.rowNodes.delete(entry.row.session);
    state.treeNodes.delete(key);
  }
  state.treeScroll.scrollTop = scroll;
  if (state.marked && state.marked.until > Date.now()) {
    const node = state.rowNodes.get(state.marked.id);
    if (node) node.classList.add("sess-marked");
  }
  scheduleDeparted();
}

// ---------------------------------------------------------------------------
// the side cards
// ---------------------------------------------------------------------------

function sideCard(title, sub) {
  const card = h("div", { class: "hcard sess-card" });
  const head = h("div", { class: "ch" }, [h("span", { class: "t" }, title)]);
  if (sub) head.appendChild(h("span", { class: "s" }, sub));
  card.appendChild(head);
  const body = h("div", { class: "sess-card-body" });
  card.appendChild(body);
  return { card, body };
}

function modelCard() {
  const { card, body } = sideCard("By model", "");
  const rows = state.view.sessions;
  const groups = groupsOf(rows, (row) => modelKey(row.model));
  if (groups.length === 0) {
    body.appendChild(h("p", { class: "sess-empty mf" }, "No sessions."));
    return card;
  }
  body.appendChild(
    h("div", { class: "sess-mini-cols" }, [
      h("span", {}, "Model"),
      h("span", { class: "n" }, "Run."),
      h("span", { class: "n" }, "Busy"),
      h("span", { class: "n" }, "Last h"),
    ])
  );
  for (const group of groups) {
    const model = group[0].model;
    const running = group.filter(isRunning);
    const busy = running.filter((row) => statusName(row) === "busy").length;
    const sum = sumFor("model", model);
    const last = sum ? sum.lastHour.out : null;
    const dot = group.some((row) => isReported(row) && row.tied === true)
      ? h("span", {
          class: "sess-tied-dot",
          title: "Thinking tied to the account",
        })
      : null;
    body.appendChild(
      h("div", { class: "sess-mini-row" }, [
        h("span", { class: "sess-mini-name" }, [model === null ? "none yet" : modelName(model), dot]),
        h("span", { class: "n" }, String(running.length)),
        h("span", { class: "n" }, String(busy)),
        sum
          ? h("span", { class: "n" }, tokens(last))
          : h("span", { class: "n" }, ""),
      ])
    );
  }
  if (rows.some((row) => isReported(row) && row.tied === true)) {
    body.appendChild(
      h("p", { class: "sess-card-note mf" }, [
        h("span", { class: "sess-tied-dot" }),
        " thinking tied to the account",
      ])
    );
  }
  return card;
}

function switchCard() {
  const { card, body } = sideCard("If you switched now", "");
  const effect = state.view.switchEffect;
  const loses = Number.isFinite(effect.losesThinking) ? effect.losesThinking : 0;
  const busy = Number.isFinite(effect.losesThinkingBusy) ? effect.losesThinkingBusy : 0;
  const resend = Number.isFinite(effect.resend) ? effect.resend : 0;
  const free = Number.isFinite(effect.free) ? effect.free : 0;
  const list = h("ul", { class: "sess-fnd" });
  const line = (count, rest) => {
    const item = h("li", {});
    item.appendChild(h("b", {}, count));
    item.appendChild(document.createTextNode(rest));
    list.appendChild(item);
  };
  if (loses > 0) {
    const models = effect.losesThinkingModels;
    if (models.length === 0) {
      throw new Error(
        "switchEffect.losesThinkingModels is empty while losesThinking is not"
      );
    }
    let rest = ` on ${nameList(models.map(modelName))} would work without their earlier thinking.`;
    if (busy > 0) rest += ` ${busy} of them ${busy === 1 ? "is" : "are"} busy.`;
    line(plural(loses, "session", "sessions"), rest);
  }
  if (resend > 0) {
    line(
      plural(resend, "session", "sessions"),
      " would send their conversation again."
    );
  }
  if (free > 0) {
    line(
      plural(free, "session", "sessions"),
      " pay nothing: idle for an hour or longer, or without an answer yet."
    );
  }
  if (list.childNodes.length === 0) {
    const item = h("li", {});
    item.appendChild(h("b", {}, "Nothing is lost"));
    item.appendChild(
      document.createTextNode(
        ": no session would work without its earlier thinking or send its conversation again."
      )
    );
    list.appendChild(item);
  }
  body.appendChild(list);
  body.appendChild(
    h("div", { class: "row sess-switch-row" }, [
      h(
        "button",
        {
          type: "button",
          class: "hbtn",
          onclick: async () => {
            try {
              await state.context.invoke("open_panel", {});
            } catch (error) {
              setError(`open_panel: ${errorMessage(error)}`);
            }
          },
        },
        "Switch…"
      ),
    ])
  );
  return card;
}

function lastHourCard() {
  const { card, body } = sideCard("Last hour", "by project");
  const rows = state.view.sessions;
  const groups = groupsOf(rows, (row) => projectKey(row.projectId));
  const bars = groups.map((group) => {
    const id = group[0].projectId;
    const sum = sumFor("project", id);
    return {
      name: group[0].project || "unassigned",
      count: sum ? sum.lastHour.out : 0,
      hasSum: Boolean(sum),
    };
  });
  bars.sort((one, other) => other.count - one.count);
  if (bars.length === 0) {
    body.appendChild(h("p", { class: "sess-empty mf" }, "No sessions."));
    return card;
  }
  const max = bars.reduce((top, bar) => Math.max(top, bar.count), 0);
  for (const bar of bars) {
    const width = max > 0 ? Math.round((bar.count / max) * 100) : 0;
    body.appendChild(
      h("div", { class: "sess-bar-row" }, [
        h("span", { class: "sess-bar-name" }, bar.name),
        h("span", { class: "sess-bar" }, [
          h("i", {
            style: { width: `${width}%` },
            class: bar.count > 0 ? "is-on" : "",
          }),
        ]),
        h(
          "span",
          { class: "n sess-bar-value" },
          bar.hasSum ? tokens(bar.count) : ""
        ),
      ])
    );
  }
  return card;
}

// A card is rebuilt only when its own numbers changed; a row's tokens in the
// tree do not touch the cards.
function setSideCard(name, key, build) {
  if (state.sideKeys[name] === key) return;
  const host = state.shell.side;
  const cards = state.sideCards;
  const previous = cards[name];
  const card = build();
  if (previous && previous.parentNode) {
    previous.parentNode.replaceChild(card, previous);
  } else {
    const order = ["model", "switch", "last"];
    let next = null;
    for (let index = order.indexOf(name) + 1; index < order.length; index += 1) {
      if (cards[order[index]]) {
        next = cards[order[index]];
        break;
      }
    }
    host.insertBefore(card, next);
  }
  cards[name] = card;
  state.sideKeys[name] = key;
}

function renderSide() {
  if (!state.shell || !state.view || !state.sideCards) return;
  const rows = state.view.sessions;
  const models = groupsOf(rows, (row) => modelKey(row.model)).map((group) => {
    const model = group[0].model;
    const running = group.filter(isRunning);
    const sum = sumFor("model", model);
    return [
      model,
      running.length,
      running.filter((row) => statusName(row) === "busy").length,
      sum ? sum.lastHour.out : null,
      group.some((row) => isReported(row) && row.tied === true),
    ];
  });
  setSideCard(
    "model",
    JSON.stringify([models, rows.some((row) => isReported(row) && row.tied === true)]),
    modelCard
  );
  setSideCard("switch", JSON.stringify(state.view.switchEffect), switchCard);
  const bars = groupsOf(rows, (row) => projectKey(row.projectId)).map((group) => {
    const sum = sumFor("project", group[0].projectId);
    return {
      name: group[0].project || "unassigned",
      count: sum ? sum.lastHour.out : 0,
      hasSum: Boolean(sum),
    };
  });
  bars.sort((one, other) => other.count - one.count);
  setSideCard("last", JSON.stringify(bars), lastHourCard);
}

// ---------------------------------------------------------------------------
// the whole view
// ---------------------------------------------------------------------------

function renderView() {
  if (!state.active || !state.view || !state.shell) return;
  const token = ++state.renderToken;
  state.timeRequests = [];
  renderControls();
  renderHeadLines();
  renderKpis();
  renderTree();
  renderSide();
  fillTimes(token);
}

async function fillTimes(token) {
  const requests = state.timeRequests;
  if (!requests || requests.length === 0 || !state.context) return;
  let texts;
  try {
    texts = await state.context.invoke("time_texts", {
      times: requests.map((request) => ({ at: request.at, style: request.style })),
    });
    if (!Array.isArray(texts) || texts.length !== requests.length) {
      throw new Error("time_texts returned an unexpected reply");
    }
  } catch (error) {
    if (!state.active) return;
    if (token === state.renderToken) setError(`time_texts: ${errorMessage(error)}`);
    for (const request of requests) {
      request.node.textContent = "?";
      request.node.title = errorMessage(error);
    }
    return;
  }
  for (let index = 0; index < requests.length; index += 1) {
    const request = requests[index];
    request.node.textContent = `${request.prefix || ""}${texts[index] || ""}${
      request.suffix || ""
    }`;
  }
}
