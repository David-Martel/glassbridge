// DEPRECATED: apps/server/src/config.ts — replaced by this native Rust server
//
// Configuration loader for the embedded GlassBridge Axum server.
//
// Environment variables mirror the Node.js server exactly so that existing
// `.env` files, CI scripts, and Tauri launch helpers continue to work without
// modification.

use std::path::{Path, PathBuf};

/// Runtime configuration for the embedded HTTP server.
///
/// # Examples
///
/// ```rust
/// use std::path::Path;
/// use glassbridge_desktop::config::Config;
///
/// let cfg = Config::from_env(Path::new("/repo"));
/// assert_eq!(cfg.host, "0.0.0.0");
/// assert_eq!(cfg.port, 3030);
/// ```
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Config {
    /// Interface to bind (default `0.0.0.0`; override with `HOST`).
    pub host: String,
    /// Port to listen on (default `3030`; override with `PORT`).
    pub port: u16,
    /// Directory for SQLite registry and persistent data.
    /// Override with `ANDROID_CONTROL_DATA_ROOT`.
    pub data_root: PathBuf,
    /// Directory containing vendored binaries (adb, scrcpy).
    pub vendor_root: PathBuf,
    /// Directory containing the compiled Vite/React frontend dist.
    pub web_dist_root: PathBuf,
}

impl Config {
    /// Construct a `Config` from environment variables with repo-relative defaults.
    ///
    /// `repo_root` should be the canonicalized repository root directory.
    /// It is used to compute all default paths, matching the Node.js behaviour
    /// where paths were resolved relative to `__dirname/../../../`.
    ///
    /// # Errors
    ///
    /// This function never fails — it falls back to defaults for every variable.
    pub fn from_env(repo_root: &Path) -> Self {
        let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_owned());

        let port = std::env::var("PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(3030);

        let data_root = std::env::var("ANDROID_CONTROL_DATA_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| repo_root.join("data"));

        let vendor_root = repo_root.join("vendor");
        let web_dist_root = repo_root.join("apps").join("web").join("dist");

        Self {
            host,
            port,
            data_root,
            vendor_root,
            web_dist_root,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn defaults_are_applied_when_env_is_absent() {
        // Remove the variables so we get clean defaults.
        // SAFETY: single-threaded test environment.
        unsafe {
            std::env::remove_var("HOST");
            std::env::remove_var("PORT");
            std::env::remove_var("ANDROID_CONTROL_DATA_ROOT");
        }

        let cfg = Config::from_env(Path::new("/repo"));
        assert_eq!(cfg.host, "0.0.0.0");
        assert_eq!(cfg.port, 3030);
        assert_eq!(cfg.data_root, PathBuf::from("/repo/data"));
        assert_eq!(cfg.vendor_root, PathBuf::from("/repo/vendor"));
        assert_eq!(cfg.web_dist_root, PathBuf::from("/repo/apps/web/dist"));
    }

    #[test]
    fn env_overrides_are_respected() {
        unsafe {
            std::env::set_var("HOST", "127.0.0.1");
            std::env::set_var("PORT", "9090");
            std::env::set_var("ANDROID_CONTROL_DATA_ROOT", "/custom/data");
        }

        let cfg = Config::from_env(Path::new("/repo"));
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 9090);
        assert_eq!(cfg.data_root, PathBuf::from("/custom/data"));

        unsafe {
            std::env::remove_var("HOST");
            std::env::remove_var("PORT");
            std::env::remove_var("ANDROID_CONTROL_DATA_ROOT");
        }
    }

    #[test]
    fn invalid_port_env_falls_back_to_default() {
        unsafe {
            std::env::set_var("PORT", "not-a-number");
        }
        let cfg = Config::from_env(Path::new("/repo"));
        assert_eq!(cfg.port, 3030);
        unsafe {
            std::env::remove_var("PORT");
        }
    }
}
