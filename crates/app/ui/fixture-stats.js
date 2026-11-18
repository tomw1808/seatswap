// The statistics fixture (step-c3 Task 8): the mock-up's made-up numbers from
// specs/page-build/data.py in exactly the JSON the Rust structs serialise to.
// The page may not add a number; this fixture may, and does: every answer is
// built from the weeks the chosen range covers and the chosen accounts.

import { answer, resolveIds, statsEmpty, statsFrame } from "./fixture/stats-frame.js";
import { activity } from "./fixture/stats-activity.js";
import { FORECAST_NO_PLAN, forecastPage } from "./fixture/stats-forecast.js";
import { overview, thisWeek } from "./fixture/stats-overview.js";
import { projectDetail, projects } from "./fixture/stats-projects.js";
import { rhythm } from "./fixture/stats-rhythm.js";
import { subscriptions } from "./fixture/stats-subscriptions.js";
import { weekSummary, weeks, windowDetail } from "./fixture/stats-weeks.js";

export function fixtureStats(command, args) {
  resolveIds();
  const params = args || {};
  const query = params.query;
  switch (command) {
    case "stats_frame":
      return statsFrame();
    case "stats_overview":
      return answer(query, overview);
    case "stats_this_week":
      return thisWeek(query);
    case "stats_weeks":
      return answer(query, weeks);
    case "stats_week_summary":
      return answer(query, (value) => weekSummary(value, params.week));
    case "stats_window_detail":
      return answer(query, (value) => windowDetail(value, params.accountId, params.end));
    case "stats_projects":
      return answer(query, projects);
    case "stats_project_detail":
      return answer(query, (value) => projectDetail(value, params.key));
    case "stats_subscriptions":
      return answer(query, subscriptions);
    case "stats_forecast":
      // The forecast never answers "empty": without a plan its one error is the
      // plain line the page shows.
      if (statsEmpty()) throw new Error(FORECAST_NO_PLAN);
      return { data: forecastPage() };
    case "stats_rhythm":
      return answer(query, rhythm);
    case "stats_activity":
      return answer(query, activity);
    default:
      throw new Error("unknown command");
  }
}
