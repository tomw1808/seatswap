// The statistics Projects page's fixture: `stats_projects` and
// `stats_project_detail`.

import {
  ACCOUNTS,
  API_PER_M,
  OUT_M,
  SUBSCRIPTIONS,
  TOKENS_PER_PERCENT,
  WEEKS,
  accountOut,
  byAbbr,
  chosenAccounts,
  fixtureMoney,
  fixtureValues,
  fixtureWholePercent,
  joinNames,
  sum,
  tokenCounts,
  usedMoney,
  weekIso,
} from "./stats-frame.js";
import { usedPercentTotal } from "./stats-weeks.js";

function keyOf(entry) {
  if (entry.key === null) return null;
  return { kind: entry.key[0], id: entry.key[1] };
}

function sameKey(first, second) {
  return Boolean(
    first &&
      second &&
      first.kind === second.kind &&
      first.id === second.id
  );
}

// The fixture's tree, in projects.rs's order: an organization, its projects,
// their folders. An organization is open by default, a project folded, so the
// folders below acme-kb and atlas show the page's fold.
const TREE = [
  { level: 0, name: "acme", share: 38, split: [15, 55, 30], sessions: 253, key: null },
  { level: 1, name: "acme-kb", share: 38, split: [14, 58, 28], sessions: 253, key: ["repo", "github.com/acme/acme-kb"] },
  { level: 2, name: "ingest", share: 22, split: [14, 58, 28], sessions: 146, key: null },
  { level: 2, name: "web", share: 11, split: [14, 58, 28], sessions: 73, key: null },
  { level: 2, name: "(top folder)", share: 5, split: [14, 58, 28], sessions: 34, key: null },
  { level: 0, name: "ledger-co", share: 62, split: [51, 7, 42], sessions: 428, key: null },
  { level: 1, name: "atlas", share: 34, split: [55, 5, 40], sessions: 268, key: ["repo", "github.com/ledger-co/atlas"] },
  { level: 2, name: "engine", share: 21, split: [55, 5, 40], sessions: 160, key: null },
  { level: 2, name: "frontend", share: 11, split: [55, 5, 40], sessions: 91, key: null },
  { level: 2, name: "(top folder)", share: 2, split: [55, 5, 40], sessions: 17, key: null },
  { level: 1, name: "ledger", share: 17, split: [44, 10, 46], sessions: 97, key: ["repo", "github.com/ledger-co/ledger"] },
  { level: 1, name: "notes", share: 11, split: [48, 10, 42], sessions: 63, key: ["repo", "github.com/ledger-co/notes"] },
];

const ORG_ACCOUNTS = { acme: ["AC"], "ledger-co": ["LB", "PR"] };

// The shares of a row's API value that are no Claude account's: OA, OL and the
// work without a known subscription. Every row carries all three, so the page
// draws each colour and the grey striped part.
const OTHER_PARTS = new Map(
  TREE.map((entry, index) => [
    `${entry.level}:${entry.name}`,
    [3 + (index % 4), 2 + ((index * 3) % 5), 2 + ((index * 2) % 3)],
  ])
);

function accountId(abbr) {
  const account = byAbbr(abbr);
  if (!account) throw new Error(`unknown account abbreviation ${JSON.stringify(abbr)}`);
  return account.id;
}

function subscriptionId(abbr) {
  const subscription = SUBSCRIPTIONS.find((candidate) => candidate.abbr === abbr);
  if (!subscription) {
    throw new Error(`unknown provider subscription ${JSON.stringify(abbr)}`);
  }
  return subscription.id;
}

function abbrOf(id) {
  const account = ACCOUNTS.find((candidate) => candidate.id === id);
  if (account) return account.abbr;
  const subscription = SUBSCRIPTIONS.find((candidate) => candidate.id === id);
  if (subscription) return subscription.abbr;
  throw new Error(`unknown subscription id ${JSON.stringify(id)}`);
}

// The row's parts: PR, LB and AC split the Claude share of its API value by the
// tree's weights, then OA, OL and the null part follow (projects.rs sends the
// largest first; ties keep the chips' order and the null part last).
function subscriptionParts(entry) {
  const others = OTHER_PARTS.get(`${entry.level}:${entry.name}`);
  if (!others) {
    throw new Error(`no subscription split for ${JSON.stringify(entry.name)}`);
  }
  const [oa, ol, unknown] = others;
  const claude = 100 - oa - ol - unknown;
  if (claude <= 0) {
    throw new Error(`the other subscriptions of ${JSON.stringify(entry.name)} leave no Claude share`);
  }
  const weights = ACCOUNTS.map((account, index) => ({
    account,
    weight: entry.split[index],
  }));
  const total = sum(weights.map((row) => row.weight));
  if (total <= 0) {
    throw new Error(`the Claude weights of ${JSON.stringify(entry.name)} are empty`);
  }
  const parts = weights.map((row) => [row.account.id, (row.weight / total) * claude]);
  parts.push([subscriptionId("OA"), oa]);
  parts.push([subscriptionId("OL"), ol]);
  parts.push([null, unknown]);
  parts.sort(
    (a, b) =>
      b[1] - a[1] ||
      (a[0] === null ? 1 : 0) - (b[0] === null ? 1 : 0) ||
      String(a[0]).localeCompare(String(b[0]))
  );
  return parts;
}

// The known part of a row or column whose missing cells the totals keep (R3).
function knownPart(cells) {
  let total = 0;
  let partial = false;
  for (const cell of cells) {
    if (cell === null) {
      partial = true;
      continue;
    }
    if (typeof cell !== "number" || !Number.isFinite(cell)) {
      throw new Error(`a matrix cell is a number or null, not ${JSON.stringify(cell)}`);
    }
    total += cell;
  }
  return { total, partial };
}

// The matrix's two Claude columns and its OA and OL columns, ids in the shape
// projects.rs writes (`subscriptions`).
const MATRIX_CLAUDE_ABBRS = ["LB", "AC"];
const MATRIX_PROVIDER_ABBRS = ["OA", "OL"];

// OL has no price, so its cell is null in one organization: that row's total,
// OL's column total and the grand total keep the known part (rules R3 and R10).
const MATRIX_CELLS = {
  acme: { LB: 12.4, AC: 16.52, OA: 2.6, OL: 1.75 },
  "ledger-co": { LB: 8.3, AC: 3.1, OA: 1.25, OL: null },
};

const MATRIX_PARTIAL = "the account has no monthly price";

function matrixSubscriptions() {
  return [
    ...MATRIX_CLAUDE_ABBRS.map(accountId),
    ...MATRIX_PROVIDER_ABBRS.map(subscriptionId),
  ];
}

function matrixCells(org, subscriptions) {
  const row = MATRIX_CELLS[org];
  if (!row) throw new Error(`no matrix cells for ${JSON.stringify(org)}`);
  return subscriptions.map((id) => {
    const abbr = abbrOf(id);
    if (!Object.prototype.hasOwnProperty.call(row, abbr)) {
      throw new Error(`no ${JSON.stringify(abbr)} cell for ${JSON.stringify(org)}`);
    }
    return row[abbr];
  });
}

// The matrix's `Values` totals: only the subscription half is drawn, so the API
// half names why it is not (rule R10).
function subscriptionValues(amount, missing) {
  return {
    api: null,
    apiCurrency: null,
    apiMissing: "the matrix draws subscription values only",
    apiPartial: false,
    subscription: amount,
    currency: "EUR",
    subscriptionMissing: missing,
    subscriptionPartial: missing !== null,
  };
}

function numericCells(cells) {
  return cells.filter((cell) => typeof cell === "number" && Number.isFinite(cell));
}

function matrixSentences(orgs, subscriptions, cells) {
  const sentences = [];
  for (const [rowIndex, org] of orgs.entries()) {
    const ownAbbrs = ORG_ACCOUNTS[org] || [];
    const own = subscriptions
      .map((id, column) => ({ column, abbr: abbrOf(id) }))
      .filter((entry) => ownAbbrs.includes(entry.abbr));
    if (own.length === 0) continue;
    const total = sum(numericCells(cells[rowIndex]));
    const ownSub = sum(
      numericCells(own.map((entry) => cells[rowIndex][entry.column]))
    );
    if (total <= 0 || ownSub <= 0) continue;
    const names = joinNames(own.map((entry) => entry.abbr));
    const noun = own.length === 1 ? "subscription" : "subscriptions";
    const verb = own.length === 1 ? "is" : "are";
    const share = (ownSub / total) * 100;
    let sentence = `${names} ${verb} ${org}'s ${noun}. It covered ${fixtureWholePercent(
      share
    )} of the work for ${org}.`;
    if (share < 100) {
      const others = subscriptions
        .map((id, column) => ({ column, abbr: abbrOf(id) }))
        .filter(
          (entry) =>
            !ownAbbrs.includes(entry.abbr) &&
            typeof cells[rowIndex][entry.column] === "number" &&
            cells[rowIndex][entry.column] > 0
        );
      const amount = sum(
        others.map((entry) => cells[rowIndex][entry.column])
      );
      if (others.length > 0 && amount > 0) {
        sentence += ` The rest ran on ${joinNames(
          others.map((entry) => entry.abbr)
        )}: ${fixtureMoney(amount, "EUR")} of their value.`;
      }
    }
    sentences.push(sentence);

    let carried = 0;
    const otherOrgs = [];
    for (const [otherIndex, other] of orgs.entries()) {
      if (other === org) continue;
      const value = sum(
        numericCells(own.map((entry) => cells[otherIndex][entry.column]))
      );
      if (value > 0) {
        carried += value;
        otherOrgs.push(other);
      }
    }
    if (carried > 0 && otherOrgs.length > 0) {
      sentences.push(
        `The other way round: ${names} carried ${fixtureMoney(
          carried,
          "EUR"
        )} of work for ${joinNames(otherOrgs)}.`
      );
    }
  }
  return sentences;
}

// The mock-up's dollar rate: 1 USD = 0.9048 EUR, looked up on 13 Nov
// (projects.rs answers the rate its API values are converted with, with the
// currency it converts into and whether the range's days had more than one).
const USD_RATE = {
  value: 0.9048,
  date: "2026-11-13",
  currency: "EUR",
  varies: false,
};

// One project used a model that has no price, early on: only the range `All`
// holds that work. There, as projects.rs answers: the project's API value is
// unknown while its tokens and sessions are there; its organization's and the
// page's API sums are partial; and a share or a subscription split is computed
// only from full halves (R3), so every row's share is `null` and the
// organization above the project has no split. The other ranges are fully
// priced.
const UNPRICED_PROJECT = "notes";
const UNPRICED_MODEL = "claude-sonnet-3-9";

function unpricedIn(query) {
  return query.range === "all";
}

function unpricedEntry() {
  const entry = TREE.find((candidate) => candidate.level === 1 && candidate.name === UNPRICED_PROJECT);
  if (!entry) throw new Error(`the unpriced project ${JSON.stringify(UNPRICED_PROJECT)} is not in the tree`);
  return entry;
}

// The organization row above the unpriced project in the tree.
function unpricedOrg() {
  const index = TREE.indexOf(unpricedEntry());
  for (let i = index; i >= 0; i -= 1) {
    if (TREE[i].level === 0) return TREE[i].name;
  }
  throw new Error("the unpriced project has no organization above it");
}

function unpricedValues(subscription) {
  const values = fixtureValues(null, subscription);
  values.apiMissing = `no price for ${UNPRICED_MODEL}`;
  return values;
}

// An organization's money: its tree share of the chosen accounts' used money.
function orgMoney(org, usedTotal) {
  const entry = TREE.find(
    (candidate) => candidate.level === 0 && candidate.name === org
  );
  if (!entry) throw new Error(`unknown organization ${JSON.stringify(org)}`);
  return (entry.share / 100) * usedTotal;
}

export function projects(query) {
  const { indexes, chosen } = chosenAccounts(query);
  const keptFraction = indexes.length / WEEKS.length;
  const usedTotal = sum(chosen.map((account) => accountUsedTotal(account, indexes)));
  const outTotal = sum(chosen.map((account) => sum(indexes.map((index) => accountOut(account, index)))));
  const apiTotal = (outTotal / 1000000) * API_PER_M;
  const anyUnpriced = unpricedIn(query);
  const nodes = TREE.map((entry) => {
    const out = (entry.share / 100) * outTotal;
    const unpriced = anyUnpriced && entry.level === 1 && entry.name === UNPRICED_PROJECT;
    const orgAccounts = (ORG_ACCOUNTS[entry.name] || [])
      .map((abbr) => byAbbr(abbr))
      .filter(Boolean)
      .map((account) => account.id);
    const subscription =
      entry.level === 0
        ? orgMoney(entry.name, usedTotal)
        : (entry.share / 100) * usedTotal;
    // The organization that holds the unpriced project sums a missing part:
    // its API value is partial (the known part, "≥"), and projects.rs gives a
    // partial or unknown value no subscription shares (R3), so neither row
    // carries a split.
    const holdsUnpriced = anyUnpriced && entry.level === 0 && entry.name === unpricedOrg();
    let values;
    if (unpriced) {
      values = unpricedValues(subscription);
    } else if (holdsUnpriced) {
      const known = ((entry.share - unpricedEntry().share) / 100) * apiTotal;
      values = fixtureValues(known, subscription);
      values.apiPartial = true;
      values.apiMissing = `no price for ${UNPRICED_MODEL}`;
    } else {
      values = fixtureValues((entry.share / 100) * apiTotal, subscription);
    }
    return {
      kind: entry.level === 0 ? "org" : entry.level === 1 ? "project" : "folder",
      level: entry.level,
      name: entry.name,
      key: keyOf(entry),
      orgAccounts,
      tokens: tokenCounts(out),
      share: anyUnpriced ? null : entry.share,
      values,
      bySubscription: unpriced || holdsUnpriced ? [] : subscriptionParts(entry),
      sessions: Math.round(entry.sessions * keptFraction),
    };
  });
  const orgs = TREE.filter((entry) => entry.level === 0).map((entry) => entry.name);
  const subscriptions = matrixSubscriptions();
  const cells = orgs.map((org) => matrixCells(org, subscriptions));
  const rowTotals = cells.map((row) => {
    const { total, partial } = knownPart(row);
    return subscriptionValues(total, partial ? MATRIX_PARTIAL : null);
  });
  const columnTotals = subscriptions.map((_, column) => {
    const { total, partial } = knownPart(cells.map((row) => row[column]));
    return subscriptionValues(total, partial ? MATRIX_PARTIAL : null);
  });
  const grand = knownPart(cells.flat());
  return {
    nodes,
    matrix: {
      orgs,
      subscriptions,
      cells,
      rowTotals,
      columnTotals,
      total: subscriptionValues(grand.total, grand.partial ? MATRIX_PARTIAL : null),
      sentences: matrixSentences(orgs, subscriptions, cells),
    },
    tokensPerPercent: chosen.flatMap((account) => {
      const percent = usedPercentTotal(account, indexes);
      if (percent <= 0) return [];
      const perPercent = TOKENS_PER_PERCENT[account.abbr];
      return perPercent === undefined ? [] : [[account.id, perPercent]];
    }),
    unknownAccountTokens: { input: 0, out: 0, cw: 0, cr: 0 },
    withoutPrice: anyUnpriced ? [UNPRICED_MODEL] : [],
    usdRate: USD_RATE,
    notes: [
      "Not in any project: use of these subscriptions without a session file on a host with seatswap (claude.ai in the browser, the phone).",
      "OpenAI and Ollama work belongs to its provider subscription, not to a Claude account.",
      "1.20M tokens: account taken from the usage readings.",
    ],
  };
}

export function projectDetail(query, key) {
  const { indexes, chosen } = chosenAccounts(query);
  const entry = TREE.find((candidate) => sameKey(keyOf(candidate), key));
  if (!entry) throw new Error(`unknown project ${JSON.stringify(key)}`);
  const keptOutTotal = sum(
    chosen.map((account) => sum(indexes.map((index) => accountOut(account, index))))
  );
  const out = (entry.share / 100) * keptOutTotal;
  const outWeights = indexes.map((index) => OUT_M[index]);
  const weightTotal = sum(outWeights);
  const modelShares = [
    ["claude-opus-5-5", 0.64],
    ["claude-sonnet-5-5", 0.24],
    ["claude-fable-5-2", 0.12],
  ];
  const weeklyByModel = indexes.map((index, position) => {
    const weekShare = weightTotal > 0 ? outWeights[position] / weightTotal : 0;
    const weekOut = out * weekShare;
    return [
      weekIso(index),
      modelShares.map(([model, share]) => [model, Math.round(weekOut * share)]),
    ];
  });
  const hosts =
    entry.name === "atlas"
      ? [["atlas", 71], ["vega", 29]]
      : entry.name === "ledger"
        ? [["atlas", 38], ["vega", 62]]
        : [["atlas", 100]];
  const unpriced = unpricedIn(query) && entry.name === UNPRICED_PROJECT;
  const subscription =
    (entry.share / 100) *
    sum(chosen.map((account) => accountUsedTotal(account, indexes)));
  return {
    name: entry.name,
    tokens: tokenCounts(out),
    agentShare: entry.name === "ledger" ? 44 : 58,
    hosts,
    values: unpriced
      ? unpricedValues(subscription)
      : fixtureValues(
          (entry.share / 100) * ((keptOutTotal / 1000000) * API_PER_M),
          subscription
        ),
    bySubscription: unpriced ? [] : subscriptionParts(entry),
    share: unpricedIn(query) ? null : entry.share,
    weeklyByModel,
    sessions: Math.round(entry.sessions * (indexes.length / WEEKS.length)),
  };
}

function accountUsedTotal(account, indexes) {
  return sum(indexes.map((index) => usedMoney(account, index)));
}
