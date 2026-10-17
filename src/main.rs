use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;
use std::time::SystemTime;

use anyhow::Context;
use clap::Parser;

use agentdiet::cas::{Cas, LineRange};
use agentdiet::cli::{Cli, Command};
use agentdiet::{Paths, control, diet, hook, init, mcp, shim};

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

fn not_yet(what: &str) -> ExitCode {
    eprintln!("agentdiet: `{what}` is not available yet (coming in a later slice).");
    ExitCode::from(2)
}

/// `agentdiet show #id [--lines a..b]`: print a vault entry (or a slice of it) raw.
fn show(paths: &Paths, id: &str, lines: Option<&str>) -> anyhow::Result<()> {
    let range = lines.map(LineRange::parse).transpose()?;
    let cas = Cas::new(paths.cas_dir());
    let bytes = cas.read(id, range, SystemTime::now())?;
    let mut out = std::io::stdout().lock();
    out.write_all(&bytes).context("could not write to stdout")?;
    out.flush().context("could not write to stdout")
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
            let stdin = std::io::stdin();
            mcp::serve(stdin.lock(), std::io::stdout().lock()).context("MCP server failed")?;
        }
        Some(Command::Show { id, lines }) => show(&paths, &id, lines.as_deref())?,
        Some(Command::Top) => return Ok(not_yet("top")),
        Some(Command::Scan { .. }) => return Ok(not_yet("scan")),
        // Normally intercepted above; kept so the match stays exhaustive.
        Some(Command::Hook { .. }) => {}
    }
    Ok(ExitCode::SUCCESS)
}
