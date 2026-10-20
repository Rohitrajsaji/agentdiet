use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;
use std::time::SystemTime;

use anyhow::Context;
use clap::Parser;

use agentdiet::cas::{Cas, CasError, LineRange};
use agentdiet::cli::{self, Cli, Command};
use agentdiet::{
    Paths, center, control, diet, hook, init, mcp, reflect, scan, shim, state, stats, status, top,
    ui,
};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();

    // Shim mode comes first, before clap or any config is touched: invoked as
    // `git`, `cargo`, ... through ~/.agentdiet/shims/.
    if let Some(name) = args.first().and_then(|a| shim::shim_name(a)) {
        install_quiet_panic_hook();
        shim::run(name, args.into_iter().skip(1).collect(), Paths::from_env);
    }

    // `agentdiet hook <event>` is invoked by assistants on every event. It never
    // fails and never blocks the agent (SPEC §3 Tier 1, "Hook router").
    if args.get(1).is_some_and(|a| a == "hook") {
        return run_hook(args.get(2));
    }

    match run() {
        Ok(code) => code,
        Err(e) => {
            ui::warn(&format!("agentdiet: {e:#}"));
            ExitCode::FAILURE
        }
    }
}

/// The engine runs in hooks and shims, where a panic message could carry tool output: print only
/// the fixed note (SPEC §3 Tier 2, "Panics").
fn install_quiet_panic_hook() {
    // `ui::warn`, not `eprintln!`: a panic inside the panic hook (stderr closed) would abort.
    std::panic::set_hook(Box::new(|_| ui::warn(diet::PANIC_NOTE)));
}

/// Always exits 0. Unknown events (and `noop`) return before stdin is touched.
fn run_hook(event: Option<&OsString>) -> ExitCode {
    let Some(event) = event.and_then(|e| e.to_str()).and_then(hook::Event::parse) else {
        return ExitCode::SUCCESS;
    };
    install_quiet_panic_hook();
    let Some(payload) = hook::read_payload(hook::STDIN_TIMEOUT, hook::MAX_PAYLOAD_BYTES) else {
        return ExitCode::SUCCESS;
    };
    let Ok(paths) = Paths::from_env() else {
        return ExitCode::SUCCESS;
    };
    let response = hook::handle(event, &payload, &paths, SystemTime::now());
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(response.stdout.as_bytes());
    let _ = out.flush();
    // A closed stderr must not cost the exit status 0 (SPEC §3 Tier 1, "Closed or broken stdio").
    for note in response.notes {
        ui::warn(&note);
    }
    ExitCode::SUCCESS
}

/// `agentdiet show <#id|last> [--lines a..b]`: print a vault entry (or a slice of it) raw. `last` is
/// the last trimmed output (SPEC §3b "`show last`"); there is no default, so an unquoted `show #id`
/// (a bare `show` to us) still fails instead of printing a different entry.
fn show(paths: &Paths, id: &str, lines: Option<&str>) -> anyhow::Result<()> {
    let range = lines
        .map(LineRange::parse)
        .transpose()
        .map_err(|e| explain(&e))?;
    // `last` is how a person looks at their own output; breadcrumbs always give agents the ID. Only
    // the ID form counts as an agent re-opening a trimmed output (SPEC §2.3).
    let by_person = id.trim().eq_ignore_ascii_case("last");
    let id = match id {
        id if !by_person => id.to_string(),
        _ => state::load(paths).last_trimmed_id.context(
            "nothing has been trimmed yet, so there is no last output to show.\n\
             `agentdiet status` shows whether an assistant is connected.",
        )?,
    };
    let cas = Cas::new(paths.cas_dir());
    let bytes = cas
        .read(&id, range, SystemTime::now())
        .map_err(|e| explain(&e))?;
    // Best effort: a dereference the lock cannot record within 100 ms is dropped (SPEC §2.3).
    if !by_person {
        let _ = state::record_dereference(paths);
    }
    // Raw bytes, never transliterated; a reader that leaves early is not an error (SPEC §2.5).
    ui::write_out(&bytes).context("could not write to stdout")
}

/// A Vault error in the CLI's words (SPEC §2.5 "Errors"): what failed, why, what to do. `CasError`'s
/// own text is agent-facing (`diet_inspect`) and stays as it is.
fn explain(e: &CasError) -> anyhow::Error {
    const WHERE_IDS_COME_FROM: &str = "IDs come from the 🥗 Diet line under a trimmed output; \
         `agentdiet show last` prints the newest. Entries are kept 7 days after their last use.";
    let short = |input: &str| -> String { input.chars().take(40).collect() };
    anyhow::anyhow!(match e {
        CasError::NotFound(id) =>
            format!("no trimmed output #{id} in the Vault.\n{WHERE_IDS_COME_FROM}"),
        CasError::Expired(id) => format!(
            "#{id} has expired: Vault entries are kept 7 days after their last use.\n\
             Run the command again to see its full output."
        ),
        CasError::InvalidId(input) => format!(
            "{:?} is not a Vault ID: IDs are 4 or more hex characters, like '#4f9a1b2c'.\n\
             Copy it from the 🥗 Diet line and quote it ('#…': an unquoted # starts a shell comment), \
             or use `agentdiet show last`.",
            short(input)
        ),
        CasError::Ambiguous { .. } => format!("{e}\nType more characters of the ID."),
        CasError::Corrupt(id) => format!(
            "#{id} is damaged in the Vault (its bytes no longer match its ID), so nothing is shown.\n\
             Run the command again to see its full output."
        ),
        CasError::InvalidRange(input) => format!(
            "{:?} is not a line range: use START..END, 1-based and inclusive, like 10..40.",
            short(input)
        ),
        CasError::Busy(_) => format!("{e}; try again in a moment."),
        CasError::Io(_) => format!("could not read the Vault: {e}"),
    })
}

/// One people-facing line on stdout (SPEC §2.5): ASCII when asked, quiet on a closed pipe.
fn say(line: &str) -> anyhow::Result<()> {
    ui::emit(&format!("{line}\n")).context("could not write to stdout")
}

fn path_var() -> OsString {
    std::env::var_os("PATH").unwrap_or_default()
}

fn run() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();
    let paths = Paths::from_env()?;

    match cli.command {
        None => center::run(&paths)?,
        Some(Command::Init {
            only,
            all,
            project,
            dry_run,
        }) => init::cmd_init(&paths, only, all, project, dry_run)?,
        // The change is made first; printing it can no longer undo or crash it.
        Some(Command::Pause) => say(&control::pause(&paths)?)?,
        Some(Command::Resume) => say(&control::resume(&paths)?)?,
        Some(Command::Eject { only, purge }) => init::cmd_eject(&paths, &only, purge)?,
        Some(Command::Disconnect { targets }) => init::cmd_eject(&paths, &targets, false)?,
        Some(Command::Mcp) => {
            // The engine runs here too (`diet_exec`): a panic message must not echo its output.
            install_quiet_panic_hook();
            let stdin = std::io::stdin();
            mcp::serve(
                stdin.lock(),
                std::io::stdout().lock(),
                &paths,
                mcp::Options::from_env(),
            )
            .context("MCP server failed")?;
        }
        Some(Command::Show { id, lines }) => show(&paths, &id, lines.as_deref())?,
        Some(Command::Status { json }) => {
            let report = status::doctor(&paths, &path_var());
            let state = state::load(&paths);
            if json {
                // Data: one JSON object and nothing else on stdout (SPEC §3b "Status").
                ui::write_out(format!("{}\n", status::status_json(&state, &report)).as_bytes())?;
            } else {
                ui::emit(&status::render_status(
                    &state,
                    &report,
                    ui::Painter::stdout(),
                ))?;
            }
        }
        Some(Command::Doctor { json }) => {
            let report = status::doctor(&paths, &path_var());
            if json {
                ui::write_out(format!("{}\n", report.to_json()).as_bytes())?;
            } else {
                ui::emit(&report.render(ui::Painter::stdout()))?;
            }
            return Ok(ExitCode::from(report.exit_code()));
        }
        // Data: the script exactly as generated.
        Some(Command::Completions { shell }) => ui::write_out(&cli::completions(shell))?,
        Some(Command::Mode { mode: None }) => {
            ui::emit(&control::mode_overview(
                &state::load(&paths),
                ui::Painter::stdout(),
            ))?;
        }
        Some(Command::Mode { mode: Some(mode) }) => say(&control::set_mode(&paths, mode)?)?,
        Some(Command::Top) => top::run(&paths)?,
        Some(Command::Stats { json }) => {
            let stats = stats::collect(&paths)
                .context("could not read this workspace's activity (.agentdiet/memory.db)")?;
            if json {
                // Data: unchanged bytes, never transliterated.
                ui::write_out(format!("{}\n", stats.to_json()).as_bytes())?;
            } else {
                ui::emit(&stats.render())?;
            }
        }
        Some(Command::Reflect { dry_run }) => {
            let report =
                reflect::reflect(&paths, dry_run, SystemTime::now()).context("reflect failed")?;
            if dry_run {
                ui::write_out(report.text.as_bytes())?;
            }
            say(&report.summary())?;
        }
        Some(Command::Scan { paths: targets }) => {
            let report = scan::scan(&paths, &targets);
            for skipped in &report.skipped {
                ui::warn(&format!("agentdiet scan: skipped {skipped}"));
            }
            for error in &report.errors {
                ui::warn(&format!("agentdiet scan: {error}"));
            }
            // A reader that leaves early never replaces the verdict: the exit status is the
            // findings' (0, 1 or 2) whatever happens to stdout (SPEC §3b, §2.5).
            let _ = ui::write_out(report.render().as_bytes());
            return Ok(ExitCode::from(report.exit_code()));
        }
        // Normally intercepted above; kept so the match stays exhaustive.
        Some(Command::Hook { .. }) => {}
    }
    Ok(ExitCode::SUCCESS)
}
