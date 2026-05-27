use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::ipc::{ProjectionState, SidecarPaths};

const CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedProjectionState {
    version: u32,
    state: ProjectionState,
}

pub fn load(paths: &SidecarPaths) -> Result<Option<ProjectionState>> {
    load_path(&paths.cache_path)
}

pub fn load_path(path: &Path) -> Result<Option<ProjectionState>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to read sidecar state cache `{}`", path.display())
            });
        }
    };

    let cached: CachedProjectionState = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse sidecar state cache `{}`", path.display()))?;
    if cached.version != CACHE_VERSION {
        bail!(
            "unsupported sidecar state cache version {} in `{}`",
            cached.version,
            path.display()
        );
    }

    Ok(Some(cached.state))
}

pub fn store(paths: &SidecarPaths, state: &ProjectionState) -> Result<()> {
    store_path(&paths.cache_path, state)
}

pub fn store_path(path: &Path, state: &ProjectionState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create sidecar state cache directory `{}`",
                parent.display()
            )
        })?;
    }

    let cache = CachedProjectionState {
        version: CACHE_VERSION,
        state: state.clone(),
    };
    let data = serde_json::to_vec(&cache)
        .with_context(|| format!("failed to encode sidecar state cache `{}`", path.display()))?;
    let tmp_path = temporary_path(path);
    fs::write(&tmp_path, data).with_context(|| {
        format!(
            "failed to write temporary sidecar state cache `{}`",
            tmp_path.display()
        )
    })?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to replace sidecar state cache `{}`", path.display()))?;
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{load_path, store_path};
    use crate::ipc::{ProjectionSession, ProjectionState};

    #[test]
    fn state_cache_round_trips_projection_state() {
        let tempdir = tempfile::tempdir().expect("create temp dir");
        let cache_path = tempdir.path().join("state.json");
        let state = ProjectionState {
            tmux_socket_path: Path::new("/tmp/tmux/default").to_path_buf(),
            sessions: vec![ProjectionSession {
                id: String::from("$1"),
                name: String::from("main"),
                attached_count: 1,
                active_window_id: None,
                windows: Vec::new(),
            }],
            clients: Vec::new(),
        };

        store_path(&cache_path, &state).expect("store state cache");

        assert_eq!(
            load_path(&cache_path).expect("load state cache"),
            Some(state)
        );
    }

    #[test]
    fn missing_state_cache_loads_as_none() {
        let tempdir = tempfile::tempdir().expect("create temp dir");

        assert!(
            load_path(&tempdir.path().join("missing.json"))
                .expect("load missing state cache")
                .is_none()
        );
    }
}
