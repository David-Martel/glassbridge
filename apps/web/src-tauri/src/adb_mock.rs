// adb_mock.rs — mock ADB response fixtures for Rust integration tests.
#![allow(dead_code)]
//
// When the native Rust backend lands it will shell out to `adb`.  These
// fixtures let tests verify parsing and routing logic without a real device.
//
// Usage:
//   use crate::adb_mock::{DEVICES_RESPONSE_ONE_PIXEL, DEVICES_RESPONSE_EMPTY};

/// `adb devices -l` output with one authorised Pixel 10 XL.
pub const DEVICES_RESPONSE_ONE_PIXEL: &str = "\
List of devices attached
19281FDEE000GS\t device transport_id:1 usb:1-1 product:mustang model:Pixel_10_XL device:mustang
";

/// `adb devices -l` output with no authorised devices.
pub const DEVICES_RESPONSE_EMPTY: &str = "List of devices attached\n";

/// `adb devices -l` output with one device in the "unauthorized" state.
pub const DEVICES_RESPONSE_UNAUTHORIZED: &str = "\
List of devices attached
19281FDEE000GS\t unauthorized transport_id:1 usb:1-1
";

/// `adb devices -l` output with two devices (Pixel 10 XL + a secondary device).
pub const DEVICES_RESPONSE_TWO: &str = "\
List of devices attached
19281FDEE000GS\t device transport_id:1 usb:1-1 product:mustang model:Pixel_10_XL device:mustang
emulator-5554\t device transport_id:2 product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 device:emu64xa
";

/// Minimal valid PNG (1×1 pixel, 4-bit grayscale + alpha).
pub const SCREENSHOT_PNG_BYTES: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // PNG signature
    0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, // IHDR length + type
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // width=1, height=1
    0x08, 0x04, 0x00, 0x00, 0x00, 0xb5, 0x1c, 0x0c, 0x02, // bit depth, color type, etc.
    0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, // IDAT length + type
    0x78, 0xda, 0x63, 0xfc, 0xff, 0x1f, 0x00, 0x03, 0x03, 0x02, 0x00, 0xef, 0x4f, 0xc5,
    0xd3, // IDAT CRC
    0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82, // IEND
];

// ---------------------------------------------------------------------------
// Parsing tests — validate the ADB output parser against fixtures
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    type DeviceMeta = (String, String, Vec<(String, String)>);

    /// Parse a line from `adb devices -l` into (serial, state, metadata pairs).
    fn parse_device_line(line: &str) -> Option<DeviceMeta> {
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        if parts.len() < 2 {
            return None;
        }
        let serial = parts[0].trim().to_string();
        let rest: Vec<&str> = parts[1].split_whitespace().collect();
        if rest.is_empty() {
            return None;
        }
        let state = rest[0].to_string();
        let meta = rest[1..]
            .iter()
            .filter(|s| s.contains(':'))
            .map(|s| {
                let mut kv = s.splitn(2, ':');
                (
                    kv.next().unwrap_or("").to_string(),
                    kv.next().unwrap_or("").to_string(),
                )
            })
            .collect();
        Some((serial, state, meta))
    }

    fn authorized_devices(output: &str) -> Vec<DeviceMeta> {
        output
            .lines()
            .skip(1) // skip "List of devices attached"
            .filter_map(|line| parse_device_line(line.trim()))
            .filter(|(_, state, _)| state == "device")
            .collect()
    }

    #[test]
    fn parses_one_pixel_device() {
        let devices = authorized_devices(DEVICES_RESPONSE_ONE_PIXEL);
        assert_eq!(devices.len(), 1);
        let (serial, state, meta) = &devices[0];
        assert_eq!(serial, "19281FDEE000GS");
        assert_eq!(state, "device");

        let model = meta
            .iter()
            .find(|(k, _)| k == "model")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        assert_eq!(model, "Pixel_10_XL");
    }

    #[test]
    fn parses_empty_device_list() {
        let devices = authorized_devices(DEVICES_RESPONSE_EMPTY);
        assert_eq!(devices.len(), 0);
    }

    #[test]
    fn ignores_unauthorized_devices() {
        let devices = authorized_devices(DEVICES_RESPONSE_UNAUTHORIZED);
        assert_eq!(
            devices.len(),
            0,
            "Unauthorized device must not appear in authorized list"
        );
    }

    #[test]
    fn parses_two_devices() {
        let devices = authorized_devices(DEVICES_RESPONSE_TWO);
        assert_eq!(devices.len(), 2);
        let serials: Vec<&str> = devices.iter().map(|(s, _, _)| s.as_str()).collect();
        assert!(serials.contains(&"19281FDEE000GS"));
        assert!(serials.contains(&"emulator-5554"));
    }

    #[test]
    fn screenshot_fixture_is_valid_png() {
        // PNG magic bytes: 0x89 P N G \r \n \x1a \n
        let sig = &SCREENSHOT_PNG_BYTES[..8];
        assert_eq!(
            sig,
            &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
            "fixture must begin with valid PNG signature"
        );
    }
}
