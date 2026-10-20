use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;
use std::time::SystemTime;

use anyhow::Context;
use clap::Parser;

use agentdiet::cas::{Cas, LineRange};
use agentdiet::cli::{Cli, Command};
use agentdiet::{
    Paths, control, diet, hook, init, mcp, reflect, scan, shim, state, stats, status, top, ui,
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
            eprintln!("agentdiet: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The engine runs in hooks and shims, where a panic message could carry tool output: print only
/// the fixed note (SPEC §3 Tier 2, "Panics").
fn install_quiet_panic_hook() {
    std::panic::set_hook(Box::new(|_| eprintln!("{}", diet::PANIC_NOTE)));
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
    for note in response.notes {
        eprintln!("{note}");
    }
    ExitCode::SUCCESS
}

/// `agentdiet show <#id|last> [--lines a..b]`: print a vault entry (or a slice of it) raw. `last` is
/// the last trimmed output (SPEC §3b "`show last`"); there is no default, so an unquoted `show #id`
/// (a bare `show` to us) still fails instead of printing a different entry.
fn show(paths: &Paths, id: &str, lines: Option<&str>) -> anyhow::Result<()> {
    let range = lines.map(LineRange::parse).transpose()?;
    let id = match id {
        id if !id.trim().eq_ignore_ascii_case("last") => id.to_string(),
        _ => state::load(paths)
            .last_trimmed_id
            .context("nothing has been trimmed yet, so there is no last output to show")?,
    };
    let id = id.as_str();
    let cas = Cas::new(paths.cas_dir());
    let bytes = cas.read(id, range, SystemTime::now())?;
    // Best effort: a dereference the lock cannot record within 100 ms is dropped (SPEC §2.3).
    let _ = state::record_dereference(paths);
    let mut out = std::io::stdout().lock();
    out.write_all(&bytes).context("could not write to stdout")?;
    out.flush().context("could not write to stdout")
}

fn path_var() -> OsString {
    std::env::var_os("PATH").unwrap_or_default()
}

fn run() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();
    let paths = Paths::from_env()?;

    match cli.command {
        None => control::run(&paths)?,
        Some(Command::Init {
            only,
            all,
            project,
            dry_run,
        }) => init::cmd_init(&paths, only, all, project, dry_run)?,
        Some(Command::Pause) => println!("{}", control::pause(&paths)?),
        Some(Command::Resume) => println!("{}", control::resume(&paths)?),
        Some(Command::Eject { purge }) => init::cmd_eject(&paths, purge)?,
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
        Some(Command::Status) => {
            let report = status::doctor(&paths, &path_var());
            let painter = ui::Painter::stdout();
            ui::emit(&status::render_status(
                &state::load(&paths),
                &report,
                painter,
            ))?;
        }
        Some(Command::Doctor) => {
            let report = status::doctor(&paths, &path_var());
            ui::emit(&report.render(ui::Painter::stdout()))?;
            return Ok(ExitCode::from(report.exit_code()));
        }
        Some(Command::Mode { mode: None }) => {
            ui::emit(&control::mode_overview(
                &state::load(&paths),
                ui::Painter::stdout(),
            ))?;
        }
        Some(Command::Mode { mode: Some(mode) }) => {
            println!("{}", control::set_mode(&paths, mode)?)
        }
        Some(Command::Top) => top::run(&paths)?,
        Some(Command::Stats { json }) => {
            let stats = stats::collect(&paths).context("could not read the run log")?;
            if json {
                println!("{}", stats.to_json());
            } else {
                print!("{}", stats.render());
            }
        }
        Some(Command::Reflect { dry_run }) => {
            let report =
                reflect::reflect(&paths, dry_run, SystemTime::now()).context("reflect failed")?;
            if dry_run {
                print!("{}", report.text);
            }
            println!("{}", report.summary());
        }
        Some(Command::Scan { paths: targets }) => {
            let report = scan::scan(&paths, &targets);
            for skipped in &report.skipped {
                eprintln!("agentdiet scan: skipped {skipped}");
            }
            for error in &report.errors {
                eprintln!("agentdiet scan: {error}");
            }
            let mut out = std::io::stdout().lock();
            out.write_all(report.render().as_bytes())
                .and_then(|()| out.flush())
                .context("could not write to stdout")?;
            return Ok(ExitCode::from(report.exit_code()));
        }
        // Normally intercepted above; kept so the match stays exhaustive.
        Some(Command::Hook { .. }) => {}
    }
    Ok(ExitCode::SUCCESS)
}
