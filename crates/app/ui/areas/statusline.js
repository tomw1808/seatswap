// The Status line area of the large window (spec §5.4): the palette, the two
// zones, the chosen segment's options, the preview, the install card and the
// recorded field names. The page edits `settings.statusline`; every change is
// saved through `save_setting` at once, and a refused save puts the stored
// value back. The preview's text and colours come only from
// `statusline_preview`, never from this file. Plain ES2020, no framework.

import {
  banner,
  clear,
  clone,
  dateTimeText,
  errorMessage,
  focusInside,
  h,
  isColor,
  isObject,
  message,
  panel,
  selectInput,
  switchControl,
} from "../common.js";

// One table for the segment kinds: the groups, the names the page uses, and
// the options that apply to each kind. It is read from the Rust types
// (`SegmentKind`, `TimeFormat`, `ColorRule` and `render.rs`): only a kind with a
// percentage can use "by load", only the model can use "by model family", only
// the effort can use "by effort", only the account can use its own colour, and
// only the two reset segments use a time format and only Text uses its text.
const KINDS = [
  { id: "account", label: "account", group: "seatswap", colors: ["none", "account", "fixed"] },
  { id: "plan", label: "plan", group: "seatswap", colors: ["none", "fixed"] },
  {
    id: "fiveHourPercent",
    label: "5 h percent",
    group: "seatswap",
    colors: ["none", "byLoad", "fixed"],
  },
  {
    id: "fiveHourReset",
    label: "5 h reset",
    group: "seatswap",
    colors: ["none", "fixed"],
    time: true,
  },
  { id: "weekPercent", label: "week percent", group: "seatswap", colors: ["none", "byLoad", "fixed"] },
  { id: "weekReset", label: "week reset", group: "seatswap", colors: ["none", "fixed"], time: true },
  {
    id: "modelPercent",
    label: "per-model %",
    group: "seatswap",
    colors: ["none", "byLoad", "fixed"],
  },
  { id: "autoBurn", label: "auto-burn: next", group: "seatswap", colors: ["none", "fixed"] },
  {
    id: "model",
    label: "model",
    group: "claudeCode",
    colors: ["none", "byModelFamily", "fixed"],
  },
  { id: "effort", label: "effort", group: "claudeCode", colors: ["none", "byEffort", "fixed"] },
  { id: "folder", label: "folder", group: "claudeCode", colors: ["none", "fixed"] },
  { id: "gitBranch", label: "git branch", group: "claudeCode", colors: ["none", "fixed"] },
  { id: "contextUsed", label: "context used", group: "claudeCode", colors: ["none", "fixed"] },
  { id: "sessionCost", label: "session cost", group: "claudeCode", colors: ["none", "fixed"] },
  { id: "linesChanged", label: "lines changed", group: "claudeCode", colors: ["none", "fixed"] },
  { id: "userHost", label: "user@host", group: "system", colors: ["none", "fixed"] },
  { id: "clock", label: "clock", group: "system", colors: ["none", "fixed"] },
  {
    id: "text",
    label: "free text",
    group: "system",
    colors: ["none", "fixed"],
    text: true,
    repeatable: true,
  },
  {
    id: "separator",
    label: "separator",
    group: "system",
    colors: ["none", "fixed"],
    repeatable: true,
  },
];

const GROUPS = [
  { id: "seatswap", label: "seatswap" },
  { id: "claudeCode", label: "Claude Code" },
  { id: "system", label: "System" },
];

const COLOUR_RULES = {
  none: "none",
  fixed: "fixed colour",
  byLoad: "by load",
  byModelFamily: "by model family",
  byEffort: "by effort",
  account: "the account's colour",
};

const TIME_FORMATS = [
  ["compactLeft", "Compact time left"],
  ["seatswap", "seatswap's"],
  ["clock", "Clock time"],
];

const DROP_OPTIONS = [
  ["never", "never"],
  ["1", "1 · first"],
  ["2", "2"],
  ["3", "3"],
];

const SCENARIOS = [
  ["live", "Live data"],
  ["nearFull", "Test: 97 %"],
  ["blocked", "Test: account blocked"],
  ["noSessionLimit", "Test: no 5 h limit"],
];

const PREVIEW_DELAY_MS = 150;

const state = {
  active: false,
  container: null,
  context: null,
  snapshot: null,
  settings: null,
  config: null,
  root: null,
  errorHost: null,
  // What the page could not read in the stored layout, and the last refused
  // save or install. Every one of these is shown as a banner that names it.
  errors: [],
  saveError: null,
  dragError: null,
  installError: null,
  presetError: null,
  keysError: null,
  fieldsError: null,
  notice: null,
  // The install state, the three presets and the optional segments' field
  // paths, read once when the area opens.
  install: null,
  presets: null,
  fields: null,
  // The chosen segment, by zone and position. The positions are rebuilt on
  // every redraw, so the selection is kept as a pair and never as a node.
  selected: null,
  focusTarget: null,
  lastZone: "left",
  // A colour rule whose values are not typed yet: it is shown in the options
  // but is not part of `state.config` and is never saved with a value the page
  // made up.
  pendingColorRule: null,
  // The layout was changed since a preset was last applied or loaded.
  dirty: false,
  presetName: null,
  pendingPreset: null,
  cardOpen: false,
  // The preview's chosen scenario, terminal background and three widths, kept
  // as the strings in the inputs so an empty or wrong value is shown, not
  // quietly replaced. `previewKey` is what the last request asked for.
  scenario: "live",
  terminal: "dark",
  widths: ["120", "100", "80"],
  previews: null,
  previewErrors: [],
  previewRows: [],
  previewErrorHost: null,
  previewTimer: null,
  previewToken: 0,
  previewKey: null,
  // The recorded field names, the record button's state and the saves. A
  // refused layout save makes every save queued behind it stale.
  keys: null,
  recording: false,
  recordingUntil: null,
  recordingTimer: null,
  recordingToken: 0,
  saving: 0,
  saveChain: Promise.resolve(),
  saveGeneration: 0,
  droppedSaves: 0,
  // The text of the last refused layout save, which the count of the changes
  // dropped after it extends.
  refusedText: null,
};

function kindOf(id) {
  return KINDS.find((kind) => kind.id === id) || null;
}

function groupKinds(group) {
  return KINDS.filter((kind) => kind.group === group);
}

function selectedSegment() {
  if (!state.selected || !state.config) return null;
  const list = state.config[state.selected.zone];
  if (!Array.isArray(list)) return null;
  const segment = list[state.selected.index];
  return isObject(segment) ? segment : null;
}

function segmentAt(selection) {
  if (!selection || !state.config) return null;
  const list = state.config[selection.zone];
  if (!Array.isArray(list)) return null;
  const segment = list[selection.index];
  return isObject(segment) ? segment : null;
}

// The payload paths a kind reads, from `statusline_fields` (the one table in
// render.rs). A kind without an entry reads none and gets no mark.
function fieldPathsOf(kind) {
  if (!Array.isArray(state.fields)) return [];
  const entry = state.fields.find((candidate) => candidate.kind === kind);
  return entry ? entry.paths : [];
}

// The note a segment gets when a recorded list exists and one of its paths is
// missing from it; no recorded list means nothing. The date is the list's time
// in the page's usual date format.
function missingFieldText(segment) {
  if (!state.keys || !isObject(segment) || typeof segment.kind !== "string") return null;
  const paths = fieldPathsOf(segment.kind);
  if (paths.length === 0) return null;
  const recorded = new Set(state.keys.keys.map((key) => key.path));
  if (paths.every((path) => recorded.has(path))) return null;
  return `Claude Code did not send this field (${dateTimeText(state.keys.at)})`;
}

// ---------------------------------------------------------------------------
// reading the stored layout
// ---------------------------------------------------------------------------

function colourRule(segment) {
  return isObject(segment.color) && typeof segment.color.rule === "string"
    ? segment.color.rule
    : null;
}

function readZone(list, side, errors) {
  list.forEach((segment, index) => {
    const at = `statusline.${side}[${index}]`;
    if (!isObject(segment)) {
      errors.push(`${at} is not an object`);
      return;
    }
    if (typeof segment.kind !== "string") {
      errors.push(`${at}.kind is missing`);
      return;
    }
    if (!kindOf(segment.kind)) {
      errors.push(`${at}.kind: unknown segment kind ${JSON.stringify(segment.kind)}`);
    }
  });
}

// The stored layout and what the page cannot draw: the two zones must be lists
// and every segment's kind must be in the KINDS table. Every other value is
// Rust's to check when the layout is saved; the page shows what it was sent.
function readConfig(raw) {
  const errors = [];
  if (!isObject(raw)) {
    return {
      config: null,
      errors: ["settings.statusline is missing; the status line layout is unknown"],
    };
  }
  if (!Array.isArray(raw.left) || !Array.isArray(raw.right)) {
    return {
      config: null,
      errors: ["settings.statusline.left and .right must be lists; the layout cannot be drawn"],
    };
  }
  readZone(raw.left, "left", errors);
  readZone(raw.right, "right", errors);
  return { config: clone(raw), errors };
}

// The settings' layout and the feed setting, with every value the page cannot
// read named once.
function readSettings(settings) {
  const read = readConfig(settings ? settings.statusline : undefined);
  if (!isObject(settings) || typeof settings.statuslineFeed !== "boolean") {
    read.errors.push(
      `settings.statuslineFeed is not true or false: ${JSON.stringify(
        settings ? settings.statuslineFeed : undefined
      )}`
    );
  }
  return read;
}

function normalizedSegment(segment) {
  if (!isObject(segment)) return null;
  const rule = colourRule(segment);
  let color = { rule: "none" };
  if (rule !== null) {
    color = { rule };
    if (rule === "fixed") color.color = segment.color.color;
    if (rule === "byLoad") {
      color.warn = segment.color.warn;
      color.bad = segment.color.bad;
    }
  }
  return {
    kind: segment.kind,
    color,
    prefix: segment.prefix === undefined ? "" : segment.prefix,
    suffix: segment.suffix === undefined ? "" : segment.suffix,
    drop: segment.drop === undefined ? null : segment.drop,
    timeFormat: segment.timeFormat === undefined ? null : segment.timeFormat,
    text: segment.text === undefined ? null : segment.text,
  };
}

function sameConfig(one, other) {
  if (!isObject(one) || !isObject(other)) return false;
  if (one.margin !== other.margin) return false;
  if (!Array.isArray(one.left) || !Array.isArray(one.right)) return false;
  if (!Array.isArray(other.left) || !Array.isArray(other.right)) return false;
  const key = (config) =>
    JSON.stringify({
      left: config.left.map(normalizedSegment),
      right: config.right.map(normalizedSegment),
    });
  return key(one) === key(other);
}

function presetNameOf(config) {
  if (!config || !Array.isArray(state.presets)) return null;
  const found = state.presets.find((entry) => sameConfig(config, entry[1]));
  return found ? found[0] : null;
}

function takeSettings(snapshot) {
  const settings = isObject(snapshot) ? snapshot.settings : null;
  if (state.saving > 0) {
    if (isObject(settings)) state.settings = settings;
    return;
  }
  state.settings = isObject(settings) ? settings : null;
  if (!state.settings) {
    state.config = null;
    state.errors = ["the settings could not be read; the status line layout is unknown"];
    return;
  }
  const read = readSettings(state.settings);
  state.config = read.config;
  state.errors = read.errors;
  state.presetName = presetNameOf(state.config);
  state.dirty = state.presetName === null;
}

// ---------------------------------------------------------------------------
// reading the commands' answers
// ---------------------------------------------------------------------------

function readInstall(value) {
  if (!isObject(value)) throw new Error("statusline install returned an unexpected reply");
  if (typeof value.installed !== "boolean") {
    throw new Error("statusline install returned no installed flag");
  }
  if (typeof value.restorable !== "boolean") {
    throw new Error("statusline install returned no restorable flag");
  }
  if (typeof value.settingsFile !== "string" || value.settingsFile === "") {
    throw new Error("statusline install returned no settings file");
  }
  if (!isObject(value.ours)) throw new Error("statusline install returned no 'ours' value");
  if (!Object.prototype.hasOwnProperty.call(value, "current")) {
    throw new Error("statusline install returned no current value");
  }
  if (!Object.prototype.hasOwnProperty.call(value, "previous")) {
    throw new Error("statusline install returned no previous value");
  }
  return value;
}

function readFields(reply) {
  if (!Array.isArray(reply)) throw new Error("statusline_fields returned an unexpected reply");
  return reply.map((entry, index) => {
    if (
      !isObject(entry) ||
      typeof entry.kind !== "string" ||
      entry.kind === "" ||
      !Array.isArray(entry.paths) ||
      entry.paths.some((path) => typeof path !== "string" || path === "")
    ) {
      throw new Error(`statusline_fields returned a bad entry at position ${index + 1}`);
    }
    return { kind: entry.kind, paths: entry.paths.slice() };
  });
}

function readPresets(reply) {
  if (!Array.isArray(reply)) throw new Error("statusline_presets returned an unexpected reply");
  return reply.map((entry, index) => {
    if (
      !Array.isArray(entry) ||
      entry.length !== 2 ||
      typeof entry[0] !== "string" ||
      !isObject(entry[1])
    ) {
      throw new Error(`statusline_presets returned a bad preset at position ${index + 1}`);
    }
    return [entry[0], entry[1]];
  });
}

function readKeys(value) {
  if (value === null) return null;
  if (!isObject(value)) throw new Error("statusline_keys returned an unexpected reply");
  if (typeof value.at !== "string" || Number.isNaN(Date.parse(value.at))) {
    throw new Error(`statusline_keys returned no valid time: ${JSON.stringify(value.at)}`);
  }
  if (!Array.isArray(value.keys)) throw new Error("statusline_keys returned no key list");
  value.keys.forEach((entry, index) => {
    if (
      !isObject(entry) ||
      typeof entry.path !== "string" ||
      entry.path === "" ||
      typeof entry.kind !== "string" ||
      entry.kind === ""
    ) {
      throw new Error(`statusline_keys returned a bad key at position ${index + 1}`);
    }
  });
  return value;
}

function readPreviewLines(reply, widths) {
  if (!Array.isArray(reply)) {
    throw new Error("statusline_preview returned an unexpected reply");
  }
  const lines = [];
  const errors = [];
  if (reply.length !== widths.length) {
    errors.push(
      `statusline_preview returned ${reply.length} lines for ${widths.length} widths`
    );
  }
  for (let index = 0; index < widths.length; index += 1) {
    const line = reply[index];
    if (!isObject(line)) {
      errors.push(`the preview line for ${widths[index]} columns is missing`);
      lines.push(null);
      continue;
    }
    const width = line.width;
    if (!Number.isInteger(width) || width < 1) {
      errors.push(`a preview line names no width: ${JSON.stringify(width)}`);
      lines.push(null);
      continue;
    }
    if (width !== widths[index]) {
      errors.push(
        `the preview line at position ${index + 1} is ${width} columns, the page asked for ${widths[index]}`
      );
      lines.push(null);
      continue;
    }
    let complete = true;
    if (!Array.isArray(line.spans)) {
      errors.push(`the preview line for ${width} columns has no spans`);
      complete = false;
    } else {
      line.spans.forEach((span, spanIndex) => {
        if (!isObject(span) || typeof span.text !== "string") {
          errors.push(`the preview line for ${width} columns has no text in span ${spanIndex + 1}`);
          complete = false;
          return;
        }
        for (const [name, colour] of [
          ["fg", span.fg],
          ["bg", span.bg],
        ]) {
          if (colour !== null && colour !== undefined && !isColor(colour)) {
            errors.push(
              `the preview line for ${width} columns has ${name} ${JSON.stringify(
                colour
              )} in span ${spanIndex + 1}, not #rrggbb`
            );
            complete = false;
          }
        }
        if (typeof span.bold !== "boolean") {
          errors.push(`the preview line for ${width} columns has no bold flag in span ${spanIndex + 1}`);
          complete = false;
        }
      });
    }
    if (typeof line.note !== "string") {
      errors.push(`the preview line for ${width} columns has no note`);
      complete = false;
    }
    if (!Array.isArray(line.dropped) || line.dropped.some((drop) => !Number.isInteger(drop))) {
      errors.push(`the preview line for ${width} columns has no dropped list`);
      complete = false;
    }
    lines.push(complete ? line : null);
  }
  return { lines, errors };
}

// ---------------------------------------------------------------------------
// saving
// ---------------------------------------------------------------------------

// Every save runs after the one before it and sends the layout as it is when
// it runs. A refused layout save puts the stored layout back and makes every
// save queued behind it stale: those were built on the refused layout and are
// dropped, with one banner that says so. A refused feed save only puts the
// switch back; the layout was never part of that save.
function queueSave(field, valueOf, options) {
  const config = isObject(options) ? options : {};
  const touchesLayout = config.layout !== false;
  const generation = state.saveGeneration;
  state.saving += 1;

  function dropStale() {
    state.droppedSaves += 1;
    state.saveError = `${state.refusedText} · ${
      state.droppedSaves
    } change(s) made after it were dropped: they were built on the refused layout`;
    renderErrors();
    return false;
  }

  function showStoredLayout() {
    if (!state.settings) return;
    state.pendingColorRule = null;
    const read = readSettings(state.settings);
    state.config = read.config;
    state.errors = read.errors;
    state.presetName = presetNameOf(state.config);
    state.dirty = state.presetName === null;
  }

  const run = async () => {
    if (generation !== state.saveGeneration) return dropStale();
    try {
      const saved = await state.context.invoke("save_setting", { field, value: valueOf() });
      if (!isObject(saved) || !isObject(saved.settings)) {
        throw new Error("save_setting returned an unexpected reply");
      }
      state.settings = saved.settings;
      if (isObject(state.snapshot)) state.snapshot.settings = saved.settings;
      state.saveError = null;
      state.droppedSaves = 0;
      state.notice =
        typeof saved.warning === "string" && saved.warning !== "" ? saved.warning : null;
      if (touchesLayout && state.saving === 1) {
        const read = readSettings(saved.settings);
        state.config = read.config;
        state.errors = read.errors;
        state.presetName = presetNameOf(state.config);
        state.dirty = state.presetName === null;
      }
      renderErrors();
      return true;
    } catch (error) {
      const text = errorMessage(error);
      state.notice = null;
      state.saveError = text;
      if (touchesLayout) {
        state.saveGeneration += 1;
        state.refusedText = text;
        state.droppedSaves = 0;
        showStoredLayout();
      }
      renderAll();
      return false;
    }
  };
  const task = state.saveChain.then(run);
  state.saveChain = task.catch(() => false).finally(() => {
    state.saving -= 1;
  });
  return task;
}

function persistConfig() {
  if (!state.config) return;
  schedulePreview();
  void queueSave("statusline", () => clone(state.config));
}

function markChanged() {
  state.dragError = null;
  state.presetName = presetNameOf(state.config);
  state.dirty = state.presetName === null;
}

// ---------------------------------------------------------------------------
// the page
// ---------------------------------------------------------------------------

function renderErrors() {
  if (!state.errorHost) return;
  clear(state.errorHost);
  for (const text of state.errors) {
    state.errorHost.appendChild(banner("error", "⚠", text));
  }
  for (const text of [state.saveError, state.dragError, state.installError, state.presetError, state.keysError, state.fieldsError]) {
    if (typeof text === "string" && text !== "") {
      state.errorHost.appendChild(banner("error", "⚠", text));
    }
  }
  if (typeof state.notice === "string" && state.notice !== "") {
    state.errorHost.appendChild(banner("info", "ℹ", state.notice));
  }
}

function renderAll() {
  if (!state.active || !state.root) return;
  clear(state.root);
  state.previewRows = [];
  state.previewErrorHost = null;
  const errors = h("div", { class: "sl-errors" });
  state.errorHost = errors;
  state.root.appendChild(errors);
  renderErrors();

  state.root.appendChild(head());
  if (state.pendingPreset !== null) state.root.appendChild(presetConfirm());
  if (state.cardOpen && state.install) state.root.appendChild(installCard());

  if (!state.config) {
    state.root.appendChild(
      message("— · the status line's layout cannot be read; there is nothing to show.", "error")
    );
    return;
  }
  state.root.appendChild(
    h("div", { class: "sl-grid" }, [
      paletteCard(),
      lineCard(),
      optionsCard(),
      previewCard(),
      fieldsCard(),
    ])
  );
  schedulePreview();
  applyFocus();
}

function applyFocus() {
  const target = state.focusTarget;
  state.focusTarget = null;
  if (!target || !state.root) return;
  for (const chip of state.root.querySelectorAll(".sl-chip")) {
    const data = chip.dataset;
    if (target.palette) {
      if (data.palette === target.palette) {
        chip.focus();
        return;
      }
    } else if (data.zone === target.zone && Number(data.index) === target.index) {
      chip.focus();
      return;
    }
  }
}

// ---------------------------------------------------------------------------
// the head: preset, restore and the install card
// ---------------------------------------------------------------------------

function presetControl() {
  const names = Array.isArray(state.presets) ? state.presets.map((entry) => entry[0]) : [];
  const options = [];
  if (names.length === 0) {
    options.push(["", "— presets could not be read"]);
  } else {
    if (!state.presetName) options.push(["", "Custom layout"]);
    for (const name of names) options.push([name, name]);
  }
  const select = selectInput(options, state.presetName || "", (value) => {
    if (value === "") {
      select.value = state.presetName || "";
      return;
    }
    if (state.dirty) {
      state.pendingPreset = value;
      renderAll();
      return;
    }
    applyPreset(value);
  });
  select.dataset.opt = "preset";
  select.setAttribute("aria-label", "Preset");
  select.disabled = names.length === 0;
  return h("span", { class: "sl-preset" }, [
    h("span", { class: "xs mf", text: "Preset" }),
    select,
  ]);
}

function applyPreset(name) {
  const found = state.presets.find((entry) => entry[0] === name);
  if (!found) {
    state.presetError = `unknown preset ${JSON.stringify(name)}`;
    renderAll();
    return;
  }
  const read = readConfig(found[1]);
  if (!read.config || read.errors.length > 0) {
    state.presetError = `preset ${JSON.stringify(name)} cannot be read: ${read.errors.join("; ")}`;
    renderAll();
    return;
  }
  state.config = read.config;
  state.errors = [];
  state.presetError = null;
  state.saveError = null;
  state.notice = null;
  state.dirty = false;
  state.presetName = name;
  state.pendingPreset = null;
  state.selected = null;
  persistConfig();
  renderAll();
}

function presetConfirm() {
  const name = state.pendingPreset;
  return panel(
    "Replace the layout?",
    "",
    [
      h("p", {
        class: "sl-sentence",
        text: `The preset “${name}” replaces the layout you changed.`,
      }),
      h("div", { class: "row sl-actions" }, [
        h("span", { class: "sp" }),
        h("button", {
          type: "button",
          class: "hbtn",
          text: "Cancel",
          onclick: () => {
            state.pendingPreset = null;
            renderAll();
          },
        }),
        h("button", {
          type: "button",
          class: "hbtn pri",
          text: "Replace",
          dataset: { action: "replace-preset" },
          onclick: () => applyPreset(name),
        }),
      ]),
    ],
    null,
    { cardClass: "sl-card sl-confirm", bodyClass: "sl-card-body" }
  );
}

function jsonText(value) {
  return JSON.stringify(value === undefined ? null : value, null, 2);
}

function installCard() {
  const install = state.install;
  return panel(
    "Use in Claude Code",
    "the one key seatswap writes in Claude Code's settings",
    [
      h("div", { class: "sl-install-path" }, [
        h("span", { class: "sl-lab", text: "File" }),
        h("code", { text: install.settingsFile }),
      ]),
      h("div", { class: "sl-json-row" }, [
        h("div", { class: "sl-json-block" }, [
          h("span", { class: "sl-lab", text: "now" }),
          h("pre", { class: "sl-json", text: jsonText(install.current) }),
        ]),
        h("div", { class: "sl-json-block" }, [
          h("span", { class: "sl-lab", text: "after" }),
          h("pre", { class: "sl-json", text: jsonText(install.ours) }),
        ]),
      ]),
      h("p", {
        class: "sl-sentence",
        text: "seatswap changes only this key. The value it replaces is kept and can be put back.",
      }),
      h("div", { class: "row sl-actions" }, [
        h("span", { class: "sp" }),
        h("button", {
          type: "button",
          class: "hbtn",
          text: "Cancel",
          onclick: () => {
            state.cardOpen = false;
            renderAll();
          },
        }),
        h("button", {
          type: "button",
          class: "hbtn pri",
          text: "Install",
          dataset: { action: "install" },
          onclick: () => void installLine(),
        }),
      ]),
    ],
    null,
    { cardClass: "sl-card sl-install", bodyClass: "sl-card-body" }
  );
}

async function installLine() {
  try {
    state.install = readInstall(await state.context.invoke("statusline_install", {}));
    state.installError = null;
    state.cardOpen = false;
    state.notice = "seatswap's status line is now in Claude Code.";
  } catch (error) {
    state.installError = errorMessage(error);
  }
  renderAll();
}

// Read Claude Code's current status line again, then show the card: the file
// may have been changed by hand or by another window since the area opened.
async function openInstallCard() {
  await loadInstall();
  if (!state.active) return;
  state.cardOpen = state.install !== null;
  renderAll();
}

async function restoreLine() {
  const before = state.install;
  try {
    state.install = readInstall(await state.context.invoke("statusline_restore", {}));
    state.installError = null;
    if (before && before.previous === null) {
      state.notice = "seatswap's status line was removed from Claude Code: there was no previous status line.";
    } else {
      state.notice = `The previous status line was put back: ${JSON.stringify(
        state.install.current
      )}`;
    }
  } catch (error) {
    state.installError = errorMessage(error);
  }
  renderAll();
}

function head() {
  const install = state.install;
  const useButton = h("button", {
    type: "button",
    class: "hbtn pri",
    text: install && install.installed ? "In use in Claude Code" : "Use in Claude Code",
    dataset: { action: "use" },
  });
  useButton.disabled = install === null || install.installed;
  if (install === null) useButton.title = "the install state could not be read";
  useButton.addEventListener("click", () => void openInstallCard());
  const restoreButton = h("button", {
    type: "button",
    class: "hbtn",
    text: "Restore previous",
    dataset: { action: "restore" },
    onclick: () => void restoreLine(),
  });
  return h("div", { class: "row sl-head" }, [
    h("p", {
      class: "win-sub sl-head-sub",
      text: "the line Claude Code shows under the prompt · drag segments between the palette and the two zones",
    }),
    h("span", { class: "sp" }),
    presetControl(),
    install && install.restorable ? restoreButton : null,
    useButton,
  ]);
}

// ---------------------------------------------------------------------------
// the palette and the line
// ---------------------------------------------------------------------------

function newSegment(kind) {
  const spec = kindOf(kind);
  const segment = { kind, color: { rule: "none" }, prefix: "", suffix: "" };
  if (spec && spec.time) segment.timeFormat = "seatswap";
  if (spec && spec.text) segment.text = "";
  return segment;
}

function segmentLabel(segment) {
  if (!isObject(segment)) return "— (not a segment)";
  const spec = typeof segment.kind === "string" ? kindOf(segment.kind) : null;
  if (spec) return spec.label;
  return `? ${JSON.stringify(segment.kind)}`;
}

function paletteCard() {
  const used = new Set();
  if (state.config) {
    for (const side of ["left", "right"]) {
      for (const segment of state.config[side]) {
        if (isObject(segment) && typeof segment.kind === "string") used.add(segment.kind);
      }
    }
  }
  const groups = GROUPS.map((group) => {
    const zone = h("div", { class: "sl-zone sl-zone-palette", role: "list" });
    for (const kind of groupKinds(group.id)) {
      if (!kind.repeatable && used.has(kind.id)) continue;
      zone.appendChild(paletteChip(kind));
    }
    return h("div", { class: "sl-group" }, [
      h("span", { class: "sl-lab", text: group.label }),
      zone,
    ]);
  });
  return panel("Segments", "not on the line", groups, null, {
    cardClass: "sl-card sl-palette",
    bodyClass: "sl-card-body",
  });
}

function paletteChip(kind) {
  const chip = h(
    "span",
    {
      class: `sl-chip sl-chip-palette${kind.repeatable ? " sl-repeatable" : ""}`,
      role: "button",
      tabindex: "0",
      draggable: "true",
      text: kind.label,
      dataset: { palette: kind.id },
      title: kind.repeatable
        ? `${kind.label} can be added more than once`
        : `Add ${kind.label} to the line`,
    }
  );
  chip.addEventListener("click", () => addSegment(kind.id, state.lastZone, null));
  chip.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      addSegment(kind.id, state.lastZone, null);
      return;
    }
    if (event.key === "l" || event.key === "L") {
      event.preventDefault();
      addSegment(kind.id, "left", null);
      return;
    }
    if (event.key === "r" || event.key === "R") {
      event.preventDefault();
      addSegment(kind.id, "right", null);
    }
  });
  chip.addEventListener("dragstart", (event) => {
    const payload = { from: "palette", kind: kind.id };
    state.dragging = payload;
    event.dataTransfer.setData("text/plain", JSON.stringify(payload));
    event.dataTransfer.effectAllowed = "move";
  });
  chip.addEventListener("dragend", () => {
    state.dragging = null;
    clearDropMarks();
  });
  return chip;
}

function addSegment(kindId, zoneId, index) {
  if (!state.config) return;
  const list = state.config[zoneId];
  if (!Array.isArray(list)) return;
  const at = index === null || index === undefined ? list.length : index;
  list.splice(at, 0, newSegment(kindId));
  state.selected = { zone: zoneId, index: at };
  state.focusTarget = { zone: zoneId, index: at };
  state.lastZone = zoneId;
  markChanged();
  persistConfig();
  renderAll();
}

function moveSegment(fromZone, fromIndex, toZone, toIndex) {
  if (!state.config) return false;
  if (fromZone === toZone && fromIndex === toIndex) return false;
  const source = state.config[fromZone];
  const target = state.config[toZone];
  if (!Array.isArray(source) || !Array.isArray(target)) return false;
  if (fromIndex < 0 || fromIndex >= source.length) return false;
  const [segment] = source.splice(fromIndex, 1);
  let at = toIndex;
  if (fromZone === toZone && fromIndex < toIndex) at -= 1;
  target.splice(at, 0, segment);
  state.selected = { zone: toZone, index: at };
  state.focusTarget = { zone: toZone, index: at };
  state.lastZone = toZone;
  markChanged();
  persistConfig();
  renderAll();
  return true;
}

function removeSegment(zoneId, index) {
  if (!state.config) return;
  const list = state.config[zoneId];
  if (!Array.isArray(list) || index < 0 || index >= list.length) return;
  const [segment] = list.splice(index, 1);
  state.selected = null;
  state.lastZone = zoneId;
  state.focusTarget = {
    palette: isObject(segment) && typeof segment.kind === "string" ? segment.kind : null,
  };
  markChanged();
  persistConfig();
  renderAll();
}

function lineCard() {
  return panel(
    "The line",
    "click a segment for its options",
    [
      h("span", { class: "sl-lab", text: "Left" }),
      zone("left"),
      h("span", { class: "sl-lab", text: "Right · flush with the right edge of the terminal" }),
      zone("right"),
      lineFoot(),
    ],
    null,
    { cardClass: "sl-card sl-line", bodyClass: "sl-card-body" }
  );
}

function zone(zoneId) {
  const list = state.config[zoneId];
  const node = h("div", {
    class: "sl-zone",
    role: "list",
    "aria-label": zoneId === "left" ? "Left zone" : "Right zone",
    dataset: { zone: zoneId },
  });
  if (list.length === 0) {
    node.appendChild(h("span", { class: "sl-empty", text: "— · drop a segment here" }));
  }
  for (let index = 0; index < list.length; index += 1) {
    node.appendChild(segmentChip(zoneId, index, list[index]));
  }
  node.addEventListener("dragover", (event) => {
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    node.classList.add("over");
  });
  node.addEventListener("dragleave", () => node.classList.remove("over"));
  node.addEventListener("drop", (event) => {
    event.preventDefault();
    node.classList.remove("over");
    const read = readDrag(event);
    state.dragging = null;
    if (read.error) {
      state.dragError = read.error;
      renderAll();
      return;
    }
    state.dragError = null;
    const source = read.payload;
    const current = state.config ? state.config[zoneId] : null;
    if (!Array.isArray(current)) {
      state.dragError = `statusline.${zoneId} is not a list; nothing was moved`;
      renderAll();
      return;
    }
    if (source.from === "palette") {
      addSegment(source.kind, zoneId, current.length);
      return;
    }
    if (source.from !== "zone") return;
    if (source.zone === zoneId && source.index === current.length - 1) return;
    moveSegment(source.zone, source.index, zoneId, current.length);
  });
  return node;
}

function segmentChip(zoneId, index, segment) {
  const spec = isObject(segment) && typeof segment.kind === "string" ? kindOf(segment.kind) : null;
  const selected =
    state.selected !== null &&
    state.selected.zone === zoneId &&
    state.selected.index === index;
  const missing = missingFieldText(segment);
  const chip = h("span", {
    class: `sl-chip${selected ? " sel" : ""}${spec ? "" : " sl-unknown"}${
      missing ? " sl-missing" : ""
    }`,
    role: "button",
    tabindex: "0",
    draggable: "true",
    text: segmentLabel(segment),
    dataset: { zone: zoneId, index: String(index), kind: spec ? spec.id : String(segment.kind) },
    title: missing
      ? `${spec ? spec.label : segment.kind} · ${missing}`
      : spec
        ? `${spec.label} · ←/→ reorder · L/R move zone · Delete removes`
        : `unknown segment kind ${JSON.stringify(segment.kind)}`,
  });
  chip.addEventListener("click", () => {
    state.selected = { zone: zoneId, index };
    state.lastZone = zoneId;
    renderAll();
  });
  chip.addEventListener("focus", () => {
    state.lastZone = zoneId;
    if (
      state.selected &&
      state.selected.zone === zoneId &&
      state.selected.index === index
    ) {
      return;
    }
    state.selected = { zone: zoneId, index };
    state.focusTarget = { zone: zoneId, index };
    renderAll();
  });
  chip.addEventListener("keydown", (event) => chipKeydown(event, zoneId, index));
  chip.addEventListener("dragstart", (event) => {
    const payload = { from: "zone", zone: zoneId, index };
    state.dragging = payload;
    event.dataTransfer.setData("text/plain", JSON.stringify(payload));
    event.dataTransfer.effectAllowed = "move";
  });
  chip.addEventListener("dragend", () => {
    state.dragging = null;
    clearDropMarks();
  });
  chip.addEventListener("dragover", (event) => {
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = "move";
    chip.classList.add("over");
  });
  chip.addEventListener("dragleave", () => chip.classList.remove("over"));
  chip.addEventListener("drop", (event) => {
    event.preventDefault();
    event.stopPropagation();
    chip.classList.remove("over");
    const read = readDrag(event);
    state.dragging = null;
    if (read.error) {
      state.dragError = read.error;
      renderAll();
      return;
    }
    state.dragError = null;
    const source = read.payload;
    if (source.from === "palette") {
      addSegment(source.kind, zoneId, index);
      return;
    }
    if (source.from !== "zone") return;
    // A segment dropped onto itself changes nothing.
    if (source.zone === zoneId && source.index === index) return;
    moveSegment(source.zone, source.index, zoneId, index);
  });
  return chip;
}

function readDrag(event) {
  if (state.dragging) return { payload: state.dragging };
  const raw = event.dataTransfer.getData("text/plain");
  if (raw === "") {
    return { error: "the dragged segment carries no data; nothing was moved" };
  }
  let payload;
  try {
    payload = JSON.parse(raw);
  } catch (error) {
    return {
      error: `the dragged segment's data cannot be read: ${errorMessage(error)}; nothing was moved`,
    };
  }
  if (!isObject(payload) || (payload.from !== "palette" && payload.from !== "zone")) {
    return {
      error: `the dragged segment's data names no source: ${JSON.stringify(
        payload
      )}; nothing was moved`,
    };
  }
  if (payload.from === "palette" && typeof payload.kind !== "string") {
    return {
      error: `the dragged segment names no kind: ${JSON.stringify(payload)}; nothing was moved`,
    };
  }
  if (
    payload.from === "zone" &&
    (typeof payload.zone !== "string" || !Number.isInteger(payload.index))
  ) {
    return {
      error: `the dragged segment names no position: ${JSON.stringify(payload)}; nothing was moved`,
    };
  }
  return { payload };
}

function clearDropMarks() {
  if (!state.root) return;
  for (const node of state.root.querySelectorAll(".sl-zone.over, .sl-chip.over")) {
    node.classList.remove("over");
  }
}

function chipKeydown(event, zoneId, index) {
  if (!state.config) return;
  const list = state.config[zoneId];
  if (!Array.isArray(list)) return;
  if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
    event.preventDefault();
    const target = event.key === "ArrowLeft" ? index - 1 : index + 1;
    if (target < 0 || target >= list.length) return;
    const segment = list[index];
    list[index] = list[target];
    list[target] = segment;
    state.selected = { zone: zoneId, index: target };
    state.focusTarget = { zone: zoneId, index: target };
    markChanged();
    persistConfig();
    renderAll();
    return;
  }
  if (event.key === "l" || event.key === "L" || event.key === "r" || event.key === "R") {
    event.preventDefault();
    const targetZone = event.key === "l" || event.key === "L" ? "left" : "right";
    if (targetZone === zoneId) return;
    const [segment] = list.splice(index, 1);
    state.config[targetZone].push(segment);
    const at = state.config[targetZone].length - 1;
    state.selected = { zone: targetZone, index: at };
    state.focusTarget = { zone: targetZone, index: at };
    state.lastZone = targetZone;
    markChanged();
    persistConfig();
    renderAll();
    return;
  }
  if (event.key === "Delete") {
    event.preventDefault();
    removeSegment(zoneId, index);
  }
}

function typedValue(input) {
  const text = String(input.value).trim();
  const number = Number(text);
  return text !== "" && Number.isFinite(number) ? number : text;
}

function lineFoot() {
  const margin = h("input", {
    type: "number",
    min: "0",
    max: "65535",
    step: "1",
    class: "inp sl-margin",
    value:
      state.config.margin === undefined || state.config.margin === null
        ? ""
        : String(state.config.margin),
    dataset: { opt: "margin" },
    "aria-label": "Margin in columns",
  });
  margin.addEventListener("change", () => {
    state.config.margin = typedValue(margin);
    markChanged();
    persistConfig();
  });

  const feedKnown =
    state.settings && typeof state.settings.statuslineFeed === "boolean";
  const on = feedKnown ? Boolean(state.settings.statuslineFeed) : false;
  const control = switchControl(
    on,
    "Pass Claude Code's live numbers on to seatswap",
    (next) => {
      void queueSave("statuslineFeed", () => next, { layout: false }).then(() => renderAll());
    },
    { words: feedKnown }
  );
  control.button.dataset.action = "feed";
  control.button.disabled = !feedKnown;
  const feedNode = feedKnown
    ? control.node
    : h("span", { class: "sws-line" }, [
        control.button,
        h("span", { class: "x13 mf", text: "—" }),
      ]);

  return h("div", { class: "row sl-foot" }, [
    h("span", { class: "row sl-margin-row" }, [
      h("span", { class: "sl-lab", text: "Margin" }),
      margin,
    ]),
    feedNode,
    h("span", { class: "x13", text: "Pass Claude Code's live numbers on to seatswap" }),
  ]);
}

// ---------------------------------------------------------------------------
// the chosen segment's options
// ---------------------------------------------------------------------------

function optionRow(label, control, note) {
  return h("div", { class: "set" }, [
    h("span", { text: label }),
    h("span", { class: "sl-option-control" }, [
      control,
      note ? h("span", { class: "mf sl-option-note", text: note }) : null,
    ]),
  ]);
}

function pendingRuleFor(selection) {
  const pending = state.pendingColorRule;
  if (!pending || !selection) return null;
  if (pending.zone !== selection.zone || pending.index !== selection.index) return null;
  return pending;
}

function changeSegment(selection, change, redraw) {
  const segment = segmentAt(selection);
  if (!segment) {
    state.saveError = `the chosen segment at statusline.${selection.zone}[${selection.index}] is no longer there; the change was not applied`;
    renderErrors();
    return false;
  }
  change(segment);
  markChanged();
  persistConfig();
  if (redraw) renderAll();
  return true;
}

function optionsCard() {
  const segment = selectedSegment();
  if (!segment) {
    return panel("Options", "the chosen segment", [
      message("Select a segment on the line to see its options."),
    ]);
  }
  const selection = { zone: state.selected.zone, index: state.selected.index };
  const spec = typeof segment.kind === "string" ? kindOf(segment.kind) : null;
  if (!spec) {
    return panel(
      `Options · unknown kind ${JSON.stringify(segment.kind)}`,
      `${state.selected.zone} · ${state.selected.index + 1}`,
      [
        banner(
          "error",
          "⚠",
          `unknown segment kind ${JSON.stringify(segment.kind)} at statusline.${state.selected.zone}[${
            state.selected.index
          }]`
        ),
      ],
      null,
      { cardClass: "sl-card sl-options", bodyClass: "sl-card-body" }
    );
  }
  const pending = pendingRuleFor(selection);
  const body = [];
  const missing = missingFieldText(segment);
  if (missing) body.push(banner("warn", "⚠", missing));
  const rule = pending ? pending.rule : colourRule(segment);
  const colourOptions = spec.colors.map((value) => [value, COLOUR_RULES[value]]);
  let colourValue = rule !== null && spec.colors.includes(rule) ? rule : "";
  if (colourValue === "") {
    colourOptions.unshift([
      "",
      rule === null
        ? "— (no colour rule stored)"
        : `— ${COLOUR_RULES[rule] || rule} (not offered for ${spec.label})`,
    ]);
  }
  const colourSelect = selectInput(colourOptions, colourValue, (next) => {
    if (next === "") return;
    chooseColourRule(selection, next);
  });
  colourSelect.dataset.opt = "color";
  body.push(optionRow("Colour", colourSelect));

  if (rule === "fixed") {
    const colorValue = pending
      ? pending.color
      : isObject(segment.color) && typeof segment.color.color === "string"
        ? segment.color.color
        : "";
    const hex = h("input", {
      type: "text",
      spellcheck: "false",
      class: "inp sl-hex",
      value: colorValue,
      dataset: { opt: "colorHex" },
      "aria-label": "Fixed colour #rrggbb",
    });
    hex.addEventListener("change", () => {
      const value = hex.value.trim();
      const live = pendingRuleFor(selection);
      if (live) live.color = value;
      if (value === "") {
        state.notice = "the fixed colour is saved once #rrggbb is set";
        renderErrors();
        return;
      }
      if (
        changeSegment(
          selection,
          (current) => {
            current.color = { rule: "fixed", color: value };
          },
          true
        )
      ) {
        state.pendingColorRule = null;
      }
    });
    body.push(
      optionRow(
        "Fixed colour",
        hex,
        pending && colorValue === "" ? "saved once #rrggbb is set" : "#rrggbb"
      )
    );
  }
  if (rule === "byLoad") {
    const warnValue = pending
      ? pending.warn
      : isObject(segment.color) && segment.color.warn !== undefined
        ? segment.color.warn
        : "";
    const badValue = pending
      ? pending.bad
      : isObject(segment.color) && segment.color.bad !== undefined
        ? segment.color.bad
        : "";
    const warn = h("input", {
      type: "number",
      min: "0",
      max: "100",
      step: "1",
      class: "inp sl-threshold",
      value: String(warnValue),
      dataset: { opt: "warn" },
      "aria-label": "Yellow from",
    });
    const bad = h("input", {
      type: "number",
      min: "0",
      max: "100",
      step: "1",
      class: "inp sl-threshold",
      value: String(badValue),
      dataset: { opt: "bad" },
      "aria-label": "Red from",
    });
    const apply = () => {
      const warnText = warn.value.trim();
      const badText = bad.value.trim();
      const live = pendingRuleFor(selection);
      if (live) {
        live.warn = warnText;
        live.bad = badText;
      }
      if (warnText === "" || badText === "") {
        state.notice = "the load rule is saved once yellow and red are set";
        renderErrors();
        return;
      }
      if (
        changeSegment(
          selection,
          (current) => {
            current.color = {
              rule: "byLoad",
              warn: typedValue(warn),
              bad: typedValue(bad),
            };
          },
          true
        )
      ) {
        state.pendingColorRule = null;
      }
    };
    warn.addEventListener("change", apply);
    bad.addEventListener("change", apply);
    body.push(
      optionRow(
        "Thresholds",
        h("span", { class: "row sl-thresholds" }, [warn, h("span", { class: "mf", text: "→" }), bad]),
        pending && (warnValue === "" || badValue === "")
          ? "saved once yellow and red are set"
          : "yellow from · red from"
      )
    );
  }
  if (spec.time) {
    const current = typeof segment.timeFormat === "string" ? segment.timeFormat : null;
    const options = TIME_FORMATS.slice();
    let value = current !== null && TIME_FORMATS.some(([format]) => format === current) ? current : "";
    if (value === "") {
      options.unshift([
        "",
        current === null
          ? "— (seatswap's is used)"
          : `— ${JSON.stringify(current)} (unknown)`,
      ]);
    }
    const format = selectInput(options, value, (next) => {
      if (next === "") return;
      changeSegment(
        selection,
        (segmentNow) => {
          segmentNow.timeFormat = next;
        },
        true
      );
    });
    format.dataset.opt = "timeFormat";
    body.push(optionRow("Time format", format));
  }
  if (spec.text) {
    const text = h("input", {
      type: "text",
      spellcheck: "false",
      class: "inp",
      value: typeof segment.text === "string" ? segment.text : "",
      dataset: { opt: "text" },
      "aria-label": "Text",
    });
    text.addEventListener("input", () => {
      changeSegment(selection, (segmentNow) => {
        segmentNow.text = text.value;
      });
    });
    body.push(optionRow("Text", text, "prints nothing when empty"));
  }
  const prefix = h("input", {
    type: "text",
    spellcheck: "false",
    class: "inp",
    value: typeof segment.prefix === "string" ? segment.prefix : "",
    dataset: { opt: "prefix" },
    "aria-label": "Prefix",
  });
  prefix.addEventListener("input", () => {
    changeSegment(selection, (segmentNow) => {
      segmentNow.prefix = prefix.value;
    });
  });
  const suffix = h("input", {
    type: "text",
    spellcheck: "false",
    class: "inp",
    value: typeof segment.suffix === "string" ? segment.suffix : "",
    dataset: { opt: "suffix" },
    "aria-label": "Suffix",
  });
  suffix.addEventListener("input", () => {
    changeSegment(selection, (segmentNow) => {
      segmentNow.suffix = suffix.value;
    });
  });
  body.push(
    optionRow(
      "Prefix / suffix",
      h("span", { class: "row sl-prefix" }, [prefix, suffix])
    )
  );

  let currentDrop = "never";
  if (segment.drop !== undefined && segment.drop !== null) {
    const offered = DROP_OPTIONS.some(([value]) => value === String(segment.drop));
    currentDrop = offered ? String(segment.drop) : "";
  }
  const dropOptions = DROP_OPTIONS.slice();
  if (currentDrop === "") {
    dropOptions.unshift(["", `— ${JSON.stringify(segment.drop)} (not offered)`]);
  }
  const drop = selectInput(dropOptions, currentDrop, (next) => {
    if (next === "") return;
    changeSegment(
      selection,
      (segmentNow) => {
        if (next === "never") delete segmentNow.drop;
        else segmentNow.drop = Number(next);
      },
      true
    );
  });
  drop.dataset.opt = "drop";
  body.push(optionRow("Drop when narrow", drop));

  return panel(
    `Options · ${spec.label}`,
    `${state.selected.zone} · ${state.selected.index + 1}`,
    body,
    null,
    { cardClass: "sl-card sl-options", bodyClass: "sl-card-body" }
  );
}

function chooseColourRule(selection, rule) {
  const segment = segmentAt(selection);
  if (!segment) {
    state.saveError = `the chosen segment at statusline.${selection.zone}[${selection.index}] is no longer there; the colour rule was not changed`;
    renderErrors();
    return;
  }
  const current = colourRule(segment);
  if (rule === "fixed") {
    const color =
      current === "fixed" && typeof segment.color.color === "string" ? segment.color.color : "";
    state.pendingColorRule = {
      zone: selection.zone,
      index: selection.index,
      rule: "fixed",
      color,
    };
    renderAll();
    return;
  }
  if (rule === "byLoad") {
    const values = current === "byLoad" ? segment.color : null;
    state.pendingColorRule = {
      zone: selection.zone,
      index: selection.index,
      rule: "byLoad",
      warn: values && values.warn !== undefined ? values.warn : "",
      bad: values && values.bad !== undefined ? values.bad : "",
    };
    renderAll();
    return;
  }
  state.pendingColorRule = null;
  segment.color = rule === "none" ? { rule: "none" } : { rule };
  markChanged();
  persistConfig();
  renderAll();
}

// ---------------------------------------------------------------------------
// the preview
// ---------------------------------------------------------------------------

function previewWidths() {
  const widths = [];
  for (let index = 0; index < state.widths.length; index += 1) {
    const text = String(state.widths[index] === undefined ? "" : state.widths[index]).trim();
    if (!/^\d+$/.test(text)) {
      return { error: `preview width ${JSON.stringify(state.widths[index])} is not a whole number of columns` };
    }
    widths.push(Number(text));
  }
  return { widths };
}

function schedulePreview() {
  if (!state.config) return;
  if (state.previewTimer !== null) clearTimeout(state.previewTimer);
  state.previewTimer = setTimeout(() => {
    state.previewTimer = null;
    void requestPreview();
  }, PREVIEW_DELAY_MS);
}

async function requestPreview() {
  if (!state.active || !state.config) return;
  const parsed = previewWidths();
  if (parsed.error) {
    state.previews = null;
    state.previewKey = null;
    state.previewErrors = [parsed.error];
    renderPreview();
    return;
  }
  const key = JSON.stringify({
    config: state.config,
    scenario: state.scenario,
    widths: parsed.widths,
  });
  if (
    key === state.previewKey &&
    Array.isArray(state.previews) &&
    state.previewErrors.length === 0
  ) {
    return;
  }
  state.previewKey = key;
  const token = ++state.previewToken;
  try {
    const reply = await state.context.invoke("statusline_preview", {
      config: clone(state.config),
      widths: parsed.widths,
      scenario: state.scenario,
    });
    if (token !== state.previewToken || !state.active) return;
    const read = readPreviewLines(reply, parsed.widths);
    state.previews = read.lines;
    state.previewErrors = read.errors;
    if (read.errors.length > 0) state.previewKey = null;
  } catch (error) {
    if (token !== state.previewToken || !state.active) return;
    state.previews = null;
    state.previewKey = null;
    state.previewErrors = [errorMessage(error)];
  }
  renderPreview();
}

function previewCard() {
  const terminal = h("div", { class: "tabs", role: "tablist", "aria-label": "Terminal background" }, [
    terminalTab("dark", "Dark terminal"),
    terminalTab("light", "Light terminal"),
  ]);
  const scenario = selectInput(SCENARIOS, state.scenario, (next) => {
    state.scenario = next;
    schedulePreview();
  });
  scenario.dataset.opt = "scenario";
  scenario.setAttribute("aria-label", "Preview scenario");
  scenario.style.width = "170px";
  scenario.style.flex = "0 0 auto";

  const head = h("div", { class: "ch" }, [
    h("span", { class: "t", text: "Preview" }),
    h("span", { class: "s", text: "live data · at three terminal widths" }),
    h("span", { class: "sp" }),
    terminal,
    scenario,
  ]);
  const errorHost = h("div", { class: "sl-prev-errors" });
  state.previewErrorHost = errorHost;
  const body = h("div", { class: "sl-prev-body" }, [errorHost]);
  for (let index = 0; index < 3; index += 1) {
    const input = h("input", {
      type: "number",
      min: "1",
      max: "65535",
      step: "1",
      class: "inp sl-width",
      value: state.widths[index],
      dataset: { width: String(index) },
      "aria-label": `Preview width ${index + 1} in columns`,
    });
    input.addEventListener("input", () => {
      state.widths[index] = input.value;
      schedulePreview();
    });
    const term = h("div", {
      class: `sl-term${state.terminal === "light" ? " light" : ""}`,
      "aria-label": `Preview line ${index + 1}`,
    });
    const note = h("span", { class: "sl-note" });
    state.previewRows.push({ input, term, note });
    body.appendChild(
      h("div", { class: "sl-prev-row" }, [
        h("span", { class: "sl-width-cell" }, [
          input,
          h("span", { class: "xs mf", text: "columns" }),
        ]),
        term,
        note,
      ])
    );
  }
  const node = h("div", { class: "hcard sl-card sl-preview" }, [head, body]);
  renderPreview();
  return node;
}

function terminalTab(value, label) {
  const button = h("button", {
    type: "button",
    class: `tab${state.terminal === value ? " ison" : ""}`,
    role: "tab",
    "aria-selected": state.terminal === value ? "true" : "false",
    text: label,
    dataset: { terminal: value },
  });
  button.addEventListener("click", () => {
    state.terminal = value;
    if (!state.root) return;
    for (const tab of state.root.querySelectorAll("[data-terminal]")) {
      tab.classList.toggle("ison", tab.dataset.terminal === value);
      tab.setAttribute("aria-selected", tab.dataset.terminal === value ? "true" : "false");
    }
    for (const term of state.previewRows) {
      term.term.classList.toggle("light", value === "light");
    }
  });
  return button;
}

function renderPreview() {
  if (!state.previewErrorHost) return;
  clear(state.previewErrorHost);
  for (const text of state.previewErrors) {
    state.previewErrorHost.appendChild(banner("error", "⚠", text));
  }
  for (let index = 0; index < state.previewRows.length; index += 1) {
    const row = state.previewRows[index];
    row.term.style.removeProperty("width");
    clear(row.term);
    row.note.textContent = "";
    if (!Array.isArray(state.previews)) {
      row.term.appendChild(h("span", { class: "sl-term-missing", text: "— · no preview yet" }));
      continue;
    }
    const line = state.previews[index];
    if (!line) {
      row.term.appendChild(
        h("span", { class: "sl-term-missing", text: "— · the preview line could not be read" })
      );
      continue;
    }
    // The box is the terminal: `width` columns of the terminal font. The
    // renderer leaves the margin empty at its right end, as a terminal does.
    row.term.style.width = `calc(${line.width}ch + 16px)`;
    for (const span of line.spans) {
      const node = h("span", { text: span.text });
      if (span.fg) node.style.color = span.fg;
      if (span.bg) node.style.backgroundColor = span.bg;
      if (span.bold) node.style.fontWeight = "600";
      row.term.appendChild(node);
    }
    row.note.textContent = line.note;
  }
}

// ---------------------------------------------------------------------------
// the field names Claude Code sends
// ---------------------------------------------------------------------------

function recordUntil(value) {
  if (typeof value !== "string") return null;
  const time = Date.parse(value);
  return Number.isNaN(time) ? null : time;
}

function stopRecording() {
  state.recordingToken += 1;
  if (state.recordingTimer !== null) {
    clearTimeout(state.recordingTimer);
    state.recordingTimer = null;
  }
  state.recording = false;
  state.recordingUntil = null;
}

function fieldsCard() {
  const recordingText =
    state.recording && state.recordingUntil !== null
      ? `Recording until ${dateTimeText(new Date(state.recordingUntil))}`
      : state.recording
        ? "Recording…"
        : "Record for a minute";
  const button = h("button", {
    type: "button",
    class: "hbtn",
    text: recordingText,
    dataset: { action: "record" },
  });
  button.disabled = state.recording;
  button.addEventListener("click", () => void recordFields());

  const children = [
    h("div", { class: "row sl-record" }, [
      button,
      h("span", {
        class: "xs mf",
        text: "the key paths only, never the values Claude Code sent",
      }),
    ]),
  ];
  if (state.keysError) children.push(banner("error", "⚠", state.keysError));
  if (state.keys) {
    children.push(
      h("p", {
        class: "sl-note",
        text: `Recorded ${dateTimeText(state.keys.at)} · ${state.keys.keys.length} fields`,
      })
    );
    const list = h("div", { class: "sl-keys", role: "list" });
    for (const key of state.keys.keys) {
      list.appendChild(
        h("div", { class: "sl-key", role: "listitem" }, [
          h("code", { text: key.path }),
          h("span", { class: "mf", text: key.kind }),
        ])
      );
    }
    children.push(list);
  } else {
    children.push(
      h("p", {
        class: "sl-note",
        text: "No field names recorded yet. Start a recording, then let Claude Code call the status line.",
      })
    );
  }
  return panel(
    "Fields Claude Code sends",
    "recorded by seatswap statusline",
    children,
    null,
    { cardClass: "sl-card sl-fields", bodyClass: "sl-card-body" }
  );
}

async function recordFields() {
  stopRecording();
  state.recording = true;
  state.keysError = null;
  state.notice = null;
  renderAll();
  try {
    await state.context.invoke("statusline_record", { minutes: 1 });
    // The command saved `statuslineRecordUntil`; the snapshot is where the one
    // written value is read, so the page does not compute the end itself.
    const snapshot = await state.context.invoke("get_snapshot", {});
    if (isObject(snapshot)) {
      state.snapshot = snapshot;
      takeSettings(snapshot);
    }
    const until = recordUntil(state.settings && state.settings.statuslineRecordUntil);
    if (until === null) {
      throw new Error("statusline_record did not set settings.statuslineRecordUntil");
    }
    state.recordingUntil = until;
    state.keys = readKeys(await state.context.invoke("statusline_keys", {}));
    state.notice = "Recording; the list fills while Claude Code calls the status line.";
    renderAll();
    startKeysPolling(until);
  } catch (error) {
    stopRecording();
    state.keysError = errorMessage(error);
    renderAll();
  }
}

// Read the list again every 5 s while the recorded minute runs, once more after
// the time has passed, and stop when the area is left (`unmount`).
function startKeysPolling(until) {
  const token = ++state.recordingToken;
  const tick = async () => {
    if (!state.active || token !== state.recordingToken) return;
    try {
      state.keys = readKeys(await state.context.invoke("statusline_keys", {}));
      state.keysError = null;
    } catch (error) {
      state.keysError = errorMessage(error);
    }
    if (!state.active || token !== state.recordingToken) return;
    if (Date.now() < until) {
      state.recording = true;
      state.recordingUntil = until;
      state.recordingTimer = setTimeout(tick, 5000);
    } else {
      state.recording = false;
      state.recordingUntil = null;
      state.recordingTimer = null;
      state.recordingToken += 1;
    }
    if (focusInside(state.container)) renderErrors();
    else renderAll();
  };
  state.recordingTimer = setTimeout(tick, 5000);
}

// ---------------------------------------------------------------------------
// enter, leave, update
// ---------------------------------------------------------------------------

async function loadPresets() {
  try {
    state.presets = readPresets(await state.context.invoke("statusline_presets", {}));
    state.presetError = null;
  } catch (error) {
    state.presets = null;
    state.presetError = errorMessage(error);
  }
}

async function loadInstall() {
  try {
    state.install = readInstall(await state.context.invoke("statusline_install_state", {}));
    state.installError = null;
  } catch (error) {
    state.install = null;
    state.installError = errorMessage(error);
  }
}

async function loadKeys() {
  try {
    state.keys = readKeys(await state.context.invoke("statusline_keys", {}));
    state.keysError = null;
  } catch (error) {
    state.keys = null;
    state.keysError = errorMessage(error);
  }
}

async function loadFields() {
  try {
    state.fields = readFields(await state.context.invoke("statusline_fields", {}));
    state.fieldsError = null;
  } catch (error) {
    state.fields = null;
    state.fieldsError = errorMessage(error);
  }
}

export async function render(container, context) {
  if (!isObject(context)) throw new Error("the Status line area needs its context");
  state.active = true;
  state.container = container;
  state.context = context;
  state.snapshot = isObject(context.snapshot) ? context.snapshot : {};
  if (state.previewTimer !== null) {
    clearTimeout(state.previewTimer);
    state.previewTimer = null;
  }
  state.previewToken += 1;
  takeSettings(state.snapshot);
  clear(container);
  const root = h("div", { class: "sl" });
  state.root = root;
  container.appendChild(root);
  if (!state.presets) await loadPresets();
  state.presetName = presetNameOf(state.config);
  state.dirty = state.presetName === null;
  await loadInstall();
  await loadKeys();
  await loadFields();
  if (!state.active || state.container !== container) return;
  renderAll();
}

function focusedChip() {
  const active = document.activeElement;
  if (!active || !state.container || !state.container.contains(active)) return null;
  if (!active.classList || !active.classList.contains("sl-chip")) return null;
  const data = active.dataset;
  if (data.palette) return { palette: data.palette };
  const index = Number(data.index);
  if (typeof data.zone === "string" && Number.isInteger(index)) {
    return { zone: data.zone, index };
  }
  return null;
}

export async function update(snapshot) {
  if (!state.active) return;
  state.snapshot = isObject(snapshot) ? snapshot : {};
  // Every snapshot can change the live numbers the preview shows, so the next
  // request must not be skipped as unchanged. A layout change still waits for
  // `schedulePreview`'s delay.
  state.previewKey = null;
  if (focusInside(state.container)) {
    takeSettings(state.snapshot);
    // Only the preview box is redrawn: the chip or field being typed in stays.
    void requestPreview();
    return;
  }
  const focused = focusedChip();
  if (focused) state.focusTarget = focused;
  takeSettings(state.snapshot);
  // Claude Code's settings file can change by hand or in another window: the
  // head buttons and the install card read it again with every snapshot.
  await loadInstall();
  if (!state.active) return;
  if (focusInside(state.container)) {
    takeSettings(state.snapshot);
    void requestPreview();
    return;
  }
  renderAll();
}

export function unmount() {
  state.active = false;
  stopRecording();
  if (state.previewTimer !== null) {
    clearTimeout(state.previewTimer);
    state.previewTimer = null;
  }
  state.previewToken += 1;
  state.container = null;
  state.context = null;
  state.snapshot = null;
  state.settings = null;
  state.config = null;
  state.root = null;
  state.errorHost = null;
  state.errors = [];
  state.saveError = null;
  state.dragError = null;
  state.installError = null;
  state.presetError = null;
  state.keysError = null;
  state.fieldsError = null;
  state.notice = null;
  state.install = null;
  state.presets = null;
  state.fields = null;
  state.selected = null;
  state.focusTarget = null;
  state.pendingColorRule = null;
  state.dirty = false;
  state.presetName = null;
  state.pendingPreset = null;
  state.cardOpen = false;
  state.widths = ["120", "100", "80"];
  state.previews = null;
  state.previewErrors = [];
  state.previewRows = [];
  state.previewErrorHost = null;
  state.previewKey = null;
  state.keys = null;
  state.recording = false;
  state.recordingUntil = null;
  state.saveGeneration = 0;
  state.droppedSaves = 0;
  state.refusedText = null;
}
