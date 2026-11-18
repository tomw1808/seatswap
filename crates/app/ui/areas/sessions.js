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
  durationText,
  errorMessage,
  h,
  isActiveView,
  isObject,
  kpi,
  modelName,
  plural,
  readChoice,
  sessionName,
  showTokens,
  valueNode,
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
  { id: "day", label: "24h" },
  { id: "week", label: "7d" },
  { id: "month", label: "30d" },
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
  // The user's own folds by session: kept while the window's page is open
  // (rule R7). Neither a render nor a leave of the Sessions area clears it.
  userFolds: new Map(),
  openAgents: new Set(),
  departed: new Map(),
  marked: null,
  markTimer: null,
  minuteTimer: null,
  secondTimer: null,
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
      return `projectModel\u0000${projectKey(of.project)}\u0000${of.provider || ""}\u0000${modelKey(of.model)}`;
    case "providerModel":
      return `providerModel\u0000${of.provider || ""}\u0000${modelKey(of.model)}`;
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
      !Number.isFinite(sum.agentsRunning) ||
      !Number.isFinite(sum.agentsDone) ||
      !isObject(sum.tokens) ||
      (sum.lastHour !== null && !isObject(sum.lastHour)) ||
      !isObject(sum.lastHourValues) ||
      !isObject(sum.values)
    ) {
      throw new Error(
        "a sum in the view is not a sum: a group's row needs sessions, agentsRunning, agentsDone, tokens, lastHour (a token count or null), lastHourValues and values"
      );
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

// A group's sum by the same key `indexSums` stored it under: `value` is the
// provider, the project id or the model for those kinds, and an object with
// the of-fields for the two-part kinds.
function sumFor(kind, value) {
  const of =
    kind === "provider"
      ? { kind, provider: value }
      : kind === "project"
        ? { kind, project: value }
        : kind === "model"
          ? { kind, model: value }
          : { kind, ...value };
  return state.sums.get(sumKey(of)) || null;
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

function tokenNode(count, className) {
  if (count === null || count === undefined) {
    return h("span", { class: className || "n" }, "");
  }
  return showTokens(h("span", { class: className || "n" }), count);
}

// The reason a last hour is not known, or null when it is known. The view says
// so with `lastHour: null`, and names the reason in `lastHourValues`; a null
// without a reason is an error of the view, never a silent blank. A last hour
// that is neither a token count nor null is an error too: the page never fills
// one in (rule 2).
function lastHourReason(lastHour, lastHourValues) {
  if (!isObject(lastHourValues)) return "the view has no value for the last hour";
  if (lastHour !== null) {
    if (!isObject(lastHour) || !Number.isFinite(lastHour.out)) {
      throw new Error("a row's lastHour is neither a token count nor null");
    }
    return null;
  }
  if (typeof lastHourValues.apiMissing === "string" && lastHourValues.apiMissing !== "") {
    return lastHourValues.apiMissing;
  }
  if (
    typeof lastHourValues.subscriptionMissing === "string" &&
    lastHourValues.subscriptionMissing !== ""
  ) {
    return lastHourValues.subscriptionMissing;
  }
  return "the view does not say why the last hour is not known";
}

// A row or group cell: the hour's tokens, or "—" with the view's reason. A known
// hour without tokens is an empty cell, as before S-3.
function fillLastHour(node, lastHour, lastHourValues) {
  const reason = lastHourReason(lastHour, lastHourValues);
  clear(node);
  if (reason) {
    node.className = "n";
    node.title = reason;
    node.appendChild(document.createTextNode("—"));
    return;
  }
  node.removeAttribute("title");
  if (lastHour.out <= 0) {
    node.className = "n";
    return;
  }
  node.className = "n pri";
  showTokens(node, lastHour.out);
}

function lastHourCell(row) {
  const node = h("span", { class: "n" });
  fillLastHour(node, row.lastHour, row.lastHourValues);
  return node;
}

// The same hour as the KPI and the cards show it: a number, even a known 0, or
// "—" with the view's reason as its hover text. `title` is that reason or "".
function lastHourSummary(sum) {
  const reason = lastHourReason(sum.lastHour, sum.lastHourValues);
  if (reason) return { node: h("span", { title: reason }, "—"), title: reason };
  return { node: showTokens(h("span"), sum.lastHour.out), title: "" };
}

// A session's or an agent's elapsed time: the seconds since `since` in the one
// duration spelling of common.js. An unreadable time shows nothing.
function sinceText(since, now) {
  if (typeof since !== "string") return "";
  const start = Date.parse(since);
  if (!Number.isFinite(start)) return "";
  return durationText(Math.max(0, Math.floor((now - start) / 1000)));
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
  state.openAgents.clear();
  state.departed.clear();
  state.treeNodes = new Map();
  state.rowNodes = new Map();
  state.sums = new Map();
  state.marked = null;
  readChoices();
  buildShell(container);

  const onView = (payload) => {
    if (!isObject(payload) || start !== state.startToken) return;
    try {
      applyView(payload);
    } catch (error) {
      setError(`sessions-view: ${errorMessage(error)}`);
    }
  };
  const unlisten = await context.listen("sessions-view", onView);
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
  if (state.secondTimer) clearInterval(state.secondTimer);
  if (state.departTimer) clearTimeout(state.departTimer);
  state.markTimer = null;
  state.minuteTimer = null;
  state.secondTimer = null;
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
  // `state.userFolds` stays: the user's fold lasts while the window's page is
  // open (rule R7), not only while the Sessions area is shown.
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
  state.userFolds.set(id, true);
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
  // The error line exists only while there is an error: an empty, hidden node
  // with the error's class would be a second place the page's errors are read
  // from (`renderHeadLines` adds the class with the text).
  state.errorHost = h("p", { hidden: true });
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
      // Item 5: the API column's own currency, which is USD while the price
      // look-ups are off, not the settings' currency of the subscription column.
      // One view is valued in one currency; a row with a missing value still
      // names it, so the header does not turn into a word because one session
      // has no tokens yet.
      const valued = view.sessions.find(
        (row) => isObject(row.values) && typeof row.values.apiCurrency === "string"
      );
      const apiCurrency = valued ? valued.values.apiCurrency : null;
      state.apiHead.textContent = apiCurrency ? `API ${apiCurrency}` : "API value";
    }
    state.unavailableHost.hidden = !view.unavailable;
    state.unavailableHost.textContent = view.unavailable
      ? `unavailable: ${view.unavailable}`
      : "";
    updateHostSelect();
  }
  const errorText = [state.subscribeError, state.error].filter(Boolean).join(" · ");
  state.errorHost.hidden = !errorText;
  state.errorHost.className = errorText ? "sess-error" : "";
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
  state.accountValue = h("span", { class: "sess-inuse" }, [
    chip,
    h("span", { class: "sess-inuse-text" }, ` ${label}${percent}`),
  ]);
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
    all ? [showTokens(h("span"), all.tokens.out), " tokens"] : "no sum in the view",
    all ? valueSummary(all.values) : ""
  );
  // The last hour's two values come from the same `sums` the tree reads, summed
  // like `values`; a view whose sum has none is an error (indexSums). An hour
  // the view does not know is "—" with the view's reason, never a 0 (finding
  // 11).
  const lastHourValues = all ? all.lastHourValues : null;
  const lastHour = all ? lastHourSummary(all) : null;
  const lastHourValue = lastHour ? lastHour.node : "no sum in the view";
  kpis[4].update(
    "Last hour",
    lastHourValue,
    lastHourValues ? valueSummary(lastHourValues) : "",
    lastHour && lastHour.title ? lastHour.title : ""
  );
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

// The rows a group lists: the live view's rows, or the grey rows when every row
// of the group has left. A group always has at least one row.
function groupRows(rows, sampleRows) {
  const listed = rows.length ? rows : sampleRows || [];
  if (listed.length === 0) {
    throw new Error("a group of the Sessions tree has no row");
  }
  return listed;
}

function groupSubText(rows) {
  const running = rows.filter(isRunning);
  const unreported = rows.filter((row) => !isReported(row)).length;
  const busy = running.filter((row) => statusName(row) === "busy").length;
  const waiting = running.filter((row) => statusName(row) === "waiting").length;
  const agents = running.reduce(
    (count, row) => count + (Number.isFinite(row.agentsRunning) ? row.agentsRunning : 0),
    0
  );
  // The count is the rows listed under the group, running and closed (finding
  // 11), so it matches the tree below it; the rows without a report are named
  // beside it, as before.
  const parts = [plural(rows.length, "session", "sessions")];
  if (unreported) parts.push(`${unreported} without a report`);
  if (busy) parts.push(`${busy} busy`);
  if (waiting) parts.push(`${waiting} waiting for you`);
  if (agents) parts.push(`${plural(agents, "agent", "agents")} running`);
  return parts.join(" · ");
}

// A model group's total row: the number of session rows listed under it, so a
// stale row with no live report still counts as one; when every row of the group
// has left, the grey rows keep the group on screen and the count comes from
// them. By model already groups by the provider above, so the sub-row does not
// repeat it (item 23). The tied mark is the bubble `tiedNode`, never a text
// (item 1); the two provider rows say nothing about thinking.
function modelSubDetail(rows, model, sampleRows) {
  const listed = groupRows(rows, sampleRows);
  return plural(listed.length, "session", "sessions");
}

function modelSubTied(rows, model, sampleRows) {
  if (model === null) return false;
  const rowsForText = groupRows(rows, sampleRows);
  return rowsForText.some((row) => isReported(row) && row.tied === true);
}

function setValueText(node, values, kind, currency, className) {
  const value = valueText(values, kind, currency);
  node.className = className;
  if (value.title) node.title = value.title;
  else node.removeAttribute("title");
  if (node.textContent !== value.text) node.textContent = value.text;
}

// A total row (item 43): a project's or a provider's row, and a model's row
// under it. Every number of it comes from its own sum — agents, tokens, the last
// hour, both values — and the Model, Status and Host cells stay empty, as the
// mock-up's group rows have them. The sum's own reasons are shown, never a 0.
function createGroupItem(item) {
  const nameNode = h("b", { class: "sess-group-name", title: item.name }, item.name);
  const subNode = h("span", { class: "mf sess-group-sub" }, ` · ${item.sub}`);
  const nameCell = h(
    "span",
    { class: "sess-name", style: { paddingLeft: `${item.depth * 14}px` } },
    [nameNode, subNode]
  );
  const agentsNode = h("span", { class: "sess-agents" });
  const tokensNode = h("span", { class: "n b6" });
  const lastNode = h("span", { class: "n" });
  const valueNodes = [
    h("span", { class: "n b6" }),
    h("span", { class: "n b6" }),
  ];
  const node = h(
    "div",
    {
      class: item.depth === 0 ? "sess-row sess-group" : "sess-row sess-total",
    },
    [
      nameCell,
      h("span", {}, ""),
      h("span", {}, ""),
      agentsNode,
      tokensNode,
      lastNode,
      valueNodes[0],
      valueNodes[1],
      h("span", {}, ""),
    ]
  );
  const entry = {
    type: "group",
    node,
    nameNode,
    subNode,
    agentsNode,
    tokensNode,
    lastNode,
    valueNodes,
    tiedMark: null,
    signature: "",
  };
  updateGroupItem(entry, item);
  return entry;
}

function updateGroupItem(entry, item) {
  if (!Array.isArray(item.rows)) {
    throw new Error(`the group ${item.name} has no rows to follow`);
  }
  const tied = item.tied === true;
  const agentData = item.rows.some(hasAgentData);
  const signature = JSON.stringify([
    item.name,
    item.sub,
    item.sum,
    item.depth,
    tied,
    agentData,
    state.view.currency,
  ]);
  if (signature === entry.signature) return;
  entry.signature = signature;
  if (entry.nameNode.textContent !== item.name) entry.nameNode.textContent = item.name;
  if (entry.nameNode.title !== item.name) entry.nameNode.title = item.name;
  const sub = ` · ${item.sub}`;
  if (entry.subNode.textContent !== sub) entry.subNode.textContent = sub;
  const nameCell = entry.node.firstChild;
  nameCell.style.paddingLeft = `${item.depth * 14}px`;
  entry.node.className =
    item.depth === 0 ? "sess-row sess-group" : "sess-row sess-total";
  // The tied model's one mark: the bubble beside its name, in either grouping.
  if (tied && !entry.tiedMark) {
    entry.tiedMark = tiedNode({ tied: true });
    nameCell.insertBefore(entry.tiedMark, entry.subNode);
  } else if (!tied && entry.tiedMark) {
    entry.tiedMark.remove();
    entry.tiedMark = null;
  }
  const sum = item.sum;
  if (!sum) {
    const title = "this group has no sum in the view";
    fillAgents(entry.agentsNode, 0, 0, false);
    for (const node of [entry.tokensNode, entry.lastNode, ...entry.valueNodes]) {
      node.className = "n";
      node.title = title;
      node.textContent = "";
    }
    return;
  }
  // A total row whose rows all show "—" for agents shows "—" itself (item 24).
  fillAgents(entry.agentsNode, sum.agentsRunning, sum.agentsDone, agentData);
  entry.tokensNode.className = "n b6";
  entry.tokensNode.removeAttribute("title");
  showTokens(entry.tokensNode, sum.tokens.out);
  fillLastHour(entry.lastNode, sum.lastHour, sum.lastHourValues);
  setValueText(entry.valueNodes[0], sum.values, "subscription", state.view.currency, "n b6");
  setValueText(entry.valueNodes[1], sum.values, "api", state.view.currency, "n b6");
}

// A sub-row of an agent list (the running agents' provider · model line). It
// carries text only; `error` marks a line that names an error of the view.
function createSubItem(item) {
  const textNode = h("span", { class: "mf" }, item.text);
  const node = h("div", { class: "sess-row sess-subrow" }, textNode);
  const entry = {
    type: "sub",
    node,
    textNode,
    text: null,
    depth: -1,
    error: false,
  };
  updateSubItem(entry, item);
  return entry;
}

function updateSubItem(entry, item) {
  const error = item.error === true;
  if (entry.text === item.text && entry.depth === item.depth && entry.error === error) return;
  entry.text = item.text;
  entry.depth = item.depth;
  entry.error = error;
  entry.textNode.style.paddingLeft = `${Math.max(0, item.depth * 14 - 2)}px`;
  if (entry.textNode.textContent !== item.text) entry.textNode.textContent = item.text;
  entry.node.classList.toggle("sess-subrow-error", error);
}

// A session with no running and no done agent and no agent list has no agent
// data at all: it says "—", never "0 · 0" (item 24). A session with no running
// agent but a done one still says "0 · 3".
function hasAgentData(row) {
  return (
    (Number.isFinite(row.agentsRunning) && row.agentsRunning > 0) ||
    (Number.isFinite(row.agentsDone) && row.agentsDone > 0) ||
    sessionHasAgents(row)
  );
}

function fillAgents(node, running, done, hasData) {
  clear(node);
  if (!hasData) {
    node.appendChild(h("span", { class: "mf" }, "—"));
    return;
  }
  node.appendChild(
    h(
      "span",
      { class: running > 0 ? "pri b6" : "mf", title: "Agents running" },
      String(Number.isFinite(running) ? running : 0)
    )
  );
  node.appendChild(h("span", { class: "mf" }, " · "));
  node.appendChild(
    h(
      "span",
      { class: "mf", title: "Agents completed" },
      String(Number.isFinite(done) ? done : 0)
    )
  );
}

function agentsCell(running, done, hasData) {
  const node = h("span", { class: "sess-agents" });
  fillAgents(node, running, done, hasData);
  return node;
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
  if (status === "busy") return h("span", { class: "sw pri" }, `busy ${sinceText(row.since, now)}`);
  if (status === "idle") return h("span", { class: "sw" }, `idle ${sinceText(row.since, now)}`);
  if (status === "waiting") {
    return h("span", { class: "sw warn" }, `waiting for you ${sinceText(row.since, now)}`);
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
  const car = h("span", { class: options.details ? "sess-car" : "sess-car0" });
  const nameParts = [car];
  if (options.wide) {
    nameParts.push(
      h("span", { class: "sess-session-name" }, [
        ...sessionName(row),
        h("span", { class: "sess-name-project mf" }, ` · ${row.project}`),
      ])
    );
  } else {
    nameParts.push(h("span", { class: "sess-session-name" }, sessionName(row)));
  }
  const model = options.wide
    ? h("span", { class: "sess-model" }, [tiedNode(row)])
    : h("span", { class: "sess-model" }, [
        modelName(row.model),
        tiedNode(row),
      ]);
  const lastHour = lastHourCell(row);
  const subscription = valueNode(row.values, "subscription", currency);
  subscription.className = "n";
  const api = valueNode(row.values, "api", currency);
  api.className = "n";
  return [
    h(
      "span",
      { class: "sess-name", style: { paddingLeft: `${options.depth * 14}px` } },
      nameParts
    ),
    model,
    statusNode(row, now),
    agentsCell(row.agentsRunning, row.agentsDone, hasAgentData(row)),
    tokenNode(row.tokens ? row.tokens.out : 0),
    lastHour,
    subscription,
    api,
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

// The status duration the row shows (`busy 5s`), or "" for a status without
// one. It is in the signature, so a changed second redraws the row; the minute
// is not.
function statusDurationText(row, now) {
  const status = statusName(row);
  if (status !== "busy" && status !== "idle" && status !== "waiting") return "";
  return sinceText(row.since, now);
}

function sessionSignature(row, options, now) {
  return JSON.stringify([
    row.label,
    row.title,
    row.session,
    row.project,
    row.model,
    statusName(row),
    statusDurationText(row, now),
    row.since,
    row.tied,
    row.agentsRunning,
    row.agentsDone,
    row.tokens ? row.tokens.out : null,
    row.lastHour === null ? null : row.lastHour ? row.lastHour.out : undefined,
    row.lastHourValues,
    row.values,
    row.agentsDoneValues,
    row.host,
    options.wide,
    options.depth,
    options.departed === true,
  ]);
}

// The agents of an open session are drawn again when one of them changed or a
// running one's shown duration changed. The signature holds the duration text,
// so no minute is in it.
function agentSignature(row, now) {
  const agents = Array.isArray(row.agents) ? row.agents : [];
  return JSON.stringify(row.agentsDoneValues) + "\u0003" + agents
    .map((agent) =>
      JSON.stringify([
        agent.agent,
        agent.parent,
        agent.kind,
        agent.task,
        agent.provider,
        agent.model,
        agent.running === true,
        agent.running === true ? sinceText(agent.first || agent.last, now) : "",
        agent.first,
        agent.last,
        agent.tokens ? agent.tokens.out : null,
        agent.values,
      ])
    )
    .join("\u0001");
}

// Rule R7: a session is open while it has agents running, and a fold the user
// set by hand keeps that session's choice while the window is open. The map is
// the only record of the user's choice; there is no second set of open rows.
function sessionOpen(row) {
  return state.userFolds.has(row.session)
    ? state.userFolds.get(row.session)
    : row.agentsRunning > 0;
}

// The user's fold, if the row shows one: a click changes `details.open` at
// once, while its `toggle` event comes later as a task. Any open state that
// differs from what this code last wrote (`entry.shownOpen`) is therefore the
// user's, whether a `toggle` reports it or a view lands before that task. Both
// the listener and `updateSessionItem` call this before they touch `.open`, so
// a view can never overwrite a click it has not recorded yet.
function noteUserFold(entry) {
  const open = entry.node.open;
  if (open === entry.shownOpen) return;
  state.userFolds.set(entry.row.session, open);
  entry.shownOpen = open;
}

function createSessionItem(row, options) {
  const details = sessionHasAgents(row);
  const open = sessionOpen(row);
  const rowNode = h(details ? "summary" : "div", { class: sessionClasses(row, options) });
  for (const cell of sessionCells(row, { ...options, details }, Date.now())) {
    rowNode.appendChild(cell);
  }
  rowNode.dataset.session = row.session;
  state.rowNodes.set(row.session, rowNode);
  const now = Date.now();
  const entry = {
    type: "session",
    row,
    options,
    structure: sessionStructure(row, options),
    signature: sessionSignature(row, options, now),
    agentsSignature: "",
    node: rowNode,
    rowNode,
    children: null,
    // The open state this code last wrote. `null` for a row without details.
    shownOpen: null,
    build: null,
  };
  if (!details) {
    entry.agentsSignature = agentSignature(row, now);
    return entry;
  }

  const container = h("details", { class: "sess-details", open });
  container.appendChild(rowNode);
  const children = h("div", { class: "sess-children" });
  container.appendChild(children);
  entry.node = container;
  entry.children = children;
  entry.shownOpen = open;
  const build = () => {
    if (children.dataset.built === "1") return;
    renderAgentList(children, entry.row, null, 4, entry.row.session);
    children.dataset.built = "1";
    entry.agentsSignature = agentSignature(entry.row, Date.now());
  };
  entry.build = build;
  if (open) build();
  container.addEventListener("toggle", () => {
    // Code sets `open` too (creation and `updateSessionItem`); `shownOpen` says
    // what it last wrote, so only a different value here is the user's click.
    noteUserFold(entry);
    if (container.open) build();
  });
  return entry;
}

// The row keeps its DOM node; only a changed row has its cells written again.
// Its children are built only while the row is open.
function updateSessionItem(entry, row, options) {
  entry.row = row;
  entry.options = options;
  const now = Date.now();
  const signature = sessionSignature(row, options, now);
  if (signature !== entry.signature) {
    entry.signature = signature;
    entry.rowNode.className = sessionClasses(row, options);
    clear(entry.rowNode);
    for (const cell of sessionCells(row, { ...options, details: entry.children !== null }, now)) {
      entry.rowNode.appendChild(cell);
    }
  }
  if (entry.children) {
    // An existing row follows the running agents: it opens when the answer
    // becomes true and closes when it becomes false, unless the user folded
    // it by hand. A click whose `toggle` task has not run yet is recorded
    // first, so this write cannot swallow it.
    noteUserFold(entry);
    const open = sessionOpen(row);
    if (entry.node.open !== open) entry.node.open = open;
    entry.shownOpen = open;
    if (entry.children.dataset.built === "1") {
      const signatureOfAgents = agentSignature(row, now);
      if (signatureOfAgents !== entry.agentsSignature) {
        entry.agentsSignature = signatureOfAgents;
        clear(entry.children);
        delete entry.children.dataset.built;
        entry.build();
      }
    } else if (open) {
      entry.build();
    }
  }
}

function agentStatus(agent, now) {
  if (agent.running) {
    const since = agent.first || agent.last;
    const duration = since ? sinceText(since, now) : "";
    return h("span", { class: "sw pri" }, duration ? `running ${duration}` : "running");
  }
  const span = h("span", { class: "sw ok" });
  appendText(span, "done ");
  if (agent.last) span.appendChild(timeNode(agent.last, "dayClock"));
  return span;
}

// An agent's kind: a flash job's is the small "flash" tag (S-3), Claude Code's
// default `general-purpose` and an agent without a kind show none, every other
// kind is the bold word it always was. Null when the row shows no kind.
function agentKindNode(agent) {
  if (agent.kind === "flash") {
    return h(
      "span",
      { class: "sess-tag flash", title: "a flash job: its workers run under it" },
      "flash"
    );
  }
  if (typeof agent.kind !== "string" || agent.kind === "" || agent.kind === "general-purpose") {
    return null;
  }
  return h("b", {}, agent.kind);
}

// The agent's name cell: its kind, then its task in grey; without a kind the
// task alone in the normal colour; with neither the word "agent", so the row
// is never empty. A task cut by the column is whole in the hover text.
function agentNameNode(agent) {
  const kind = agentKindNode(agent);
  const task = typeof agent.task === "string" && agent.task !== "" ? agent.task : null;
  const name = h("span", { class: "sess-agent-name", title: task });
  if (kind) {
    name.appendChild(kind);
    if (task) name.appendChild(h("span", { class: "mf" }, ` ${task}`));
  } else {
    name.appendChild(task ? h("span", {}, task) : h("b", {}, "agent"));
  }
  return name;
}

function agentCells(agent, children, depth) {
  const now = Date.now();
  const view = state.view;
  const name = agentNameNode(agent);
  const running = children.filter((child) => child.running).length;
  const done = children.length - running;
  const subscription = valueNode(agent.values, "subscription", view.currency);
  subscription.className = "n";
  const api = valueNode(agent.values, "api", view.currency);
  api.className = "n";
  return [
    h(
      "span",
      { class: "sess-name", style: { paddingLeft: `${depth * 14}px` } },
      [h("span", { class: children.length ? "sess-car" : "sess-car0" }), name]
    ),
    h("span", { class: "sess-model" }, modelName(agent.model)),
    agentStatus(agent, now),
    children.length ? agentsCell(running, done, true) : h("span", {}, ""),
    tokenNode(agent.tokens ? agent.tokens.out : 0),
    h("span", { class: "n" }, ""),
    subscription,
    api,
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

function completedNode(session, agents, depth, parentKey, values) {
  const key = `${parentKey}\u0000completed`;
  const open = state.openAgents.has(key);
  const total = agents.reduce(
    (count, agent) => count + (agent.tokens && Number.isFinite(agent.tokens.out) ? agent.tokens.out : 0),
    0
  );
  // `values` is the view's `agentsDoneValues`, or a reason why this line has
  // none; either way the cell says "—" with the reason rather than nothing.
  const lineValues =
    typeof values === "string"
      ? { subscription: null, subscriptionMissing: values, api: null, apiMissing: values }
      : values;
  const subscription = valueNode(lineValues, "subscription", state.view.currency);
  subscription.className = "n";
  const api = valueNode(lineValues, "api", state.view.currency);
  api.className = "n";
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
    subscription,
    api,
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

// The agent's own provider (`Claude Code`, `DeepSeek Harness`, …), or null when
// the view gave it none.
function agentProvider(agent) {
  return typeof agent.provider === "string" && agent.provider !== "" ? agent.provider : null;
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
  // Running agents are grouped by their own provider and model: a flash job
  // under a Claude Code session is a DeepSeek Harness agent. An agent without a
  // provider is an error line naming the session, never a guessed provider.
  for (const group of groupsOf(
    running,
    (agent) => `${agentProvider(agent) || ""}\u0000${modelKey(agent.model)}`
  )) {
    const provider = agentProvider(group[0]);
    const model = modelName(group[0].model);
    const count = plural(group.length, "agent", "agents");
    container.appendChild(
      createSubItem({
        text: provider
          ? `${provider} · ${model} · ${count}`
          : `error: an agent of the session ${session.session} has no provider · ${model} · ${count}`,
        depth,
        error: provider === null,
      }).node
    );
    for (const agent of group) {
      container.appendChild(
        agentNode(session, agent, childAgents(session, agent.agent), depth)
      );
    }
  }
  // The view carries one summed value for all of a session's done agents. Only
  // the session's own completed line can show it, and only when that line holds
  // exactly those agents; otherwise the line says why it has no value. A view
  // row without the field (a host's report from an older seatswap) shows "—"
  // with the view's own reason.
  if (done.length) {
    const allDone = agents.filter((agent) => !agent.running);
    let values;
    if (parentId !== null) {
      values = "the view has no value per parent agent";
    } else if (allDone.length !== done.length) {
      values = "the value of the done agents includes agents started by other agents; it is in the session's value";
    } else {
      values = session.agentsDoneValues;
    }
    container.appendChild(completedNode(session, done, depth, parentKey, values));
  }
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
      rows: viewRows,
      tied: false,
      depth: 0,
    });
    for (const providerRows of providerGroups(group.entries.map((entry) => entry.row))) {
      const provider = providerRows[0].provider || "";
      for (const modelRows of modelGroups(providerRows)) {
        const model = modelRows[0].model;
        const leaf = group.entries.filter(
          (entry) =>
            (entry.row.provider || "") === provider &&
            modelKey(entry.row.model) === modelKey(model)
        );
        const viewLeafRows = leaf.filter((entry) => !entry.departed).map((entry) => entry.row);
        const sampleLeafRows = leaf.map((entry) => entry.row);
        items.push({
          type: "group",
          key: `projectModel\u0000${group.key}\u0000${provider}\u0000${modelKey(model)}`,
          name: model === null ? modelName(model) : `${provider} · ${modelName(model)}`,
          sub: modelSubDetail(viewLeafRows, model, sampleLeafRows),
          sum: sumFor("projectModel", {
            project: group.projectId,
            provider,
            model,
          }),
          rows: viewLeafRows.length ? viewLeafRows : sampleLeafRows,
          tied: modelSubTied(viewLeafRows, model, sampleLeafRows),
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
      rows: viewRows,
      tied: false,
      depth: 0,
    });
    for (const modelRows of modelGroups(group.entries.map((entry) => entry.row))) {
      const model = modelRows[0].model;
      const leaf = group.entries.filter(
        (entry) => modelKey(entry.row.model) === modelKey(model)
      );
      const viewLeafRows = leaf.filter((entry) => !entry.departed).map((entry) => entry.row);
      const sampleLeafRows = leaf.map((entry) => entry.row);
      items.push({
        type: "group",
        key: `model\u0000${group.provider}\u0000${modelKey(model)}`,
        name: modelName(model),
        sub: modelSubDetail(viewLeafRows, model, sampleLeafRows),
        sum: sumFor("providerModel", { provider: group.provider, model }),
        rows: viewLeafRows.length ? viewLeafRows : sampleLeafRows,
        tied: modelSubTied(viewLeafRows, model, sampleLeafRows),
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
    const last = sum ? lastHourSummary(sum).node : h("span", { title: "this model has no sum in the view" }, "");
    last.classList.add("n");
    const tied = group.some((row) => isReported(row) && row.tied === true);
    body.appendChild(
      h("div", { class: "sess-mini-row" }, [
        h("span", { class: "sess-mini-name" }, [
          modelName(model),
          tied ? tiedNode({ tied: true }) : null,
        ]),
        h("span", { class: "n" }, String(running.length)),
        h("span", { class: "n" }, String(busy)),
        last,
      ])
    );
  }
  if (rows.some((row) => isReported(row) && row.tied === true)) {
    body.appendChild(
      h("p", { class: "sess-card-note mf" }, [
        tiedNode({ tied: true }),
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

// A project's last hour for the side card: the sum's hour (null when the view
// does not know it) and the two figures of the mock-up. The card and its rebuild
// key read the same shape.
function lastHourBars(rows) {
  return groupsOf(rows, (row) => projectKey(row.projectId)).map((group) => {
    const sum = sumFor("project", group[0].projectId);
    return {
      name: group[0].project || "unassigned",
      lastHour: sum ? sum.lastHour : null,
      hasSum: Boolean(sum),
      values: sum ? sum.lastHourValues : null,
    };
  });
}

function lastHourCard() {
  const { card, body } = sideCard("Last hour", "by project");
  const bars = lastHourBars(state.view.sessions);
  const countOf = (bar) => (bar.lastHour ? bar.lastHour.out : null);
  bars.sort((one, other) => {
    const a = Number.isFinite(countOf(one)) ? countOf(one) : -1;
    const b = Number.isFinite(countOf(other)) ? countOf(other) : -1;
    return b - a;
  });
  if (bars.length === 0) {
    body.appendChild(h("p", { class: "sess-empty mf" }, "No sessions."));
    return card;
  }
  const max = bars.reduce(
    (top, bar) => (Number.isFinite(countOf(bar)) ? Math.max(top, countOf(bar)) : top),
    0
  );
  for (const bar of bars) {
    const count = countOf(bar);
    const width = Number.isFinite(count) && max > 0 ? Math.round((count / max) * 100) : 0;
    // The mock-up's second figure after the tokens: the last hour's
    // subscription value, with its currency (there is no currency header here).
    const value = bar.values ? valueText(bar.values, "subscription", null) : null;
    const summary = bar.hasSum
      ? lastHourSummary({ lastHour: bar.lastHour, lastHourValues: bar.values })
      : { node: null, title: "" };
    const title = summary.title || (value && value.title ? value.title : "");
    const text =
      summary.title || !value ? [summary.node] : [summary.node, ` · ${value.text}`];
    body.appendChild(
      h("div", { class: "sess-bar-row" }, [
        h("span", { class: "sess-bar-name" }, bar.name),
        h("span", { class: "sess-bar" }, [
          h("i", {
            style: { width: `${width}%` },
            class: Number.isFinite(count) && count > 0 ? "is-on" : "",
          }),
        ]),
        h("span", { class: "n sess-bar-value", title: title || null }, text),
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
      sum ? sum.lastHour : null,
      group.some((row) => isReported(row) && row.tied === true),
    ];
  });
  setSideCard(
    "model",
    JSON.stringify([models, rows.some((row) => isReported(row) && row.tied === true)]),
    modelCard
  );
  setSideCard("switch", JSON.stringify(state.view.switchEffect), switchCard);
  const bars = lastHourBars(rows);
  const countOf = (bar) => (bar.lastHour ? bar.lastHour.out : null);
  bars.sort((one, other) => {
    const a = Number.isFinite(countOf(one)) ? countOf(one) : -1;
    const b = Number.isFinite(countOf(other)) ? countOf(other) : -1;
    return b - a;
  });
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
  updateSecondTimer();
}

// Is a shown duration under a minute ("busy 5s", "running 5s")? Only the rows
// on screen count; the timer exists while one of them does.
function showsSeconds() {
  if (!state.treeScroll) return false;
  return Array.from(state.treeScroll.querySelectorAll(".sw")).some(
    (node) => /\b\d+s\b/.test(node.textContent || "") && node.getClientRects().length > 0
  );
}

// A duration under a minute is redrawn every second; while every shown duration
// is a minute or longer no one-second timer runs (finding 16). The minute timer
// above still redraws a row when its "2h 31min" changes.
function updateSecondTimer() {
  if (!state.active) return;
  const needed = showsSeconds();
  if (needed && !state.secondTimer) {
    state.secondTimer = setInterval(() => {
      if (state.active && state.view) renderView();
    }, 1000);
  } else if (!needed && state.secondTimer) {
    clearInterval(state.secondTimer);
    state.secondTimer = null;
  }
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
