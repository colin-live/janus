//! Offline validation shared by `check` and `run` (things `load_project` cannot know: files on disk).
//!
//! * the world file exists and contains every bound `DEF` -> otherwise an **error** (the adapter would
//!   fail at start anyway; failing early is cheaper);
//! * a missing world file or model executable is only a **warning** (they may be produced later; `run`
//!   fails by itself if a model cannot be spawned).

use std::path::{Path, PathBuf};

use coupling_config::{check_world_defs, Launch, ResolvedProject};

#[derive(Debug, Default)]
pub struct Findings {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

fn exe_exists(exe: &Path) -> bool {
    exe.is_file() || exe.with_extension("exe").is_file()
}

pub fn validate_project(project: &ResolvedProject) -> Findings {
    let mut f = Findings::default();

    if let Some(world) = &project.sim.world {
        // `load_project` already resolved the path against the project directory.
        let world: PathBuf = world.clone();
        if !world.is_file() {
            f.warnings.push(format!(
                "world file {} not found: DEF check skipped",
                world.display()
            ));
        } else {
            let mut wanted: Vec<(String, String, String)> = Vec::new();
            for inst in &project.instances {
                for ent in &inst.entities {
                    wanted.push((inst.name.clone(), ent.name.clone(), ent.webots_def.clone()));
                }
            }
            let defs: Vec<&str> = wanted.iter().map(|(_, _, d)| d.as_str()).collect();
            let missing = check_world_defs(&world, &defs);
            for (inst, ent, def) in &wanted {
                if missing.contains(def) {
                    f.errors.push(format!(
                        "instance {inst}, entity {ent}: DEF {def} not found in {}",
                        world.display()
                    ));
                }
            }
        }
    }

    for inst in &project.instances {
        if let Launch::Spawn { exe, .. } = &inst.launch {
            if !exe_exists(exe) {
                f.warnings.push(format!(
                    "instance {}: model executable {} not found (must exist before `run`)",
                    inst.name,
                    exe.display()
                ));
            }
        }
    }
    f
}
