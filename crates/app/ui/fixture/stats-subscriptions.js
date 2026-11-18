// The statistics Subscriptions page's fixture: the mock-up's made-up figures in
// the one `stats_subscriptions` answer (spec R6 of specs/plan-1.3/PLAN.md).

// The snapshot module imports fixture-stats, which imports this file; loading it
// first makes a direct import of this module initialise stats-frame before the
// other fixtures read its constants.
import "../fixture.js";

import {
  API_TOTAL,
  SUBSCRIPTIONS,
  WEEKS,
  byAbbr,
  resolveIds,
  round1,
  round2,
  sum,
} from "./stats-frame.js";

const MODELS = {
  opus: "claude-opus-5-5",
  sonnet: "claude-sonnet-5-5",
  haiku: "claude-haiku-5-5",
  gpt: "gpt-5.5-codex",
  deepseek: "deepseek-v4.1-flash",
  astra: "gpt-6-astra",
};

// M-2's reason for a subscription with no pair of priced work in the range.
const NOT_MEASURED = "not measured yet: no pair with priced work in this range";

// M-2's reason for a subscription whose range holds no finished weekly (or,
// for Ollama, monthly) window.
const NO_FINISHED = "no finished week yet in this range";

// The mock-up's typical tokens per hour of work, in millions (data.py TYP).
const TOKENS_PER_HOUR = {
  [MODELS.opus]: 5.3e6,
  [MODELS.sonnet]: 5.6e6,
  [MODELS.gpt]: 3.1e6,
  [MODELS.deepseek]: 3.0e6,
};

// The mock-up's subscriptions: its plans, monthly prices and used percentages
// over its twelve weeks. O2 is the second OpenAI login: no finished week, no
// price and no totals.
const ROWS = [
  {
    abbr: "LB",
    company: "anthropic",
    plan: "Max",
    price: 180,
    used: [100, 100, 96, 100, 100, 88, 74, 81, 69, 92, 94, 97],
    workHours: 380,
    perPercent: { [MODELS.opus]: 1.6e6, [MODELS.sonnet]: 2.7e6 },
    mIx: 1.9,
    since: null,
    entered: null,
  },
  {
    abbr: "AC",
    company: "anthropic",
    plan: "Team Premium",
    price: 140,
    used: [91, 100, 100, 97, 100, 72, 58, 64, 49, 71, 91, 93],
    workHours: 300,
    perPercent: { [MODELS.opus]: 1.02e6, [MODELS.sonnet]: 2.16e6 },
    mIx: 1.4,
    since: null,
    entered: null,
  },
  {
    abbr: "PR",
    company: "anthropic",
    plan: "Ultra",
    price: 360,
    used: [null, null, null, null, null, 61, 83, 77, 90, 68, 88, 90],
    workHours: 270,
    perPercent: { [MODELS.opus]: 3.2e6, [MODELS.sonnet]: 5.4e6 },
    mIx: 3.8,
    since: "2026-09-21T00:00:00.000Z",
    entered: null,
  },
  {
    abbr: "OA",
    company: "openai",
    plan: "ChatGPT Plus",
    price: 23,
    used: [55, 62, 66, 58, 64, 60, 57, 63, 68, 61, 59, 66],
    workHours: 108,
    perPercent: { [MODELS.gpt]: 0.42e6 },
    mIx: 0.42,
    since: null,
    entered: null,
    api: 296,
  },
  {
    abbr: "O2",
    company: "openai",
    plan: "Pro",
    price: null,
    used: Array(12).fill(null),
    workHours: null,
    perPercent: {},
    mIx: null,
    since: null,
    entered: null,
    api: null,
    missing: "no finished week yet in this range",
  },
  {
    abbr: "OL",
    company: "ollama",
    plan: "Pro",
    price: 18.1,
    used: Array(12).fill(22),
    workHours: 14,
    perPercent: { [MODELS.deepseek]: 2.4e6 },
    mIx: 2.4,
    since: null,
    entered: { amount: 20, currency: "USD" },
    api: 33,
  },
];

for (const row of ROWS) {
  const used = row.used.filter((value) => value !== null);
  row.weeks = used.length;
  row.average = row.weeks > 0 ? sum(used) / row.weeks : null;
  row.weekPrice = row.price === null ? null : (row.price * 12) / 52;
  row.usedMoney =
    row.weekPrice === null
      ? 0
      : sum(used.map((value) => (value / 100) * row.weekPrice));
  row.paid = row.weekPrice === null ? 0 : row.weekPrice * row.weeks;
  row.expiredMoney =
    row.weekPrice === null
      ? 0
      : sum(used.map((value) => ((100 - value) / 100) * row.weekPrice));
}

const CLAUDE_USED = sum(
  ROWS.filter((row) => row.company === "anthropic").map((row) => row.usedMoney)
);
for (const row of ROWS) {
  if (row.api === undefined) {
    row.api =
      row.company === "anthropic" && CLAUDE_USED > 0 && row.weeks > 0
        ? (row.usedMoney / CLAUDE_USED) * API_TOTAL
        : null;
  }
}

function rowByAbbr(abbr) {
  const row = ROWS.find((candidate) => candidate.abbr === abbr);
  if (row === undefined) throw new Error(`the fixture has no subscription ${JSON.stringify(abbr)}`);
  return row;
}

function accountId(abbr) {
  const account = byAbbr(abbr);
  if (!account || typeof account.id !== "string" || account.id === "") {
    throw new Error(`the built-in snapshot has no account ${JSON.stringify(abbr)}`);
  }
  return account.id;
}

function subscriptionId(abbr) {
  const subscription = SUBSCRIPTIONS.find((candidate) => candidate.abbr === abbr);
  if (!subscription || typeof subscription.id !== "string" || subscription.id === "") {
    throw new Error(
      `the built-in snapshot has no provider subscription ${JSON.stringify(abbr)}`
    );
  }
  return subscription.id;
}

function idOf(abbr) {
  return rowByAbbr(abbr).company === "anthropic" ? accountId(abbr) : subscriptionId(abbr);
}

// A model's hours of typical work in a full limit: 100 % × what 1 % buys,
// divided by the mock-up's usual tokens per working hour. Ollama's month is
// shown per week, as the mock-up's "a month: 80 h" note does.
function modelHours(row, model) {
  const per = row.perPercent[model];
  const hours = (100 * per) / TOKENS_PER_HOUR[model];
  return row.abbr === "OL" ? (hours * 12) / 52 : hours;
}

function planRows() {
  return ROWS.map((row) => ({
    id: idOf(row.abbr),
    company: row.company,
    plan: row.plan,
    models: Object.keys(row.perPercent).map((model) => ({
      model,
      hours: modelHours(row, model),
    })),
    trend:
      row.abbr === "AC"
        ? {
            model: MODELS.opus,
            since: "2026-10-19T00:00:00.000Z",
            percent: -20,
            before: 1.28e6,
            after: 1.02e6,
          }
        : null,
    basisHours: row.workHours,
    thin: row.workHours !== null && row.workHours < 40,
    missing: row.abbr === "O2" ? NOT_MEASURED : null,
  }));
}

function sizes() {
  return {
    reference: accountId("LB"),
    rows: [
      {
        id: accountId("LB"),
        plan: "Max",
        ofReference: 1,
        low: 0.95,
        high: 1.05,
        byName: 1,
        measured: true,
      },
      {
        id: accountId("AC"),
        plan: "Team Premium",
        ofReference: 0.64,
        low: 0.59,
        high: 0.69,
        byName: 1,
        measured: true,
      },
      {
        id: accountId("PR"),
        plan: "Ultra",
        ofReference: 2,
        low: 1.9,
        high: 2.1,
        byName: 4,
        measured: true,
      },
    ],
  };
}

function usageRow(row) {
  return {
    id: idOf(row.abbr),
    plan: row.plan,
    since: row.since,
    priceMonth: row.price,
    entered: row.entered,
    usedPercent: row.weeks > 0 ? round1(row.average) : null,
    expired: row.weeks > 0 ? round2(row.expiredMoney) : null,
    api: row.api === null ? null : round2(row.api),
    timesPaid:
      row.api !== null && row.paid > 0 ? round2(row.api / row.paid) : null,
    missing: row.missing || null,
  };
}

function usageGroupsWith(makeRow) {
  return ["anthropic", "openai", "ollama"].map((company) => {
    const rows = ROWS.filter((row) => row.company === company);
    const prices = rows.map((row) => row.price).filter((price) => price !== null);
    return {
      company,
      count: rows.length,
      priceMonth: prices.length === 0 ? null : round2(sum(prices)),
      rows: rows.map(makeRow),
    };
  });
}

function usageGroups() {
  return usageGroupsWith(usageRow);
}

function usageTotal() {
  const priced = ROWS.filter((row) => row.abbr !== "O2");
  const paid = sum(priced.map((row) => row.paid));
  const used = sum(priced.map((row) => row.usedMoney));
  const api = sum(priced.map((row) => row.api || 0));
  return {
    id: "all",
    plan: null,
    since: null,
    priceMonth: round2(sum(priced.map((row) => row.price || 0))),
    entered: null,
    usedPercent: paid > 0 ? round1((used / paid) * 100) : null,
    expired: round2(sum(priced.map((row) => row.expiredMoney))),
    api: round2(api),
    timesPaid: paid > 0 ? round2(api / paid) : null,
    missing: null,
  };
}

function weekPoints(values, spread) {
  const start = WEEKS.length - values.length;
  return values.map((value, index) => {
    const low = spread[index] === undefined ? spread : spread[index];
    return {
      week: `${WEEKS[start + index]}T00:00:00.000Z`,
      tokens: Math.round(value * 1e6),
      low: Math.round((value - low) * 1e6),
      high: Math.round((value + low) * 1e6),
    };
  });
}

function nowRow(model, tokensPerPercent) {
  return [
    {
      model,
      tokens: Math.round(tokensPerPercent),
      minutes: round1((tokensPerPercent / TOKENS_PER_HOUR[model]) * 60),
    },
  ];
}

function perPercentRows() {
  const lbWeek = [1.61, 1.58, 1.6, 1.62, 1.59, 1.6, 1.61, 1.59, 1.6, 1.62, 1.6, 1.59];
  const acWeek = [1.27, 1.29, 1.28, 1.26, 1.29, 1.28, 1.27, 1.3, 1.31, 1.03, 1.01, 1.02];
  const prWeek = [3.19, 3.22, 3.2, 3.18, 3.21, 3.2, 3.19];
  const oaWeek = [0.42, 0.41, 0.42, 0.43, 0.42, 0.42, 0.41, 0.42, 0.42, 0.43, 0.42, 0.42];
  const olMonth = Array(12).fill(2.4);
  const flat = (value, length) => Array(length).fill(value);

  const lbHold = round1(((100 * 0.16e6) / TOKENS_PER_HOUR[MODELS.opus]) * 60);
  const acHold = round1(((100 * 0.12e6) / TOKENS_PER_HOUR[MODELS.opus]) * 60);
  const oaHold = round1(((100 * 0.07e6) / TOKENS_PER_HOUR[MODELS.gpt]) * 60);

  return [
    {
      id: accountId("LB"),
      limit: "week",
      model: MODELS.opus,
      points: weekPoints(lbWeek, 0.06),
      change: null,
      now: [
        ...nowRow(MODELS.opus, 1.6e6),
        ...nowRow(MODELS.sonnet, 2.7e6),
      ],
    },
    {
      id: accountId("LB"),
      limit: "5h",
      model: MODELS.opus,
      points: weekPoints(flat(0.16, 12), flat(0.01, 12)),
      change: null,
      now: nowRow(MODELS.opus, 0.16e6),
      windowHoldsMinutes: lbHold,
    },
    {
      id: accountId("AC"),
      limit: "week",
      model: MODELS.opus,
      points: weekPoints(acWeek, [0.07, 0.07, 0.07, 0.07, 0.07, 0.07, 0.07, 0.07, 0.15, 0.07, 0.07, 0.07]),
      change: {
        model: MODELS.opus,
        since: "2026-10-19T00:00:00.000Z",
        percent: -20,
        before: 1.28e6,
        after: 1.02e6,
      },
      now: [
        ...nowRow(MODELS.opus, 1.02e6),
        ...nowRow(MODELS.sonnet, 2.16e6),
      ],
    },
    {
      id: accountId("AC"),
      limit: "5h",
      model: MODELS.opus,
      points: weekPoints(flat(0.12, 12), flat(0.01, 12)),
      change: null,
      now: nowRow(MODELS.opus, 0.12e6),
      windowHoldsMinutes: acHold,
    },
    {
      id: accountId("PR"),
      limit: "week",
      model: MODELS.opus,
      points: weekPoints(prWeek, [0.12, 0.12, 0.12, 0.12, 0.12, 0.12, 0.25]),
      change: null,
      now: [
        ...nowRow(MODELS.opus, 3.2e6),
        ...nowRow(MODELS.sonnet, 5.4e6),
      ],
    },
    {
      id: subscriptionId("OA"),
      limit: "week",
      model: MODELS.gpt,
      points: weekPoints(oaWeek, 0.03),
      change: null,
      now: nowRow(MODELS.gpt, 0.42e6),
    },
    {
      id: subscriptionId("OA"),
      limit: "5h",
      model: MODELS.gpt,
      points: weekPoints(flat(0.07, 12), flat(0.01, 12)),
      change: null,
      now: nowRow(MODELS.gpt, 0.07e6),
      windowHoldsMinutes: oaHold,
    },
    {
      id: subscriptionId("OL"),
      limit: "month",
      model: MODELS.deepseek,
      points: weekPoints(olMonth, 0.02),
      change: null,
      now: nowRow(MODELS.deepseek, 2.4e6),
    },
  ];
}

function tokensWeek() {
  const models = [
    { model: MODELS.opus, company: "anthropic" },
    { model: MODELS.sonnet, company: "anthropic" },
    { model: MODELS.haiku, company: "anthropic" },
    { model: MODELS.gpt, company: "openai" },
    { model: MODELS.astra, company: "openai" },
  ];
  const cell = (tokens, low, high, estimated = false, thin = false, measured = true) => ({
    tokens: Math.round(tokens * 1e6),
    low: Math.round(low * 1e6),
    high: Math.round(high * 1e6),
    estimated,
    thin,
    measured,
  });
  const estimated = (tokens) => cell(tokens, tokens, tokens, true, false);
  // a factor that is still its plan-size prior: no range, not measured
  const assumed = (tokens) => cell(tokens, tokens, tokens, false, false, false);
  const missing = (reason) => ({ missing: reason });
  const notOffered = (company) => missing(`not offered by ${company}`);
  const rows = [
    {
      id: accountId("LB"),
      cells: [
        cell(160, 156, 164),
        cell(270, 262, 278),
        estimated(810),
        notOffered("Anthropic"),
        notOffered("Anthropic"),
      ],
      missing: null,
    },
    {
      id: accountId("AC"),
      cells: [
        cell(102, 95, 109),
        cell(216, 209, 223),
        estimated(648),
        notOffered("Anthropic"),
        notOffered("Anthropic"),
      ],
      missing: null,
    },
    {
      id: accountId("PR"),
      cells: [
        cell(320, 308, 332),
        cell(540, 520, 560),
        estimated(1620),
        notOffered("Anthropic"),
        notOffered("Anthropic"),
      ],
      missing: null,
    },
    {
      id: subscriptionId("OA"),
      cells: [
        notOffered("OpenAI"),
        notOffered("OpenAI"),
        notOffered("OpenAI"),
        assumed(42),
        missing(`no price for ${MODELS.astra}`),
      ],
      missing: null,
    },
    {
      id: subscriptionId("O2"),
      cells: models.map(() => missing(NOT_MEASURED)),
      missing: NOT_MEASURED,
    },
  ];
  return { models, rows };
}

function totals() {
  const priced = ROWS.filter((row) => row.abbr !== "O2");
  const priceMonth = sum(priced.map((row) => row.price || 0));
  const apiMonth =
    sum(priced.filter((row) => row.weeks > 0).map((row) => row.api / row.weeks)) *
    (52 / 12);
  const tokensMonth =
    sum(
      priced.map(
        (row) =>
          row.average *
          row.mIx *
          (row.abbr === "OL" ? 12 / 52 : 1) *
          1e6
      )
    ) *
    (52 / 12);
  return {
    count: priced.length,
    priceMonth: round2(priceMonth),
    apiMonth: round2(apiMonth),
    timesPaid: priceMonth > 0 ? round2(apiMonth / priceMonth) : null,
    tokensUsedMonth: Math.round(tokensMonth),
    weeks: 12,
    partial: priced
      .filter((row) => row.weeks > 0 && row.weeks < 12)
      .map((row) => ({ id: idOf(row.abbr), weeks: row.weeks })),
  };
}

function noFinishedUsageRow(row) {
  return {
    ...usageRow(row),
    usedPercent: null,
    expired: null,
    api: null,
    timesPaid: null,
    missing: NO_FINISHED,
  };
}

function noFinishedUsageTotal() {
  return {
    id: "all",
    plan: null,
    since: null,
    priceMonth: null,
    entered: null,
    usedPercent: null,
    expired: null,
    api: null,
    timesPaid: null,
    missing: NO_FINISHED,
  };
}

function noFinishedTotals() {
  return {
    count: 0,
    priceMonth: null,
    apiMonth: null,
    timesPaid: null,
    tokensUsedMonth: null,
    weeks: 0,
    partial: [],
  };
}

// A range with no finished window measures no plan: the reference and every
// ratio over it are missing, the names' sizes stay.
function noFinishedSizes() {
  const measured = sizes();
  return {
    reference: null,
    rows: measured.rows.map((row) => ({
      ...row,
      ofReference: null,
      low: null,
      high: null,
      // no measured reference, so no size by name either (M-2 fix 3)
      byName: null,
      measured: false,
    })),
  };
}

function noFinishedPerPercent() {
  return perPercentRows().map((entry) => ({
    ...entry,
    points: [],
    change: null,
  }));
}

function finishedPage() {
  return {
    totals: totals(),
    plans: planRows(),
    sizes: sizes(),
    usage: usageGroups(),
    usageTotal: usageTotal(),
    perPercent: perPercentRows(),
    tokensWeek: tokensWeek(),
    currency: "EUR",
  };
}

function nothingFinishedPage() {
  return {
    totals: noFinishedTotals(),
    plans: planRows(),
    sizes: noFinishedSizes(),
    usage: usageGroupsWith(noFinishedUsageRow),
    usageTotal: noFinishedUsageTotal(),
    perPercent: noFinishedPerPercent(),
    tokensWeek: tokensWeek(),
    currency: "EUR",
  };
}

export function subscriptions(query) {
  resolveIds();
  // "This week" holds the running week only: no weekly window has finished,
  // so the answer is the one with nothing counted. Every other range keeps
  // the mock-up's figures.
  if (query !== null && typeof query === "object" && query.range === "thisWeek") {
    return nothingFinishedPage();
  }
  return finishedPage();
}
