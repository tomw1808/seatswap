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
  fixtureValues,
  sum,
  tokenCounts,
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

function chosenSplit(entry, chosen) {
  const weights = chosen.map((account) => {
    const index = ACCOUNTS.indexOf(account);
    return { account, weight: entry.split[index] };
  });
  const total = sum(weights.map((row) => row.weight));
  return weights
    .map((row) => [row.account.id, total > 0 ? (row.weight / total) * 100 : 0])
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
}

function accountUsedTotal(account, indexes) {
  return sum(indexes.map((index) => usedMoney(account, index)));
}

function accountOrgTotal(abbr) {
  return sum(Object.values(ORG_ACC).map((org) => org[abbr] ?? 0));
}

function matrixCell(org, account, indexes) {
  const values = ORG_ACC[org];
  if (!values) return 0;
  const total = accountOrgTotal(account.abbr);
  if (total <= 0) return 0;
  return accountUsedTotal(account, indexes) * ((values[account.abbr] ?? 0) / total);
}

function matrixSentences(orgs, chosen, cells, indexes) {
  const sentences = [];
  for (const [orgIndex, org] of orgs.entries()) {
    const ownAbbrs = ORG_ACCOUNTS[org] || [];
    const own = chosen.filter((account) => ownAbbrs.includes(account.abbr));
    if (own.length === 0) continue;
    const total = sum(cells[orgIndex]);
    const ownSub = sum(own.map((account) => matrixCell(org, account, indexes)));
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
        (account) => !own.includes(account) && matrixCell(org, account, indexes) > 0
      );
      const amount = sum(others.map((account) => matrixCell(org, account, indexes)));
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
      const value = sum(own.map((account) => matrixCell(other, account, indexes)));
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
    const orgAccounts = (ORG_ACCOUNTS[entry.name] || [])
      .map((abbr) => byAbbr(abbr))
      .filter(Boolean)
      .map((account) => account.id);
    return {
      kind: entry.level === 0 ? "org" : entry.level === 1 ? "project" : "folder",
      level: entry.level,
      name: entry.name,
      key: keyOf(entry),
      orgAccounts,
      tokens: tokenCounts(out),
      share: entry.share,
      values: fixtureValues((entry.share / 100) * apiTotal, (entry.share / 100) * usedTotal),
      byAccount: chosenSplit(entry, chosen),
      sessions: Math.round(entry.sessions * keptFraction),
    };
  });
  const orgs = TREE.filter((entry) => entry.level === 0).map((entry) => entry.name);
  const cells = orgs.map((org) => chosen.map((account) => matrixCell(org, account, indexes)));
  return {
    nodes,
    matrix: {
      orgs,
      accounts: chosen.map((account) => account.id),
      cells,
      sentences: matrixSentences(orgs, chosen, cells, indexes),
    },
    tokensPerPercent: chosen.flatMap((account) => {
      const percent = usedPercentTotal(account, indexes);
      if (percent <= 0) return [];
      const perPercent = TOKENS_PER_PERCENT[account.abbr];
      return perPercent === undefined ? [] : [[account.id, perPercent]];
    }),
    unknownAccountTokens: { input: 0, out: 0, cw: 0, cr: 0 },
    withoutPrice: [],
    notes: [
      "Not in any project: use of these accounts without a session file on a host with seatswap (claude.ai in the browser, the phone).",
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
  return {
    name: entry.name,
    tokens: tokenCounts(out),
    agentShare: entry.name === "ledger" ? 44 : 58,
    hosts,
    values: fixtureValues(
      (entry.share / 100) * ((keptOutTotal / 1000000) * API_PER_M),
      (entry.share / 100) * sum(chosen.map((account) => accountUsedTotal(account, indexes)))
    ),
    weeklyByModel,
    sessions: Math.round(entry.sessions * (indexes.length / WEEKS.length)),
  };
}
