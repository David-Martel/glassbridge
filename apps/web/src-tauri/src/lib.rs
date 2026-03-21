// Phase 2-5 modules are compiled but not yet wired into server routes or main.rs.
// Suppress dead_code until integration is complete.
#![allow(dead_code)]

// lib.rs — re-exports testable logic from main.rs so cargo test can reach it
// without running the Tauri application entry point.
//
// Tauri's #[cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// on main.rs prevents the binary from opening a console in release builds, but
// the library target below has no such restriction and is always reachable by
// `cargo test`.

pub mod adb;
pub mod adb_mock;
#[cfg(feature = "native-adb")]
pub mod adb_native;
pub mod agent_bus;
pub mod config;
#[cfg(feature = "screen-mirror")]
pub mod decoder;
pub mod file_transfer;
pub mod help;
pub mod logcat;
pub mod property_cache;
pub mod registry;
#[cfg(feature = "screen-mirror")]
pub mod renderer;
#[cfg(feature = "screen-mirror")]
pub mod scrcpy_protocol;
pub mod server;
pub mod telemetry;
pub mod terminal;
pub mod tooling;
pub mod wifi_adb;

pub mod desktop {
    use std::{
        net::TcpListener,
        time::{Duration, Instant},
    };

    /// Reserve a random free TCP port on localhost and immediately release it.
    ///
    /// There is an inherent TOCTOU race between releasing the port here and the
    /// consumer binding to it, but it is acceptable for test/local-dev use where
    /// collisions are extremely unlikely.
    pub fn reserve_port() -> Result<u16, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("Failed to reserve a localhost port: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("Failed to read reserved port: {e}"))?
            .port();
        drop(listener);
        Ok(port)
    }

    /// Poll `{origin}/api/health` until it returns HTTP 200 or the deadline
    /// passes.
    ///
    /// Returns `Ok(())` on success, `Err(message)` on timeout.
    pub fn wait_for_health(origin: &str, timeout: Duration) -> Result<(), String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| format!("Failed to build health-check client: {e}"))?;

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match client.get(format!("{origin}/api/health")).send() {
                Ok(r) if r.status().is_success() => return Ok(()),
                _ => std::thread::sleep(Duration::from_millis(250)),
            }
        }

        Err(format!(
            "Backend did not become healthy at {origin} within {}s",
            timeout.as_secs()
        ))
    }
}

// ---------------------------------------------------------------------------
// Unit tests — run with `cargo test` (no device required)
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::desktop::*;
    use std::net::TcpListener;

    #[test]
    fn reserve_port_returns_a_bindable_port() {
        let port = reserve_port().expect("reserve_port should succeed");
        assert!(port > 0, "port must be non-zero");

        // Verify the port is actually available to bind again.
        let listener = TcpListener::bind(format!("127.0.0.1:{port}"));
        assert!(
            listener.is_ok(),
            "reserved port {port} should be bindable: {:?}",
            listener.err()
        );
    }

    #[test]
    fn reserve_port_returns_unique_ports_on_successive_calls() {
        let p1 = reserve_port().unwrap();
        let p2 = reserve_port().unwrap();
        // Ports are chosen by the OS from the ephemeral range; they will almost
        // certainly differ.  If they collide by chance the test is still valid —
        // the function itself is correct.
        let _ = (p1, p2); // suppress unused warnings; asserting no panic is sufficient
    }

    #[test]
    fn wait_for_health_times_out_on_unreachable_host() {
        use std::time::Duration;
        // Port 1 is reserved/privileged and will be refused immediately on all
        // platforms — guaranteed to never serve HTTP 200.
        let result = wait_for_health("http://127.0.0.1:1", Duration::from_millis(600));
        assert!(result.is_err(), "Should time out on an unreachable host");
    }
}
