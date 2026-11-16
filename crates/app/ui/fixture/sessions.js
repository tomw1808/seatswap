// The Sessions area's fixture: `sessions_view` and the `?fixture=agents400`
// session the bench opens.

import { isObject } from "../common.js";
import { isoIn } from "../fixture.js";

// ---------------------------------------------------------------------------
// the Sessions area's made-up view (spec §3.8), shaped by gui/window.rs
// ---------------------------------------------------------------------------

const SESSION_SUBV = 1.4696153846153845; // EUR per million output tokens
const SESSION_APIV = 20.357142857142858; // EUR at API prices
const SESSION_PROJECTS = {
  atlas: "github.com/sam/atlas",
  "acme-kb": "github.com/acme/acme-kb",
  ledger: "github.com/sam/ledger",
  notes: "github.com/sam/notes",
};

function fixtureTokenCounts(million) {
  return { input: 0, out: Math.round(million * 1000000), cw: 0, cr: 0 };
}

function fixtureSessionValues(million, currency = "EUR") {
  if (!(million > 0)) {
    return {
      api: null,
      apiCurrency: null,
      apiMissing: "no tokens yet",
      subscription: null,
      currency,
      subscriptionMissing: "no tokens yet",
    };
  }
  return {
    api: Number((million * SESSION_APIV).toFixed(2)),
    apiCurrency: currency,
    apiMissing: null,
    subscription: Number((million * SESSION_SUBV).toFixed(2)),
    currency,
    subscriptionMissing: null,
  };
}

function fixtureTodayAt(hours, minutes) {
  const date = new Date();
  date.setHours(hours, minutes, 0, 0);
  if (date.getTime() > Date.now()) date.setTime(Date.now() - (hours * 60 + minutes) * 60000);
  return date.toISOString();
}

function fixtureDaysAgoAt(days, hours, minutes) {
  const date = new Date();
  date.setDate(date.getDate() - days);
  date.setHours(hours, minutes, 0, 0);
  return date.toISOString();
}

function fixtureAgent(session, key, parent, kind, task, model, running, first, last, million) {
  return {
    agent: `${session}-${key}`,
    parent: parent ? `${session}-${parent}` : null,
    kind,
    task,
    model,
    running,
    first,
    last,
    tokens: fixtureTokenCounts(million),
    values: fixtureSessionValues(million),
  };
}

function fixtureFinishedAgents(session, host, count, tokensFor) {
  const agents = [];
  for (let index = 0; index < count; index += 1) {
    const last = new Date(Date.now() - (index + 1) * 7 * 60000).toISOString();
    agents.push(
      fixtureAgent(
        session,
        `done-${index}`,
        null,
        index % 3 === 0 ? "general-purpose" : "Explore",
        host === "atlas" ? `finished task ${index + 1}` : null,
        index % 2 === 0 ? "claude-sonnet-5-5" : "claude-opus-5-5",
        false,
        new Date(Date.parse(last) - 5 * 60000).toISOString(),
        last,
        tokensFor(index)
      )
    );
  }
  return agents;
}

// `?fixture=agents400` in the address lists one very large session: 400 agents,
// 380 completed and 20 running, five of the running ones with agents of their
// own. It exercises the tree's depth and its many-agent case.
function fixtureWantsAgents400() {
  if (typeof location === "undefined" || typeof URLSearchParams !== "function") return false;
  try {
    return new URLSearchParams(location.search).get("fixture") === "agents400";
  } catch {
    return false;
  }
}

function fixtureAgents400(session, host) {
  const agents = [];
  const task = (index) =>
    host === "atlas" ? `many-agent task ${index + 1}` : null;
  for (let index = 0; index < 10; index += 1) {
    agents.push(
      fixtureAgent(
        session,
        `run-${index}`,
        null,
        index % 3 === 0 ? "Plan" : index % 2 === 0 ? "Explore" : "general-purpose",
        task(index),
        index % 2 === 0 ? "claude-sonnet-5-5" : "claude-opus-5-5",
        true,
        isoIn(-(index + 1) * 60000),
        null,
        0.05
      )
    );
    if (index < 5) {
      for (let child = 0; child < 2; child += 1) {
        agents.push(
          fixtureAgent(
            session,
            `run-${index}-child-${child}`,
            `run-${index}`,
            "Explore",
            task(10 + index * 2 + child),
            "claude-sonnet-5-5",
            true,
            isoIn(-(index + 2) * 60000),
            null,
            0.05
          )
        );
      }
    }
  }
  agents.push(...fixtureFinishedAgents(session, host, 380, () => 0.01));
  return agents;
}

function fixtureSessionRows() {
  const atlasDone = fixtureFinishedAgents("atlas-a6", "atlas", 38, (index) => {
    if (index === 0) return 0.3;
    if (index === 1) return 0.9;
    return Number(((9.6 - 1.2) / 36).toFixed(4));
  });
  atlasDone[0].task = "find the config loader";
  atlasDone[0].last = fixtureTodayAt(9, 2);
  atlasDone[0].first = new Date(Date.parse(atlasDone[0].last) - 5 * 60000).toISOString();
  atlasDone[1].task = "write the migration";
  atlasDone[1].last = fixtureTodayAt(8, 41);
  atlasDone[1].first = new Date(Date.parse(atlasDone[1].last) - 5 * 60000).toISOString();
  for (const index of [36, 37]) {
    atlasDone[index].last = isoIn(-26 * 3600000);
    atlasDone[index].first = isoIn(-27 * 3600000);
  }

  const a6Children = [
    fixtureAgent(
      "atlas-a6",
      "gp-explore",
      "gp",
      "Explore",
      "find the callers of retry()",
      "claude-sonnet-5-5",
      true,
      isoIn(-2 * 60000),
      null,
      0.1
    ),
  ];
  const a6Agents = [
    fixtureAgent(
      "atlas-a6",
      "gp",
      null,
      "general-purpose",
      "fix the failing tests",
      "claude-opus-5-5",
      true,
      isoIn(-12 * 60000),
      null,
      1.1
    ),
    fixtureAgent(
      "atlas-a6",
      "plan",
      null,
      "Plan",
      "design the retry logic",
      "claude-opus-5-5",
      true,
      isoIn(-60000),
      null,
      0.2
    ),
    fixtureAgent(
      "atlas-a6",
      "explore",
      null,
      "Explore",
      "map the order engine",
      "claude-sonnet-5-5",
      true,
      isoIn(-3 * 60000),
      null,
      0.4
    ),
    ...a6Children,
    ...atlasDone,
  ];

  const s56Agents = fixtureFinishedAgents("atlas-56", "atlas", 12, () => 0.1);
  s56Agents[0].task = "read the pricing module";

  const ddAgents = [
    fixtureAgent(
      "acme-kb-dd",
      "explore",
      null,
      "Explore",
      "list the ingest sources",
      "claude-sonnet-5-5",
      true,
      isoIn(-2 * 60000),
      null,
      0.1
    ),
    ...fixtureFinishedAgents("acme-kb-dd", "atlas", 6, () => 0.2166667),
  ];

  const b3Agents = fixtureFinishedAgents("atlas-b3", "vega", 4, () => 0.1);
  b3Agents[0].task = null;
  const staleFableAgents = fixtureFinishedAgents("acme-kb-91", "ledger", 5, () => 0.1);
  for (const agent of staleFableAgents.slice(0, 2)) {
    agent.running = true;
    agent.last = null;
  }
  const staleSonnetAgents = fixtureFinishedAgents("acme-kb-a2", "ledger", 3, () => 0.08);
  staleSonnetAgents[0].running = true;
  staleSonnetAgents[0].last = null;
  const ledgerAgents = fixtureFinishedAgents("ledger-02", "vega", 1, () => 0.05);
  const s47Agents = fixtureFinishedAgents("notes-47", "atlas", 2, (index) =>
    index === 0 ? 0.6 : 0.5
  );
  s47Agents[0].task = "research a usage endpoint";
  s47Agents[1].task = "research a second provider";

  const make = (name, project, host, status, model, since, million, lastHour, running, done, agents, options = {}) => ({
    session: name,
    host,
    label: name,
    projectId: { kind: "repo", id: SESSION_PROJECTS[project] },
    project,
    org: null,
    cwd: `/home/sam/projects/${project}`,
    provider: options.provider || "Claude Code",
    model,
    tied: options.tied === true,
    status,
    since,
    started: since,
    agentsRunning: running,
    agentsDone: done,
    agents,
    tokens: fixtureTokenCounts(million),
    lastHour: fixtureTokenCounts(lastHour),
    values: fixtureSessionValues(million, options.currency || "EUR"),
  });

  const rows = [
    make("atlas-a6", "atlas", "atlas", "busy", "claude-opus-5-5", isoIn(-(2 * 60 + 31) * 60000), 12.4, 1.9, 3, 38, a6Agents),
    make("atlas-56", "atlas", "atlas", "idle", "claude-opus-5-5", isoIn(-6 * 3600000), 8.1, 0, 0, 12, s56Agents),
    make("atlas-11", "atlas", "atlas", "closed", "claude-opus-5-5", fixtureTodayAt(9, 12), 7.9, 0, 0, 17, []),
    make("atlas-b3", "atlas", "vega", "idle", "claude-fable-5-2", isoIn(-100 * 60000), 3.9, 0.5, 0, 4, b3Agents, { tied: true, currency: "USD" }),
    make("acme-kb-dd", "acme-kb", "atlas", "waiting", "claude-opus-5-5", isoIn(-4 * 60000), 2.2, 0.8, 1, 6, ddAgents),
    make("acme-kb-3c", "acme-kb", "atlas", "closed", "claude-opus-5-5", fixtureTodayAt(8, 40), 4.4, 0, 0, 9, []),
    make("atlas-c4", "atlas", "atlas", "closed", "claude-mythos-5-2", fixtureTodayAt(7, 30), 1.1, 0, 0, 5, [], { tied: true }),
    make("acme-kb-74", "acme-kb", "atlas", "idle", "claude-sonnet-5-5", isoIn(-10 * 3600000), 0.3, 0, 0, 0, [], { tied: true }),
    make("acme-kb-91", "acme-kb", "ledger", { other: "no report" }, "claude-fable-5-2", isoIn(-3 * 3600000), 1.2, 0, 2, 3, staleFableAgents, { tied: true }),
    make("acme-kb-a2", "acme-kb", "ledger", { other: "no report" }, "claude-sonnet-5-5", isoIn(-3 * 3600000), 0.9, 0, 1, 2, staleSonnetAgents),
    make("ledger-02", "ledger", "vega", "idle", "claude-opus-5-5", isoIn(-17 * 3600000), 0.1, 0, 0, 1, ledgerAgents),
    make("ledger-9e", "ledger", "vega", "closed", "claude-opus-5-5", fixtureDaysAgoAt(3, 22, 5), 1.6, 0, 0, 3, []),
    make("ledger-c1", "ledger", "atlas", "idle", "gpt-6-astra", isoIn(-35 * 60000), 0.4, 0, 0, 0, [], {
      provider: "Codex",
    }),
    make("notes-47", "notes", "atlas", "busy", "claude-fable-5-2", isoIn(-6 * 60000), 5.3, 1.2, 0, 2, s47Agents, { tied: true }),
    make("notes-b9", "notes", "atlas", "idle", null, isoIn(-19 * 3600000), 0, 0, 0, 0, []),
  ];
  if (fixtureWantsAgents400()) {
    rows.push(
      make(
        "atlas-7c",
        "atlas",
        "atlas",
        "busy",
        "claude-opus-5-5",
        isoIn(-60 * 60000),
        4.8,
        0.8,
        20,
        380,
        fixtureAgents400("atlas-7c", "atlas")
      )
    );
  }
  return rows;
}

function fixtureAddTokens(one, other) {
  return {
    input: one.input + other.input,
    out: one.out + other.out,
    cw: one.cw + other.cw,
    cr: one.cr + other.cr,
  };
}

function fixtureSums(rows) {
  const groups = new Map();
  const group = (of) => {
    const key = JSON.stringify(of);
    if (!groups.has(key)) {
      groups.set(key, {
        of,
        sessions: 0,
        tokens: fixtureTokenCounts(0),
        lastHour: fixtureTokenCounts(0),
      });
    }
    return groups.get(key);
  };
  group({ kind: "all" });
  for (const row of rows) {
    const keys = [
      { kind: "all" },
      { kind: "provider", provider: row.provider },
      { kind: "project", project: row.projectId },
      { kind: "projectModel", project: row.projectId, model: row.model },
      { kind: "model", model: row.model },
    ];
    for (const key of keys) {
      const found = group(key);
      found.sessions += 1;
      found.tokens = fixtureAddTokens(found.tokens, row.tokens);
      found.lastHour = fixtureAddTokens(found.lastHour, row.lastHour);
    }
  }
  return Array.from(groups.values()).map((found) => ({
    ...found,
    values: fixtureSessionValues(found.tokens.out / 1000000),
  }));
}

export function fixtureSessionsView(args) {
  const options = isObject(args) && isObject(args.options) ? args.options : {};
  const closed = typeof options.closed === "string" ? options.closed : "none";
  if (!["none", "day", "week", "month"].includes(closed)) {
    throw new Error(`unknown closed filter ${JSON.stringify(options.closed)}`);
  }
  const host = typeof options.host === "string" && options.host !== "" ? options.host : null;
  const now = Date.now();
  const days = closed === "day" ? 1 : closed === "week" ? 7 : closed === "month" ? 30 : 0;
  let rows = fixtureSessionRows();
  if (closed === "none") {
    rows = rows.filter((row) => row.status !== "closed");
  } else {
    rows = rows.filter(
      (row) => row.status !== "closed" || Date.parse(row.since) >= now - days * 86400000
    );
  }
  if (host !== null) rows = rows.filter((row) => row.host === host);
  return {
    at: new Date(now).toISOString(),
    thisHost: "atlas",
    hosts: [
      { host: "atlas", at: new Date(now).toISOString(), stale: false, this: true },
      { host: "vega", at: new Date(now - 2 * 60000).toISOString(), stale: false, this: false },
      {
        host: "ledger",
        at: new Date(now - 3 * 3600000).toISOString(),
        stale: true,
        this: false,
      },
    ],
    sessions: rows,
    sums: fixtureSums(rows),
    switchEffect: {
      losesThinking: 3,
      losesThinkingBusy: 1,
      losesThinkingModels: ["claude-fable-5-2", "claude-sonnet-5-5"],
      resend: 2,
      free: 4,
    },
    currency: "EUR",
    unavailable: null,
  };
}
