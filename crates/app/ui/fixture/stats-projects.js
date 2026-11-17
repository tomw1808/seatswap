// The statistics Projects page's fixture: `stats_projects` and
// `stats_project_detail`.

import {
  ACCOUNTS,
  API_PER_M,
  ORG_ACC,
  ORG_ACCOUNTS,
  OUT_M,
  TOKENS_PER_PERCENT,
  TREE,
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

// The work of other providers than Claude, in % of a row's API value, by the
// row's level and name: OpenAI sessions in atlas/engine and Ollama work in
// ledger. projects.rs sends them as `byProvider` by company key, beside the
// accounts' split.
const PROVIDERS = {
  "0:sam": [
    ["openai", 6.8],
    ["ollama", 2.6],
  ],
  "1:atlas": [["openai", 11.7]],
  "2:engine": [["openai", 19]],
  "1:ledger": [["ollama", 9]],
};

function providersOf(entry) {
  return PROVIDERS[`${entry.level}:${entry.name}`] || [];
}

// The chosen accounts' split of the part of the row's API value that is
// Claude's: the account chips do not choose another provider's work.
function chosenSplit(entry, chosen) {
  const claude = 100 - sum(providersOf(entry).map(([, percent]) => percent));
  const weights = chosen.map((account) => {
    const index = ACCOUNTS.indexOf(account);
    return { account, weight: entry.split[index] };
  });
  const total = sum(weights.map((row) => row.weight));
  return weights
    .map((row) => [row.account.id, total > 0 ? (row.weight / total) * claude : 0])
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
}

function accountUsedTotal(account, indexes) {
  return sum(indexes.map((index) => usedMoney(account, index)));
}

// A sum of matrix cells: unknown as soon as one cell is, as projects.rs sums
// them, so a missing cell is never counted as a measured zero.
function addCells(cells) {
  let total = 0;
  for (const cell of cells) {
    if (typeof cell !== "number" || !Number.isFinite(cell)) return null;
    total += cell;
  }
  return total;
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

// One project's model has no price: its API value is unknown while its tokens
// and sessions are there, so projects.rs sends `null` for its share.
const UNPRICED_PROJECT = "notes";
const UNPRICED_MODEL = "claude-fable-5-2";

function unpricedValues(subscription) {
  const values = fixtureValues(null, subscription);
  values.apiMissing = `no price for ${UNPRICED_MODEL}`;
  return values;
}

// An organization's money: its tree share of the chosen accounts' used money.
// The matrix cells split this one number by the organization's account weights,
// so an organization's matrix row total is exactly its tree row's value.
function orgMoney(org, usedTotal) {
  const entry = TREE.find(
    (candidate) => candidate.level === 0 && candidate.name === org
  );
  if (!entry) throw new Error(`unknown organization ${JSON.stringify(org)}`);
  return (entry.share / 100) * usedTotal;
}

function matrixCell(org, account, usedTotal) {
  const weights = ORG_ACC[org];
  if (!weights) throw new Error(`unknown organization ${JSON.stringify(org)}`);
  const total = sum(Object.values(weights));
  if (total <= 0) {
    throw new Error(`organization ${JSON.stringify(org)} has no account weights`);
  }
  return orgMoney(org, usedTotal) * ((weights[account.abbr] ?? 0) / total);
}

function matrixSentences(orgs, chosen, cells, usedTotal) {
  const sentences = [];
  for (const [orgIndex, org] of orgs.entries()) {
    const ownAbbrs = ORG_ACCOUNTS[org] || [];
    const own = chosen.filter((account) => ownAbbrs.includes(account.abbr));
    if (own.length === 0) continue;
    const total = sum(cells[orgIndex]);
    const ownSub = sum(own.map((account) => matrixCell(org, account, usedTotal)));
    if (total <= 0 || ownSub <= 0) continue;
    const names = joinNames(own.map((account) => account.abbr));
    const noun = own.length === 1 ? "subscription" : "subscriptions";
    const verb = own.length === 1 ? "is" : "are";
    const share = (ownSub / total) * 100;
    let sentence = `${names} ${verb} ${org}'s ${noun}. It covered ${fixtureWholePercent(
      share
    )} of the work for ${org}.`;
    if (share < 100) {
      const others = chosen.filter(
        (account) =>
          !own.includes(account) && matrixCell(org, account, usedTotal) > 0
      );
      const amount = sum(
        others.map((account) => matrixCell(org, account, usedTotal))
      );
      if (others.length > 0 && amount > 0) {
        sentence += ` The rest ran on ${joinNames(
          others.map((account) => account.abbr)
        )}: ${fixtureMoney(amount, "EUR")} of their value.`;
      }
    }
    sentences.push(sentence);

    let carried = 0;
    const otherOrgs = [];
    for (const [otherIndex, other] of orgs.entries()) {
      if (other === org) continue;
      const value = sum(
        own.map((account) => matrixCell(other, account, usedTotal))
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

export function projects(query) {
  const { indexes, chosen } = chosenAccounts(query);
  const keptFraction = indexes.length / WEEKS.length;
  const usedTotal = sum(chosen.map((account) => accountUsedTotal(account, indexes)));
  const outTotal = sum(chosen.map((account) => sum(indexes.map((index) => accountOut(account, index)))));
  const apiTotal = (outTotal / 1000000) * API_PER_M;
  const nodes = TREE.map((entry) => {
    const out = (entry.share / 100) * outTotal;
    const unpriced = entry.name === UNPRICED_PROJECT;
    const orgAccounts = (ORG_ACCOUNTS[entry.name] || [])
      .map((abbr) => byAbbr(abbr))
      .filter(Boolean)
      .map((account) => account.id);
    const subscription =
      entry.level === 0
        ? orgMoney(entry.name, usedTotal)
        : (entry.share / 100) * usedTotal;
    return {
      kind: entry.level === 0 ? "org" : entry.level === 1 ? "project" : "folder",
      level: entry.level,
      name: entry.name,
      key: keyOf(entry),
      orgAccounts,
      tokens: tokenCounts(out),
      share: unpriced ? null : entry.share,
      values: unpriced
        ? unpricedValues(subscription)
        : fixtureValues((entry.share / 100) * apiTotal, subscription),
      byAccount: chosenSplit(entry, chosen),
      byProvider: providersOf(entry),
      sessions: Math.round(entry.sessions * keptFraction),
    };
  });
  const orgs = TREE.filter((entry) => entry.level === 0).map((entry) => entry.name);
  const cells = orgs.map((org) =>
    chosen.map((account) => matrixCell(org, account, usedTotal))
  );
  return {
    nodes,
    matrix: {
      orgs,
      accounts: chosen.map((account) => account.id),
      cells,
      rowTotals: cells.map(addCells),
      columnTotals: chosen.map((account, column) =>
        addCells(cells.map((row) => row[column]))
      ),
      total: addCells(cells.flat()),
      sentences: matrixSentences(orgs, chosen, cells, usedTotal),
    },
    tokensPerPercent: chosen.flatMap((account) => {
      const percent = usedPercentTotal(account, indexes);
      if (percent <= 0) return [];
      const perPercent = TOKENS_PER_PERCENT[account.abbr];
      return perPercent === undefined ? [] : [[account.id, perPercent]];
    }),
    unknownAccountTokens: { input: 0, out: 0, cw: 0, cr: 0 },
    withoutPrice: [],
    usdRate: USD_RATE,
    notes: [
      "Not in any project: use of these accounts without a session file on a host with seatswap (claude.ai in the browser, the phone).",
      "OpenAI and Ollama work belongs to its provider subscription, not to a Claude account.",
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
  const unpriced = entry.name === UNPRICED_PROJECT;
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
    share: unpriced ? null : entry.share,
    weeklyByModel,
    sessions: Math.round(entry.sessions * (indexes.length / WEEKS.length)),
  };
}
