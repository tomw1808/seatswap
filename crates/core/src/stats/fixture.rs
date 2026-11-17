//! The fixture every statistics test uses (step-c3.md, "The fixture every test
//! of this plan uses"), written as the app's own files: the history of host
//! `atlas`, `accounts.json` with the settings, `prices.json` and `fx.json`.
//! Every file goes through the module that owns it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, Utc};

use crate::api::LimitKind;
use crate::history::{
    Event, EventKind, HistoryDir, LimitSample, Sample, SessionRecord, Source, TokenRecord,
    UpRecord, WindowRecord,
};
use crate::money::{price_key, Fx, ModelPrice, PricePoint, Prices};
use crate::project::ProjectKey;
use crate::projects::{self, Projects};
use crate::store::{
    Account, AccountsFile, FileSecrets, LoginState, ProviderSubscription, Settings, Store,
};
use crate::types::OAuthAccount;

#[cfg(test)]
use super::Ctx;

const LB_ID: &str = "7c1e4b2a9d3f4e6ab8c52f0a1d7e9b34";
const LB_ACC: &str = "6f1c2a9e-1d4b-4c7f-9a3e-8b5d2e6f1c04";
const LB_ORG: &str = "a92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20";
const AC_ID: &str = "2b7d9c1e5a4f4c3b9e8d7f6a5b4c3d2e";
const AC_ACC: &str = "3b8d4c1f-7a2e-4d69-8f51-0c9a6e3b2d17";
const AC_ORG: &str = "d4f6a8c2-0e71-4b39-a5c8-7e2d1f6b3a90";

/// The fixture's history root. It has `path()` like the `TempDir` the tests
/// used to hold, so the tests that create a second host beside `atlas` read the
/// same folder as the fixture's own history. A test's `tiny()` keeps its
/// `TempDir` here, so the folder is removed when the `Tiny` is dropped;
/// `fixture::write` itself leaves the guard empty.
pub struct TinyDir {
    path: PathBuf,
    _guard: Option<Box<dyn std::any::Any + Send>>,
}

impl TinyDir {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keep the test's temp folder alive for as long as this path is needed.
    #[cfg(test)]
    pub(crate) fn keep(&mut self, guard: Box<dyn std::any::Any + Send>) {
        self._guard = Some(guard);
    }
}

/// The fixture every test of this plan uses: host `atlas`, local time UTC,
/// "now" = Thu 19 Nov 2026 10:00.
pub struct Tiny {
    /// The history root: `<root>/history`, the folder the `atlas` host folder
    /// lives in.
    pub dir: TinyDir,
    pub history: HistoryDir,
    pub accounts: Vec<Account>,
    /// The provider subscriptions `accounts.json` stores; a test adds them
    /// when its records belong to one (decision P-2 1).
    pub provider_subscriptions: Vec<ProviderSubscription>,
    pub settings: Settings,
    pub prices: Prices,
    pub fx: Fx,
    pub projects: Projects,
    /// The stats page's history read warnings; a test starts it empty.
    pub warnings: RefCell<Vec<String>>,
    pub now: DateTime<Utc>,
    pub offset: FixedOffset,
    pub windows: Vec<WindowRecord>,
}

#[cfg(test)]
impl Tiny {
    pub(crate) fn ctx(&self) -> Ctx<'_> {
        Ctx {
            history: &self.history,
            accounts: &self.accounts,
            provider_subscriptions: &self.provider_subscriptions,
            settings: &self.settings,
            prices: &self.prices,
            fx: &self.fx,
            projects: &self.projects,
            warnings: &self.warnings,
            now: self.now,
            zone: self.offset,
        }
    }

    pub(crate) fn lb(&self) -> &Account {
        &self.accounts[0]
    }

    pub(crate) fn ac(&self) -> &Account {
        &self.accounts[1]
    }

    pub(crate) fn lb_ref(&self) -> crate::history::AccountRef {
        crate::history::AccountRef::of(self.lb())
    }

    pub(crate) fn ac_ref(&self) -> crate::history::AccountRef {
        crate::history::AccountRef::of(self.ac())
    }
}

/// Write the whole fixture as seatswap's own files under `root`: the history
/// in the folder `AppPaths::at(root)` uses, `accounts.json`, `prices.json` and
/// `fx.json`. A file that cannot be written is an error with its reason.
pub fn write(root: &Path) -> Result<Tiny, String> {
    match std::fs::read_dir(root) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(format!("{} is not empty", root.display()));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot read {}: {error}", root.display())),
    }

    let now = utc("2026-11-19T10:00:00Z")?;
    let offset =
        FixedOffset::east_opt(0).ok_or_else(|| "the UTC offset cannot be built".to_string())?;

    let accounts = vec![
        account(LB_ID, LB_ACC, LB_ORG, "LB", "Max", 180.0, "#3b82f6")?,
        account(
            AC_ID,
            AC_ACC,
            AC_ORG,
            "AC",
            "Team Premium",
            140.0,
            "#f59e0b",
        )?,
    ];
    let lb = crate::history::AccountRef::of(&accounts[0]);
    let ac = crate::history::AccountRef::of(&accounts[1]);

    let atlas = ProjectKey::Repo("github.com/sam/atlas".to_string());
    let acme_kb = ProjectKey::Repo("github.com/acme/acme-kb".to_string());
    let engine = "/home/sam/projects/atlas/engine";
    let frontend = "/home/sam/projects/atlas/frontend";
    let acme_folder = "/home/sam/projects/acme-kb";

    let windows = vec![
        weekly_window(
            &ac,
            utc("2026-11-02T02:00:00Z")?,
            utc("2026-11-09T02:00:00Z")?,
            50.0,
            "Team Premium",
        ),
        weekly_window(
            &lb,
            utc("2026-11-02T17:00:00Z")?,
            utc("2026-11-09T17:00:00Z")?,
            100.0,
            "Max",
        ),
        weekly_window(
            &ac,
            utc("2026-11-09T02:00:00Z")?,
            utc("2026-11-16T02:00:00Z")?,
            90.0,
            "Team Premium",
        ),
        weekly_window(
            &lb,
            utc("2026-11-09T17:00:00Z")?,
            utc("2026-11-16T17:00:00Z")?,
            80.0,
            "Max",
        ),
    ];

    let events = vec![
        Event {
            t: utc("2026-11-02T08:00:00Z")?,
            kind: EventKind::Live {
                account: Some(lb.clone()),
            },
        },
        Event {
            t: utc("2026-11-07T10:00:00Z")?,
            kind: EventKind::Blocked {
                account: lb.clone(),
                k: LimitKind::WeeklyAll,
                m: None,
            },
        },
        Event {
            t: utc("2026-11-07T12:00:00Z")?,
            kind: EventKind::Switch {
                from: Some(lb.clone()),
                to: ac.clone(),
                by: crate::history::By::User,
                reason: None,
                cwd: None,
                sessions: None,
                waited_secs: None,
            },
        },
        Event {
            t: utc("2026-11-09T17:00:00Z")?,
            kind: EventKind::Unblocked {
                account: lb.clone(),
                k: LimitKind::WeeklyAll,
                m: None,
            },
        },
        Event {
            t: utc("2026-11-10T09:00:00Z")?,
            kind: EventKind::Switch {
                from: Some(ac.clone()),
                to: lb.clone(),
                by: crate::history::By::Auto,
                reason: None,
                cwd: None,
                sessions: None,
                waited_secs: Some(120),
            },
        },
        Event {
            t: utc("2026-11-12T15:00:00Z")?,
            kind: EventKind::Switch {
                from: Some(lb.clone()),
                to: ac.clone(),
                by: crate::history::By::Agent,
                reason: None,
                cwd: None,
                sessions: None,
                waited_secs: None,
            },
        },
        Event {
            t: utc("2026-11-13T02:00:00Z")?,
            kind: EventKind::Blocked {
                account: ac.clone(),
                k: LimitKind::Session,
                m: None,
            },
        },
        Event {
            t: utc("2026-11-13T03:00:00Z")?,
            kind: EventKind::Unblocked {
                account: ac.clone(),
                k: LimitKind::Session,
                m: None,
            },
        },
        Event {
            t: utc("2026-11-14T10:30:00Z")?,
            kind: EventKind::Waited {
                session: "s3".to_string(),
                project: atlas.clone(),
                secs: 600,
            },
        },
    ];

    let tokens = vec![
        token("2026-11-03T09:00:00Z", "s1", engine, &atlas, 1_000_000)?,
        token("2026-11-03T10:00:00Z", "s1", engine, &atlas, 1_000_000)?,
        token(
            "2026-11-08T09:00:00Z",
            "s2",
            acme_folder,
            &acme_kb,
            1_000_000,
        )?,
        token("2026-11-11T09:00:00Z", "s4", engine, &atlas, 1_000_000)?,
        token("2026-11-14T10:00:00Z", "s3", frontend, &atlas, 2_000_000)?,
        token("2026-11-14T11:00:00Z", "s3", frontend, &atlas, 1_000_000)?,
    ];

    let sessions = vec![
        session(
            "s1",
            engine,
            &atlas,
            "2026-11-03T09:00:00Z",
            "2026-11-03T11:00:00Z",
            "2026-11-03T11:05:00Z",
        )?,
        session(
            "s2",
            acme_folder,
            &acme_kb,
            "2026-11-08T09:00:00Z",
            "2026-11-08T10:00:00Z",
            "2026-11-08T10:05:00Z",
        )?,
        session(
            "s4",
            engine,
            &atlas,
            "2026-11-11T09:00:00Z",
            "2026-11-11T10:00:00Z",
            "2026-11-11T10:05:00Z",
        )?,
        session(
            "s3",
            frontend,
            &atlas,
            "2026-11-14T10:00:00Z",
            "2026-11-14T12:00:00Z",
            "2026-11-14T12:05:00Z",
        )?,
    ];

    let samples = vec![
        Sample {
            t: now - Duration::minutes(1),
            account: lb.clone(),
            src: Source::Poll,
            l: vec![LimitSample {
                k: LimitKind::WeeklyAll,
                m: None,
                p: 30.0,
                r: Some(utc("2026-11-23T17:00:00Z")?),
                b: true,
            }],
        },
        Sample {
            t: now - Duration::minutes(1),
            account: ac.clone(),
            src: Source::Poll,
            l: vec![LimitSample {
                k: LimitKind::WeeklyAll,
                m: None,
                p: 40.0,
                r: Some(utc("2026-11-23T02:00:00Z")?),
                b: true,
            }],
        },
    ];

    let mut ups = Vec::new();
    let mut at = utc("2026-11-03T08:00:00Z")?;
    while at <= utc("2026-11-03T12:00:00Z")? {
        ups.push(UpRecord { t: at });
        at += Duration::minutes(5);
    }

    let mut prices = Prices::default();
    prices.models.insert(
        price_key("anthropic", "claude-opus-5-5"),
        vec![PricePoint {
            from: utc("2026-01-01T00:00:00Z")?,
            price: ModelPrice {
                input: 4.0,
                output: 20.0,
                cache_write: 5.0,
                cache_read: 0.2,
                tiered: false,
            },
        }],
    );
    let mut fx = Fx {
        currency: "EUR".to_string(),
        ..Fx::default()
    };
    let rate_day = NaiveDate::from_ymd_opt(2026, 1, 1)
        .ok_or_else(|| "the fixture's rate day cannot be built".to_string())?;
    fx.take(rate_day, 1.0);
    let settings = Settings {
        currency: Some("EUR".to_string()),
        host_name: Some("atlas".to_string()),
        ..Settings::default()
    };

    let app = crate::paths::AppPaths::at(root);
    let history = HistoryDir::open(&app, &settings).map_err(|error| error.to_string())?;
    history
        .append_many(&windows)
        .map_err(|error| error.to_string())?;
    history
        .append_many(&events)
        .map_err(|error| error.to_string())?;
    history
        .append_many(&tokens)
        .map_err(|error| error.to_string())?;
    history
        .append_many(&sessions)
        .map_err(|error| error.to_string())?;
    history
        .append_many(&samples)
        .map_err(|error| error.to_string())?;
    history
        .append_many(&ups)
        .map_err(|error| error.to_string())?;

    let file = AccountsFile {
        accounts: accounts.clone(),
        settings: settings.clone(),
        ..AccountsFile::default()
    };
    Store::new(app.clone(), Box::new(FileSecrets::new(app.secrets_dir())))
        .save(&file)
        .map_err(|error| error.to_string())?;
    prices.save(&app).map_err(|error| error.to_string())?;
    fx.save(&app).map_err(|error| error.to_string())?;

    let (projects, _warnings) =
        projects::load(&history, &accounts).map_err(|error| error.to_string())?;

    Ok(Tiny {
        dir: TinyDir {
            path: history.root().to_path_buf(),
            _guard: None,
        },
        history,
        accounts,
        provider_subscriptions: Vec::new(),
        settings,
        prices,
        fx,
        projects,
        warnings: RefCell::new(Vec::new()),
        now,
        offset,
        windows,
    })
}

fn utc(text: &str) -> Result<DateTime<Utc>, String> {
    text.parse::<DateTime<Utc>>()
        .map_err(|error| format!("the fixture date {text:?}: {error}"))
}

fn account(
    id: &str,
    acc: &str,
    org: &str,
    abbr: &str,
    plan: &str,
    price: f64,
    color: &str,
) -> Result<Account, String> {
    Ok(Account {
        id: id.to_string(),
        account_uuid: acc.to_string(),
        organization_uuid: org.to_string(),
        label: abbr.to_string(),
        abbr: abbr.to_string(),
        color: color.to_string(),
        email: Some(format!("{}@mail.example", abbr.to_ascii_lowercase())),
        org_name: None,
        plan: Some(plan.to_string()),
        price: Some(price),
        price_currency: Some("EUR".to_string()),
        oauth_account: OAuthAccount(serde_json::Map::new()),
        added_at: utc("2026-11-01T00:00:00Z")?,
        last_used_at: None,
        login_state: LoginState::Ok,
        login_ends_at_ms: None,
        auto_burn: true,
    })
}

fn weekly_window(
    account: &crate::history::AccountRef,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    final_percent: f64,
    plan: &str,
) -> WindowRecord {
    WindowRecord {
        account: account.clone(),
        k: LimitKind::WeeklyAll,
        m: None,
        start: Some(start),
        end,
        final_percent,
        lost: Some(100.0 - final_percent),
        last_sample: end - Duration::minutes(2),
        plan: Some(plan.to_string()),
        uncertain: false,
        reset: false,
        from: None,
    }
}

fn token(
    t: &str,
    session: &str,
    cwd: &str,
    project: &ProjectKey,
    out: u64,
) -> Result<TokenRecord, String> {
    Ok(TokenRecord {
        t: utc(t)?,
        minutes: 60,
        session: session.to_string(),
        cwd: cwd.to_string(),
        project: project.clone(),
        model: "claude-opus-5-5".to_string(),
        input: 0,
        out,
        cw: 0,
        cr: 0,
        answers: 1,
        agent_answers: 0,
    })
}

fn session(
    session: &str,
    cwd: &str,
    project: &ProjectKey,
    first: &str,
    last: &str,
    closed_at: &str,
) -> Result<SessionRecord, String> {
    Ok(SessionRecord {
        t: utc(closed_at)?,
        session: session.to_string(),
        cwd: cwd.to_string(),
        project: project.clone(),
        entrypoint: Some("cli".to_string()),
        title: None,
        first: utc(first)?,
        last: utc(last)?,
        agents_started: 0,
        agents_finished: 0,
        closed: true,
    })
}
