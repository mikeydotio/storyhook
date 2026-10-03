//! Private RV-10 discovery fixtures for store-free client boundaries.
use sha2::{Digest, Sha256};
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
use std::path::{Path, PathBuf};

/// A ready generation whose provider returns only its recorded inspection.
pub struct Protection {
    /// User home containing persistent discovery.
    pub home: PathBuf,
    /// Public logical socket.
    pub socket: PathBuf,
    /// Private endpoint bound to this generation.
    pub endpoint: PathBuf,
    /// Record that a test may invalidate to prove fail-closed behavior.
    pub activation: PathBuf,
}

impl Protection {
    /// Publish a private fixture; no tmux server or real installation is changed.
    pub fn new(home: &Path) -> Self {
        let home = home.canonicalize().unwrap();
        let socket = home.join("logical");
        let generation = "a".repeat(32);
        let endpoint = home.join(format!(".rv-{generation}/s"));
        let executable = home.join("revivify");
        let state = home.join("snapshots");
        let activation = home
            .join(".local/state/tmux-revivify/activation")
            .join(format!(
                "{:x}.json",
                Sha256::digest(socket.as_os_str().as_bytes())
            ));
        std::fs::create_dir_all(activation.parent().unwrap()).unwrap();
        let record = serde_json::json!({
            "version": 1, "active": true, "socket": socket, "executable": executable,
            "state_dir": state, "generation": generation, "endpoint": endpoint,
            "identity": {"host":"fixture", "boot":"fixture", "pid":1, "start":"fixture"},
            "phase":"ready", "reservation_host":"fixture", "reservation_boot":"fixture", "history":[],
            "ownership_state":"reachable", "restore_ready":true,
            "generation_state_dir":state.join("generations").join(&generation)
        });
        std::fs::write(&activation, serde_json::to_vec(&record).unwrap()).unwrap();
        std::fs::set_permissions(&activation, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(
            &executable,
            format!("#!/bin/sh\ncat '{}'\n", activation.display()),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            home,
            socket,
            endpoint,
            activation,
        }
    }
}
