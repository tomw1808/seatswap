//! The `seatswap` command line (spec §4, §9.1; docs/implementation.md
//! "app: CLI").
//!
//! This module only parses arguments, calls the core library and prints. Every
//! decision about an account, a usage percentage or which login is live is made
//! in `seatswap_core`.

use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, Utc};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use serde::Serialize;

use seatswap_core::api::{Api, Usage};
use seatswap_core::atomic::{self, Access};
use seatswap_core::engine::{suggestion, AccountView, Engine, PollReason, Suggestion, UsageCache};
use seatswap_core::history::{AccountRef, By, EventKind, Recorder};
use seatswap_core::paths::{self, AppPaths, ClaudePaths, PathsError};
use seatswap_core::store::{self, Account, AccountsFile, LoginState, Settings, Store, StoreError};
use seatswap_core::switcher::{self, LiveState, SwitchError, UnsavedPolicy};

use crate::format::{class_name, format_age, format_reset_at, integer_percent, percent, tooltip};
use crate::gui::ipc;
use crate::ops;
use crate::statusline;

/// Colour used when there is no live account or no usage to judge (the grey of
/// the unknown tray ring, docs/implementation.md icon.rs).
const NO_LOAD_COLOR: &str = "#555b66";

/// The skill shipped with the program (spec §6.4).
const AGENT_SKILL: &str = include_str!("../assets/agent-skill/SKILL.md");

const AGENTS_MAY_NOT_READ: &str = "agents may not read usage (seatswap settings → Agents)";
const AGENTS_MAY_NOT_SWITCH: &str = "agents may not switch accounts (seatswap settings → Agents)";
const AGENTS_MAY_NOT_CHANGE: &str = "agents may not change seatswap's accounts or settings";

/// Who started this call (spec §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Caller {
    Person,
    Agent,
}

/// Agent when `CLAUDECODE` is exactly `1`, or `AI_AGENT` is set and not empty.
/// `env` is the environment to read, never the process's own.
pub(crate) fn caller(env: &dyn Fn(&str) -> Option<String>) -> Caller {
    if env("CLAUDECODE").as_deref() == Some("1") {
        return Caller::Agent;
    }
    if env("AI_AGENT").is_some_and(|value| !value.is_empty()) {
        return Caller::Agent;
    }
    Caller::Person
}

/// What an agent may do with one command (spec §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    Allowed,
    Refused(&'static str),
}

/// The command line of `seatswap`.
#[derive(Debug, Parser)]
#[command(
    name = "seatswap",
    version,
    about = "Switch Claude Code between Claude accounts",
    long_about = None
)]
struct Cli {
    /// seatswap's own data folder (default: the platform config folder).
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Claude Code's config folder (default: the claudeConfigDir setting, then
    /// $CLAUDE_CONFIG_DIR, then ~/.claude).
    #[arg(long, global = true, value_name = "DIR")]
    claude_config_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

impl Cli {
    /// Make `--data-dir` and `--claude-config-dir` absolute once, before any
    /// use, so the folders are the same whatever the current folder is later
    /// (the autostart entry starts seatswap from the home folder).
    fn make_folders_absolute(&mut self) -> Result<(), PathsError> {
        for folder in [&mut self.data_dir, &mut self.claude_config_dir] {
            if let Some(path) = folder.as_deref() {
                *folder = Some(paths::absolute(path)?);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// List the stored accounts and their cached usage.
    List {
        /// Poll the usage before printing.
        #[arg(long)]
        refresh: bool,
    },

    /// Switch Claude Code to an account (by id or abbreviation).
    Use {
        /// Account id or abbreviation.
        key: String,

        /// Save the live login as an account first (optional label).
        #[arg(
            long,
            value_name = "LABEL",
            num_args = 0..=1,
            conflicts_with = "discard_current"
        )]
        save_current: Option<Option<String>>,

        /// Throw the live login away instead of saving it.
        #[arg(long, conflicts_with = "save_current")]
        discard_current: bool,
    },

    /// Switch to the next usable account in list order, wrapping around.
    Next {
        /// Save the live login as an account first (optional label).
        #[arg(
            long,
            value_name = "LABEL",
            num_args = 0..=1,
            conflicts_with = "discard_current"
        )]
        save_current: Option<Option<String>>,

        /// Throw the live login away instead of saving it.
        #[arg(long, conflicts_with = "save_current")]
        discard_current: bool,
    },

    /// Save the live login as a new account (or update the stored copy).
    Add {
        /// Label for a newly created account.
        #[arg(long, value_name = "LABEL")]
        label: Option<String>,
    },

    /// Remove an account and its stored login. Claude Code's files stay as they are.
    Rm {
        /// Account id or abbreviation.
        key: String,

        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },

    /// Change an account's label, abbreviation or colour.
    Edit {
        /// Account id or abbreviation.
        key: String,

        #[arg(long, value_name = "LABEL")]
        label: Option<String>,

        #[arg(long, value_name = "ABBR")]
        abbr: Option<String>,

        #[arg(long, value_name = "#RRGGBB")]
        color: Option<String>,
    },

    /// Install or remove seatswap's agent skill in Claude Code's config folder.
    AgentSkill {
        #[command(subcommand)]
        action: SkillAction,
    },

    /// Ask the running tray app to open its pop-up.
    Panel {
        /// Which view the pop-up opens on.
        #[arg(value_enum, default_value_t = PanelView::Main)]
        view: PanelView,
    },

    /// Print the live account and its binding limit.
    Status {
        /// Print JSON: `views` (every account), `active` (the id of the account in use) and
        /// `suggestion` (where to switch, or null).
        #[arg(long, conflicts_with_all = ["waybar", "i3blocks"])]
        json: bool,

        /// Print one waybar JSON line.
        #[arg(long, conflicts_with_all = ["json", "i3blocks"])]
        waybar: bool,

        /// Print i3blocks' three lines.
        #[arg(long, conflicts_with_all = ["json", "waybar"])]
        i3blocks: bool,

        /// Poll the usage before printing.
        #[arg(long)]
        refresh: bool,
    },

    /// Print Claude Code's status line; reads its JSON from standard input.
    Statusline,
}

/// Install or remove the bundled skill (spec §6.4).
#[derive(Debug, Subcommand)]
pub(crate) enum SkillAction {
    /// Write the skill into Claude Code's config folder.
    Install {
        /// Replace a skill file that differs from the bundled one.
        #[arg(long)]
        force: bool,
    },
    /// Remove the skill and its folder when it is empty.
    Uninstall {
        /// Remove a skill file that differs from the bundled one.
        #[arg(long)]
        force: bool,
    },
}

/// The view `seatswap panel` asks the running tray app to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum PanelView {
    Main,
    Add,
    Settings,
}

impl PanelView {
    fn as_str(self) -> &'static str {
        match self {
            PanelView::Main => "main",
            PanelView::Add => "add",
            PanelView::Settings => "settings",
        }
    }
}

/// What every command needs: the open store, Claude Code's folders and the
/// settings that were in effect when the store was opened.
struct Context {
    store: Store,
    claude: ClaudePaths,
    settings: Settings,
}

/// Everything the command line can fail with. Anything that is not an unsaved
/// live login exits 1.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    Message(String),

    /// The live login belongs to no stored account; `use`/`next` exit 2 for this.
    #[error("{0}")]
    UnsavedCurrent(String),

    /// The agent gate refused this call; exits 3 and is recorded first.
    #[error("{0}")]
    AgentRefused(&'static str),

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Switch(#[from] SwitchError),

    #[error(transparent)]
    Engine(#[from] seatswap_core::engine::EngineError),

    #[error(transparent)]
    Paths(#[from] PathsError),

    #[error(transparent)]
    Ipc(#[from] ipc::IpcError),

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error("cannot encode JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl CliError {
    fn exit_code(&self) -> u8 {
        match self {
            CliError::UnsavedCurrent(_) => 2,
            CliError::AgentRefused(_) => 3,
            _ => 1,
        }
    }
}

/// Parse the command line, run the command and turn the result into a process
/// exit code. Errors are printed as `seatswap: <message>` on stderr.
pub fn run() -> std::process::ExitCode {
    let mut cli = Cli::parse();
    if let Err(error) = cli.make_folders_absolute() {
        let error = CliError::from(error);
        let (line, code) = folders_error(cli.command.as_ref(), &error);
        match line {
            Some(line) => println!("{line}"),
            None => eprintln!("seatswap: {error}"),
        }
        return std::process::ExitCode::from(code);
    }
    let caller = caller(&|name| std::env::var(name).ok());
    #[cfg(feature = "gui")]
    if cli.command.is_none() && no_subcommand(caller) == NoSubcommand::StartApp {
        return match run_gui(&cli) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("seatswap: {error}");
                std::process::ExitCode::from(error.exit_code())
            }
        };
    }
    // Not `lock()`: `rm` asks its question through `io::stdin()` itself, and
    // std's stdin lock is not reentrant, so a lock held here would hang it.
    let mut stdin = io::stdin();
    let code = match execute(&mut cli, caller, &mut stdin) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("seatswap: {error}");
            error.exit_code()
        }
    };
    std::process::ExitCode::from(code)
}

/// A folder that cannot be made absolute before any command runs: for the
/// status line that error is the same one line `seatswap: <reason>` on standard
/// output and exit 0 as every other status line error; every other command
/// keeps it on standard error and exits 1. `None` is the command without a
/// subcommand.
fn folders_error(command: Option<&Command>, error: &CliError) -> (Option<String>, u8) {
    match command {
        Some(Command::Statusline) => (Some(format!("seatswap: {error}")), 0),
        _ => (None, error.exit_code()),
    }
}

/// What a call without a subcommand does (spec §6.1, §6.2): a person starts the
/// tray app in a `gui` build, or sees the help text without `gui`; an agent may
/// not start the app at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoSubcommand {
    #[cfg(feature = "gui")]
    StartApp,
    #[cfg(not(feature = "gui"))]
    PrintHelp,
    Refuse,
}

fn no_subcommand(caller: Caller) -> NoSubcommand {
    match caller {
        Caller::Agent => NoSubcommand::Refuse,
        Caller::Person => {
            #[cfg(feature = "gui")]
            {
                NoSubcommand::StartApp
            }
            #[cfg(not(feature = "gui"))]
            {
                NoSubcommand::PrintHelp
            }
        }
    }
}

/// Start the tray app. A store that cannot be opened is not fatal: the tray
/// starts with the error in its tooltip so the person can fix it from settings.
#[cfg(feature = "gui")]
fn run_gui(cli: &Cli) -> Result<std::process::ExitCode, CliError> {
    let app = AppPaths::resolve(cli.data_dir.as_deref())?;
    Ok(crate::gui::run(
        app,
        cli.claude_config_dir.clone(),
        cli.data_dir.clone(),
    ))
}

fn execute(cli: &mut Cli, caller: Caller, stdin: &mut dyn Read) -> Result<(), CliError> {
    let Some(command) = cli.command.take() else {
        return match no_subcommand(caller) {
            // An agent's call without a subcommand opens the store like any
            // other agent call, so the refusal can be recorded with its cwd.
            NoSubcommand::Refuse => refuse_no_subcommand(cli),
            // A person: without `gui` the help text. With `gui`, `run()` starts
            // the tray app before `execute`, so this is only reached by an
            // in-process test, which gets the help text too.
            _person => {
                Cli::command().print_help().map_err(CliError::Io)?;
                println!();
                Ok(())
            }
        };
    };
    // Claude Code calls the status line every few seconds with `CLAUDECODE` set,
    // as an agent. It is never gated and never recorded: it runs before any
    // context is opened, so it cannot touch the history or the store's locks.
    if matches!(command, Command::Statusline) {
        println!("{}", statusline(cli, stdin));
        return Ok(());
    }
    match caller {
        // A person's call is never gated, and `panel` keeps needing only the
        // data folder: it talks to the running tray app.
        Caller::Person => {
            if let Command::Panel { view } = &command {
                return panel(cli, *view);
            }
            let context = open_context(cli)?;
            run_command(context, command, By::Cli, None)
        }
        // An agent's call needs the settings and a history to record it, so the
        // context is opened before the gate and before any command does anything.
        Caller::Agent => {
            let context = open_context(cli)?;
            let cwd = current_cwd();
            let verdict = gate(&command, caller, &context.settings);
            record_agent_call(
                &context,
                &command_text(&command),
                matches!(verdict, Gate::Allowed),
                cwd.clone(),
            );
            match verdict {
                Gate::Allowed => run_command(context, command, By::Agent, cwd),
                Gate::Refused(text) => Err(CliError::AgentRefused(text)),
            }
        }
    }
}

/// A bare `seatswap` from an agent is a refused agent call: it opens the store
/// like the other agent commands, records the refusal, then exits 3.
fn refuse_no_subcommand(cli: &Cli) -> Result<(), CliError> {
    let context = open_context(cli)?;
    record_agent_call(&context, "(no subcommand)", false, current_cwd());
    Err(CliError::AgentRefused(AGENTS_MAY_NOT_CHANGE))
}

fn run_command(
    context: Context,
    command: Command,
    by: By,
    cwd: Option<String>,
) -> Result<(), CliError> {
    match command {
        Command::List { refresh } => list(context, refresh),
        Command::Use {
            key,
            save_current,
            discard_current,
        } => use_account(context, &key, save_current, discard_current, by, cwd),
        Command::Next {
            save_current,
            discard_current,
        } => next_account(context, save_current, discard_current, by, cwd),
        Command::Add { label } => add(context, label),
        Command::Rm { key, yes } => remove(context, &key, yes),
        Command::Edit {
            key,
            label,
            abbr,
            color,
        } => edit(context, &key, label, abbr, color),
        Command::AgentSkill { action } => agent_skill(&context, action),
        // Handled before the store was opened; kept for exhaustiveness.
        Command::Panel { .. } | Command::Statusline => Ok(()),
        Command::Status {
            json,
            waybar,
            i3blocks,
            refresh,
        } => status(context, json, waybar, i3blocks, refresh),
    }
}

/// `seatswap statusline`: the one line Claude Code shows. It prints that line
/// on standard output and never fails the command: every error becomes
/// `seatswap: <reason>` there, and the exit code stays 0. `stdin` is the
/// payload's reader: `run` passes the process's standard input, every test
/// passes bytes, so no test can wait on a terminal.
fn statusline(cli: &Cli, stdin: &mut dyn Read) -> String {
    let mut input = Vec::new();
    let now = Utc::now();
    match stdin.read_to_end(&mut input) {
        Ok(_) => statusline_output(cli, &input, now),
        Err(error) => format!("seatswap: cannot read standard input: {error}"),
    }
}

/// The status line built from bytes already read; [`statusline`] without the
/// process's standard input, so a test can pass the payload itself.
fn statusline_output(cli: &Cli, stdin: &[u8], now: DateTime<Utc>) -> String {
    match statusline_line(cli, stdin, now) {
        Ok(line) => line,
        Err(reason) => format!("seatswap: {reason}"),
    }
}

/// Everything the line needs, read without a lock, without the network and
/// without opening a secret store: `accounts.json`, the usage cache and Claude
/// Code's two files.
fn statusline_line(cli: &Cli, stdin: &[u8], now: DateTime<Utc>) -> Result<String, String> {
    let app = AppPaths::resolve(cli.data_dir.as_deref()).map_err(|error| error.to_string())?;
    if !app.accounts_file().is_file() {
        return Err(format!("no accounts.json in {}", app.data_dir.display()));
    }
    let file = store::load_accounts(&app).map_err(|error| error.to_string())?;
    file.settings
        .validate()
        .map_err(|error| error.to_string())?;
    let paths = resolve_paths(
        app,
        cli.claude_config_dir.as_deref(),
        file.settings.claude_config_dir.as_deref(),
    )
    .map_err(|error| error.to_string())?;
    let (app, claude) = paths.into_parts();
    let cache = UsageCache::load(&app).map_err(|error| error.to_string())?;
    let live = switcher::live_state(&claude, &file).map_err(|error| error.to_string())?;
    let payload = statusline::parse(stdin);
    let facts = statusline::facts(
        &payload,
        &file,
        &live,
        &cache,
        claude.home(),
        user_name()?,
        now,
    )?;
    Ok(statusline::render::render(
        &file.settings.statusline,
        &payload,
        &facts,
        statusline::width::terminal_width(),
        &Local,
    ))
}

/// The user name of the line's `user@host`: `$USER` on Unix, `%USERNAME%` on
/// Windows. Not set is an error the line shows, never a made-up name.
fn user_name() -> Result<String, String> {
    #[cfg(windows)]
    let variable = "USERNAME";
    #[cfg(not(windows))]
    let variable = "USER";
    match std::env::var(variable) {
        Ok(name) if !name.is_empty() => Ok(name),
        _ => Err(format!("{variable} is not set")),
    }
}

/// The gate table of spec §6.2. A person is always allowed.
pub(crate) fn gate(command: &Command, caller: Caller, settings: &Settings) -> Gate {
    if caller == Caller::Person {
        return Gate::Allowed;
    }
    match command {
        // Claude Code calls it every two seconds as an agent, for a line and
        // nothing else: no setting and no caller can refuse it.
        Command::Statusline => Gate::Allowed,
        Command::List { .. } | Command::Status { .. } => {
            if settings.agents_read {
                Gate::Allowed
            } else {
                Gate::Refused(AGENTS_MAY_NOT_READ)
            }
        }
        Command::Use {
            save_current,
            discard_current,
            ..
        }
        | Command::Next {
            save_current,
            discard_current,
            ..
        } => {
            if save_current.is_some() || *discard_current {
                Gate::Refused(AGENTS_MAY_NOT_CHANGE)
            } else if settings.agents_switch {
                Gate::Allowed
            } else {
                Gate::Refused(AGENTS_MAY_NOT_SWITCH)
            }
        }
        Command::Add { .. }
        | Command::Rm { .. }
        | Command::Edit { .. }
        | Command::AgentSkill { .. }
        | Command::Panel { .. } => Gate::Refused(AGENTS_MAY_NOT_CHANGE),
    }
}

/// The subcommand, its positional values and the names of its flags, without
/// free-text values (`--label`, `--abbr`, `--color`, `--save-current`).
fn command_text(command: &Command) -> String {
    match command {
        Command::List { refresh } => flags("list", &[("--refresh", *refresh)]),
        Command::Use {
            key,
            save_current,
            discard_current,
        } => flags(
            &format!("use {key}"),
            &[
                ("--save-current", save_current.is_some()),
                ("--discard-current", *discard_current),
            ],
        ),
        Command::Next {
            save_current,
            discard_current,
        } => flags(
            "next",
            &[
                ("--save-current", save_current.is_some()),
                ("--discard-current", *discard_current),
            ],
        ),
        Command::Add { label } => flags("add", &[("--label", label.is_some())]),
        Command::Rm { key, yes } => flags(&format!("rm {key}"), &[("--yes", *yes)]),
        Command::Edit {
            key,
            label,
            abbr,
            color,
        } => flags(
            &format!("edit {key}"),
            &[
                ("--label", label.is_some()),
                ("--abbr", abbr.is_some()),
                ("--color", color.is_some()),
            ],
        ),
        Command::AgentSkill { action } => match action {
            SkillAction::Install { force } => flags("agent-skill install", &[("--force", *force)]),
            SkillAction::Uninstall { force } => {
                flags("agent-skill uninstall", &[("--force", *force)])
            }
        },
        Command::Panel { view } => format!("panel {}", view.as_str()),
        Command::Statusline => "statusline".to_string(),
        Command::Status {
            json,
            waybar,
            i3blocks,
            refresh,
        } => flags(
            "status",
            &[
                ("--json", *json),
                ("--waybar", *waybar),
                ("--i3blocks", *i3blocks),
                ("--refresh", *refresh),
            ],
        ),
    }
}

fn flags(base: &str, flags: &[(&str, bool)]) -> String {
    let mut text = base.to_string();
    for (name, set) in flags {
        if *set {
            text.push(' ');
            text.push_str(name);
        }
    }
    text
}

/// Every agent call is one `agentCall` event, allowed or refused. A history
/// that cannot be written is one warning and never changes the call's result.
fn record_agent_call(context: &Context, command: &str, allowed: bool, cwd: Option<String>) {
    let result = (|| -> Result<(), String> {
        let recorder = ops::open_history(context.store.app_paths(), &context.settings)?;
        recorder
            .event(
                Utc::now(),
                EventKind::AgentCall {
                    command: command.to_string(),
                    cwd,
                    allowed,
                },
            )
            .map_err(|error| ops::history_unwritable(&error))
    })();
    if let Err(message) = result {
        eprintln!("seatswap: warning: {message}");
    }
}

fn current_cwd() -> Option<String> {
    match record_cwd(std::env::current_dir()) {
        Ok(cwd) => Some(cwd),
        Err(reason) => {
            eprintln!("seatswap: warning: {reason}; the agent call is recorded without a folder");
            None
        }
    }
}

/// The folder to record for an agent call: its text only when `current_dir`
/// could be read and its name is valid UTF-8; otherwise the reason it is left
/// out. A record is never silently given a replaced name or dropped.
fn record_cwd(dir: io::Result<PathBuf>) -> Result<String, String> {
    match dir {
        Ok(path) => match path.to_str() {
            Some(text) => Ok(text.to_string()),
            None => Err("the current folder's name is not UTF-8".to_string()),
        },
        Err(error) => Err(format!("the current folder cannot be read: {error}")),
    }
}

// ---------------------------------------------------------------------------
// Folder resolution and the store

/// The two folders every front end works with.
#[derive(Debug, Clone)]
pub(crate) struct Paths {
    pub app: AppPaths,
    pub claude: ClaudePaths,
}

impl Paths {
    pub(crate) fn into_parts(self) -> (AppPaths, ClaudePaths) {
        (self.app, self.claude)
    }
}

/// Resolve both folders in the order docs/implementation.md fixes:
/// Claude folder: `--claude-config-dir`, else the `claudeConfigDir` setting,
/// else `$CLAUDE_CONFIG_DIR`, else `~/.claude`. The data folder is resolved by
/// [`AppPaths::resolve`] first because the setting lives inside it.
pub(crate) fn resolve_paths(
    app: AppPaths,
    claude_config_dir: Option<&Path>,
    setting_claude_config_dir: Option<&Path>,
) -> Result<Paths, PathsError> {
    let claude = if let Some(dir) = claude_config_dir {
        ClaudePaths::resolve(Some(dir))?
    } else if let Some(dir) = setting_claude_config_dir {
        ClaudePaths::resolve(Some(dir))?
    } else {
        ClaudePaths::resolve(None)?
    };
    Ok(Paths { app, claude })
}

fn open_context(cli: &Cli) -> Result<Context, CliError> {
    let app = AppPaths::resolve(cli.data_dir.as_deref())?;
    let (store, warning) = store::open_store(app)?;
    if let Some(warning) = warning {
        eprintln!("seatswap: warning: {warning}");
    }
    let file = store.load()?;
    let paths = resolve_paths(
        store.app_paths().clone(),
        cli.claude_config_dir.as_deref(),
        file.settings.claude_config_dir.as_deref(),
    )?;
    let (_, claude) = paths.into_parts();
    Ok(Context {
        store,
        claude,
        settings: file.settings,
    })
}

/// `seatswap panel`: ask the running tray app to open a view.
fn panel(cli: &Cli, view: PanelView) -> Result<(), CliError> {
    let app = AppPaths::resolve(cli.data_dir.as_deref())?;
    ipc::send_open_panel(&app, view.as_str())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands

fn list(context: Context, refresh: bool) -> Result<(), CliError> {
    let now = Utc::now();
    let mut engine = Engine::new(context.claude, context.store, Api::new())?;
    let views = account_views(&mut engine, refresh, PollReason::PanelOpened, now)?;
    for view in &views {
        println!("{}", list_line(view, now));
    }
    Ok(())
}

fn use_account(
    context: Context,
    key: &str,
    save_current: Option<Option<String>>,
    discard_current: bool,
    by: By,
    cwd: Option<String>,
) -> Result<(), CliError> {
    let policy = unsaved_policy(save_current, discard_current);
    let now = Utc::now();
    let outcome = switcher::switch_to(&context.claude, &context.store, key, policy, now)
        .map_err(switch_error)?;
    print_switch(&context.store, &outcome.to);
    record_cli_switch(&context, &outcome, by, cwd, now);
    Ok(())
}

/// The switch is done; a history that cannot be written is a warning, never a
/// failed command.
fn record_cli_switch(
    context: &Context,
    outcome: &switcher::SwitchOutcome,
    by: By,
    cwd: Option<String>,
    at: DateTime<Utc>,
) {
    let result = (|| -> Result<(), String> {
        let file = context.store.load().map_err(|error| error.to_string())?;
        let target = file
            .find(&outcome.to)
            .ok_or_else(|| format!("the store has no account {}", outcome.to))?;
        let to = AccountRef::of(target);
        let from = outcome
            .from
            .as_deref()
            .and_then(|id| file.find(id))
            .map(AccountRef::of);
        let saved_new = outcome.saved_new.as_deref().and_then(|id| file.find(id));
        let recorder = ops::open_history(context.store.app_paths(), &context.settings)?;
        ops::record_switch(&recorder, from, to, saved_new, by, None, cwd, None, at)
            .map_err(|error| error.to_string())
    })();
    if let Err(error) = result {
        eprintln!("seatswap: warning: the switch was not written to the history: {error}");
    }
}

/// The switch already happened, so an account list that cannot name it is a
/// sentence, not a failed command.
fn print_switch(store: &Store, account_id: &str) {
    match ops::switch_message(store, account_id) {
        Ok(message) => println!("{message}"),
        Err(error) => println!("{}", ops::switched_without_list(&error)),
    }
}

fn next_account(
    context: Context,
    save_current: Option<Option<String>>,
    discard_current: bool,
    by: By,
    cwd: Option<String>,
) -> Result<(), CliError> {
    let policy = unsaved_policy(save_current, discard_current);
    let now = Utc::now();

    let mut file = context.store.load()?;
    let mut live = switcher::live_state(&context.claude, &file)?;
    if let LiveState::Unknown { email, .. } = &live {
        match &policy {
            UnsavedPolicy::Refuse => {
                return Err(CliError::UnsavedCurrent(unsaved_message(email.clone())))
            }
            UnsavedPolicy::Save { label } => {
                // Saving first turns the live login into an account, so "next
                // after the live one" has a position in the list.
                let recorder = cli_history(&context);
                let (_, history_error) = ops::save_live_login(
                    &context.claude,
                    &context.store,
                    recorder.as_ref(),
                    label.clone(),
                    now,
                )
                .map_err(CliError::Message)?;
                if let Some(error) = history_error {
                    eprintln!("seatswap: warning: {error}");
                }
                file = context.store.load()?;
                live = switcher::live_state(&context.claude, &file)?;
            }
            UnsavedPolicy::Discard => {}
        }
    }
    if let LiveState::Unknown { email, .. } = &live {
        return Err(CliError::UnsavedCurrent(unsaved_message(email.clone())));
    }

    let target = next_target(&file, &live)?;
    let outcome = switcher::switch_to(&context.claude, &context.store, &target, policy, now)
        .map_err(switch_error)?;
    print_switch(&context.store, &outcome.to);
    record_cli_switch(&context, &outcome, by, cwd, now);
    Ok(())
}

/// The CLI's history recorder, or `None` with the reason printed once. The
/// command itself never fails because the history cannot be opened.
fn cli_history(context: &Context) -> Option<Recorder> {
    match ops::open_history(context.store.app_paths(), &context.settings) {
        Ok(recorder) => Some(recorder),
        Err(error) => {
            eprintln!("seatswap: warning: {error}");
            None
        }
    }
}

fn add(context: Context, label: Option<String>) -> Result<(), CliError> {
    let recorder = cli_history(&context);
    let (account, history_error) = ops::save_live_login(
        &context.claude,
        &context.store,
        recorder.as_ref(),
        label,
        Utc::now(),
    )
    .map_err(CliError::Message)?;
    println!("Saved {} — {}", account.abbr, account.label);
    if let Some(error) = history_error {
        eprintln!("seatswap: warning: {error}");
    }
    Ok(())
}

fn remove(context: Context, key: &str, yes: bool) -> Result<(), CliError> {
    let file = context.store.load()?;
    let account = file
        .find_by_key(key)
        .ok_or_else(|| CliError::Message(format!("unknown account {key}")))?;
    if !yes && !confirm_remove(account)? {
        println!("Not removed.");
        return Ok(());
    }
    let recorder = cli_history(&context);
    let (removed, history_error) =
        ops::remove_account(&context.store, key, recorder.as_ref(), Utc::now())
            .map_err(CliError::Message)?;
    println!("Removed {} — {}", removed.abbr, removed.label);
    if let Some(error) = history_error {
        eprintln!("seatswap: warning: {error}");
    }
    Ok(())
}

/// Ask `Remove AC — Acme? [y/N]` on a terminal. stdin that is not a
/// terminal is refused: a script must pass `--yes`.
fn confirm_remove(account: &Account) -> Result<bool, CliError> {
    if !io::stdin().is_terminal() {
        return Err(CliError::Message(format!(
            "refusing to remove {} — {} without --yes (stdin is not a terminal)",
            account.abbr, account.label
        )));
    }
    print!("Remove {} — {}? [y/N] ", account.abbr, account.label);
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

fn edit(
    context: Context,
    key: &str,
    label: Option<String>,
    abbr: Option<String>,
    color: Option<String>,
) -> Result<(), CliError> {
    if label.is_none() && abbr.is_none() && color.is_none() {
        return Err(CliError::Message(
            "nothing to change; give --label, --abbr or --color".to_string(),
        ));
    }
    let account = switcher::edit_account(
        &context.store,
        key,
        label.as_deref(),
        abbr.as_deref(),
        color.as_deref(),
    )?;
    println!("Updated {} — {}", account.abbr, account.label);
    Ok(())
}

/// `agent-skill install` / `uninstall`: the file below
/// `<claude config dir>/skills/seatswap/`. Nothing outside these two commands
/// writes to Claude Code's config folder.
fn agent_skill(context: &Context, action: SkillAction) -> Result<(), CliError> {
    let folder = context.claude.skills_dir().join("seatswap");
    let path = folder.join("SKILL.md");
    match action {
        SkillAction::Install { force } => install_skill(&folder, &path, force),
        SkillAction::Uninstall { force } => uninstall_skill(&folder, &path, force),
    }
}

fn install_skill(folder: &Path, path: &Path, force: bool) -> Result<(), CliError> {
    let bundled = AGENT_SKILL.as_bytes();
    match fs::read(path) {
        Ok(existing) if existing == bundled => {
            println!("already installed: {}", path.display());
            return Ok(());
        }
        Ok(_) if !force => {
            return Err(CliError::Message(format!(
                "{} already exists and differs from seatswap's bundled skill; use --force to replace it",
                path.display()
            )))
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(CliError::Store(StoreError::Io {
                path: path.to_path_buf(),
                source,
            }))
        }
    }
    atomic::ensure_private_dir(folder).map_err(|source| {
        CliError::Store(StoreError::Io {
            path: folder.to_path_buf(),
            source,
        })
    })?;
    atomic::write_atomic(path, bundled, Access::Private).map_err(|source| {
        CliError::Store(StoreError::Io {
            path: path.to_path_buf(),
            source,
        })
    })?;
    println!("{}", path.display());
    Ok(())
}

fn uninstall_skill(folder: &Path, path: &Path, force: bool) -> Result<(), CliError> {
    let existing = match fs::read(path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            println!("not installed: {}", path.display());
            return Ok(());
        }
        Err(source) => {
            return Err(CliError::Store(StoreError::Io {
                path: path.to_path_buf(),
                source,
            }))
        }
    };
    if existing != AGENT_SKILL.as_bytes() && !force {
        return Err(CliError::Message(format!(
            "{} differs from seatswap's bundled skill; use --force to remove it",
            path.display()
        )));
    }
    fs::remove_file(path).map_err(|source| {
        CliError::Store(StoreError::Io {
            path: path.to_path_buf(),
            source,
        })
    })?;
    println!("removed {}", path.display());
    match fs::remove_dir(folder) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(source) => {
            return Err(CliError::Store(StoreError::Io {
                path: folder.to_path_buf(),
                source,
            }))
        }
    }
    Ok(())
}

fn status(
    context: Context,
    json: bool,
    waybar: bool,
    i3blocks: bool,
    refresh: bool,
) -> Result<(), CliError> {
    let now = Utc::now();
    let mut engine = Engine::new(context.claude, context.store, Api::new())?;
    let views = account_views(&mut engine, refresh, PollReason::Timer, now)?;

    if json {
        let active = views
            .iter()
            .find(|view| view.is_active)
            .map(|view| view.id.clone());
        let suggestion = suggestion(&views, &context.settings).map(SuggestionJson::from);
        let output = StatusJson {
            views: &views,
            active,
            suggestion,
        };
        println!("{}", serde_json::to_string(&output)?);
    } else if waybar {
        println!("{}", serde_json::to_string(&waybar_line(&views, now))?);
    } else if i3blocks {
        let (full, short, color) = i3blocks_lines(&views, now);
        println!("{full}");
        println!("{short}");
        println!("{color}");
    } else if let Some(view) = active_view(&views) {
        println!("{}", status_line(view, now));
    } else {
        println!("no live account");
    }
    Ok(())
}

fn account_views(
    engine: &mut Engine<Api>,
    refresh: bool,
    reason: PollReason,
    now: DateTime<Utc>,
) -> Result<Vec<AccountView>, CliError> {
    if refresh {
        Ok(engine.poll(reason, now)?)
    } else {
        Ok(engine.views(now)?)
    }
}

fn unsaved_policy(save_current: Option<Option<String>>, discard_current: bool) -> UnsavedPolicy {
    if discard_current {
        UnsavedPolicy::Discard
    } else if let Some(label) = save_current {
        UnsavedPolicy::Save { label }
    } else {
        UnsavedPolicy::Refuse
    }
}

fn switch_error(error: SwitchError) -> CliError {
    match error {
        SwitchError::CurrentNotSaved { email } => CliError::UnsavedCurrent(unsaved_message(email)),
        other => CliError::Switch(other),
    }
}

fn unsaved_message(email: Option<String>) -> String {
    match email {
        Some(email) => format!(
            "the current login ({email}) is not saved; rerun with --save-current or --discard-current"
        ),
        None => "the current login is not saved; rerun with --save-current or --discard-current"
            .to_string(),
    }
}

/// The next Ok account after the live one, wrapping around. With no known live
/// account (no login, an ended login with no identity, or a discarded unknown
/// login) the first Ok account is next.
fn next_target(file: &AccountsFile, live: &LiveState) -> Result<String, CliError> {
    let current = match live {
        LiveState::Known { account_id } => Some(account_id.as_str()),
        LiveState::Ended { account_id } => account_id.as_deref(),
        LiveState::Unknown { .. } | LiveState::NoLogin => None,
    };
    let usable: Vec<&Account> = file
        .accounts
        .iter()
        .filter(|account| account.login_state == LoginState::Ok)
        .collect();
    if usable.is_empty() {
        return Err(CliError::Message(
            "no account with a usable login; add one with seatswap add".to_string(),
        ));
    }
    let index = match current.and_then(|id| usable.iter().position(|account| account.id == id)) {
        Some(position) => (position + 1) % usable.len(),
        None => 0,
    };
    Ok(usable[index].id.clone())
}

// ---------------------------------------------------------------------------
// Plain text

fn list_line(view: &AccountView, now: DateTime<Utc>) -> String {
    let mark = if view.is_active { "* " } else { "  " };
    let session = view
        .usage
        .as_ref()
        .and_then(Usage::session)
        .map(|limit| percent(limit.percent))
        .unwrap_or_else(|| "-".to_string());
    let week = view
        .usage
        .as_ref()
        .and_then(Usage::weekly_all)
        .map(|limit| percent(limit.percent))
        .unwrap_or_else(|| "-".to_string());

    let mut fields = vec![
        format!("{mark}{}", view.abbr),
        view.label.clone(),
        view.email.clone().unwrap_or_else(|| "-".to_string()),
        view.plan.clone().unwrap_or_else(|| "-".to_string()),
        format!("session {session}"),
        format!("week {week}"),
        login_ends_text(view.login_days_left).unwrap_or_else(|| "-".to_string()),
    ];
    if view.login_state == LoginState::LoggedOut {
        fields.push("LOGGED OUT".to_string());
    }
    if view.usage.is_some() {
        if let Some(fetched_at) = view.fetched_at {
            fields.push(format!("({})", format_age(fetched_at, now)));
        }
    }
    if let Some(error) = &view.last_error {
        fields.push(format!("error: {error}"));
    }
    fields.join("  ")
}

fn status_line(view: &AccountView, now: DateTime<Utc>) -> String {
    match view.usage.as_ref().and_then(Usage::binding) {
        Some(limit) => {
            let mut text = format!("{} {}", view.abbr, percent(limit.percent));
            if let Some(resets_at) = limit.resets_at {
                text.push_str(" · ");
                text.push_str(&format_reset_at(resets_at, now, &Local));
            }
            text
        }
        None => view.abbr.clone(),
    }
}

fn active_view(views: &[AccountView]) -> Option<&AccountView> {
    views.iter().find(|view| view.is_active)
}

fn login_ends_text(days: Option<i64>) -> Option<String> {
    match days {
        Some(days) if days < 0 => Some("login ended".to_string()),
        Some(days) => Some(format!("login ends in {days} d")),
        None => None,
    }
}

// ---------------------------------------------------------------------------
// Machine-readable output

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusJson<'a> {
    views: &'a [AccountView],
    active: Option<String>,
    suggestion: Option<SuggestionJson>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SuggestionJson {
    from_id: String,
    to_id: String,
    from_percent: f64,
    to_percent: f64,
    from_resets_at: Option<DateTime<Utc>>,
}

impl From<Suggestion> for SuggestionJson {
    fn from(suggestion: Suggestion) -> SuggestionJson {
        SuggestionJson {
            from_id: suggestion.from_id,
            to_id: suggestion.to_id,
            from_percent: suggestion.from_percent,
            to_percent: suggestion.to_percent,
            from_resets_at: suggestion.from_resets_at,
        }
    }
}

#[derive(Debug, Serialize)]
struct WaybarLine {
    text: String,
    tooltip: String,
    class: &'static str,
    percentage: Option<i64>,
}

fn waybar_line(views: &[AccountView], now: DateTime<Utc>) -> WaybarLine {
    let Some(view) = active_view(views) else {
        return WaybarLine {
            text: "—".to_string(),
            tooltip: "no live account".to_string(),
            class: "none",
            percentage: None,
        };
    };
    let Some(usage) = &view.usage else {
        return waybar_without_usage(view);
    };
    let Some(binding) = usage.binding() else {
        return waybar_without_usage(view);
    };
    WaybarLine {
        text: format!("{} {}", view.abbr, percent(binding.percent)),
        tooltip: tooltip(view, usage, now),
        class: class_name(Some(binding.level())),
        percentage: Some(integer_percent(binding.percent)),
    }
}

fn waybar_without_usage(view: &AccountView) -> WaybarLine {
    WaybarLine {
        text: "—".to_string(),
        tooltip: format!("{} · {} — no usage yet", view.abbr, view.label),
        class: "none",
        percentage: None,
    }
}

fn i3blocks_lines(views: &[AccountView], now: DateTime<Utc>) -> (String, String, String) {
    match active_view(views) {
        Some(view) => {
            let level = view
                .usage
                .as_ref()
                .and_then(Usage::binding)
                .map(|limit| limit.level());
            let short = match view.usage.as_ref().and_then(Usage::binding) {
                Some(limit) => format!("{} {}", view.abbr, percent(limit.percent)),
                None => view.abbr.clone(),
            };
            let color = level.map(|level| level.color()).unwrap_or(NO_LOAD_COLOR);
            (status_line(view, now), short, color.to_string())
        }
        None => ("—".to_string(), "—".to_string(), NO_LOAD_COLOR.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Utc;
    use serde_json::{json, Value};

    use seatswap_core::engine::AccountView;
    use seatswap_core::paths::ClaudePaths;
    use seatswap_core::store::{FileSecrets, LoginState, Settings, Store};

    const FIRST_ACCOUNT_UUID: &str = "4b7e2f91-3c5a-4d8e-9f1b-6a2c8e4d7b30";
    const FIRST_ORG_UUID: &str = "e2a6c9d1-7f4b-4a3e-8c5d-1b9f0a7e3c62";
    const SECOND_ACCOUNT_UUID: &str = "8e1d5b7a-6c3f-4a2b-9d8e-1f2a3b4c5d6e";
    const SECOND_ORG_UUID: &str = "5f2c8a1b-9d4e-4f6a-8b3c-7e2d1a9c4b5e";
    const THIRD_ACCOUNT_UUID: &str = "2b7e9c4d-1a3f-4c5e-8b6d-7f0a1c2e3d4b";
    const THIRD_ORG_UUID: &str = "9a1c3e5f-7b9d-4f2a-8c6e-0d1b3a5c7e9f";

    struct Fixture {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        config_dir: PathBuf,
        data_dir: PathBuf,
    }

    fn live(index: usize) -> (String, String, String, &'static str, &'static str) {
        match index {
            0 => (
                FIRST_ACCOUNT_UUID.to_string(),
                FIRST_ORG_UUID.to_string(),
                "one@example.com".to_string(),
                "Acme",
                "Acme",
            ),
            _ => (
                SECOND_ACCOUNT_UUID.to_string(),
                SECOND_ORG_UUID.to_string(),
                "two@example.com".to_string(),
                "Private",
                "Private",
            ),
        }
    }

    fn credentials_json(org_uuid: &str) -> String {
        json!({
            "claudeAiOauth": {
                "accessToken": format!("sk-ant-oat01-{}", "a".repeat(95)),
                "refreshToken": format!("sk-ant-ort01-{}", "b".repeat(95)),
                "expiresAt": 1_794_356_040_000_i64,
                "refreshTokenExpiresAt": 1_796_772_840_000_i64,
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": "team",
                "subscriptionTypeSource": "profile",
                "rateLimitTier": "default_claude_max_5x"
            },
            "organizationUuid": org_uuid
        })
        .to_string()
    }

    fn identity_json(account_uuid: &str, org_uuid: &str, email: &str, org_name: &str) -> String {
        json!({
            "numStartups": 41,
            "oauthAccount": {
                "accountUuid": account_uuid,
                "emailAddress": email,
                "organizationUuid": org_uuid,
                "organizationName": org_name,
                "organizationRole": "admin",
                "billingType": "stripe_subscription",
                "hasExtraUsageEnabled": false
            },
            "projects": {}
        })
        .to_string()
    }

    impl Fixture {
        fn new() -> Fixture {
            let tmp = tempfile::tempdir().expect("tempdir");
            let home = tmp.path().to_path_buf();
            let config_dir = home.join("claude");
            let data_dir = home.join("data");
            fs::create_dir_all(&config_dir).expect("config folder");
            fs::create_dir_all(&data_dir).expect("data folder");
            fs::write(
                data_dir.join("accounts.json"),
                json!({
                    "version": 1,
                    "accounts": [],
                    "settings": {"hostName": "atlas"},
                    "secretsBackend": "file"
                })
                .to_string(),
            )
            .expect("accounts file");
            let fixture = Fixture {
                _tmp: tmp,
                home,
                config_dir,
                data_dir,
            };
            fixture.write_live(0);
            fixture
        }

        fn write_live(&self, index: usize) {
            let (account_uuid, org_uuid, email, org_name, _label) = live(index);
            fs::write(
                self.config_dir.join(".credentials.json"),
                credentials_json(&org_uuid),
            )
            .expect("credentials");
            fs::write(
                self.config_dir.join(".claude.json"),
                identity_json(&account_uuid, &org_uuid, &email, org_name),
            )
            .expect("identity");
        }

        fn claude(&self) -> ClaudePaths {
            ClaudePaths::for_config_dir(&self.config_dir, &self.home)
        }

        fn store(&self) -> Store {
            Store::new(
                AppPaths::at(&self.data_dir),
                Box::new(FileSecrets::new(self.data_dir.join("secrets"))),
            )
        }

        fn add_account(&self, index: usize) {
            let (_, _, _, _, label) = live(index);
            ops::save_live_login(
                &self.claude(),
                &self.store(),
                None,
                Some(label.to_string()),
                Utc::now(),
            )
            .expect("the live login is saved");
        }

        fn set_agents(&self, read: bool, switch: bool) {
            let store = self.store();
            let mut file = store.load().expect("accounts");
            file.settings.agents_read = read;
            file.settings.agents_switch = switch;
            store.save(&file).expect("settings saved");
        }

        fn cli(&self, command: Command) -> Cli {
            Cli {
                data_dir: Some(self.data_dir.clone()),
                claude_config_dir: Some(self.config_dir.clone()),
                command: Some(command),
            }
        }

        fn cli_without_command(&self) -> Cli {
            Cli {
                data_dir: Some(self.data_dir.clone()),
                claude_config_dir: Some(self.config_dir.clone()),
                command: None,
            }
        }

        fn run(&self, command: Command, caller: Caller) -> Result<(), CliError> {
            let mut cli = self.cli(command);
            let mut stdin: &[u8] = b"";
            execute(&mut cli, caller, &mut stdin)
        }

        fn events(&self) -> Vec<Value> {
            let folder = self.data_dir.join("history").join("atlas");
            let mut names = Vec::new();
            if let Ok(entries) = fs::read_dir(&folder) {
                for entry in entries {
                    let entry = entry.expect("history entry");
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with("events-") && name.ends_with(".jsonl") {
                        names.push(entry.path());
                    }
                }
            }
            names.sort();
            let mut values = Vec::new();
            for name in names {
                let text = fs::read_to_string(name).expect("events file");
                for line in text.lines() {
                    values.push(serde_json::from_str(line).expect("an event line"));
                }
            }
            values
        }

        fn live_bytes(&self) -> (Vec<u8>, Vec<u8>) {
            (
                fs::read(self.config_dir.join(".credentials.json")).expect("credentials"),
                fs::read(self.config_dir.join(".claude.json")).expect("identity"),
            )
        }
    }

    fn settings(read: bool, switch: bool) -> Settings {
        Settings {
            agents_read: read,
            agents_switch: switch,
            ..Settings::default()
        }
    }

    fn plain_use(key: &str) -> Command {
        Command::Use {
            key: key.to_string(),
            save_current: None,
            discard_current: false,
        }
    }

    fn plain_next() -> Command {
        Command::Next {
            save_current: None,
            discard_current: false,
        }
    }

    fn all_commands() -> Vec<Command> {
        vec![
            Command::List { refresh: false },
            Command::List { refresh: true },
            plain_use("PR"),
            Command::Use {
                key: "PR".to_string(),
                save_current: Some(None),
                discard_current: false,
            },
            Command::Use {
                key: "PR".to_string(),
                save_current: None,
                discard_current: true,
            },
            plain_next(),
            Command::Add { label: None },
            Command::Rm {
                key: "PR".to_string(),
                yes: true,
            },
            Command::Edit {
                key: "PR".to_string(),
                label: None,
                abbr: None,
                color: None,
            },
            Command::AgentSkill {
                action: SkillAction::Install { force: false },
            },
            Command::AgentSkill {
                action: SkillAction::Uninstall { force: false },
            },
            Command::Panel {
                view: PanelView::Main,
            },
            Command::Status {
                json: false,
                waybar: false,
                i3blocks: false,
                refresh: false,
            },
            Command::Statusline,
        ]
    }

    fn env(pairs: &[(&str, &str)]) -> Caller {
        caller(&|name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        })
    }

    #[test]
    fn a_person_is_never_gated() {
        let settings = settings(false, false);
        for command in all_commands() {
            assert_eq!(
                gate(&command, Caller::Person, &settings),
                Gate::Allowed,
                "{command:?}"
            );
        }
    }

    #[test]
    fn an_agent_is_detected_by_either_marker() {
        assert_eq!(env(&[("CLAUDECODE", "1")]), Caller::Agent);
        assert_eq!(
            env(&[("AI_AGENT", "claude-code_2-1-322_agent")]),
            Caller::Agent
        );
        assert_eq!(env(&[("CLAUDECODE", "0")]), Caller::Person);
        assert_eq!(env(&[("AI_AGENT", "")]), Caller::Person);
        assert_eq!(env(&[]), Caller::Person);
    }

    #[test]
    fn a_cwd_is_recorded_only_when_it_is_utf8() {
        assert_eq!(
            record_cwd(Ok(PathBuf::from("/home/sam/projects/atlas"))),
            Ok("/home/sam/projects/atlas".to_string())
        );
        let reason = record_cwd(Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "denied",
        )))
        .expect_err("a folder that cannot be read is not recorded");
        assert!(reason.contains("cannot be read"), "{reason}");

        #[cfg(unix)]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;

            let path = PathBuf::from(OsStr::from_bytes(b"/home/sam/\xff"));
            let reason =
                record_cwd(Ok(path)).expect_err("a name that is not UTF-8 is not recorded");
            assert!(reason.contains("not UTF-8"), "{reason}");
        }
    }

    #[test]
    fn the_gate_table() {
        // list and status: agentsRead decides, whatever agentsSwitch says.
        for read in [false, true] {
            for switch in [false, true] {
                let settings = settings(read, switch);
                let expected = if read {
                    Gate::Allowed
                } else {
                    Gate::Refused(AGENTS_MAY_NOT_READ)
                };
                for command in [
                    Command::List { refresh: false },
                    Command::List { refresh: true },
                    Command::Status {
                        json: true,
                        waybar: false,
                        i3blocks: false,
                        refresh: false,
                    },
                    Command::Status {
                        json: false,
                        waybar: true,
                        i3blocks: false,
                        refresh: true,
                    },
                ] {
                    assert_eq!(
                        gate(&command, Caller::Agent, &settings),
                        expected,
                        "agentsRead {read}, agentsSwitch {switch}: {command:?}"
                    );
                }
            }
        }

        // use and next without the unsaved-current flags: agentsSwitch decides,
        // whatever agentsRead says.
        for read in [false, true] {
            for switch in [false, true] {
                let settings = settings(read, switch);
                let expected = if switch {
                    Gate::Allowed
                } else {
                    Gate::Refused(AGENTS_MAY_NOT_SWITCH)
                };
                for command in [plain_use("PR"), plain_next()] {
                    assert_eq!(
                        gate(&command, Caller::Agent, &settings),
                        expected,
                        "agentsRead {read}, agentsSwitch {switch}: {command:?}"
                    );
                }
            }
        }

        // Everything that changes accounts or settings is always refused.
        for (read, switch) in [(false, false), (false, true), (true, false), (true, true)] {
            let settings = settings(read, switch);
            for command in [
                Command::Use {
                    key: "PR".to_string(),
                    save_current: Some(None),
                    discard_current: false,
                },
                Command::Use {
                    key: "PR".to_string(),
                    save_current: None,
                    discard_current: true,
                },
                Command::Next {
                    save_current: Some(Some("x".to_string())),
                    discard_current: false,
                },
                Command::Next {
                    save_current: None,
                    discard_current: true,
                },
                Command::Add { label: None },
                Command::Rm {
                    key: "PR".to_string(),
                    yes: true,
                },
                Command::Edit {
                    key: "PR".to_string(),
                    label: Some("Acme".to_string()),
                    abbr: None,
                    color: Some("#3b82f6".to_string()),
                },
                Command::AgentSkill {
                    action: SkillAction::Install { force: true },
                },
                Command::AgentSkill {
                    action: SkillAction::Uninstall { force: false },
                },
                Command::Panel {
                    view: PanelView::Settings,
                },
            ] {
                assert_eq!(
                    gate(&command, Caller::Agent, &settings),
                    Gate::Refused(AGENTS_MAY_NOT_CHANGE)
                );
            }
        }
    }

    #[test]
    fn the_statusline_is_never_gated() {
        // Every setting pair allows it to an agent: Claude Code calls it with
        // `CLAUDECODE=1` set, and it must print.
        for read in [false, true] {
            for switch in [false, true] {
                assert_eq!(
                    gate(&Command::Statusline, Caller::Agent, &settings(read, switch)),
                    Gate::Allowed,
                    "agentsRead {read}, agentsSwitch {switch}"
                );
            }
        }
        assert_eq!(
            gate(
                &Command::Statusline,
                Caller::Person,
                &settings(false, false)
            ),
            Gate::Allowed
        );

        // A run as an agent leaves no `agentCall` event behind.
        let fixture = Fixture::new();
        let mut cli = fixture.cli(Command::Statusline);
        let mut stdin: &[u8] = b"";
        execute(&mut cli, Caller::Agent, &mut stdin).expect("the status line is allowed");
        let events = fixture.events();
        assert!(
            !events.iter().any(|event| event["type"] == "agentCall"),
            "{events:?}"
        );
    }

    #[test]
    fn statusline_never_fails() {
        let fixture = Fixture::new();
        fs::remove_file(fixture.data_dir.join("accounts.json")).expect("the accounts file");
        let now = Utc::now();

        let line = statusline_output(&fixture.cli(Command::Statusline), b"", now);
        assert!(line.starts_with("seatswap: "), "{line}");

        // The command itself is Ok, so the process exits 0.
        let mut cli = fixture.cli(Command::Statusline);
        let mut stdin: &[u8] = b"";
        assert!(execute(&mut cli, Caller::Person, &mut stdin).is_ok());
    }

    #[test]
    fn the_statusline_reads_the_bytes_it_is_given() {
        // The input is a reader the test hands over, never the process's
        // standard input: a test run on a terminal or an open pipe would hang.
        let fixture = Fixture::new();
        fixture.add_account(0);
        let mut input: &[u8] = br#"{"model":{"id":"claude-fable-5-2","display_name":"Fable 5.2"}}"#;
        let line = statusline(&fixture.cli(Command::Statusline), &mut input);
        assert!(line.contains("Fable 5.2"), "{line}");
    }

    #[test]
    fn an_unreadable_statusline_input_is_the_lines_one_line() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "the pipe closed"))
            }
        }
        let fixture = Fixture::new();
        let line = statusline(&fixture.cli(Command::Statusline), &mut Broken);
        assert!(
            line.starts_with("seatswap: cannot read standard input: "),
            "{line}"
        );
    }

    #[test]
    fn a_folder_error_is_the_status_lines_one_line_and_exit_zero() {
        let error = CliError::from(PathsError::Absolute {
            path: PathBuf::from("relative/data"),
            source: io::Error::new(io::ErrorKind::NotFound, "the current folder was deleted"),
        });

        // The status line: one line on standard output and exit 0, like every
        // other status line error.
        let (line, code) = folders_error(Some(&Command::Statusline), &error);
        assert_eq!(line, Some(format!("seatswap: {error}")));
        assert_eq!(code, 0);

        // Every other command keeps the error on standard error and exit 1.
        for command in all_commands()
            .into_iter()
            .filter(|command| !matches!(command, Command::Statusline))
        {
            let (line, code) = folders_error(Some(&command), &error);
            assert_eq!(line, None, "{command:?}");
            assert_eq!(code, 1, "{command:?}");
        }
        let (line, code) = folders_error(None, &error);
        assert_eq!(line, None);
        assert_eq!(code, 1);
    }

    #[test]
    fn the_command_text_leaves_out_free_text() {
        assert_eq!(command_text(&plain_use("PR")), "use PR");
        assert_eq!(
            command_text(&Command::Use {
                key: "PR".to_string(),
                save_current: Some(None),
                discard_current: false,
            }),
            "use PR --save-current"
        );
        assert_eq!(
            command_text(&Command::Status {
                json: true,
                waybar: false,
                i3blocks: false,
                refresh: true,
            }),
            "status --json --refresh"
        );
        assert_eq!(command_text(&Command::Statusline), "statusline");
        assert_eq!(
            command_text(&Command::Edit {
                key: "PR".to_string(),
                label: Some("Private".to_string()),
                abbr: None,
                color: Some("#3b82f6".to_string()),
            }),
            "edit PR --label --color"
        );
        assert_eq!(
            command_text(&Command::AgentSkill {
                action: SkillAction::Install { force: true },
            }),
            "agent-skill install --force"
        );
        assert_eq!(
            command_text(&Command::AgentSkill {
                action: SkillAction::Uninstall { force: true },
            }),
            "agent-skill uninstall --force"
        );
        assert_eq!(
            command_text(&Command::Panel {
                view: PanelView::Settings,
            }),
            "panel settings"
        );
    }

    #[test]
    fn a_refused_call_exits_3_and_is_recorded() {
        let fixture = Fixture::new();
        fixture.add_account(0);
        fixture.write_live(1);
        fixture.add_account(1);
        fixture.set_agents(false, false);
        let before = fixture.live_bytes();

        let error = fixture
            .run(plain_use("PR"), Caller::Agent)
            .expect_err("an agent may not switch here");
        assert_eq!(error.exit_code(), 3);
        match error {
            CliError::AgentRefused(text) => assert_eq!(text, AGENTS_MAY_NOT_SWITCH),
            other => panic!("expected AgentRefused, got {other:?}"),
        }

        let events = fixture.events();
        let call = events
            .iter()
            .find(|event| event["type"] == "agentCall")
            .expect("an agentCall event");
        assert_eq!(call["allowed"], false);
        assert_eq!(call["command"], "use PR");
        assert!(call["cwd"].is_string(), "{call}");
        assert_eq!(fixture.live_bytes(), before);
    }

    #[test]
    fn the_no_subcommand_decision_depends_on_the_caller() {
        assert_eq!(no_subcommand(Caller::Agent), NoSubcommand::Refuse);
        #[cfg(feature = "gui")]
        assert_eq!(no_subcommand(Caller::Person), NoSubcommand::StartApp);
        #[cfg(not(feature = "gui"))]
        assert_eq!(no_subcommand(Caller::Person), NoSubcommand::PrintHelp);
    }

    #[test]
    fn an_agent_without_a_subcommand_is_refused_and_recorded() {
        let fixture = Fixture::new();
        let before = fixture.live_bytes();
        let mut cli = fixture.cli_without_command();
        let mut stdin: &[u8] = b"";

        let error = execute(&mut cli, Caller::Agent, &mut stdin)
            .expect_err("an agent may not start the tray app");
        assert_eq!(error.exit_code(), 3);
        match error {
            CliError::AgentRefused(text) => assert_eq!(text, AGENTS_MAY_NOT_CHANGE),
            other => panic!("expected AgentRefused, got {other:?}"),
        }

        let events = fixture.events();
        let call = events
            .iter()
            .find(|event| event["type"] == "agentCall")
            .expect("an agentCall event");
        assert_eq!(call["allowed"], false);
        assert_eq!(call["command"], "(no subcommand)");
        assert!(call["cwd"].is_string(), "{call}");
        assert_eq!(fixture.live_bytes(), before);
    }

    #[test]
    fn an_allowed_agent_switch_is_recorded_as_agent() {
        let fixture = Fixture::new();
        fixture.add_account(0);
        fixture.write_live(1);
        fixture.add_account(1);
        fixture.set_agents(true, true);

        fixture
            .run(plain_use("AC"), Caller::Agent)
            .expect("an allowed agent switch");

        let events = fixture.events();
        let switch = events
            .iter()
            .find(|event| event["type"] == "switch")
            .expect("a switch event");
        assert_eq!(switch["by"], "agent");
        assert!(switch["cwd"].is_string(), "{switch}");
        let call = events
            .iter()
            .find(|event| event["type"] == "agentCall")
            .expect("an agentCall event");
        assert_eq!(call["allowed"], true);
        assert_eq!(call["command"], "use AC");
    }

    #[test]
    fn skill_install_writes_the_bundled_text() {
        let fixture = Fixture::new();
        let path = fixture.claude().skills_dir().join("seatswap/SKILL.md");

        fixture
            .run(
                Command::AgentSkill {
                    action: SkillAction::Install { force: false },
                },
                Caller::Person,
            )
            .expect("the skill is installed");
        assert_eq!(
            fs::read_to_string(&path).expect("the skill file"),
            AGENT_SKILL
        );
        let modified = fs::metadata(&path)
            .expect("skill metadata")
            .modified()
            .expect("modification time");

        // A second install writes nothing.
        fixture
            .run(
                Command::AgentSkill {
                    action: SkillAction::Install { force: false },
                },
                Caller::Person,
            )
            .expect("a second install is a no-op");
        assert_eq!(
            fs::read_to_string(&path).expect("the skill file"),
            AGENT_SKILL
        );
        assert_eq!(
            fs::metadata(&path)
                .expect("skill metadata")
                .modified()
                .expect("modification time"),
            modified
        );

        // A different file is not replaced without --force.
        fs::write(&path, "another skill").expect("a changed skill");
        let error = fixture
            .run(
                Command::AgentSkill {
                    action: SkillAction::Install { force: false },
                },
                Caller::Person,
            )
            .expect_err("the changed file is kept");
        match error {
            CliError::Message(text) => {
                assert!(text.contains(&path.display().to_string()), "{text}");
                assert!(text.contains("--force"), "{text}");
            }
            other => panic!("expected a message, got {other:?}"),
        }
        assert_eq!(
            fs::read_to_string(&path).expect("the changed skill"),
            "another skill"
        );

        // --force replaces it.
        fixture
            .run(
                Command::AgentSkill {
                    action: SkillAction::Install { force: true },
                },
                Caller::Person,
            )
            .expect("--force replaces the changed file");
        assert_eq!(
            fs::read_to_string(&path).expect("the skill file"),
            AGENT_SKILL
        );
    }

    #[test]
    fn skill_uninstall_removes_the_file_and_the_empty_folder() {
        let fixture = Fixture::new();
        let folder = fixture.claude().skills_dir().join("seatswap");
        let path = folder.join("SKILL.md");
        fixture
            .run(
                Command::AgentSkill {
                    action: SkillAction::Install { force: false },
                },
                Caller::Person,
            )
            .expect("the skill is installed");
        assert!(path.is_file());

        fixture
            .run(
                Command::AgentSkill {
                    action: SkillAction::Uninstall { force: false },
                },
                Caller::Person,
            )
            .expect("the skill is removed");
        assert!(!path.exists());
        assert!(!folder.exists());
    }

    #[test]
    fn skill_uninstall_keeps_an_edited_file_unless_forced() {
        let fixture = Fixture::new();
        let folder = fixture.claude().skills_dir().join("seatswap");
        let path = folder.join("SKILL.md");
        fs::create_dir_all(&folder).expect("the skill folder");
        fs::write(&path, "my own skill").expect("an edited skill");
        let edited = fs::read(&path).expect("the edited bytes");

        let error = fixture
            .run(
                Command::AgentSkill {
                    action: SkillAction::Uninstall { force: false },
                },
                Caller::Person,
            )
            .expect_err("an edited skill is kept");
        match error {
            CliError::Message(text) => assert_eq!(
                text,
                format!(
                    "{} differs from seatswap's bundled skill; use --force to remove it",
                    path.display()
                )
            ),
            other => panic!("expected a message, got {other:?}"),
        }
        assert!(path.is_file());
        assert_eq!(fs::read(&path).expect("the kept skill"), edited);

        fixture
            .run(
                Command::AgentSkill {
                    action: SkillAction::Uninstall { force: true },
                },
                Caller::Person,
            )
            .expect("--force removes the edited skill");
        assert!(!path.exists());
        assert!(!folder.exists());
    }

    #[test]
    fn a_skill_file_error_names_its_path() {
        // `skills` is a file, not a folder.
        let fixture = Fixture::new();
        let skills = fixture.claude().skills_dir();
        fs::write(&skills, "not a folder").expect("a file where the folder goes");
        for action in [
            SkillAction::Install { force: false },
            SkillAction::Uninstall { force: false },
        ] {
            let error = fixture
                .run(Command::AgentSkill { action }, Caller::Person)
                .expect_err("a file where the folder goes is an error");
            let text = error.to_string();
            assert!(text.contains(&skills.display().to_string()), "{text}");
        }

        // `SKILL.md` is a folder, not a file.
        let fixture = Fixture::new();
        let path = fixture.claude().skills_dir().join("seatswap/SKILL.md");
        fs::create_dir_all(&path).expect("a folder where the skill goes");
        for action in [
            SkillAction::Install { force: false },
            SkillAction::Uninstall { force: false },
        ] {
            let error = fixture
                .run(Command::AgentSkill { action }, Caller::Person)
                .expect_err("a folder where the skill goes is an error");
            let text = error.to_string();
            assert!(text.contains(&path.display().to_string()), "{text}");
        }
    }

    #[test]
    fn add_writes_one_account_added_with_the_plan() {
        let fixture = Fixture::new();

        fixture
            .run(
                Command::Add {
                    label: Some("Acme".to_string()),
                },
                Caller::Person,
            )
            .expect("the live login is saved");

        let events = fixture.events();
        let added: Vec<&Value> = events
            .iter()
            .filter(|event| event["type"] == json!("accountAdded"))
            .collect();
        assert_eq!(added.len(), 1, "{events:?}");
        assert_eq!(added[0]["account"]["acc"], json!(FIRST_ACCOUNT_UUID));
        assert_eq!(added[0]["account"]["org"], json!(FIRST_ORG_UUID));
        assert_eq!(added[0]["plan"], json!("Team"));

        // Saving the same login again is an update, not a second account.
        fixture
            .run(Command::Add { label: None }, Caller::Person)
            .expect("the stored copy is updated");
        let events = fixture.events();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == json!("accountAdded"))
                .count(),
            1,
            "{events:?}"
        );
    }

    #[test]
    fn rm_yes_writes_one_account_removed() {
        let fixture = Fixture::new();
        fixture.add_account(0);

        fixture
            .run(
                Command::Rm {
                    key: "AC".to_string(),
                    yes: true,
                },
                Caller::Person,
            )
            .expect("the account is removed");

        let events = fixture.events();
        let removed: Vec<&Value> = events
            .iter()
            .filter(|event| event["type"] == json!("accountRemoved"))
            .collect();
        assert_eq!(removed.len(), 1, "{events:?}");
        assert_eq!(removed[0]["account"]["acc"], json!(FIRST_ACCOUNT_UUID));
        assert_eq!(removed[0]["account"]["org"], json!(FIRST_ORG_UUID));
        assert!(fixture
            .store()
            .load()
            .expect("accounts")
            .accounts
            .is_empty());
    }

    #[test]
    fn a_switch_that_saves_the_unsaved_login_writes_account_added_first() {
        let fixture = Fixture::new();
        fixture.add_account(0);
        fixture.write_live(1);
        fixture.add_account(1);
        // A third, unsaved live login is what the switch must save.
        fs::write(
            fixture.config_dir.join(".credentials.json"),
            credentials_json(THIRD_ORG_UUID),
        )
        .expect("third credentials");
        fs::write(
            fixture.config_dir.join(".claude.json"),
            identity_json(
                THIRD_ACCOUNT_UUID,
                THIRD_ORG_UUID,
                "three@example.com",
                "Atlas",
            ),
        )
        .expect("third identity");

        fixture
            .run(
                Command::Use {
                    key: "AC".to_string(),
                    save_current: Some(Some("Third".to_string())),
                    discard_current: false,
                },
                Caller::Person,
            )
            .expect("the unsaved login is saved and the switch is done");

        let events = fixture.events();
        let added = events
            .iter()
            .position(|event| event["type"] == json!("accountAdded"))
            .expect("the saved account's accountAdded line");
        let switched = events
            .iter()
            .position(|event| event["type"] == json!("switch"))
            .expect("the switch line");
        assert!(added < switched, "{events:?}");
        assert_eq!(events[added]["account"]["acc"], json!(THIRD_ACCOUNT_UUID));
        assert_eq!(events[added]["plan"], json!("Team"));
        assert_eq!(events[switched]["to"]["acc"], json!(FIRST_ACCOUNT_UUID));
    }

    #[test]
    fn status_json_has_usable() {
        let views = vec![account_view(true), account_view(false)];
        let output = StatusJson {
            views: &views,
            active: None,
            suggestion: None,
        };
        let value = serde_json::to_value(&output).expect("status JSON");
        let views = value["views"].as_array().expect("views");
        assert_eq!(views.len(), 2);
        for view in views {
            assert!(
                view.get("usable").and_then(Value::as_bool).is_some(),
                "{view}"
            );
        }
        assert_eq!(views[0]["usable"], true);
        assert_eq!(views[1]["usable"], false);
    }

    fn account_view(usable: bool) -> AccountView {
        AccountView {
            id: "id".to_string(),
            label: "Acme".to_string(),
            abbr: "AC".to_string(),
            color: "#3b82f6".to_string(),
            email: Some("one@example.com".to_string()),
            plan: None,
            is_active: false,
            login_state: LoginState::Ok,
            login_ends_at_ms: None,
            login_days_left: None,
            usage: None,
            usable,
            blocked_until: None,
            blocker: None,
            fetched_at: None,
            last_error: None,
        }
    }
}
