//! `<project dir>/.janus/run.json`: how the Webots adapter (B8) finds the simulation id.

use std::io;
use std::path::{Path, PathBuf};

pub fn run_file_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".janus").join("run.json")
}

pub fn render(sim_id: u64, instances: &[(String, u32)], pid: u32) -> String {
    let list: Vec<_> = instances
        .iter()
        .map(|(name, id)| serde_json::json!({ "name": name, "instance_id": id }))
        .collect();
    let doc =
        serde_json::json!({ "sim_id": format!("{sim_id:016x}"), "instances": list, "pid": pid });
    serde_json::to_string_pretty(&doc).expect("run.json is plain data")
}

/// Write the file atomically enough for the adapter (temp file + rename).
pub fn write(project_dir: &Path, sim_id: u64, instances: &[(String, u32)]) -> io::Result<PathBuf> {
    let path = run_file_path(project_dir);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, render(sim_id, instances, std::process::id()))?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_matches_the_documented_shape() {
        let text = render(
            0x1234abcd5678ef90,
            &[("shoulder".to_string(), 1), ("elbow".to_string(), 2)],
            77,
        );
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["sim_id"], "1234abcd5678ef90");
        assert_eq!(v["pid"], 77);
        assert_eq!(v["instances"][1]["name"], "elbow");
        assert_eq!(v["instances"][1]["instance_id"], 2);
    }

    #[test]
    fn write_then_remove() {
        let dir = std::env::temp_dir().join(format!("janus-b5-run-{}", std::process::id()));
        let p = write(&dir, 1, &[("m".into(), 1)]).unwrap();
        assert!(p.ends_with(".janus/run.json") || p.ends_with(".janus\\run.json"));
        assert!(p.is_file());
        remove(&p);
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
