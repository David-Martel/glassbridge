// scrcpy_protocol.rs — client-side scrcpy socket protocol for GlassBridge.
//
// This module implements the CLIENT side of the scrcpy wire protocol.  The
// scrcpy-server JAR runs on the Android device; we connect to its sockets over
// an ADB tunnel.
//
// Protocol reference (from vendor/scrcpy-src/app/src/demuxer.c):
//   Video socket:
//     1. Read codec header : codec_id(u32be) + width(u32be) + height(u32be) = 12 bytes
//     2. Per frame         : pts_flags(u64be) + packet_size(u32be) + <packet_size> bytes
//        - bit 63 of pts_flags = config packet flag
//        - bit 62 of pts_flags = keyframe flag
//        - bits 0–61 = PTS value
//
//   Control socket (client → device):
//     Per message: type_byte(u8) + payload (see ControlMessage variants).
//     All multi-byte integers are big-endian.
//
// All items are gated behind `#[cfg(feature = "screen-mirror")]`.

#![cfg(feature = "screen-mirror")]

use std::io::{self, Read, Write};
use std::net::TcpStream;

use thiserror::Error;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from the scrcpy client.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::scrcpy_protocol::ScrcpyError;
///
/// let e = ScrcpyError::InvalidMagic { got: 0xDEAD };
/// assert!(e.to_string().contains("magic"));
/// ```
#[derive(Debug, Error)]
pub enum ScrcpyError {
    /// The codec magic received from the server does not match a known codec.
    #[error("unexpected codec magic: 0x{got:08x}")]
    InvalidMagic { got: u32 },

    /// The server reported that the video stream was explicitly disabled.
    #[error("video stream disabled by device (codec_id=0)")]
    StreamDisabled,

    /// The server reported a configuration error.
    #[error("stream configuration error on device (codec_id=1)")]
    StreamConfigError,

    /// A packet with `packet_size == 0` was received, which the protocol
    /// considers invalid.
    #[error("received a packet with size=0")]
    ZeroSizePacket,

    /// The underlying TCP stream returned an error.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// A control message was too large to serialize.
    #[error("control message payload too large: {size} bytes")]
    PayloadTooLarge { size: usize },
}

impl ScrcpyError {
    /// Returns `true` when this is a plain I/O error (connection lost, etc.).
    pub fn is_io(&self) -> bool {
        matches!(self, Self::Io(_))
    }

    /// Returns `true` when the device side rejected or disabled the stream.
    pub fn is_stream_error(&self) -> bool {
        matches!(
            self,
            Self::StreamDisabled | Self::StreamConfigError | Self::InvalidMagic { .. }
        )
    }
}

// ---------------------------------------------------------------------------
// Codec IDs (ASCII 4-byte tags, big-endian u32)
// ---------------------------------------------------------------------------

/// Known codec identifiers sent by the scrcpy server in the video header.
///
/// Values are the ASCII bytes of the codec tag packed into a `u32`.
pub mod codec_id {
    /// H.264 / AVC (`"h264"` = `0x68323634`).
    pub const H264: u32 = 0x6832_3634;
    /// H.265 / HEVC (`"h265"` = `0x68323635`).
    pub const H265: u32 = 0x6832_3635;
    /// AV1 (`"av1\0"` = `0x0061_7631`).
    pub const AV1: u32 = 0x0061_7631;
}

// ---------------------------------------------------------------------------
// Packet flag constants (top two bits of the 64-bit PTS field)
// ---------------------------------------------------------------------------

const FLAG_CONFIG: u64 = 1 << 63;
const FLAG_KEYFRAME: u64 = 1 << 62;
const PTS_MASK: u64 = FLAG_KEYFRAME - 1;

// ---------------------------------------------------------------------------
// Wire data structures
// ---------------------------------------------------------------------------

/// Codec metadata received at the start of the video socket connection.
///
/// Sent once immediately after the connection is established.
#[derive(Debug, Clone)]
pub struct CodecInfo {
    /// Codec identifier (see [`codec_id`] constants).
    pub codec_id: u32,
    /// Initial display width in pixels.
    pub width: u32,
    /// Initial display height in pixels.
    pub height: u32,
}

/// A single video frame (or codec-config packet) read from the video socket.
///
/// Config packets carry SPS + PPS data in Annex-B format and must be
/// processed by the decoder before any data frame is decoded.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// `true` for codec-config packets (SPS/PPS), `false` for frame data.
    pub is_config: bool,
    /// `true` when this is an IDR / keyframe.
    pub is_keyframe: bool,
    /// Presentation timestamp, in microseconds (0 for config packets).
    pub pts: u64,
    /// Raw H.264/H.265/AV1 NAL data (Annex-B format for H.264).
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Control message types
// ---------------------------------------------------------------------------

/// Control messages sent from the client to the device.
///
/// The wire format is identical to the scrcpy C client (control_msg.c).
/// All integers are big-endian.  The first byte of every serialised message
/// is the type discriminant from the enum below.
#[derive(Debug, Clone)]
pub enum ControlMessage {
    /// Inject a key event.
    ///
    /// Wire: `[0x00] action(u8) keycode(u32) repeat(u32) metastate(u32)` = 14 bytes.
    InjectKeycode {
        /// `0` = down, `1` = up, `2` = multiple.
        action: u8,
        /// Android keycode value.
        keycode: u32,
        /// Repeat count.
        repeat: u32,
        /// Android meta-state bitmask.
        metastate: u32,
    },

    /// Inject text input (up to 300 UTF-8 bytes).
    ///
    /// Wire: `[0x01] length(u32) text(UTF-8)`.
    InjectText {
        /// Text string (will be truncated if longer than 300 bytes).
        text: String,
    },

    /// Inject a touch/pointer event.
    ///
    /// Wire: `[0x02] action(u8) pointer_id(u64) x(u32) y(u32)
    ///         screen_w(u16) screen_h(u16) pressure(u16)
    ///         action_button(u32) buttons(u32)` = 32 bytes.
    InjectTouch {
        /// Motion event action (down=0, up=1, move=2, …).
        action: u8,
        /// Pointer identifier (`u64::MAX` = mouse, `u64::MAX - 1` = generic finger).
        pointer_id: u64,
        /// X coordinate in screen pixels.
        x: u32,
        /// Y coordinate in screen pixels.
        y: u32,
        /// Physical screen width used to normalise coordinates.
        screen_width: u16,
        /// Physical screen height used to normalise coordinates.
        screen_height: u16,
        /// Pressure encoded as a 16-bit fixed-point value (0 = none, 0xFFFF = max).
        pressure: u16,
        /// Action button mask.
        action_button: u32,
        /// Current buttons bitmask.
        buttons: u32,
    },

    /// Inject a scroll event.
    ///
    /// Wire: `[0x03] x(u32) y(u32) screen_w(u16) screen_h(u16)
    ///         hscroll(i16) vscroll(i16) buttons(u32)` = 21 bytes.
    InjectScroll {
        /// X coordinate in screen pixels.
        x: u32,
        /// Y coordinate in screen pixels.
        y: u32,
        /// Physical screen width.
        screen_width: u16,
        /// Physical screen height.
        screen_height: u16,
        /// Horizontal scroll delta (normalised to −0x8000…0x7FFF).
        hscroll: i16,
        /// Vertical scroll delta (normalised to −0x8000…0x7FFF).
        vscroll: i16,
        /// Buttons bitmask.
        buttons: u32,
    },

    /// Send the Back key or turn the screen on.
    ///
    /// Wire: `[0x04] action(u8)` = 2 bytes.
    BackOrScreenOn {
        /// Key action: `0` = down, `1` = up.
        action: u8,
    },

    /// Expand the notification panel.
    ///
    /// Wire: `[0x05]` = 1 byte.
    ExpandNotificationPanel,

    /// Expand the quick settings panel.
    ///
    /// Wire: `[0x06]` = 1 byte.
    ExpandSettingsPanel,

    /// Collapse all expanded panels.
    ///
    /// Wire: `[0x07]` = 1 byte.
    CollapsePanels,

    /// Request device clipboard content.
    ///
    /// Wire: `[0x08] copy_key(u8)` = 2 bytes.
    GetClipboard {
        /// Copy-key hint: `0` = none, `1` = copy, `2` = cut.
        copy_key: u8,
    },

    /// Push text into device clipboard, optionally paste it.
    ///
    /// Wire: `[0x09] sequence(u64) paste(u8) length(u32) text(UTF-8)`.
    SetClipboard {
        /// Monotonically increasing sequence number for deduplication.
        sequence: u64,
        /// If `true`, paste the text immediately after setting.
        paste: bool,
        /// Clipboard text (will be truncated to fit in the max message size).
        text: String,
    },

    /// Turn the display on or off.
    ///
    /// Wire: `[0x0a] on(u8)` = 2 bytes.
    SetDisplayPower {
        /// `true` = power on, `false` = power off.
        on: bool,
    },

    /// Rotate the device display.
    ///
    /// Wire: `[0x0b]` = 1 byte.
    RotateDevice,
}

/// Discriminant byte values matching the scrcpy C enum `sc_control_msg_type`.
mod msg_type {
    pub const INJECT_KEYCODE: u8 = 0x00;
    pub const INJECT_TEXT: u8 = 0x01;
    pub const INJECT_TOUCH: u8 = 0x02;
    pub const INJECT_SCROLL: u8 = 0x03;
    pub const BACK_OR_SCREEN_ON: u8 = 0x04;
    pub const EXPAND_NOTIFICATION: u8 = 0x05;
    pub const EXPAND_SETTINGS: u8 = 0x06;
    pub const COLLAPSE_PANELS: u8 = 0x07;
    pub const GET_CLIPBOARD: u8 = 0x08;
    pub const SET_CLIPBOARD: u8 = 0x09;
    pub const SET_DISPLAY_POWER: u8 = 0x0a;
    pub const ROTATE_DEVICE: u8 = 0x0b;
}

// Maximum length for InjectText payload (matches SC_CONTROL_MSG_INJECT_TEXT_MAX_LENGTH).
const INJECT_TEXT_MAX_BYTES: usize = 300;
// Maximum clipboard text size (fits within SC_CONTROL_MSG_MAX_SIZE = 256 KiB).
const SET_CLIPBOARD_MAX_BYTES: usize = (1 << 18) - 14;

impl ControlMessage {
    /// Serialise this message into a `Vec<u8>` using the scrcpy wire format.
    ///
    /// # Errors
    ///
    /// Returns [`ScrcpyError::PayloadTooLarge`] if the serialised form exceeds
    /// the maximum allowed message size.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "screen-mirror")] {
    /// use glassbridge_desktop::scrcpy_protocol::ControlMessage;
    ///
    /// let msg = ControlMessage::ExpandNotificationPanel;
    /// let bytes = msg.serialize().unwrap();
    /// assert_eq!(bytes, &[0x05]);
    /// # }
    /// ```
    pub fn serialize(&self) -> Result<Vec<u8>, ScrcpyError> {
        match self {
            Self::InjectKeycode {
                action,
                keycode,
                repeat,
                metastate,
            } => {
                let mut buf = Vec::with_capacity(14);
                buf.push(msg_type::INJECT_KEYCODE);
                buf.push(*action);
                buf.extend_from_slice(&keycode.to_be_bytes());
                buf.extend_from_slice(&repeat.to_be_bytes());
                buf.extend_from_slice(&metastate.to_be_bytes());
                Ok(buf)
            }

            Self::InjectText { text } => {
                let payload = truncate_utf8(text.as_bytes(), INJECT_TEXT_MAX_BYTES);
                let mut buf = Vec::with_capacity(1 + 4 + payload.len());
                buf.push(msg_type::INJECT_TEXT);
                buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                buf.extend_from_slice(payload);
                Ok(buf)
            }

            Self::InjectTouch {
                action,
                pointer_id,
                x,
                y,
                screen_width,
                screen_height,
                pressure,
                action_button,
                buttons,
            } => {
                let mut buf = Vec::with_capacity(32);
                buf.push(msg_type::INJECT_TOUCH);
                buf.push(*action);
                buf.extend_from_slice(&pointer_id.to_be_bytes());
                buf.extend_from_slice(&x.to_be_bytes());
                buf.extend_from_slice(&y.to_be_bytes());
                buf.extend_from_slice(&screen_width.to_be_bytes());
                buf.extend_from_slice(&screen_height.to_be_bytes());
                buf.extend_from_slice(&pressure.to_be_bytes());
                buf.extend_from_slice(&action_button.to_be_bytes());
                buf.extend_from_slice(&buttons.to_be_bytes());
                Ok(buf)
            }

            Self::InjectScroll {
                x,
                y,
                screen_width,
                screen_height,
                hscroll,
                vscroll,
                buttons,
            } => {
                let mut buf = Vec::with_capacity(21);
                buf.push(msg_type::INJECT_SCROLL);
                buf.extend_from_slice(&x.to_be_bytes());
                buf.extend_from_slice(&y.to_be_bytes());
                buf.extend_from_slice(&screen_width.to_be_bytes());
                buf.extend_from_slice(&screen_height.to_be_bytes());
                buf.extend_from_slice(&hscroll.to_be_bytes());
                buf.extend_from_slice(&vscroll.to_be_bytes());
                buf.extend_from_slice(&buttons.to_be_bytes());
                Ok(buf)
            }

            Self::BackOrScreenOn { action } => Ok(vec![msg_type::BACK_OR_SCREEN_ON, *action]),

            Self::ExpandNotificationPanel => Ok(vec![msg_type::EXPAND_NOTIFICATION]),

            Self::ExpandSettingsPanel => Ok(vec![msg_type::EXPAND_SETTINGS]),

            Self::CollapsePanels => Ok(vec![msg_type::COLLAPSE_PANELS]),

            Self::GetClipboard { copy_key } => Ok(vec![msg_type::GET_CLIPBOARD, *copy_key]),

            Self::SetClipboard {
                sequence,
                paste,
                text,
            } => {
                let payload = truncate_utf8(text.as_bytes(), SET_CLIPBOARD_MAX_BYTES);
                let mut buf = Vec::with_capacity(1 + 8 + 1 + 4 + payload.len());
                buf.push(msg_type::SET_CLIPBOARD);
                buf.extend_from_slice(&sequence.to_be_bytes());
                buf.push(*paste as u8);
                buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                buf.extend_from_slice(payload);
                Ok(buf)
            }

            Self::SetDisplayPower { on } => Ok(vec![msg_type::SET_DISPLAY_POWER, *on as u8]),

            Self::RotateDevice => Ok(vec![msg_type::ROTATE_DEVICE]),
        }
    }
}

// ---------------------------------------------------------------------------
// ScrcpyClient
// ---------------------------------------------------------------------------

/// Client for the scrcpy device-mirroring server.
///
/// Manages connections to the video and control sockets exposed by the scrcpy
/// server JAR running on the Android device.
///
/// # Lifecycle
///
/// 1. Push the server JAR to the device and start it via ADB.
/// 2. Call [`ScrcpyClient::connect_sockets`] to open the TCP connections over
///    an ADB forward tunnel.
/// 3. Call [`ScrcpyClient::read_codec_info`] once to receive the stream header.
/// 4. Loop calling [`ScrcpyClient::read_video_frame`] for decoded packets.
/// 5. Call [`ScrcpyClient::send_control`] to inject input events.
/// 6. Call [`ScrcpyClient::disconnect`] to close everything cleanly.
///
/// # Examples
///
/// ```rust,no_run
/// # #[cfg(feature = "screen-mirror")] {
/// use glassbridge_desktop::scrcpy_protocol::ScrcpyClient;
///
/// let mut client = ScrcpyClient::new("19281FDEE000GS");
/// // client.connect_sockets(video_addr, control_addr)?;
/// # }
/// ```
pub struct ScrcpyClient {
    /// ADB serial of the target device.
    serial: String,
    /// Connected video socket.
    video_socket: Option<TcpStream>,
    /// Connected control socket.
    control_socket: Option<TcpStream>,
    /// Codec info received after opening the video socket.
    codec_info: Option<CodecInfo>,
}

impl ScrcpyClient {
    /// Create a new client for the device identified by `serial`.
    ///
    /// Sockets are not opened until [`connect_sockets`](Self::connect_sockets)
    /// is called.
    pub fn new(serial: impl Into<String>) -> Self {
        Self {
            serial: serial.into(),
            video_socket: None,
            control_socket: None,
            codec_info: None,
        }
    }

    /// Return the device serial this client was constructed for.
    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// Return a reference to the most recently read [`CodecInfo`], if any.
    pub fn codec_info(&self) -> Option<&CodecInfo> {
        self.codec_info.as_ref()
    }

    /// Connect the video and control sockets to the scrcpy server.
    ///
    /// `video_addr` and `control_addr` are `"host:port"` strings for the
    /// ADB-forwarded TCP ports.
    ///
    /// # Errors
    ///
    /// Returns [`ScrcpyError::Io`] if either connection fails.
    pub fn connect_sockets(
        &mut self,
        video_addr: &str,
        control_addr: &str,
    ) -> Result<(), ScrcpyError> {
        info!(serial = %self.serial, video = %video_addr, control = %control_addr, "connecting scrcpy sockets");
        let video = TcpStream::connect(video_addr)?;
        let control = TcpStream::connect(control_addr)?;
        self.video_socket = Some(video);
        self.control_socket = Some(control);
        Ok(())
    }

    /// Read the codec metadata header from the video socket.
    ///
    /// Must be called exactly once after [`connect_sockets`](Self::connect_sockets).
    /// The result is cached and accessible via [`codec_info`](Self::codec_info).
    ///
    /// # Errors
    ///
    /// - [`ScrcpyError::StreamDisabled`] if the device reports codec_id = 0.
    /// - [`ScrcpyError::StreamConfigError`] if the device reports codec_id = 1.
    /// - [`ScrcpyError::InvalidMagic`] for any unrecognised codec.
    /// - [`ScrcpyError::Io`] on connection errors.
    pub fn read_codec_info(&mut self) -> Result<&CodecInfo, ScrcpyError> {
        let socket = self
            .video_socket
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "video socket not open"))?;

        let codec_id = read_u32_be(socket)?;

        match codec_id {
            0 => return Err(ScrcpyError::StreamDisabled),
            1 => return Err(ScrcpyError::StreamConfigError),
            codec_id::H264 | codec_id::H265 | codec_id::AV1 => {}
            other => return Err(ScrcpyError::InvalidMagic { got: other }),
        }

        let width = read_u32_be(socket)?;
        let height = read_u32_be(socket)?;

        debug!(codec_id, width, height, "received codec info");

        self.codec_info = Some(CodecInfo {
            codec_id,
            width,
            height,
        });

        Ok(self.codec_info.as_ref().expect("just set"))
    }

    /// Read one video packet from the video socket.
    ///
    /// Blocks until a complete packet is available.  Returns `None` when the
    /// server signals end-of-stream (the connection was closed).
    ///
    /// # Errors
    ///
    /// - [`ScrcpyError::ZeroSizePacket`] if the server sends an empty packet.
    /// - [`ScrcpyError::Io`] on connection errors.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # #[cfg(feature = "screen-mirror")] {
    /// use glassbridge_desktop::scrcpy_protocol::ScrcpyClient;
    ///
    /// let mut client = ScrcpyClient::new("serial");
    /// // After connect + read_codec_info:
    /// // while let Ok(Some(frame)) = client.read_video_frame() { ... }
    /// # }
    /// ```
    pub fn read_video_frame(&mut self) -> Result<Option<VideoFrame>, ScrcpyError> {
        let socket = self
            .video_socket
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "video socket not open"))?;

        // Read the 12-byte packet header.
        let mut header = [0u8; 12];
        match read_exact_or_eof(socket, &mut header)? {
            ReadResult::Eof => return Ok(None),
            ReadResult::Ok => {}
        }

        let pts_flags = u64::from_be_bytes(header[0..8].try_into().expect("slice is 8 bytes"));
        let packet_size = u32::from_be_bytes(header[8..12].try_into().expect("slice is 4 bytes"));

        if packet_size == 0 {
            return Err(ScrcpyError::ZeroSizePacket);
        }

        let is_config = (pts_flags & FLAG_CONFIG) != 0;
        let is_keyframe = (pts_flags & FLAG_KEYFRAME) != 0;
        let pts = if is_config { 0 } else { pts_flags & PTS_MASK };

        // Read the NAL data.
        let mut data = vec![0u8; packet_size as usize];
        socket.read_exact(&mut data)?;

        debug!(
            is_config,
            is_keyframe,
            pts,
            size = packet_size,
            "received video frame"
        );

        Ok(Some(VideoFrame {
            is_config,
            is_keyframe,
            pts,
            data,
        }))
    }

    /// Serialise and send a [`ControlMessage`] to the device.
    ///
    /// # Errors
    ///
    /// - [`ScrcpyError::PayloadTooLarge`] if the message serialises to more
    ///   than the maximum allowed size.
    /// - [`ScrcpyError::Io`] on connection errors.
    pub fn send_control(&mut self, msg: ControlMessage) -> Result<(), ScrcpyError> {
        let socket = self.control_socket.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "control socket not open")
        })?;

        let bytes = msg.serialize()?;
        socket.write_all(&bytes)?;
        socket.flush()?;
        Ok(())
    }

    /// Close both sockets and release resources.
    ///
    /// It is safe to call this more than once.
    pub fn disconnect(&mut self) {
        if let Some(s) = self.video_socket.take() {
            if let Err(e) = s.shutdown(std::net::Shutdown::Both) {
                warn!("error shutting down video socket: {e}");
            }
        }
        if let Some(s) = self.control_socket.take() {
            if let Err(e) = s.shutdown(std::net::Shutdown::Both) {
                warn!("error shutting down control socket: {e}");
            }
        }
        self.codec_info = None;
        info!(serial = %self.serial, "scrcpy client disconnected");
    }
}

impl Drop for ScrcpyClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}

// ---------------------------------------------------------------------------
// Internal I/O helpers
// ---------------------------------------------------------------------------

enum ReadResult {
    Ok,
    Eof,
}

/// Read exactly `buf.len()` bytes, returning `ReadResult::Eof` if zero bytes
/// were available (clean EOF from the server), or propagating any other I/O error.
fn read_exact_or_eof(reader: &mut TcpStream, buf: &mut [u8]) -> Result<ReadResult, ScrcpyError> {
    use std::io::ErrorKind;
    let mut total = 0usize;
    while total < buf.len() {
        match reader.read(&mut buf[total..]) {
            Ok(0) if total == 0 => return Ok(ReadResult::Eof),
            Ok(0) => {
                return Err(ScrcpyError::Io(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "connection closed mid-packet",
                )))
            }
            Ok(n) => total += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(ScrcpyError::Io(e)),
        }
    }
    Ok(ReadResult::Ok)
}

/// Read a big-endian `u32` from `reader`.
fn read_u32_be(reader: &mut TcpStream) -> Result<u32, ScrcpyError> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_be_bytes(buf))
}

/// Return the longest valid UTF-8 prefix of `bytes` that fits within `max_len` bytes.
fn truncate_utf8(bytes: &[u8], max_len: usize) -> &[u8] {
    if bytes.len() <= max_len {
        return bytes;
    }
    // Walk back from max_len until we land on a UTF-8 character boundary.
    let mut end = max_len;
    while end > 0 && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    &bytes[..end]
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    // -- ControlMessage serialisation ----------------------------------------

    #[test]
    fn expand_notification_is_one_byte() {
        let bytes = ControlMessage::ExpandNotificationPanel.serialize().unwrap();
        assert_eq!(bytes, &[0x05]);
    }

    #[test]
    fn expand_settings_is_one_byte() {
        let bytes = ControlMessage::ExpandSettingsPanel.serialize().unwrap();
        assert_eq!(bytes, &[0x06]);
    }

    #[test]
    fn collapse_panels_is_one_byte() {
        let bytes = ControlMessage::CollapsePanels.serialize().unwrap();
        assert_eq!(bytes, &[0x07]);
    }

    #[test]
    fn rotate_device_is_one_byte() {
        let bytes = ControlMessage::RotateDevice.serialize().unwrap();
        assert_eq!(bytes, &[0x0b]);
    }

    #[test]
    fn back_or_screen_on_encodes_action() {
        let bytes = ControlMessage::BackOrScreenOn { action: 0 }
            .serialize()
            .unwrap();
        assert_eq!(bytes.len(), 2);
        assert_eq!(bytes[0], msg_type::BACK_OR_SCREEN_ON);
        assert_eq!(bytes[1], 0);
    }

    #[test]
    fn set_display_power_on() {
        let bytes = ControlMessage::SetDisplayPower { on: true }
            .serialize()
            .unwrap();
        assert_eq!(bytes, &[0x0a, 0x01]);
    }

    #[test]
    fn set_display_power_off() {
        let bytes = ControlMessage::SetDisplayPower { on: false }
            .serialize()
            .unwrap();
        assert_eq!(bytes, &[0x0a, 0x00]);
    }

    #[test]
    fn get_clipboard_copy_key_encoded() {
        let bytes = ControlMessage::GetClipboard { copy_key: 1 }
            .serialize()
            .unwrap();
        assert_eq!(bytes, &[0x08, 0x01]);
    }

    #[test]
    fn inject_keycode_is_14_bytes() {
        let bytes = ControlMessage::InjectKeycode {
            action: 0,
            keycode: 4, // KEYCODE_BACK
            repeat: 0,
            metastate: 0,
        }
        .serialize()
        .unwrap();
        assert_eq!(bytes.len(), 14);
        assert_eq!(bytes[0], msg_type::INJECT_KEYCODE);
        assert_eq!(bytes[1], 0); // action = down
                                 // keycode at [2..6] big-endian
        assert_eq!(u32::from_be_bytes(bytes[2..6].try_into().unwrap()), 4u32);
    }

    #[test]
    fn inject_text_empty_string() {
        let bytes = ControlMessage::InjectText {
            text: String::new(),
        }
        .serialize()
        .unwrap();
        assert_eq!(bytes[0], msg_type::INJECT_TEXT);
        // 4-byte length field = 0
        assert_eq!(&bytes[1..5], &[0, 0, 0, 0]);
        assert_eq!(bytes.len(), 5);
    }

    #[test]
    fn inject_text_hello() {
        let bytes = ControlMessage::InjectText {
            text: "hello".into(),
        }
        .serialize()
        .unwrap();
        assert_eq!(bytes[0], msg_type::INJECT_TEXT);
        assert_eq!(u32::from_be_bytes(bytes[1..5].try_into().unwrap()), 5u32);
        assert_eq!(&bytes[5..], b"hello");
    }

    #[test]
    fn inject_text_truncates_beyond_300_bytes() {
        let long_text = "a".repeat(500);
        let bytes = ControlMessage::InjectText { text: long_text }
            .serialize()
            .unwrap();
        let payload_len = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
        assert!(
            payload_len <= INJECT_TEXT_MAX_BYTES,
            "payload must be ≤ 300 bytes"
        );
    }

    #[test]
    fn inject_touch_is_32_bytes() {
        let bytes = ControlMessage::InjectTouch {
            action: 0,
            pointer_id: u64::MAX, // mouse pointer
            x: 100,
            y: 200,
            screen_width: 1080,
            screen_height: 2400,
            pressure: 0xFFFF,
            action_button: 0,
            buttons: 1,
        }
        .serialize()
        .unwrap();
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes[0], msg_type::INJECT_TOUCH);
        // pointer_id = u64::MAX → 8 bytes of 0xFF at [2..10]
        assert!(bytes[2..10].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn inject_scroll_is_21_bytes() {
        let bytes = ControlMessage::InjectScroll {
            x: 540,
            y: 1200,
            screen_width: 1080,
            screen_height: 2400,
            hscroll: 0,
            vscroll: -1000,
            buttons: 0,
        }
        .serialize()
        .unwrap();
        assert_eq!(bytes.len(), 21);
        assert_eq!(bytes[0], msg_type::INJECT_SCROLL);
    }

    #[test]
    fn set_clipboard_encodes_sequence_and_paste() {
        let bytes = ControlMessage::SetClipboard {
            sequence: 42,
            paste: true,
            text: "test".into(),
        }
        .serialize()
        .unwrap();
        assert_eq!(bytes[0], msg_type::SET_CLIPBOARD);
        assert_eq!(u64::from_be_bytes(bytes[1..9].try_into().unwrap()), 42u64);
        assert_eq!(bytes[9], 1); // paste = true
    }

    // -- Packet flag parsing -------------------------------------------------

    /// Simulate parsing the 8-byte PTS+flags field as the real demuxer does.
    fn parse_pts_flags(raw: u64) -> (bool, bool, u64) {
        let is_config = (raw & FLAG_CONFIG) != 0;
        let is_keyframe = (raw & FLAG_KEYFRAME) != 0;
        let pts = if is_config { 0 } else { raw & PTS_MASK };
        (is_config, is_keyframe, pts)
    }

    #[test]
    fn config_flag_bit63_detected() {
        let raw: u64 = FLAG_CONFIG | 12345;
        let (is_config, is_keyframe, pts) = parse_pts_flags(raw);
        assert!(is_config);
        assert!(!is_keyframe);
        assert_eq!(pts, 0, "config packets have pts=0");
    }

    #[test]
    fn keyframe_flag_bit62_detected() {
        let raw: u64 = FLAG_KEYFRAME | 99_000;
        let (is_config, is_keyframe, pts) = parse_pts_flags(raw);
        assert!(!is_config);
        assert!(is_keyframe);
        assert_eq!(pts, 99_000);
    }

    #[test]
    fn pts_mask_strips_flag_bits() {
        // PTS_MASK should be all bits below bit 62.
        assert_eq!(PTS_MASK, 0x3FFF_FFFF_FFFF_FFFF);
    }

    #[test]
    fn pts_preserved_without_flags() {
        let expected_pts: u64 = 1_234_567_890;
        let raw: u64 = expected_pts; // no flags set
        let (is_config, is_keyframe, pts) = parse_pts_flags(raw);
        assert!(!is_config);
        assert!(!is_keyframe);
        assert_eq!(pts, expected_pts);
    }

    // -- Codec IDs -----------------------------------------------------------

    #[test]
    fn codec_ids_are_ascii_tags() {
        assert_eq!(&codec_id::H264.to_be_bytes(), b"h264");
        assert_eq!(&codec_id::H265.to_be_bytes(), b"h265");
        // AV1 has a leading null byte
        assert_eq!(&codec_id::AV1.to_be_bytes()[1..], b"av1");
    }

    // -- truncate_utf8 -------------------------------------------------------

    #[test]
    fn truncate_utf8_short_string_unchanged() {
        let s = b"hello";
        assert_eq!(truncate_utf8(s, 100), s);
    }

    #[test]
    fn truncate_utf8_truncates_at_boundary() {
        // "é" is two UTF-8 bytes: 0xC3 0xA9
        let s = "éé".as_bytes(); // 4 bytes total
                                 // max_len=3 would split "é" — must step back to 2
        let result = truncate_utf8(s, 3);
        assert!(
            std::str::from_utf8(result).is_ok(),
            "result must be valid UTF-8"
        );
        assert_eq!(result.len(), 2); // only the first "é"
    }

    // -- ScrcpyError helpers -------------------------------------------------

    #[test]
    fn is_io_distinguishes_variant() {
        let e = ScrcpyError::Io(io::Error::new(io::ErrorKind::BrokenPipe, "pipe"));
        assert!(e.is_io());
        assert!(!e.is_stream_error());
    }

    #[test]
    fn is_stream_error_covers_all_stream_variants() {
        assert!(ScrcpyError::StreamDisabled.is_stream_error());
        assert!(ScrcpyError::StreamConfigError.is_stream_error());
        assert!(ScrcpyError::InvalidMagic { got: 0 }.is_stream_error());
    }
}
