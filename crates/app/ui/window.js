// seatswap's large window (spec §3): the area tabs, the area's head and its
// content. Plain ES2020, no build step. Talks to the tray app through the Tauri
// 2 global API; without it, runs on the shared built-in fixture.

import {
  HAS_TAURI,
  applyTheme,
  banner,
  clear,
  errorMessage,
  followTheme,
  h,
  invoke,
  isObject,
  listen,
  message,
} from "./common.js";
import { currentFixture, ensureFixture } from "./fixture.js";

// The four areas, in the mock-up's order (`nav()` in specs/page-build/data.py).
const AREAS = [
  { id: "sessions", title: "Sessions" },
  { id: "statistics", title: "Statistics" },
  { id: "statusline", title: "Status line" },
  {
    id: "settings",
    title: "Settings",
    // The mock-up's Settings head has no h1: its sub-line sits directly under
    // the tabs (decision 26). The tab still says "Settings".
    noTitle: true,
    sub: "every setting of seatswap · the gear in the pop-up opens this",
  },
];

const state = {
  // The target as it came from the hash or `open-area`; null only before boot.
  target: null,
  area: null,
  sub: null,
  snapshot: null,
  error: null,
};

const modules = new Map();
// The loaded module by area, kept apart from its import Promise: a leave while
// the first render is still in flight must be able to call its `unmount`.
const loaded = new Map();
// A later hash or `open-area` wins over a dynamic import still in flight.
let renderToken = 0;
// The module that last rendered, so a snapshot can update it in place.
let rendered = { area: null, module: null, busy: false, snapshot: null };
// The target an `open-area` asked for; its `hashchange` is an open request.
let openTarget = null;

function findArea(id) {
  return AREAS.find((area) => area.id === id) || null;
}

// "settings/accounts" -> { area: "settings", sub: "accounts" };
// "sessions?<id>" -> { area: "sessions", sub: "<id>" }; "" -> sessions.
// The same shape as gui/window.rs's `parse_target`: one optional sub-area, one
// `?` item only for sessions, no empty, and names of `a-z`, `0-9` and `-`.
function isSlug(value) {
  return typeof value === "string" && /^[a-z0-9-]+$/.test(value);
}

function isSubArea(sub) {
  const mark = sub.indexOf("?");
  if (mark < 0) return isSlug(sub);
  return isSlug(sub.slice(0, mark)) && isSlug(sub.slice(mark + 1));
}

function parseTarget(target) {
  if (typeof target !== "string") return null;
  if (!target.includes("/")) {
    const mark = target.indexOf("?");
    const area = mark < 0 ? target : target.slice(0, mark);
    const item = mark < 0 ? null : target.slice(mark + 1);
    if (!findArea(area)) return null;
    if (item === null) return { area, sub: null };
    if (area !== "sessions" || !isSlug(item)) return null;
    return { area, sub: item };
  }
  const parts = target.split("/");
  const area = parts[0];
  if (!findArea(area)) return null;
  if (parts.length === 1) return { area, sub: null };
  if (parts.length > 2) return null;
  const sub = parts[1];
  if (!sub || !isSubArea(sub)) return null;
  return { area, sub };
}

// The area the address names; an empty hash opens Sessions.
function hashTarget() {
  const raw = location.hash.startsWith("#") ? location.hash.slice(1) : "";
  return raw === "" ? "sessions" : raw;
}

function themeSetting() {
  const settings = state.snapshot && state.snapshot.settings;
  return isObject(settings) && settings.theme ? settings.theme : "system";
}

// The area tabs, the current one marked, as the mock-up's `nav()`.
function renderHead() {
  const tabs = document.getElementById("area-tabs");
  const title = document.getElementById("area-title");
  const sub = document.getElementById("area-sub");
  if (!tabs || !title || !sub) return;

  clear(tabs);
  for (const area of AREAS) {
    const current = area.id === state.area;
    tabs.appendChild(
      h(
        "button",
        {
          type: "button",
          class: `tab${current ? " ison" : ""}`,
          "aria-pressed": current ? "true" : "false",
          onclick: () => open(area.id),
        },
        area.title
      )
    );
  }

  const area = findArea(state.area);
  if (!area) {
    title.hidden = false;
    title.textContent = "Unknown area";
    sub.textContent = "";
    sub.hidden = true;
    return;
  }
  title.hidden = area.noTitle === true;
  title.textContent = title.hidden ? "" : area.title;
  if (area.sub) {
    sub.textContent = area.sub;
    sub.hidden = false;
  } else {
    sub.textContent = "";
    sub.hidden = true;
  }
}

async function loadArea(id) {
  if (!modules.has(id)) {
    modules.set(
      id,
      import(`./areas/${id}.js`)
        .then((module) => {
          if (!module || typeof module.render !== "function") {
            throw new Error(`areas/${id}.js has no render function`);
          }
          loaded.set(id, module);
          return module;
        })
        .catch((error) => {
          // A broken module (a syntax error, a bad import) must not be
          // swallowed: its reason goes to the console, and `renderArea` puts
          // the same message on the page.
          console.warn(`[window] cannot load areas/${id}.js`, error);
          throw error;
        })
    );
  }
  return modules.get(id);
}

// The snapshot's error, in the pop-up's banner above the area. The host sits
// between the head and the area so an area module that clears its container
// cannot remove it.
function renderSnapshotBanner() {
  const container = document.getElementById("area");
  if (!container || !container.parentNode) return;
  let host = document.getElementById("snapshot-banner");
  if (!host) {
    host = h("div", { id: "snapshot-banner", class: "area-banner" });
    container.parentNode.insertBefore(host, container);
  }
  clear(host);
  const text = state.snapshot && state.snapshot.error;
  host.hidden = !text;
  if (text) host.appendChild(banner("error", "⚠", String(text)));
}

// Render the current area into the container. The old content stays until the
// module replaces it, so a slow load is not a blank area. A module that is not
// there yet says so; a module that fails to render says what went wrong.
async function renderArea(options = {}) {
  const container = document.getElementById("area");
  if (!container) return;
  const token = ++renderToken;
  renderSnapshotBanner();

  const area = findArea(state.area);
  if (!area) {
    rendered = { area: null, module: null, busy: false, snapshot: state.snapshot };
    clear(container);
    if (state.error) container.appendChild(message(state.error, "error"));
    container.appendChild(
      message(`Unknown area ${JSON.stringify(state.target === null ? "" : state.target)}.`)
    );
    return;
  }

  let module;
  try {
    module = await loadArea(area.id);
  } catch (error) {
    if (token !== renderToken) return;
    rendered = { area: area.id, module: null, busy: false, snapshot: state.snapshot };
    clear(container);
    if (state.error) container.appendChild(message(state.error, "error"));
    container.appendChild(
      message(`This page could not be loaded: ${errorMessage(error)}`, "error")
    );
    return;
  }
  if (token !== renderToken) return;
  const snapshot = state.snapshot;
  rendered = { area: area.id, module: null, busy: true, snapshot };
  try {
    await module.render(container, {
      sub: state.sub,
      snapshot,
      invoke,
      listen,
      fromOpen: options.fromOpen === true,
    });
  } catch (error) {
    if (token !== renderToken) return;
    rendered = { area: area.id, module: null, busy: false, snapshot };
    clear(container);
    container.appendChild(
      message(`Cannot show ${area.title}: ${errorMessage(error)}`, "error")
    );
    return;
  }
  if (token !== renderToken) return;
  rendered = { area: area.id, module, busy: false, snapshot };
  // A snapshot arrived while the module rendered: apply it now.
  if (state.snapshot !== snapshot) {
    if (typeof module.update === "function") await updateArea();
    else await renderArea();
  }
}

// A snapshot updates an area that has an `update` in place; an area without one
// is rendered again, and so is one that has not rendered yet. An update takes
// the latest snapshot even when another one arrives while it runs.
async function updateArea() {
  const current = rendered;
  if (!state.area || current.busy) return;
  if (current.area !== state.area) return; // its render is starting or in flight
  if (!current.module) {
    await renderArea();
    return;
  }
  if (typeof current.module.update !== "function") {
    await renderArea();
    return;
  }
  current.busy = true;
  try {
    let snapshot = state.snapshot;
    do {
      current.snapshot = snapshot;
      await current.module.update(snapshot);
      snapshot = state.snapshot;
    } while (rendered === current && snapshot !== current.snapshot);
  } catch (error) {
    if (rendered !== current) return;
    const container = document.getElementById("area");
    const area = findArea(state.area);
    if (container) {
      clear(container);
      container.appendChild(
        message(
          `Cannot update ${area ? area.title : "the area"}: ${errorMessage(error)}`,
          "error"
        )
      );
    }
    rendered = { area: state.area, module: null, busy: false, snapshot: state.snapshot };
    return;
  } finally {
    current.busy = false;
  }
}

// Show a target and, unless it is the address already, put it there too. An
// `open-area` request tells the area's render, so an item it names is applied
// even when the address already names the same item.
function open(target) {
  if (location.hash.slice(1) === target) {
    show(target, true);
    return;
  }
  openTarget = target;
  location.hash = target;
}

// An area module may hold a subscription or a listener; leaving it asks the
// module to let go. `rendered.module` is empty while the module's first render
// is in flight, so the loaded module is asked from `loaded` as well.
function unloadArea() {
  const area = rendered.area || state.area;
  if (!area) return;
  const module = rendered.module || loaded.get(area) || null;
  if (!module || typeof module.unmount !== "function") return;
  try {
    module.unmount();
  } catch (error) {
    console.warn(`[window] cannot leave areas/${area}.js`, error);
  }
}

function show(target, fromOpen = false) {
  state.target = typeof target === "string" ? target : "";
  const parsed = parseTarget(state.target);
  const nextArea = parsed ? parsed.area : null;
  if (state.area && nextArea !== state.area) unloadArea();
  state.area = nextArea;
  state.sub = parsed ? parsed.sub : null;
  renderHead();
  renderArea({ fromOpen });
}

async function boot() {
  if (!HAS_TAURI) ensureFixture();
  applyTheme("system");
  followTheme(themeSetting);

  await listen("open-area", (payload) => {
    const target = typeof payload === "string" ? payload : payload && payload.target;
    if (typeof target === "string" && target) open(target);
  });
  await listen("snapshot", (payload) => {
    if (!isObject(payload)) return;
    state.snapshot = payload;
    // A failed first read drew its message into the area: a full render
    // takes it away. Otherwise the area updates in place.
    const hadError = state.error !== null;
    state.error = null;
    applyTheme(themeSetting());
    renderSnapshotBanner();
    if (hadError) renderArea();
    else updateArea();
  });

  window.addEventListener("hashchange", () => {
    const target = hashTarget();
    const fromOpen = openTarget === target;
    openTarget = null;
    show(target, fromOpen);
  });

  if (HAS_TAURI) {
    try {
      state.snapshot = await invoke("get_snapshot", {});
    } catch (error) {
      state.error = errorMessage(error);
    }
  } else {
    state.snapshot = currentFixture();
  }
  applyTheme(themeSetting());
  show(hashTarget());
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot, { once: true });
} else {
  boot();
}
