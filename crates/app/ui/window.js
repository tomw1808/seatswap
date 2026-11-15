// seatswap's large window (spec §3): the area tabs, the area's head and its
// content. Plain ES2020, no build step. Talks to the tray app through the Tauri
// 2 global API; without it, runs on the shared built-in fixture.

import {
  HAS_TAURI,
  applyTheme,
  clear,
  errorMessage,
  h,
  invoke,
  isObject,
  listen,
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
    sub: "every setting of seatswap · the gear in the pop-up opens this",
  },
];

// Shown for an area whose page does not exist yet (no file under ui/areas/).
const NOT_YET = "Comes with a later version.";

const state = {
  // The target as it came from the hash or `open-area`; null only before boot.
  target: null,
  area: null,
  sub: null,
  snapshot: null,
  error: null,
};

const modules = new Map();
// A later hash or `open-area` wins over a dynamic import still in flight.
let renderToken = 0;

function findArea(id) {
  return AREAS.find((area) => area.id === id) || null;
}

// "settings/accounts" -> { area: "settings", sub: "accounts" }; "" -> sessions.
// The same shape as gui/window.rs's `parse_target`: one optional sub, no empty.
function parseTarget(target) {
  if (typeof target !== "string") return null;
  const slash = target.indexOf("/");
  const area = slash < 0 ? target : target.slice(0, slash);
  if (!findArea(area)) return null;
  if (slash < 0) return { area, sub: null };
  const sub = target.slice(slash + 1);
  if (!sub || sub.includes("/")) return null;
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

function wireTheme() {
  if (!window.matchMedia) return;
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  if (typeof media.addEventListener === "function") {
    media.addEventListener("change", () => {
      if (themeSetting() === "system") applyTheme("system");
    });
  }
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
    title.textContent = "Unknown area";
    sub.textContent = "";
    sub.hidden = true;
    return;
  }
  title.textContent = area.title;
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
      import(`./areas/${id}.js`).catch((error) => {
        // A module that does not exist yet is the normal case until its task
        // builds it. The real reason goes to the console, where a broken
        // module is not swallowed.
        console.warn(`[window] cannot load areas/${id}.js`, error);
        return null;
      })
    );
  }
  return modules.get(id);
}

function message(text, className) {
  return h("p", { class: className ? `area-message ${className}` : "area-message" }, text);
}

// Render the current area into the container. A module that is not there yet
// says so; a module that fails to render says what went wrong.
async function renderArea() {
  const container = document.getElementById("area");
  if (!container) return;
  const token = ++renderToken;
  clear(container);
  if (state.error) container.appendChild(message(state.error, "error"));

  const area = findArea(state.area);
  if (!area) {
    container.appendChild(
      message(`Unknown area ${JSON.stringify(state.target === null ? "" : state.target)}.`)
    );
    return;
  }

  const module = await loadArea(area.id);
  if (token !== renderToken) return;
  if (!module || typeof module.render !== "function") {
    container.appendChild(message(NOT_YET));
    return;
  }
  try {
    await module.render(container, {
      sub: state.sub,
      snapshot: state.snapshot,
      invoke,
      listen,
    });
  } catch (error) {
    if (token !== renderToken) return;
    clear(container);
    container.appendChild(
      message(`Cannot show ${area.title}: ${errorMessage(error)}`, "error")
    );
  }
}

// Show a target and, unless it is the address already, put it there too.
function open(target) {
  if (location.hash.slice(1) === target) {
    show(target);
    return;
  }
  location.hash = target;
}

function show(target) {
  state.target = typeof target === "string" ? target : "";
  const parsed = parseTarget(state.target);
  state.area = parsed ? parsed.area : null;
  state.sub = parsed ? parsed.sub : null;
  renderHead();
  renderArea();
}

async function boot() {
  if (!HAS_TAURI) ensureFixture();
  applyTheme("system");
  wireTheme();

  await listen("open-area", (payload) => {
    const target = typeof payload === "string" ? payload : payload && payload.target;
    if (typeof target === "string" && target) open(target);
  });
  await listen("snapshot", (payload) => {
    if (!isObject(payload)) return;
    state.snapshot = payload;
    state.error = null;
    applyTheme(themeSetting());
    renderArea();
  });

  window.addEventListener("hashchange", () => show(hashTarget()));

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
