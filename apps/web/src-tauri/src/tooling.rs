// DEPRECATED: apps/server/src/tooling.ts — replaced by this native Rust tooling module
//
// Tool path resolution and process-spawning primitives for GlassBridge.
//
// Resolution order for each tool:
//   1. `vendor/tool-manifest.json` → `executablePath` field
//   2. Compile-time fallback list (see [`ADB_FALLBACKS`], [`SCRCPY_FALLBACKS`])
//
// All public functions are `async` and use `tokio::fs` for non-blocking I/O.

use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::fs;
use tokio::process::Command;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by tool path resolution.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::tooling::ToolError;
///
/// let e = ToolError::NotInstalled { tool: "adb".into() };
/// assert!(e.to_string().contains("adb"));
/// ```
#[derive(Debug, Error)]
pub enum ToolError {
    /// The tool executable could not be found in the manifest or any fallback path.
    #[error("{tool} is not installed locally — run the bootstrap action first")]
    NotInstalled { tool: String },

    /// The manifest file exists but could not be parsed as valid JSON.
    #[error("failed to parse tool-manifest.json: {0}")]
    #[allow(dead_code)]
    ManifestParse(String),

    /// An I/O error occurred while reading the manifest.
    #[error("I/O error reading tool-manifest.json: {0}")]
    ManifestIo(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Status snapshot for a single vendored tool.
///
/// `installed` is `true` only when the resolved path exists on disk.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    /// Whether the tool executable was found on disk.
    pub installed: bool,
    /// Resolved absolute path, if found.
    pub path: Option<PathBuf>,
    /// First line of `<tool> --version` / `<tool> version` output, if the
    /// tool is installed.
    pub version: Option<String>,
}

/// Status snapshot for all vendored tools.
#[derive(Debug, Clone)]
pub struct ToolStatus {
    /// The vendor directory that was searched.
    pub vendor_root: PathBuf,
    /// ADB tool status.
    pub adb: ToolInfo,
    /// scrcpy tool status.
    pub scrcpy: ToolInfo,
}

// ---------------------------------------------------------------------------
// Manifest shape (private)
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct ManifestEntry {
    #[serde(rename = "executablePath")]
    executable_path: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ToolManifest {
    adb: Option<ManifestEntry>,
    scrcpy: Option<ManifestEntry>,
}

// ---------------------------------------------------------------------------
// Fallback paths (relative to vendor_root)
// ---------------------------------------------------------------------------

const ADB_FALLBACKS: &[&str] = &[
    "platform-tools/platform-tools/adb.exe",
    "platform-tools/adb.exe",
];

const SCRCPY_FALLBACKS: &[&str] = &["scrcpy/scrcpy-win64-v3.3.4/scrcpy.exe", "scrcpy/scrcpy.exe"];

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Return a combined status report for adb and scrcpy.
///
/// Never fails — missing tools are reported via [`ToolInfo::installed`] being
/// `false` rather than propagating an error.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// use glassbridge_desktop::tooling::get_tool_status;
///
/// # async fn example() {
/// let status = get_tool_status(Path::new("/repo/vendor")).await;
/// println!("adb installed: {}", status.adb.installed);
/// # }
/// ```
pub async fn get_tool_status(vendor_root: &Path) -> ToolStatus {
    let manifest = read_tool_manifest(vendor_root).await;

    let adb_path = resolve_tool_path("adb", vendor_root, &manifest, ADB_FALLBACKS).await;
    let scrcpy_path = resolve_tool_path("scrcpy", vendor_root, &manifest, SCRCPY_FALLBACKS).await;

    let adb_exists = matches!(&adb_path, Some(p) if file_exists(p).await);
    let scrcpy_exists = matches!(&scrcpy_path, Some(p) if file_exists(p).await);

    let adb_version = if adb_exists {
        adb_path
            .as_ref()
            .and_then(|p| futures_read_version_sync(p, &["version"]))
    } else {
        None
    };

    let scrcpy_version = if scrcpy_exists {
        scrcpy_path
            .as_ref()
            .and_then(|p| futures_read_version_sync(p, &["--version"]))
    } else {
        None
    };

    ToolStatus {
        vendor_root: vendor_root.to_path_buf(),
        adb: ToolInfo {
            installed: adb_exists,
            path: adb_path,
            version: adb_version,
        },
        scrcpy: ToolInfo {
            installed: scrcpy_exists,
            path: scrcpy_path,
            version: scrcpy_version,
        },
    }
}

/// Resolve the adb executable path, returning an error if it cannot be found.
///
/// # Errors
///
/// Returns [`ToolError::NotInstalled`] when adb is not present in the manifest
/// or any fallback path.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// use glassbridge_desktop::tooling::get_adb_path;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let path = get_adb_path(Path::new("/repo/vendor")).await?;
/// println!("adb at {}", path.display());
/// # Ok(())
/// # }
/// ```
pub async fn get_adb_path(vendor_root: &Path) -> Result<PathBuf, ToolError> {
    let manifest = read_tool_manifest(vendor_root).await;
    resolve_tool_path("adb", vendor_root, &manifest, ADB_FALLBACKS)
        .await
        .ok_or_else(|| ToolError::NotInstalled {
            tool: "adb".to_owned(),
        })
}

/// Resolve the scrcpy executable path, returning an error if it cannot be found.
///
/// # Errors
///
/// Returns [`ToolError::NotInstalled`] when scrcpy is not present in the
/// manifest or any fallback path.
pub async fn get_scrcpy_path(vendor_root: &Path) -> Result<PathBuf, ToolError> {
    let manifest = read_tool_manifest(vendor_root).await;
    resolve_tool_path("scrcpy", vendor_root, &manifest, SCRCPY_FALLBACKS)
        .await
        .ok_or_else(|| ToolError::NotInstalled {
            tool: "scrcpy".to_owned(),
        })
}

// ---------------------------------------------------------------------------
// Process helpers (pub(crate) — consumed by adb.rs)
// ---------------------------------------------------------------------------

/// Output captured from a child process.
#[derive(Debug, Clone)]
pub(crate) struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Spawn `program` with `args`, wait for it to exit, and collect stdout/stderr
/// as UTF-8 strings.
///
/// Unlike the Node.js implementation this does **not** reject non-zero exit
/// codes — callers decide whether a non-zero code is an error.  This lets
/// `adb shell am force-stop` (which can exit 1 on some devices) succeed.
pub(crate) async fn run_command(program: &Path, args: &[&str]) -> Result<ProcessOutput, ToolError> {
    let output = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(ToolError::ManifestIo)?;

    Ok(ProcessOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Spawn `program` with `args` and collect stdout as raw bytes (for binary
/// data such as PNG screenshots).
pub(crate) async fn run_command_buffer(
    program: &Path,
    args: &[&str],
) -> Result<(Vec<u8>, i32), ToolError> {
    let output = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(ToolError::ManifestIo)?;

    Ok((output.stdout, output.status.code().unwrap_or(-1)))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Read and parse `vendor_root/tool-manifest.json`.
///
/// Returns `None` on any I/O or parse failure — callers fall back to the
/// compile-time path list.
async fn read_tool_manifest(vendor_root: &Path) -> Option<ToolManifest> {
    let manifest_path = vendor_root.join("tool-manifest.json");
    let raw = fs::read_to_string(&manifest_path).await.ok()?;
    serde_json::from_str::<ToolManifest>(&raw).ok()
}

/// Try, in order:
///   1. `manifest[tool].executablePath` (if it exists on disk)
///   2. Each entry in `fallbacks` joined to `vendor_root` (first that exists)
///
/// Returns `None` if nothing is found.
async fn resolve_tool_path(
    tool: &str,
    vendor_root: &Path,
    manifest: &Option<ToolManifest>,
    fallbacks: &[&str],
) -> Option<PathBuf> {
    // 1. Manifest entry
    if let Some(manifest) = manifest {
        let entry = match tool {
            "adb" => manifest.adb.as_ref(),
            "scrcpy" => manifest.scrcpy.as_ref(),
            _ => None,
        };
        if let Some(executable_path) = entry.and_then(|e| e.executable_path.as_deref()) {
            let p = PathBuf::from(executable_path);
            if file_exists(&p).await {
                return Some(p);
            }
        }
    }

    // 2. Fallback list
    for &rel in fallbacks {
        let candidate = vendor_root.join(rel);
        if file_exists(&candidate).await {
            return Some(candidate);
        }
    }

    None
}

/// Non-blocking existence check using `tokio::fs::metadata`.
async fn file_exists(path: &Path) -> bool {
    fs::metadata(path).await.is_ok()
}

/// Run `<program> <args>` synchronously (blocking) and return the first
/// non-empty output line.  Used only during status reporting where a single
/// blocking call is acceptable.
///
/// Returns `None` on any failure.
fn futures_read_version_sync(program: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    combined
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn adb_fallbacks_are_non_empty() {
        assert!(!ADB_FALLBACKS.is_empty());
    }

    #[test]
    fn scrcpy_fallbacks_are_non_empty() {
        assert!(!SCRCPY_FALLBACKS.is_empty());
    }

    #[tokio::test]
    async fn get_adb_path_errors_on_missing_vendor_root() {
        let result = get_adb_path(Path::new("/nonexistent/vendor/root/xyz")).await;
        assert!(
            result.is_err(),
            "should return NotInstalled when nothing exists"
        );
        let err = result.unwrap_err();
        assert!(
            matches!(err, ToolError::NotInstalled { .. }),
            "wrong error variant: {err}"
        );
    }

    #[tokio::test]
    async fn get_scrcpy_path_errors_on_missing_vendor_root() {
        let result = get_scrcpy_path(Path::new("/nonexistent/vendor/root/xyz")).await;
        assert!(matches!(
            result.unwrap_err(),
            ToolError::NotInstalled { .. }
        ));
    }

    #[tokio::test]
    async fn get_tool_status_does_not_panic_on_missing_vendor_root() {
        let status = get_tool_status(Path::new("/nonexistent/vendor/root/xyz")).await;
        assert!(!status.adb.installed);
        assert!(!status.scrcpy.installed);
        assert!(status.adb.path.is_none());
        assert!(status.scrcpy.path.is_none());
    }

    #[tokio::test]
    async fn manifest_parse_returns_none_for_bad_json() {
        // Write a temp file with invalid JSON and ensure we fall back gracefully.
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest_path = dir.path().join("tool-manifest.json");
        tokio::fs::write(&manifest_path, b"not json!!")
            .await
            .unwrap();
        let result = read_tool_manifest(dir.path()).await;
        assert!(result.is_none(), "bad JSON must return None, not panic");
    }

    #[tokio::test]
    async fn manifest_executable_path_is_preferred() {
        // Create a temp tree: vendor/tool-manifest.json + a fake adb binary.
        let dir = tempfile::tempdir().expect("tempdir");
        let fake_adb = dir.path().join("custom_adb.exe");
        tokio::fs::write(&fake_adb, b"fake").await.unwrap();

        let manifest_json = serde_json::json!({
            "adb": { "executablePath": fake_adb.to_str().unwrap() },
            "scrcpy": {}
        });
        let manifest_path = dir.path().join("tool-manifest.json");
        tokio::fs::write(&manifest_path, serde_json::to_vec(&manifest_json).unwrap())
            .await
            .unwrap();

        let resolved = get_adb_path(dir.path()).await.expect("should resolve");
        assert_eq!(resolved, fake_adb);
    }
}
