// The Settings area's fixture answers: projects and organizations
// (`projects_view`, `projects_edit`) and the Settings-only answers
// (`history_view`, `history_import`, `history_export`, `money_view`,
// `lookup_now`, `save_setting` and the status line's saved settings).

import { clone, isObject } from "../common.js";
import { DEFAULT_SETTINGS, fixtureOpenProviders } from "../fixture.js";

// The history view as the app opened it, plus the saved settings' values when
// they differ: a restart would use those, as the real command reports.
export function fixtureHistoryView(fixture) {
  const view = clone(fixture.history);
  const settings = fixture.settings || {};
  const host =
    typeof settings.hostName === "string" ? settings.hostName.trim() : "";
  view.savedHost = host !== "" && host !== view.host ? host : null;
  const dir =
    typeof settings.historyDir === "string" ? settings.historyDir.trim() : "";
  view.savedDir = dir !== "" && dir !== view.dir ? dir : null;
  view.bytes = view.hosts.reduce((total, host) => total + (Number(host.bytes) || 0), 0);
  // Decision 3: a machine with Dropbox and no history folder set gets
  // `<Dropbox>/seatswap` as a suggestion. Only the `?fixture=dropbox` variant
  // pretends such a machine; every other fixture has no Dropbox.
  view.suggestedFolder = fixtureDropboxSuggestion(dir);
  view.suggestedError = fixtureDropboxError();
  return view;
}

// The suggestion path Dropbox's info.json would give, as the view shows it with
// `~` for the home folder. No file is read or written for it.
function fixtureDropboxSuggestion(dir) {
  if (typeof location === "undefined" || typeof URLSearchParams !== "function") return null;
  if (new URLSearchParams(location.search).get("fixture") !== "dropbox") return null;
  if (dir !== "") return null;
  return "~/Dropbox/seatswap";
}

// The proof of the error line: `?fixture=dropbox-error` is a machine whose
// Dropbox file cannot be read, so the view carries the reason and no
// suggestion.
function fixtureDropboxError() {
  if (typeof location === "undefined" || typeof URLSearchParams !== "function") return null;
  if (new URLSearchParams(location.search).get("fixture") !== "dropbox-error") return null;
  return "Dropbox's info file /home/sam/.dropbox/info.json: not JSON: expected value at line 1 column 12";
}

// The command's `expand_tilde`: `~/…` is the fixture's home folder.
export function fixtureExpand(path) {
  if (path === "~") return "/home/sam";
  if (path.startsWith("~/")) return `/home/sam/${path.slice(2)}`;
  return path;
}

// The money view with the fixture's current settings, as the Rust thread would
// compute it from the stored files. `rateFetchedAt` stays on the same minute
// for the whole day, so a snapshot tick never changes the view's JSON (the
// page compares that JSON to keep the money rows still, decision 25).
export function fixtureMoneyView(fixture) {
  const view = Object.assign({}, clone(fixture.money));
  const currency =
    typeof fixture.settings.currency === "string" ? fixture.settings.currency : null;
  view.currency = currency;
  view.lookUp = Boolean(fixture.settings.lookUpPrices);
  view.modelsWithPrice = Array.isArray(fixture.priceList) ? fixture.priceList.length : 0;
  view.rateSource = "European Central Bank via Frankfurter";
  view.pricesSource = "models.dev";
  view.rateFetchedAt = null;
  if (!currency) {
    view.rate = null;
  } else if (currency === "USD") {
    view.rate = { value: 1, date: null };
  } else if (!view.lookUp || currency !== "EUR") {
    // The fixture's stored looked-up point is in EUR; with the look-ups off no
    // rate is applied and the API value is in US dollars (decision 3).
    view.rate = null;
  } else {
    view.rate = { value: 0.9048, date: "2026-11-13" };
    view.rateFetchedAt = fixtureRateFetchedAt();
  }
  return view;
}

// Today at 10:10 in this computer's local time: the fixture's rate was fetched
// that morning, as the page's line reads ("fetched today 10:10").
function fixtureRateFetchedAt() {
  const at = new Date();
  at.setHours(10, 10, 0, 0);
  return at.toISOString();
}

// ---------------------------------------------------------------------------
// Projects and organizations (spec §3.6) in the fixture: a fixed view that
// `projects_view` returns and `projects_edit` changes in place. The real app
// resolves the shared log in core::projects; this is only for checking the
// page in a browser.
// ---------------------------------------------------------------------------

function fixtureAccountRef(account) {
  return { acc: account.id, org: `org:${account.orgName || account.id}` };
}

function fixtureIdText(id) {
  if (!isObject(id) || typeof id.id !== "string") return "";
  return typeof id.host === "string" && id.host !== ""
    ? `${id.kind}:${id.id}\u0000${id.host}`
    : `${id.kind}:${id.id}`;
}

function fixtureSameRef(first, second) {
  return Boolean(first && second && first.acc === second.acc && first.org === second.org);
}

export function fixtureProjectView(fixture) {
  return {
    projects: clone(fixture.projectView.projects),
    orgs: fixture.projectView.orgs.map((org) =>
      Object.assign(clone(org), {
        ownerRepositories: fixtureOwnerRepositories(fixture.projectView.projects, org.id),
      })
    ),
    accountRefs: fixture.accounts.map((account) => ({
      id: account.id,
      account: fixtureAccountRef(account),
    })),
  };
}

// The repositories whose owner is this organization, as core::projects finds
// them: the part before the repository's name in `github.com/<owner>/<name>`.
// The owner is compared with the same name rule as core (case-insensitive).
function fixtureOwnerRepositories(projects, orgId) {
  const found = new Set();
  for (const project of projects) {
    if (typeof project.repo !== "string" || project.repo === "") continue;
    const parts = project.repo.split("/").filter((part) => part !== "");
    if (parts.length < 2) continue;
    if (fixtureOrganizationNamesMatch(parts[parts.length - 2], orgId)) found.add(project.repo);
  }
  return [...found].sort();
}

function fixtureFindProject(projects, id) {
  return projects.find((project) => fixtureIdText(project.key) === fixtureIdText(id)) || null;
}

function fixtureFindOrg(orgs, id) {
  return orgs.find((org) => org.id === id) || null;
}

function fixtureCheckName(what, value) {
  const text = String(value === undefined || value === null ? "" : value);
  if (text.trim() === "") throw new Error(`a ${what} name must not be only spaces`);
  const length = [...text].length;
  if (length > 40) {
    throw new Error(`a ${what} name must be 1 to 40 characters, not ${length}`);
  }
  return text;
}

function fixtureCheckIdentity(value) {
  const text = String(value === undefined || value === null ? "" : value);
  if (text.trim() === "") throw new Error("an organization name must not be empty");
  return text;
}

// The same rule as core::projects::same_organization_name: compare the names
// after `toLowerCase()`, Unicode case included.
function fixtureOrganizationNamesMatch(first, second) {
  return first.toLowerCase() === second.toLowerCase();
}

// The same refusals as `ProjectEdit::validate`, then the fixed view is changed
// directly; nothing is appended when a check throws.
export function fixtureEditProject(fixture, edit) {
  if (!isObject(edit)) throw new Error("projects_edit needs an edit");
  const view = fixture.projectView;
  switch (String(edit.edit || "")) {
    case "rename": {
      const project = fixtureFindProject(view.projects, edit.key);
      if (!project) throw new Error("unknown project");
      project.name = fixtureCheckName("project", edit.name);
      break;
    }
    case "orgRename": {
      const org = fixtureFindOrg(view.orgs, String(edit.org));
      if (!org) throw new Error(`unknown organization ${String(edit.org)}`);
      org.name = fixtureCheckName("organization", edit.name);
      break;
    }
    case "join": {
      if (fixtureIdText(edit.key) === fixtureIdText(edit.into)) {
        throw new Error("a project cannot be joined into itself");
      }
      const from = fixtureFindProject(view.projects, edit.key);
      const into = fixtureFindProject(view.projects, edit.into);
      if (!from || !into) throw new Error("unknown project");
      for (const [host, list] of Object.entries(from.folders || {})) {
        const folders = into.folders[host] || (into.folders[host] = []);
        for (const cwd of list) if (!folders.includes(cwd)) folders.push(cwd);
      }
      into.sessions = (Number(into.sessions) || 0) + (Number(from.sessions) || 0);
      into.joined = [...(into.joined || []), { key: clone(from.key), name: from.name }];
      view.projects = view.projects.filter((project) => project !== from);
      break;
    }
    case "unjoin": {
      for (const project of view.projects) {
        const joined = Array.isArray(project.joined) ? project.joined : [];
        const found = joined.find(
          (entry) => fixtureIdText(entry.key) === fixtureIdText(edit.key)
        );
        if (!found) continue;
        project.joined = joined.filter((entry) => entry !== found);
        view.projects.push({
          key: clone(found.key),
          name: found.name,
          repo: found.key.kind === "repo" ? found.key.id : null,
          org: null,
          joined: [],
          folders: {},
          sessions: 0,
        });
        break;
      }
      break;
    }
    case "assign": {
      const project = fixtureFindProject(view.projects, edit.key);
      if (!project) throw new Error("unknown project");
      const org =
        edit.org === null || edit.org === undefined ? null : fixtureCheckIdentity(edit.org);
      if (org !== null && !fixtureFindOrg(view.orgs, org)) {
        view.orgs.push({ id: org, name: org, accounts: [], projects: 0 });
      }
      project.org = org;
      break;
    }
    case "orgJoin": {
      const org = fixtureCheckIdentity(edit.org);
      const into = fixtureCheckIdentity(edit.into);
      if (org === into) throw new Error("an organization cannot be joined into itself");
      const source = fixtureFindOrg(view.orgs, org);
      const target = fixtureFindOrg(view.orgs, into);
      if (!source || !target) throw new Error("unknown organization");
      for (const account of source.accounts || []) {
        if (!target.accounts.some((entry) => fixtureSameRef(entry, account))) {
          target.accounts.push(account);
        }
      }
      for (const project of view.projects) if (project.org === org) project.org = into;
      view.orgs = view.orgs.filter((entry) => entry !== source);
      break;
    }
    case "accountOrg": {
      const ref =
        fixture.accounts
          .map((account) => fixtureAccountRef(account))
          .find((entry) => fixtureSameRef(entry, edit.account)) || null;
      if (!ref) throw new Error("unknown account");
      for (const org of view.orgs) {
        org.accounts = (org.accounts || []).filter((entry) => !fixtureSameRef(entry, ref));
      }
      if (edit.org !== null && edit.org !== undefined) {
        const org = fixtureCheckIdentity(edit.org);
        const target = fixtureFindOrg(view.orgs, org);
        if (!target) throw new Error(`unknown organization ${org}`);
        target.accounts.push(ref);
      }
      break;
    }
    case "orgAdd": {
      const name = fixtureCheckName("organization", edit.name);
      // No silent trimming: the real core refuses outer white space, and the
      // page trims what was typed before it sends.
      if (name.trim() !== name) {
        throw new Error("an organization name must not start or end with a space");
      }
      const exists = view.orgs.some(
        (org) =>
          fixtureOrganizationNamesMatch(org.id, name) ||
          fixtureOrganizationNamesMatch(org.name, name)
      );
      if (exists) throw new Error(`an organization named ${name} already exists`);
      view.orgs.push({ id: name, name, accounts: [], projects: 0 });
      view.orgs.sort((first, second) => first.name.localeCompare(second.name));
      break;
    }
    case "orgRemove": {
      const org = fixtureFindOrg(view.orgs, String(edit.org));
      if (!org) throw new Error(`the organization ${String(edit.org)} does not exist`);
      const owners = fixtureOwnerRepositories(view.projects, org.id);
      if (owners.length > 0) {
        throw new Error(
          `the organization ${org.name} comes from the owner of ${owners[0]}; it cannot be removed`
        );
      }
      for (const project of view.projects) if (project.org === org.id) project.org = null;
      view.orgs = view.orgs.filter((entry) => entry !== org);
      break;
    }
    default:
      throw new Error(`unknown project edit ${String(edit.edit)}`);
  }
  for (const org of view.orgs) {
    org.projects = view.projects.filter((project) => project.org === org.id).length;
  }
}

export function validateFixtureSettings(settings) {
  const merged = Object.assign({}, DEFAULT_SETTINGS, settings);
  if (!["percent", "percentReset", "off"].includes(merged.trayText)) {
    throw new Error(`invalid trayText ${JSON.stringify(merged.trayText)}`);
  }
  if (!["binding", "session", "week"].includes(merged.iconLimit)) {
    throw new Error(`invalid iconLimit ${JSON.stringify(merged.iconLimit)}`);
  }
  if (!["letters", "percent"].includes(merged.iconText)) {
    throw new Error(`invalid iconText ${JSON.stringify(merged.iconText)}`);
  }
  if (!["system", "light", "dark"].includes(merged.theme)) {
    throw new Error(`invalid theme ${JSON.stringify(merged.theme)}`);
  }
  if (!["thisHost", "allHosts"].includes(merged.notifyWaitingHosts)) {
    throw new Error(
      `invalid notifyWaitingHosts ${JSON.stringify(merged.notifyWaitingHosts)}`
    );
  }
  if (!Number.isInteger(merged.pollActiveSecs) || merged.pollActiveSecs < 30) {
    throw new Error(`pollActiveSecs must be at least 30s, got ${merged.pollActiveSecs}`);
  }
  if (!Number.isInteger(merged.pollInactiveSecs) || merged.pollInactiveSecs < 30) {
    throw new Error(`pollInactiveSecs must be at least 30s, got ${merged.pollInactiveSecs}`);
  }
  if (
    merged.claudeConfigDir !== null &&
    merged.claudeConfigDir !== undefined &&
    !String(merged.claudeConfigDir).startsWith("/")
  ) {
    throw new Error(
      `claudeConfigDir must be an absolute path, got ${merged.claudeConfigDir}`
    );
  }
  if (
    merged.historyDir !== null &&
    merged.historyDir !== undefined &&
    !String(merged.historyDir).startsWith("/")
  ) {
    throw new Error(`historyDir must be an absolute path, got ${merged.historyDir}`);
  }
  if (
    merged.codexHome !== null &&
    merged.codexHome !== undefined &&
    !String(merged.codexHome).startsWith("/")
  ) {
    throw new Error(`codexHome must be an absolute path, got ${merged.codexHome}`);
  }
  if (
    typeof merged.ollamaUrl !== "string" ||
    !/^https?:\/\/[^/\s]+/.test(merged.ollamaUrl)
  ) {
    throw new Error(
      `ollamaUrl must start with http:// or https:// and name a host, got ${JSON.stringify(merged.ollamaUrl)}`
    );
  }
  if (merged.hostName !== null && merged.hostName !== undefined) {
    const name = String(merged.hostName);
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$/.test(name)) {
      throw new Error(
        `host name ${JSON.stringify(name)} must be 1 to 63 characters of A-Z, a-z, 0-9, '.', '_' or '-' and must start with a letter or a digit; set hostName in the settings`
      );
    }
  }
  if (
    merged.currency !== null &&
    merged.currency !== undefined &&
    !/^[A-Z]{3}$/.test(String(merged.currency))
  ) {
    throw new Error(
      `currency must be three capital letters, got ${JSON.stringify(merged.currency)}`
    );
  }
  if (typeof merged.lookUpPrices !== "boolean") {
    throw new Error(`lookUpPrices must be true or false, got ${merged.lookUpPrices}`);
  }
  return merged;
}

// One field replaced, the way the worker's `merge_setting` does it: the field
// must be a settings key, the path settings expand a typed `~/`, and the
// result is validated as a whole.
export function fixtureSaveSetting(fixture, field, value) {
  const base = Object.assign({}, DEFAULT_SETTINGS, fixture.settings || {});
  if (!Object.prototype.hasOwnProperty.call(base, field)) {
    throw new Error(`unknown setting ${field}`);
  }
  const next = clone(base);
  next[field] = value === undefined ? null : value;
  if (
    (field === "claudeConfigDir" || field === "historyDir" || field === "codexHome") &&
    typeof next[field] === "string" &&
    (next[field] === "~" || next[field].startsWith("~/"))
  ) {
    next[field] = fixtureExpand(next[field]);
  }
  fixture.settings = validateFixtureSettings(next);
  fixture.providers = fixtureOpenProviders(fixture);
  if (fixture.burn) {
    fixture.burn.on = fixture.settings.autoBurn;
    if (fixture.settings.autoBurn) fixture.burn.suggestion = null;
  }
  fixture.lastPollAt = fixture.lastPollAt || new Date().toISOString();
  return { settings: clone(fixture.settings), warning: null };
}
