// agent_bus.rs — Optional agent-bus HTTP client for GlassBridge development coordination.
//
// This module provides a lightweight, fully optional HTTP client for the Redis+PostgreSQL
// agent coordination bus running on localhost:8400.  All public methods silently no-op when
// the bus is unavailable — callers never need to handle `BusError` for production paths.
//
// Usage pattern:
//   ```rust,no_run
//   let bus = AgentBusClient::try_connect("glassbridge").await;
//   if let Some(ref b) = bus {
//       let _ = b.set_presence("online", &["adb", "device-monitor"]).await;
//   }
//   ```
//
// The client is intended for development-time coordination only.  In production (release
// builds with no bus present) `try_connect` returns `None` within 2 seconds and the
// application continues normally.
//
// HTTP API surface used (agent-bus rust-cli v0.4+):
//   GET  /health                    → `BusHealth`
//   PUT  /presence/{agent}          → set presence
//   POST /messages                  → post a message
//
// All requests time out in 2 seconds.  No retries are attempted.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;
use tracing::debug;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by agent-bus HTTP operations.
///
/// These are informational only — callers that use the optional API pattern
/// (`if let Some(ref b) = bus { let _ = b.method().await; }`) never need to
/// inspect individual variants.
#[derive(Debug, Error)]
pub enum BusError {
    /// The HTTP client returned an error (connection refused, timeout, etc.).
    #[error("bus HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// The server returned a non-2xx status code.
    #[error("bus returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
}

impl BusError {
    /// Returns `true` if the error is a network-level failure (connection
    /// refused, timeout).  Useful for callers that want to distinguish
    /// transient from server errors.
    pub fn is_network(&self) -> bool {
        match self {
            Self::Http(e) => e.is_connect() || e.is_timeout(),
            Self::Status { .. } => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Wire types — match agent-bus HTTP API schemas exactly
// ---------------------------------------------------------------------------

/// POST /messages request body.
#[derive(Debug, Serialize)]
struct SendRequest<'a> {
    sender: &'a str,
    recipient: &'a str,
    topic: &'a str,
    body: &'a str,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    tags: &'a [&'a str],
}

/// PUT /presence/{agent} request body.
#[derive(Debug, Serialize)]
struct PresenceRequest<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    capabilities: &'a [&'a str],
    ttl_seconds: u64,
}

/// GET /health response body (subset of fields we care about).
///
/// # Examples
///
/// ```rust
/// use serde_json::json;
/// let v = json!({"ok": true, "stream_length": 42});
/// let h: glassbridge_desktop::agent_bus::BusHealth = serde_json::from_value(v).unwrap();
/// assert!(h.ok);
/// assert_eq!(h.stream_length, 42);
/// ```
#[derive(Debug, Deserialize)]
pub struct BusHealth {
    /// `true` when Redis is reachable and the stream is writable.
    pub ok: bool,
    /// Current number of messages in the Redis stream.
    pub stream_length: u64,
}

// ---------------------------------------------------------------------------
// DeviceEvent — broadcast payload
// ---------------------------------------------------------------------------

/// A structured device lifecycle event for broadcast to the agent bus.
///
/// Serialised as a JSON object and embedded in the `body` field of a bus
/// message so that any agent subscribed to the `device-event` topic can parse
/// the full context.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::agent_bus::DeviceEvent;
///
/// let ev = DeviceEvent {
///     event_type: "connected".into(),
///     serial: "19281FDEE000GS".into(),
///     model: Some("Pixel 10 XL".into()),
///     state: "device".into(),
/// };
/// let json = serde_json::to_string(&ev).unwrap();
/// assert!(json.contains("\"eventType\":\"connected\""));
/// assert!(json.contains("\"serial\":\"19281FDEE000GS\""));
/// ```
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEvent {
    /// Lifecycle action: `"connected"`, `"disconnected"`, or `"state_changed"`.
    pub event_type: String,
    /// ADB serial identifier (USB serial or `ip:port` for WiFi devices).
    pub serial: String,
    /// Human-readable model name, if known.
    pub model: Option<String>,
    /// ADB connection state at time of event: `"device"`, `"unauthorized"`, etc.
    pub state: String,
}

// ---------------------------------------------------------------------------
// AgentBusClient
// ---------------------------------------------------------------------------

/// Lightweight HTTP client for the agent coordination bus.
///
/// Construct via [`AgentBusClient::try_connect`].  If the bus is not running,
/// that method returns `None` and the application continues unaffected.
///
/// All methods follow the same non-blocking contract:
/// - They issue a single HTTP request with a 2-second timeout.
/// - They return `Err(BusError)` on failure rather than panicking.
/// - Callers should generally discard errors with `let _ = b.method().await;`.
///
/// # Thread Safety
///
/// `AgentBusClient` is `Clone + Send + Sync` — it wraps a `reqwest::Client`
/// (which internally uses an `Arc`) and two cheap `String` fields.
///
/// # Examples
///
/// ```rust,no_run
/// # async fn example() {
/// use glassbridge_desktop::agent_bus::AgentBusClient;
///
/// let bus = AgentBusClient::try_connect("glassbridge").await;
/// if let Some(ref b) = bus {
///     let _ = b.set_presence("online", &["adb", "device-monitor"]).await;
/// }
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct AgentBusClient {
    http: Client,
    base_url: String,
    agent_id: String,
}

impl AgentBusClient {
    /// Attempt to connect to the agent bus.
    ///
    /// Reads `AGENT_BUS_URL` from the environment; defaults to
    /// `http://localhost:8400`.  Issues a `GET /health` request with a 2-second
    /// timeout.  Returns `None` if the bus is unreachable or unhealthy.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # async fn example() {
    /// use glassbridge_desktop::agent_bus::AgentBusClient;
    ///
    /// match AgentBusClient::try_connect("glassbridge").await {
    ///     Some(client) => println!("bus connected"),
    ///     None         => println!("bus not available — continuing"),
    /// }
    /// # }
    /// ```
    pub async fn try_connect(agent_id: &str) -> Option<Self> {
        let base_url =
            std::env::var("AGENT_BUS_URL").unwrap_or_else(|_| "http://localhost:8400".to_string());

        let http = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .ok()?;

        // Perform a health check; any failure means the bus is not available.
        let resp = http.get(format!("{base_url}/health")).send().await.ok()?;

        if !resp.status().is_success() {
            debug!(
                agent_bus.url = %base_url,
                status = %resp.status(),
                "agent-bus health check failed — bus disabled"
            );
            return None;
        }

        debug!(
            agent_bus.url = %base_url,
            agent_id = %agent_id,
            "agent-bus connected"
        );

        Some(Self {
            http,
            base_url,
            agent_id: agent_id.to_string(),
        })
    }

    /// Return the base URL this client is connected to.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Return this client's agent identifier.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    // -- Core operations -------------------------------------------------------

    /// Announce or update this agent's presence on the bus.
    ///
    /// Maps to `PUT /presence/{agent}`.  The presence entry expires after
    /// `ttl_seconds` (default 180 s in the server, clamped 1–86400).
    ///
    /// # Errors
    ///
    /// Returns [`BusError`] on HTTP failure or non-2xx response.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # async fn example(client: &glassbridge_desktop::agent_bus::AgentBusClient) {
    /// let _ = client.set_presence("online", &["adb", "wifi-adb"]).await;
    /// # }
    /// ```
    pub async fn set_presence(&self, status: &str, capabilities: &[&str]) -> Result<(), BusError> {
        let body = PresenceRequest {
            status,
            capabilities,
            ttl_seconds: 180,
        };

        debug!(
            agent_bus.agent = %self.agent_id,
            agent_bus.status = %status,
            "set_presence"
        );

        let resp = self
            .http
            .put(format!("{}/presence/{}", self.base_url, self.agent_id))
            .json(&body)
            .send()
            .await?;

        Self::check_status(resp).await
    }

    /// Post a message to the bus addressed to `recipient`.
    ///
    /// Maps to `POST /messages`.  `tags` are attached as-is; the caller is
    /// responsible for adding `repo:<name>` tags per bus conventions.
    ///
    /// # Errors
    ///
    /// Returns [`BusError`] on HTTP failure or non-2xx response.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # async fn example(client: &glassbridge_desktop::agent_bus::AgentBusClient) {
    /// let _ = client
    ///     .post_message("all", "status", "device scan started", &["repo:pixel"])
    ///     .await;
    /// # }
    /// ```
    pub async fn post_message(
        &self,
        recipient: &str,
        topic: &str,
        body: &str,
        tags: &[&str],
    ) -> Result<(), BusError> {
        let req = SendRequest {
            sender: &self.agent_id,
            recipient,
            topic,
            body,
            tags,
        };

        debug!(
            agent_bus.recipient = %recipient,
            agent_bus.topic = %topic,
            "post_message"
        );

        let resp = self
            .http
            .post(format!("{}/messages", self.base_url))
            .json(&req)
            .send()
            .await?;

        Self::check_status(resp).await
    }

    /// Broadcast a device lifecycle event to all agents on the bus.
    ///
    /// Posts to `recipient = "all"`, `topic = "device-event"`.  The
    /// [`DeviceEvent`] is serialised as a JSON string in the message body so
    /// that downstream agents can parse it with their own schema.
    ///
    /// Silently succeeds if serialisation fails (should never happen in
    /// practice given the fixed struct layout).
    ///
    /// # Errors
    ///
    /// Returns [`BusError`] on HTTP failure or non-2xx response.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # async fn example(client: &glassbridge_desktop::agent_bus::AgentBusClient) {
    /// use glassbridge_desktop::agent_bus::DeviceEvent;
    ///
    /// let _ = client.broadcast_device_event(DeviceEvent {
    ///     event_type: "connected".into(),
    ///     serial: "192.168.1.5:5555".into(),
    ///     model: Some("Pixel 8".into()),
    ///     state: "device".into(),
    /// }).await;
    /// # }
    /// ```
    pub async fn broadcast_device_event(&self, event: DeviceEvent) -> Result<(), BusError> {
        let body = match serde_json::to_string(&event) {
            Ok(s) => s,
            Err(e) => {
                debug!("agent_bus: failed to serialize DeviceEvent: {e}");
                return Ok(());
            }
        };

        self.post_message(
            "all",
            "device-event",
            &body,
            &["repo:pixel", "glassbridge", "device"],
        )
        .await
    }

    // -- Internal helpers ------------------------------------------------------

    /// Convert a non-2xx HTTP response into a [`BusError::Status`].
    async fn check_status(resp: reqwest::Response) -> Result<(), BusError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let code = status.as_u16();
        let body = resp.text().await.unwrap_or_default();
        debug!(agent_bus.status = %code, agent_bus.body = %body, "bus returned error");
        Err(BusError::Status { status: code, body })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- BusHealth deserialisation -------------------------------------------

    #[test]
    fn bus_health_deserialises_ok_true() {
        let json = r#"{"ok": true, "stream_length": 1234}"#;
        let h: BusHealth = serde_json::from_str(json).expect("BusHealth deserialise");
        assert!(h.ok);
        assert_eq!(h.stream_length, 1234);
    }

    #[test]
    fn bus_health_deserialises_ok_false() {
        let json = r#"{"ok": false, "stream_length": 0}"#;
        let h: BusHealth = serde_json::from_str(json).expect("BusHealth deserialise");
        assert!(!h.ok);
        assert_eq!(h.stream_length, 0);
    }

    // -- SendRequest serialisation -------------------------------------------

    #[test]
    fn send_request_serialises_all_fields() {
        let req = SendRequest {
            sender: "glassbridge",
            recipient: "all",
            topic: "status",
            body: "hello",
            tags: &["repo:pixel", "glassbridge"],
        };
        let v = serde_json::to_value(&req).expect("serialise SendRequest");
        assert_eq!(v["sender"], "glassbridge");
        assert_eq!(v["recipient"], "all");
        assert_eq!(v["topic"], "status");
        assert_eq!(v["body"], "hello");
        assert_eq!(v["tags"][0], "repo:pixel");
        assert_eq!(v["tags"][1], "glassbridge");
    }

    #[test]
    fn send_request_omits_empty_tags() {
        let req = SendRequest {
            sender: "glassbridge",
            recipient: "all",
            topic: "ping",
            body: "ok",
            tags: &[],
        };
        let v = serde_json::to_value(&req).expect("serialise SendRequest no tags");
        // tags field must be absent when empty (skip_serializing_if)
        assert!(v.get("tags").is_none(), "tags should be omitted when empty");
    }

    // -- PresenceRequest serialisation ---------------------------------------

    #[test]
    fn presence_request_serialises_correctly() {
        let req = PresenceRequest {
            status: "online",
            capabilities: &["adb", "wifi-adb"],
            ttl_seconds: 180,
        };
        let v = serde_json::to_value(&req).expect("serialise PresenceRequest");
        assert_eq!(v["status"], "online");
        assert_eq!(v["ttl_seconds"], 180);
        assert_eq!(v["capabilities"][0], "adb");
        assert_eq!(v["capabilities"][1], "wifi-adb");
    }

    #[test]
    fn presence_request_omits_empty_capabilities() {
        let req = PresenceRequest {
            status: "idle",
            capabilities: &[],
            ttl_seconds: 60,
        };
        let v = serde_json::to_value(&req).expect("serialise PresenceRequest no caps");
        assert!(
            v.get("capabilities").is_none(),
            "capabilities should be omitted when empty"
        );
    }

    // -- DeviceEvent serialisation -------------------------------------------

    #[test]
    fn device_event_serialises_camel_case() {
        let ev = DeviceEvent {
            event_type: "connected".into(),
            serial: "19281FDEE000GS".into(),
            model: Some("Pixel 10 XL".into()),
            state: "device".into(),
        };
        let v = serde_json::to_value(&ev).expect("serialise DeviceEvent");
        // camelCase keys
        assert!(
            v.get("eventType").is_some(),
            "must have eventType (camelCase)"
        );
        assert!(
            v.get("event_type").is_none(),
            "must NOT have event_type (snake_case)"
        );
        assert_eq!(v["eventType"], "connected");
        assert_eq!(v["serial"], "19281FDEE000GS");
        assert_eq!(v["model"], "Pixel 10 XL");
        assert_eq!(v["state"], "device");
    }

    #[test]
    fn device_event_null_model_serialises() {
        let ev = DeviceEvent {
            event_type: "disconnected".into(),
            serial: "192.168.1.5:5555".into(),
            model: None,
            state: "offline".into(),
        };
        let v = serde_json::to_value(&ev).expect("serialise DeviceEvent null model");
        assert!(v["model"].is_null());
    }

    #[test]
    fn device_event_broadcast_body_is_valid_json() {
        // Verify that the body string produced by broadcast_device_event
        // would be valid JSON (tested here without making HTTP calls).
        let ev = DeviceEvent {
            event_type: "state_changed".into(),
            serial: "emulator-5554".into(),
            model: None,
            state: "unauthorized".into(),
        };
        let body = serde_json::to_string(&ev).expect("serialise event body");
        let parsed: serde_json::Value =
            serde_json::from_str(&body).expect("event body must be valid JSON");
        assert_eq!(parsed["eventType"], "state_changed");
    }

    // -- BusError helpers ----------------------------------------------------

    #[test]
    fn bus_error_status_variant_display() {
        let e = BusError::Status {
            status: 500,
            body: "internal server error".into(),
        };
        assert!(e.to_string().contains("500"));
        assert!(!e.is_network());
    }
}
