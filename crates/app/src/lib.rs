//! seatswap: CLI subcommands, and the tray app when built with the `gui` feature.
//! The two programs `seatswap` and `seatswap-tray` both call [`run`].

mod burn_text;
mod cli;
mod format;
mod gui;
mod ops;
// No `gui` gate: the status line command runs in the CLI-only build too.
mod statusline;

#[cfg(feature = "gui")]
mod icon;

/// Parse the command line and do what it asks; the whole program.
pub fn run() -> std::process::ExitCode {
    cli::run()
}
