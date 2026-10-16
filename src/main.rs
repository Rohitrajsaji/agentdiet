use std::ffi::OsString;
use std::process::ExitCode;
use std::time::SystemTime;

use anyhow::Context;
use clap::Parser;

use agentdiet::cas::{Cas, LineRange};
use agentdiet::cli::{Cli, Command};
use agentdiet::{Paths, control, init, mcp, shim};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();

    // Shim mode comes first, before clap or any config is touched: invoked as
    // `git`, `cargo`, ... through ~/.agentdiet/shims/.
    if let Some(name) = args.first().and_then(|a| shim::shim_name(a)) {
        shim::passthrough(name, args.into_iter().skip(1).collect());
    }

    // `agentdiet hook <event>` is invoked by assistants on every event. Until
    // the hook slice lands it must do nothing: exit 0, no output, no stdin
    // read, never blocking the agent (fail open).
    if args.get(1).is_some_and(|a| a == "hook") {
        return ExitCode::SUCCESS;
    }

    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("agentdiet: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn not_yet(what: &str) -> ExitCode {
    eprintln!("agentdiet: `{what}` is not available yet (coming in a later slice).");
    ExitCode::from(2)
}

/// `agentdiet show #id [--lines a..b]`: print a vault entry (or a slice of it) raw.
fn show(paths: &Paths, id: &str, lines: Option<&str>) -> anyhow::Result<()> {
    use std::io::Write;

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
