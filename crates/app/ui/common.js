// seatswap page helpers shared by the pop-up (panel.js) and the large window
// (window.js). Moved out of panel.js unchanged; both pages import them from here.

import { fixtureInvoke } from "./fixture.js";

// ---------------------------------------------------------------------------
// constants
// ---------------------------------------------------------------------------

const PALETTE = [
  "#3b82f6", "#f59e0b", "#10b981", "#ef4444", "#8b5cf6",
  "#ec4899", "#14b8a6", "#f97316", "#6366f1", "#84cc16",
];

// The tray's grey for an unknown or invalid account colour (icon.rs GREY).
const TRAY_GREY = "#555b66";

const LEVEL_COLORS = {
  fine: "#22c55e",
  close: "#f59e0b",
  critical: "#ef4444",
  locked: "#ef4444",
};

const TAURI_GLOBAL = typeof window !== "undefined" ? window.__TAURI__ : undefined;
const HAS_TAURI = Boolean(
  TAURI_GLOBAL && TAURI_GLOBAL.core && typeof TAURI_GLOBAL.core.invoke === "function"
);

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function clamp(value, low, high) {
  return Math.min(high, Math.max(low, value));
}

function clone(value) {
  return value === undefined || value === null ? value : JSON.parse(JSON.stringify(value));
}

function errorMessage(error) {
  if (typeof error === "string") return error;
  if (error && typeof error.message === "string") return error.message;
  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}

function isColor(value) {
  return typeof value === "string" && /^#[0-9a-fA-F]{6}$/.test(value);
}

function hexToRgb(value) {
  if (!isColor(value)) return null;
  const number = parseInt(value.slice(1), 16);
  return {
    r: (number >> 16) & 0xff,
    g: (number >> 8) & 0xff,
    b: number & 0xff,
  };
}

function rgba(value, alpha) {
  const rgb = hexToRgb(value);
  if (!rgb) return `rgba(139, 147, 167, ${alpha})`;
  return `rgba(${rgb.r}, ${rgb.g}, ${rgb.b}, ${alpha})`;
}

function rgbToHex(rgb) {
  const part = (number) => clamp(Math.round(number), 0, 255).toString(16).padStart(2, "0");
  return `#${part(rgb.r)}${part(rgb.g)}${part(rgb.b)}`;
}

function mixHex(first, second, ratio) {
  const a = hexToRgb(first);
  const b = hexToRgb(second);
  if (!a || !b) return first;
  const mix = (x, y) => x + (y - x) * ratio;
  return rgbToHex({ r: mix(a.r, b.r), g: mix(a.g, b.g), b: mix(a.b, b.b) });
}

function relativeLuminance(value) {
  const rgb = hexToRgb(value);
  if (!rgb) return 0;
  const channel = (part) => {
    const scaled = part / 255;
    return scaled <= 0.03928 ? scaled / 12.92 : Math.pow((scaled + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * channel(rgb.r) + 0.7152 * channel(rgb.g) + 0.0722 * channel(rgb.b);
}

function contrastText(background) {
  const luminance = relativeLuminance(background);
  const whiteRatio = 1.05 / (luminance + 0.05);
  const darkRatio = (luminance + 0.05) / (relativeLuminance("#0f1115") + 0.05);
  return darkRatio >= whiteRatio ? "#0f1115" : "#ffffff";
}

function limitLevel(limit) {
  const percent = Number(limit && limit.percent);
  if (!Number.isFinite(percent)) return "fine";
  if (percent >= 100) return "locked";
  if (percent >= 90 || limit.severity === "critical") return "critical";
  if (percent >= 70) return "close";
  return "fine";
}

function shortPercent(value) {
  const number = Number(value);
  if (!Number.isFinite(number)) return "–";
  const rounded = Math.round(number * 10) / 10;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

function clear(node) {
  while (node && node.firstChild) node.removeChild(node.firstChild);
}

function setStyles(node, styles) {
  for (const [key, value] of Object.entries(styles)) {
    if (value === undefined || value === null) continue;
    if (key.startsWith("--")) node.style.setProperty(key, String(value));
    else node.style[key] = value;
  }
}

function applyProps(node, props) {
  for (const [key, value] of Object.entries(props || {})) {
    if (key === "hidden") {
      node.hidden = Boolean(value);
      continue;
    }
    if (key === "checked") {
      node.checked = Boolean(value);
      continue;
    }
    if (key === "disabled") {
      node.disabled = Boolean(value);
      continue;
    }
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") node.className = value;
    else if (key === "text") node.textContent = String(value);
    else if (key === "style") setStyles(node, value);
    else if (key === "dataset") {
      for (const [dataKey, dataValue] of Object.entries(value)) {
        if (dataValue !== undefined && dataValue !== null) node.dataset[dataKey] = String(dataValue);
      }
    } else if (key === "value") {
      node.value = value;
    } else if (key.startsWith("on") && typeof value === "function") {
      node.addEventListener(key.slice(2), value);
    } else if (value === true) {
      node.setAttribute(key, "");
    } else {
      node.setAttribute(key, String(value));
    }
  }
}

function appendChildren(node, children) {
  const list = Array.isArray(children) ? children : [children];
  for (const child of list) {
    if (child === undefined || child === null || child === false) continue;
    if (typeof child === "string" || typeof child === "number") {
      node.appendChild(document.createTextNode(String(child)));
    } else {
      node.appendChild(child);
    }
  }
}

function h(tag, props, children) {
  const node = document.createElement(tag);
  applyProps(node, props || {});
  if (children !== undefined) appendChildren(node, children);
  return node;
}

function applyTheme(setting) {
  // Callers that follow a stored setting pass it; this page alone knows where
  // that setting is kept.
  const wanted = setting || "system";
  let resolved = wanted;
  if (wanted !== "light" && wanted !== "dark") {
    const light =
      typeof window !== "undefined" &&
      window.matchMedia &&
      window.matchMedia("(prefers-color-scheme: light)").matches;
    resolved = light ? "light" : "dark";
  }
  document.documentElement.dataset.theme = resolved;
}

// ---------------------------------------------------------------------------
// bridge: invoke() and listen() over the Tauri 2 global
// ---------------------------------------------------------------------------

async function invoke(command, args) {
  if (HAS_TAURI) {
    return TAURI_GLOBAL.core.invoke(command, args || {});
  }
  return fixtureInvoke(command, args || {});
}

async function listen(eventName, handler) {
  if (HAS_TAURI && TAURI_GLOBAL.event && typeof TAURI_GLOBAL.event.listen === "function") {
    const unlisten = await TAURI_GLOBAL.event.listen(eventName, (event) => {
      handler(event && Object.prototype.hasOwnProperty.call(event, "payload") ? event.payload : event);
    });
    return unlisten;
  }
  return () => {};
}

// ---------------------------------------------------------------------------
// money and token counts (spec §1.4; the plan's Global Constraints)
// ---------------------------------------------------------------------------

// "1,234 EUR" from 10 on, "6.47 EUR" below 10. A bad value is an error, never a
// fallback: the page shows the error text.
function money(amount, currency) {
  if (typeof amount !== "number" || !Number.isFinite(amount) || amount < 0) {
    throw new Error(`invalid amount ${String(amount)}: use a finite number of 0 or more`);
  }
  if (typeof currency !== "string" || !/^[A-Z]{3}$/.test(currency)) {
    throw new Error(
      `invalid currency ${JSON.stringify(currency)}: use three capital letters A to Z`
    );
  }
  const value = Math.round(amount * 100) / 100;
  const number =
    value < 10
      ? value.toFixed(2)
      : String(Math.round(value)).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return `${number} ${currency}`;
}

// "12.4 M" from a million, "845 k" from a thousand, "0" and the count below that.
function tokens(n) {
  if (typeof n !== "number" || !Number.isFinite(n) || n < 0) {
    throw new Error(`invalid token count ${String(n)}: use a finite number of 0 or more`);
  }
  if (n >= 1000000) return `${(n / 1000000).toFixed(1)} M`;
  if (n >= 1000) return `${Math.round(n / 1000)} k`;
  return String(Math.round(n));
}

export {
  PALETTE,
  TRAY_GREY,
  LEVEL_COLORS,
  TAURI_GLOBAL,
  HAS_TAURI,
  isObject,
  clamp,
  clone,
  errorMessage,
  isColor,
  hexToRgb,
  rgba,
  rgbToHex,
  mixHex,
  relativeLuminance,
  contrastText,
  limitLevel,
  shortPercent,
  clear,
  setStyles,
  applyProps,
  appendChildren,
  h,
  applyTheme,
  invoke,
  listen,
  money,
  tokens,
};
