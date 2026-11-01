use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use coupling_config::{load_project, ResolvedProject};
use coupling_host::cli::{self, Command, USAGE};
use coupling_host::validate::validate_project;
use coupling_host::{
    log_error, log_info, log_warn, logging, signals, Host, HostError, HostOptions, TickOutcome,
};

const SUPERVISION_TICK: Duration = Duration::from_millis(50);

fn random_sim_id() -> u64 {
    let mut h = RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    h.write_u128(nanos);
    h.finish() | 1
}

/// Load + offline validation shared by every subcommand that needs a project.
fn load_checked(path: &Path) -> Result<ResolvedProject, HostError> {
    let project = load_project(path)?;
    let findings = validate_project(&project);
    for w in &findings.warnings {
        log_warn!("{w}");
    }
    if findings.errors.is_empty() {
        Ok(project)
    } else {
        Err(HostError::Validation(findings.errors))
    }
}

fn cmd_check(path: &Path) -> Result<(), HostError> {
    let project = load_checked(path)?;
    let slots: usize = project.instances.iter().map(|i| i.slots.len()).sum();
    println!(
        "OK: {} instance(s), {} slot(s)",
        project.instances.len(),
        slots
    );
    Ok(())
}

fn cmd_manifest(path: &Path, instance: Option<&str>, sim_id: u64) -> Result<(), HostError> {
    let project = load_project(path)?;
    let selected: Vec<_> = match instance {
        Some(name) => vec![project.instance(name).ok_or_else(|| {
            HostError::Usage(format!("no instance named {name:?} in the project"))
        })?],
        None => project.instances.iter().collect(),
    };
    let manifests: Vec<_> = selected
        .iter()
        .map(|i| i.to_manifest(&project.sim, sim_id))
        .collect();
    let text = if instance.is_some() {
        serde_json::to_string_pretty(&manifests[0])
    } else {
        serde_json::to_string_pretty(&manifests)
    };
    println!("{}", text.expect("manifest is plain data"));
    Ok(())
}

fn cmd_run(
    path: &Path,
    sim_id: Option<u64>,
    hello_timeout_ms: u64,
    spawn: bool,
) -> Result<(), HostError> {
    let project = load_checked(path)?;
    signals::install();
    let opts = HostOptions {
        sim_id: sim_id.unwrap_or_else(random_sim_id),
        spawn,
        hello_timeout: Duration::from_millis(hello_timeout_ms),
    };
    log_info!(
        "starting {} instance(s), sim_id {:016x}",
        project.instances.len(),
        opts.sim_id
    );
    let mut host = Host::start(&project, &opts)?;
    host.publish_run_file(&project.base_dir)?;
    println!("READY sim_id={:016x}", host.sim_id());

    while !signals::stop_requested() {
        if host.tick() == TickOutcome::AllClosed {
            log_info!("every instance is closed: the simulation has ended");
            break;
        }
        std::thread::sleep(SUPERVISION_TICK);
    }
    if signals::stop_requested() {
        log_info!("stop requested");
    }
    host.shutdown();
    Ok(())
}

fn real_main() -> i32 {
    logging::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse(&args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return 0;
        }
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return 1;
        }
    };
    let result = match command {
        Command::Help => Ok(()),
        Command::Check { project } => cmd_check(&project),
        Command::Manifest {
            project,
            instance,
            sim_id,
        } => cmd_manifest(&project, instance.as_deref(), sim_id),
        Command::Run {
            project,
            sim_id,
            hello_timeout_ms,
            spawn,
        } => cmd_run(&project, sim_id, hello_timeout_ms, spawn),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            log_error!("{e}");
            e.exit_code()
        }
    }
}

fn main() {
    std::process::exit(real_main());
}
