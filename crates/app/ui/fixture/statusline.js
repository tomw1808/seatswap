// The status line builder's fixture answers: `statusline_presets`,
// `statusline_kinds`, `statusline_preview`, `statusline_record`,
// `statusline_keys`, `statusline_fields`, `statusline_install_state`,
// `statusline_install` and `statusline_restore`.

import { clone, isColor, isObject } from "../common.js";
import { DEFAULT_SETTINGS, isoIn } from "../fixture.js";
import { SEPARATOR_CHARS } from "../areas/statusline.js";

// ---------------------------------------------------------------------------
// the status line builder (spec §5.4). This is fixture data for the browser,
// not a second renderer: the real preview comes from `statusline_preview`,
// which runs the same `render_spans` as the installed command.
// ---------------------------------------------------------------------------

// The one kind table `statusline_kinds` sends, in the palette's order. It is
// written the way `store.rs`'s `KindSpec` serialises, so the Rust test that
// reads this file can compare it with `SegmentKind::ALL`.
export const FIXTURE_KINDS = [
  { "kind": "account", "label": "account", "group": "seatswap", "colors": ["none", "account", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "plan", "label": "plan", "group": "seatswap", "colors": ["none", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "fiveHourPercent", "label": "5h percent", "group": "seatswap", "colors": ["none", "byLoad", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "fiveHourReset", "label": "5h reset", "group": "seatswap", "colors": ["none", "fixed"], "time": true, "text": false, "repeatable": false },
  { "kind": "weekPercent", "label": "week percent", "group": "seatswap", "colors": ["none", "byLoad", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "weekReset", "label": "week reset", "group": "seatswap", "colors": ["none", "fixed"], "time": true, "text": false, "repeatable": false },
  { "kind": "modelPercent", "label": "per-model %", "group": "seatswap", "colors": ["none", "byLoad", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "autoBurn", "label": "auto-burn: next", "group": "seatswap", "colors": ["none", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "model", "label": "model", "group": "claudeCode", "colors": ["none", "byModelFamily", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "effort", "label": "effort", "group": "claudeCode", "colors": ["none", "byEffort", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "folder", "label": "folder", "group": "claudeCode", "colors": ["none", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "gitBranch", "label": "git branch", "group": "claudeCode", "colors": ["none", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "contextUsed", "label": "context used", "group": "claudeCode", "colors": ["none", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "sessionCost", "label": "session cost", "group": "claudeCode", "colors": ["none", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "linesChanged", "label": "lines changed", "group": "claudeCode", "colors": ["none", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "userHost", "label": "user@host", "group": "system", "colors": ["none", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "clock", "label": "clock", "group": "system", "colors": ["none", "fixed"], "time": false, "text": false, "repeatable": false },
  { "kind": "text", "label": "free text", "group": "system", "colors": ["none", "fixed"], "time": false, "text": true, "repeatable": true },
  { "kind": "separator", "label": "separator", "group": "system", "colors": ["none", "fixed"], "time": false, "text": false, "repeatable": true }
];

function fixtureKindLabel(kind) {
  const spec = FIXTURE_KINDS.find((entry) => entry.kind === kind);
  return spec ? spec.label : kind;
}

function fixtureCurrentLine() {
  return clone(DEFAULT_SETTINGS.statusline);
}

// The three `StatuslineConfig::presets()`, as data: the preset "My current
// line", "Limits only" and "Everything".
export function fixturePresets() {
  const current = fixtureCurrentLine();
  const limits = { left: [], right: clone(current.right), margin: current.margin };
  const left = [
    { kind: "userHost", color: { rule: "fixed", color: "#22c55e" } },
    { kind: "text", color: { rule: "none" }, text: ":" },
    { kind: "folder", color: { rule: "fixed", color: "#3b82f6" } },
    { kind: "gitBranch", color: { rule: "fixed", color: "#22c55e" }, prefix: " ", drop: 3 },
    { kind: "model", color: { rule: "byModelFamily" }, prefix: " " },
    { kind: "effort", color: { rule: "byEffort" }, prefix: " " },
    { kind: "plan", color: { rule: "none" }, prefix: " " },
    { kind: "modelPercent", color: { rule: "none" }, prefix: " " },
    { kind: "autoBurn", color: { rule: "none" }, prefix: " " },
    { kind: "contextUsed", color: { rule: "none" }, prefix: " " },
    { kind: "sessionCost", color: { rule: "none" }, prefix: " " },
    { kind: "linesChanged", color: { rule: "none" }, prefix: " " },
    { kind: "clock", color: { rule: "none" }, prefix: " " },
    { kind: "separator", color: { rule: "none" }, prefix: " ", suffix: " " },
  ];
  return [
    ["My current line", current],
    ["Limits only", limits],
    ["Everything", { left, right: clone(current.right), margin: current.margin }],
  ];
}

function fixtureResetText(kind, segment) {
  const format =
    isObject(segment) && typeof segment.timeFormat === "string"
      ? segment.timeFormat
      : "seatswap";
  if (kind === "fiveHourReset") {
    return format === "clock" ? "19:30" : "4h 45min";
  }
  if (format === "clock") return "Mon 02:00";
  if (format === "compactLeft") return "3d 16h";
  return "1d 18h";
}

// One segment's made-up text for the browser preview. The shapes follow the
// renderer's values; the numbers are the README §5 payload's.
function fixtureSampleValue(kind, segment, scenario) {
  switch (kind) {
    case "account":
      return "AC";
    case "plan":
      return "Team 5x";
    case "fiveHourPercent":
      return scenario === "noSessionLimit" ? null : "12";
    case "fiveHourReset":
      return scenario === "noSessionLimit" ? null : fixtureResetText(kind, segment);
    case "weekPercent":
      if (scenario === "nearFull") return "97";
      if (scenario === "blocked") return "100";
      return "48";
    case "weekReset":
      return fixtureResetText(kind, segment);
    case "modelPercent":
      return "Fable 21%";
    case "autoBurn":
      return "LB 12:00";
    case "model":
      return "Fable 5.2";
    case "effort":
      return "high";
    case "folder":
      return "~/projects/atlas";
    case "gitBranch":
      return "main";
    case "userHost":
      return "sam@atlas";
    case "clock":
      return "14:32";
    case "text":
      return typeof segment.text === "string" && segment.text !== "" ? segment.text : null;
    case "separator": {
      // The page's one table of the six characters (store.rs's
      // `SeparatorChar`); a separator saved without one is a pipe.
      const chosen = typeof segment.separator === "string" ? segment.separator : "pipe";
      const found = SEPARATOR_CHARS.find(([id]) => id === chosen);
      return found ? found[1] : null;
    }
    case "contextUsed":
      return "41%";
    case "sessionCost":
      return "$3.20";
    case "linesChanged":
      return "+120 -8";
    default:
      return null;
  }
}

function fixtureSamplePercent(kind, scenario) {
  if (kind === "fiveHourPercent") return scenario === "noSessionLimit" ? null : 12;
  if (kind === "weekPercent") {
    if (scenario === "nearFull") return 97;
    if (scenario === "blocked") return 100;
    return 48;
  }
  if (kind === "modelPercent") return 21;
  return null;
}

function fixtureSpanColour(kind, segment, scenario) {
  const rule =
    isObject(segment.color) && typeof segment.color.rule === "string"
      ? segment.color.rule
      : "none";
  if (rule === "fixed" && isColor(segment.color.color)) {
    return { fg: segment.color.color };
  }
  if (rule === "account") {
    return { fg: "#dbeafe", bg: "#3b82f6" };
  }
  if (rule === "byModelFamily") return { fg: "#c084fc" };
  if (rule === "byEffort") return { fg: "#fbbf24" };
  if (rule === "byLoad") {
    const percent = fixtureSamplePercent(kind, scenario);
    if (percent === null) return {};
    const warn = Number(segment.color.warn);
    const bad = Number(segment.color.bad);
    if (percent >= bad) return { fg: "#ef4444" };
    if (percent >= warn) return { fg: "#eab308" };
    return { fg: "#22c55e" };
  }
  return {};
}

function fixturePiece(segment, scenario) {
  if (!isObject(segment) || typeof segment.kind !== "string") {
    throw new Error("a statusline preview segment needs a kind");
  }
  const value = fixtureSampleValue(segment.kind, segment, scenario);
  if (value === null) return null;
  const prefix = typeof segment.prefix === "string" ? segment.prefix : "";
  const suffix = typeof segment.suffix === "string" ? segment.suffix : "";
  return {
    kind: segment.kind,
    drop:
      Number.isInteger(segment.drop) && segment.drop >= 1 ? segment.drop : null,
    prefix,
    value,
    suffix,
    text: `${prefix}${value}${suffix}`,
    colour: fixtureSpanColour(segment.kind, segment, scenario),
  };
}

// One segment as the spans `render.rs` writes: a plain prefix, the value with
// the colour rule's foreground and background, and the suffix. A suffix keeps
// the value's foreground unless the rule paints a background: then the colour
// stops at the value, as it does for the account tag.
function fixtureSpans(piece) {
  const colour = piece.colour || {};
  const fg = colour.fg || null;
  const bg = colour.bg || null;
  const spans = [];
  if (piece.prefix !== "") {
    spans.push({ text: piece.prefix, fg: null, bg: null, bold: false });
  }
  if (piece.value !== "") {
    spans.push({ text: piece.value, fg, bg, bold: false });
  }
  if (piece.suffix !== "") {
    spans.push({ text: piece.suffix, fg: bg ? null : fg, bg: null, bold: false });
  }
  return spans;
}

function fixtureDroppedNote(names) {
  if (names.length === 0) return "everything fits";
  if (names.length === 1) return `${names[0]} is dropped`;
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]} are dropped`;
}

// The browser's stand-in for `statusline_preview`: the same shape, one made-up
// line per width, cut by the segments' drop numbers. It is deliberately not a
// copy of the Rust renderer; it only gives the page something to draw.
export function fixtureStatuslinePreview(config, widths, scenario) {
  if (!isObject(config) || !Array.isArray(config.left) || !Array.isArray(config.right)) {
    throw new Error("statusline_preview needs config.left and config.right");
  }
  if (!Number.isInteger(config.margin) || config.margin < 0) {
    throw new Error(`statusline.margin must be a whole number of columns, got ${JSON.stringify(config.margin)}`);
  }
  if (!Array.isArray(widths) || widths.length === 0) {
    throw new Error("the preview needs at least one width");
  }
  for (const width of widths) {
    if (!Number.isInteger(width)) {
      throw new Error(`a preview width must be a whole number of columns, got ${JSON.stringify(width)}`);
    }
    if (width === 0) {
      throw new Error("a preview width must be at least 1 column; 0 cannot show a line");
    }
    if (width < 0 || width > 65535) {
      throw new Error(`invalid value: integer \`${width}\`, expected u16`);
    }
  }
  if (!["live", "nearFull", "blocked", "noSessionLimit"].includes(scenario)) {
    throw new Error(`unknown preview scenario ${JSON.stringify(scenario)}`);
  }
  const leftPieces = config.left.map((segment) => fixturePiece(segment, scenario));
  const rightPieces = config.right.map((segment) => fixturePiece(segment, scenario));
  return widths.map((width) => {
    let level = 0;
    let chosen = null;
    while (true) {
      const left = leftPieces.filter((piece) => piece === null || piece.drop === null || piece.drop > level);
      const right = rightPieces.filter((piece) => piece === null || piece.drop === null || piece.drop > level);
      const leftLength = left.reduce((sum, piece) => sum + (piece ? piece.text.length : 0), 0);
      const rightLength = right.reduce((sum, piece) => sum + (piece ? piece.text.length : 0), 0);
      // The same gap rule as render.rs: the two spaces belong between two zones
      // that both print something; an empty zone needs none.
      const gapRule = leftLength > 0 && rightLength > 0 ? 2 : 0;
      const gap = Math.max(gapRule, width - config.margin - leftLength - rightLength);
      const fits = leftLength + gapRule + rightLength <= width - config.margin;
      chosen = { left, right, gap, fits };
      if (fits || level >= 3) break;
      level += 1;
    }
    const droppedNames = [];
    const dropped = [];
    for (const piece of leftPieces.concat(rightPieces)) {
      if (!piece || piece.drop === null || piece.drop > level) continue;
      const name = fixtureKindLabel(piece.kind);
      if (!droppedNames.includes(name)) droppedNames.push(name);
      if (!dropped.includes(piece.drop)) dropped.push(piece.drop);
    }
    dropped.sort((one, other) => one - other);
    const spans = [];
    for (const piece of chosen.left) {
      if (!piece) continue;
      for (const span of fixtureSpans(piece)) spans.push(span);
    }
    const rightPrints = chosen.right.some((piece) => piece !== null && piece.text.length > 0);
    if (rightPrints && chosen.gap > 0) {
      spans.push({ text: " ".repeat(chosen.gap), fg: null, bg: null, bold: false });
    }
    for (const piece of chosen.right) {
      if (!piece) continue;
      for (const span of fixtureSpans(piece)) spans.push(span);
    }
    return { width, spans, dropped, note: fixtureDroppedNote(droppedNames) };
  });
}

// Most, but not all, of the optional segments' paths: `cost.total_lines_removed`
// is left out, so an optional segment's missing field is visible in the browser.
export function fixtureRecordedKeys() {
  return {
    at: new Date().toISOString(),
    keys: [
      { path: "context_window.context_window_size", kind: "number" },
      { path: "context_window.used_percentage", kind: "number" },
      { path: "cost.total_cost_usd", kind: "number" },
      { path: "cost.total_duration_ms", kind: "number" },
      { path: "cost.total_lines_added", kind: "number" },
      { path: "cwd", kind: "string" },
      { path: "effort.level", kind: "string" },
      { path: "model.display_name", kind: "string" },
      { path: "model.id", kind: "string" },
      { path: "rate_limits.five_hour.resets_at", kind: "number" },
      { path: "rate_limits.five_hour.used_percentage", kind: "number" },
      { path: "rate_limits.seven_day.resets_at", kind: "number" },
      { path: "rate_limits.seven_day.used_percentage", kind: "number" },
      { path: "session_id", kind: "string" },
      { path: "version", kind: "string" },
      { path: "workspace.current_dir", kind: "string" },
    ],
  };
}

// The payload fields the optional segments read, as `statusline_fields`
// returns them: the same three kinds and paths as render.rs's one table.
export function fixtureFields() {
  return [
    { kind: "contextUsed", paths: ["context_window.used_percentage"] },
    { kind: "sessionCost", paths: ["cost.total_cost_usd"] },
    { kind: "linesChanged", paths: ["cost.total_lines_added", "cost.total_lines_removed"] },
  ];
}

export function sameFixtureValue(one, other) {
  return JSON.stringify(one) === JSON.stringify(other);
}

// window.rs's `install_state_of`: `restorable` is the record's own rule, and
// `previous` is what the record holds (absent means there is no record).
export function fixtureInstallState(fixture) {
  const record = isObject(fixture.settings) ? fixture.settings.statuslineInstall : null;
  const claude = fixture.statuslineClaude;
  const current = claude.current === undefined ? null : claude.current;
  const installed = current !== null && sameFixtureValue(current, claude.ours);
  const restorable =
    isObject(record) &&
    (installed || (current !== null && sameFixtureValue(current, record.installed)));
  const previous =
    isObject(record) && record.previous !== undefined ? clone(record.previous) : null;
  return {
    current: clone(current),
    ours: clone(claude.ours),
    installed,
    restorable,
    previous,
    settingsFile: claude.settingsFile,
  };
}
