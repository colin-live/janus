//! Hand-rolled command line parsing (three subcommands do not justify a dependency).

use std::path::PathBuf;

pub const DEFAULT_HELLO_TIMEOUT_MS: u64 = 10_000;

pub const USAGE: &str = "\
coupling-host — binds Webots objects to external model processes (Janus)

USAGE:
  coupling-host check    --project <project.yaml>
  coupling-host manifest --project <project.yaml> [--instance <name>] [--sim-id <hex>]
  coupling-host run      --project <project.yaml> [--sim-id <hex>] [--hello-timeout-ms <n>] [--no-spawn]

OPTIONS:
  -p, --project <path>      project.yaml
      --instance <name>     only this instance (manifest)
      --sim-id <hex>        simulation id, 1-16 hex digits (default: 0 for `manifest`, random for `run`)
      --hello-timeout-ms    how long to wait for every model's hello (default 10000)
      --no-spawn            treat every instance as externally started (attach)
  -h, --help                this text

EXIT CODES: 0 ok | 1 configuration / validation | 2 handshake | 3 fatal runtime error";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Check {
        project: PathBuf,
    },
    Manifest {
        project: PathBuf,
        instance: Option<String>,
        sim_id: u64,
    },
    Run {
        project: PathBuf,
        sim_id: Option<u64>,
        hello_timeout_ms: u64,
        spawn: bool,
    },
    Help,
}

pub fn parse_sim_id(s: &str) -> Result<u64, String> {
    let digits = s.trim_start_matches("0x").trim_start_matches("0X");
    if digits.is_empty() || digits.len() > 16 {
        return Err(format!("invalid --sim-id {s:?}: expected 1-16 hex digits"));
    }
    u64::from_str_radix(digits, 16)
        .map_err(|_| format!("invalid --sim-id {s:?}: expected hex digits"))
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut it = args.iter();
    let sub = match it.next() {
        None => return Err("missing subcommand".into()),
        Some(s) if s == "-h" || s == "--help" || s == "help" => return Ok(Command::Help),
        Some(s) => s.as_str(),
    };

    let mut project: Option<PathBuf> = None;
    let mut instance: Option<String> = None;
    let mut sim_id: Option<u64> = None;
    let mut hello_timeout_ms = DEFAULT_HELLO_TIMEOUT_MS;
    let mut spawn = true;

    while let Some(a) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "-p" | "--project" => project = Some(PathBuf::from(value("--project")?)),
            "--instance" => instance = Some(value("--instance")?),
            "--sim-id" => sim_id = Some(parse_sim_id(&value("--sim-id")?)?),
            "--hello-timeout-ms" => {
                let v = value("--hello-timeout-ms")?;
                hello_timeout_ms = v
                    .parse()
                    .map_err(|_| format!("invalid --hello-timeout-ms {v:?}"))?;
            }
            "--no-spawn" => spawn = false,
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }

    let project = project.ok_or_else(|| "--project is required".to_string());
    match sub {
        "check" => Ok(Command::Check { project: project? }),
        "manifest" => Ok(Command::Manifest {
            project: project?,
            instance,
            sim_id: sim_id.unwrap_or(0),
        }),
        "run" => Ok(Command::Run {
            project: project?,
            sim_id,
            hello_timeout_ms,
            spawn,
        }),
        other => Err(format!("unknown subcommand {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn check_and_manifest() {
        assert_eq!(
            parse(&v(&["check", "-p", "a.yaml"])).unwrap(),
            Command::Check {
                project: "a.yaml".into()
            }
        );
        assert_eq!(
            parse(&v(&[
                "manifest",
                "--project",
                "a.yaml",
                "--instance",
                "m",
                "--sim-id",
                "0xABC"
            ]))
            .unwrap(),
            Command::Manifest {
                project: "a.yaml".into(),
                instance: Some("m".into()),
                sim_id: 0xabc
            }
        );
        // `manifest` is deterministic by default
        assert!(matches!(
            parse(&v(&["manifest", "-p", "a.yaml"])).unwrap(),
            Command::Manifest { sim_id: 0, .. }
        ));
    }

    #[test]
    fn run_options() {
        assert_eq!(
            parse(&v(&[
                "run",
                "-p",
                "a.yaml",
                "--no-spawn",
                "--hello-timeout-ms",
                "250"
            ]))
            .unwrap(),
            Command::Run {
                project: "a.yaml".into(),
                sim_id: None,
                hello_timeout_ms: 250,
                spawn: false
            }
        );
        assert!(matches!(
            parse(&v(&["run", "-p", "a.yaml"])).unwrap(),
            Command::Run {
                spawn: true,
                hello_timeout_ms: DEFAULT_HELLO_TIMEOUT_MS,
                ..
            }
        ));
    }

    #[test]
    fn errors_are_reported() {
        assert!(parse(&v(&[])).is_err());
        assert!(parse(&v(&["check"])).unwrap_err().contains("--project"));
        assert!(parse(&v(&["frobnicate", "-p", "x"])).is_err());
        assert!(parse(&v(&["run", "-p", "x", "--sim-id", "zz"])).is_err());
        assert!(parse(&v(&["run", "-p", "x", "--sim-id", "12345678901234567"])).is_err());
        assert!(parse(&v(&["run", "-p"])).is_err());
        assert!(parse(&v(&["run", "-p", "x", "--bogus"])).is_err());
    }

    #[test]
    fn help() {
        assert_eq!(parse(&v(&["--help"])).unwrap(), Command::Help);
        assert_eq!(parse(&v(&["run", "-h"])).unwrap(), Command::Help);
    }
}
