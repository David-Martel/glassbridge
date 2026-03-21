// DEPRECATED: apps/server/src/help.ts — replaced by this native Rust server
//
// Static help content for the GlassBridge UI.
//
// The topics are returned verbatim from `GET /api/help/topics`.  All strings
// are `&'static str` — zero heap allocation, zero I/O.

use serde::Serialize;

/// A single help topic surfaced in the GlassBridge UI.
///
/// Serialises to `{ "id": "…", "title": "…", "bullets": ["…", …] }`,
/// matching the camelCase JSON produced by the Node.js server.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelpTopic {
    /// Stable identifier used by the frontend to anchor deep-links.
    pub id: &'static str,
    /// Human-readable heading displayed in the help panel.
    pub title: &'static str,
    /// Ordered list of short guidance sentences shown as bullet points.
    pub bullets: &'static [&'static str],
}

/// Return all help topics in display order.
///
/// The returned slice is `'static` — no allocation occurs.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::help::help_topics;
///
/// let topics = help_topics();
/// assert!(!topics.is_empty());
/// assert_eq!(topics[0].id, "connect");
/// ```
pub fn help_topics() -> &'static [HelpTopic] {
    TOPICS
}

static TOPICS: &[HelpTopic] = &[
    HelpTopic {
        id: "connect",
        title: "Connect A Device",
        bullets: &[
            "Enable Developer Options and USB debugging on the phone.",
            "Accept the RSA fingerprint prompt the first time the workstation connects.",
            "If the device shows unauthorized, reconnect USB and re-accept the prompt.",
        ],
    },
    HelpTopic {
        id: "realtime",
        title: "Real-Time Control",
        bullets: &[
            "Use scrcpy for low-latency visual control instead of repeated screenshots.",
            "Low Latency trades image detail for gesture response.",
            "High Quality is better for demos and visual inspection than active driving.",
        ],
    },
    HelpTopic {
        id: "apps",
        title: "App Operations",
        bullets: &[
            "Refresh packages to pull current third-party apps from the selected device.",
            "Launch starts the default launcher activity with monkey.",
            "Force Stop is useful for repeatable cold-start testing.",
        ],
    },
    HelpTopic {
        id: "recovery",
        title: "Recovery",
        bullets: &[
            "If the device is offline, restart adb and reconnect the cable.",
            "If binaries are missing, run the local bootstrap action.",
            "Keep only one adb server active on the workstation to avoid contention.",
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_four_topics() {
        assert_eq!(help_topics().len(), 4);
    }

    #[test]
    fn topic_ids_are_unique() {
        let topics = help_topics();
        let mut ids: Vec<&str> = topics.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), topics.len(), "duplicate topic id detected");
    }

    #[test]
    fn known_ids_are_present() {
        let ids: Vec<&str> = help_topics().iter().map(|t| t.id).collect();
        assert!(ids.contains(&"connect"));
        assert!(ids.contains(&"realtime"));
        assert!(ids.contains(&"apps"));
        assert!(ids.contains(&"recovery"));
    }

    #[test]
    fn every_topic_has_at_least_one_bullet() {
        for topic in help_topics() {
            assert!(
                !topic.bullets.is_empty(),
                "topic '{}' has no bullets",
                topic.id
            );
        }
    }

    #[test]
    fn serialises_to_expected_json_shape() {
        let topics = help_topics();
        let json = serde_json::to_value(topics).expect("serialisation must not fail");
        let first = &json[0];
        assert!(first.get("id").is_some(), "missing 'id' field");
        assert!(first.get("title").is_some(), "missing 'title' field");
        assert!(first.get("bullets").is_some(), "missing 'bullets' field");
        // camelCase — no snake_case keys should leak through
        assert!(first.get("approval_status").is_none());
    }
}
