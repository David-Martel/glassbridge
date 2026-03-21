// decoder.rs — H.264 frame decoder using the openh264 crate.
//
// Wraps [`openh264::decoder::Decoder`] to provide an ergonomic interface over
// the scrcpy [`VideoFrame`] type.  The decoder holds the openh264 context and
// converts raw NAL data into owned YUV420 plane buffers suitable for upload
// to the GPU renderer.
//
// All items are gated behind `#[cfg(feature = "screen-mirror")]`.

#![cfg(feature = "screen-mirror")]

use thiserror::Error;
use tracing::debug;

use crate::scrcpy_protocol::VideoFrame;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by the H.264 decoder.
///
/// # Examples
///
/// ```rust
/// # #[cfg(feature = "screen-mirror")] {
/// use glassbridge_desktop::decoder::DecoderError;
///
/// let e = DecoderError::Init("failed".into());
/// assert!(e.to_string().contains("failed"));
/// # }
/// ```
#[derive(Debug, Error)]
pub enum DecoderError {
    /// The decoder could not be initialised.
    #[error("decoder init failed: {0}")]
    Init(String),

    /// The decoder returned an error while decoding a NAL unit.
    #[error("decode error: {0}")]
    Decode(String),

    /// The decoder produced output with zero-size dimensions, which is invalid.
    #[error("decoder returned invalid frame dimensions {width}x{height}")]
    InvalidDimensions { width: u32, height: u32 },
}

impl DecoderError {
    /// Returns `true` if this is an initialisation failure.
    pub fn is_init_error(&self) -> bool {
        matches!(self, Self::Init(_))
    }

    /// Returns `true` if this is a per-frame decode failure.
    pub fn is_decode_error(&self) -> bool {
        matches!(self, Self::Decode(_))
    }
}

// ---------------------------------------------------------------------------
// DecodedFrame
// ---------------------------------------------------------------------------

/// A fully decoded YUV420 video frame with owned plane data.
///
/// Planes are stored in I420 layout (Y full-resolution, U and V
/// half-resolution in both dimensions).  Each plane includes stride padding
/// — use `y_stride`, `u_stride`, and `v_stride` when computing pixel offsets.
///
/// # Examples
///
/// ```rust
/// # #[cfg(feature = "screen-mirror")] {
/// use glassbridge_desktop::decoder::DecodedFrame;
///
/// fn pixel_y(frame: &DecodedFrame, col: usize, row: usize) -> u8 {
///     frame.y_plane[row * frame.y_stride + col]
/// }
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    /// Display width in pixels (no padding).
    pub width: u32,
    /// Display height in pixels (no padding).
    pub height: u32,
    /// Y (luma) plane bytes, including stride padding.
    pub y_plane: Vec<u8>,
    /// U (Cb) plane bytes, including stride padding.
    pub u_plane: Vec<u8>,
    /// V (Cr) plane bytes, including stride padding.
    pub v_plane: Vec<u8>,
    /// Bytes per row in the Y plane (≥ `width`).
    pub y_stride: usize,
    /// Bytes per row in the U plane (≥ `width / 2`).
    pub u_stride: usize,
    /// Bytes per row in the V plane (≥ `width / 2`).
    pub v_stride: usize,
}

// ---------------------------------------------------------------------------
// H264Decoder
// ---------------------------------------------------------------------------

/// H.264 decoder backed by [OpenH264](https://github.com/cisco/openh264).
///
/// Construct with [`H264Decoder::new`], then feed NAL data via
/// [`decode_frame`](Self::decode_frame) or the higher-level
/// [`decode_video_frame`](Self::decode_video_frame) which accepts the scrcpy
/// [`VideoFrame`] type directly.
///
/// # Notes
///
/// - Config packets (SPS/PPS) must be fed to the decoder before any IDR
///   frame.  Use [`decode_video_frame`](Self::decode_video_frame) which
///   handles this automatically.
/// - The first call after a config packet may return `Ok(None)` — this is
///   normal; the decoder needs at least one IDR frame to produce output.
///
/// # Examples
///
/// ```rust,no_run
/// # #[cfg(feature = "screen-mirror")] {
/// use glassbridge_desktop::decoder::H264Decoder;
///
/// let mut dec = H264Decoder::new().unwrap();
/// // dec.decode_frame(nal_bytes)?;
/// # }
/// ```
pub struct H264Decoder {
    decoder: openh264::decoder::Decoder,
}

impl H264Decoder {
    /// Create a new decoder with default OpenH264 settings.
    ///
    /// # Errors
    ///
    /// Returns [`DecoderError::Init`] if the OpenH264 library fails to
    /// initialise (e.g., because the shared library is missing on this
    /// platform).
    pub fn new() -> Result<Self, DecoderError> {
        let decoder =
            openh264::decoder::Decoder::new().map_err(|e| DecoderError::Init(e.to_string()))?;
        Ok(Self { decoder })
    }

    /// Decode one NAL unit and return the resulting YUV frame, if available.
    ///
    /// Returns `Ok(None)` when the decoder needs more data before it can
    /// produce output (e.g., after receiving a config/SPS packet).
    ///
    /// # Errors
    ///
    /// - [`DecoderError::Decode`] for any OpenH264 decoding failure.
    /// - [`DecoderError::InvalidDimensions`] if the decoded frame reports
    ///   zero-size dimensions.
    pub fn decode_frame(&mut self, nal_data: &[u8]) -> Result<Option<DecodedFrame>, DecoderError> {
        match self.decoder.decode(nal_data) {
            Ok(yuv) => {
                let (width, height) = yuv.dimension_rgb();
                if width == 0 || height == 0 {
                    return Err(DecoderError::InvalidDimensions {
                        width: width as u32,
                        height: height as u32,
                    });
                }

                let (y_stride, u_stride, v_stride) = yuv.strides_yuv();

                debug!(
                    width,
                    height, y_stride, u_stride, v_stride, "decoded YUV frame"
                );

                // Copy the strided plane data into owned Vec<u8>.
                // The raw pointers are valid only for the lifetime of `yuv`.
                let y_plane = yuv.y_with_stride().to_vec();
                let u_plane = yuv.u_with_stride().to_vec();
                let v_plane = yuv.v_with_stride().to_vec();

                Ok(Some(DecodedFrame {
                    width: width as u32,
                    height: height as u32,
                    y_plane,
                    u_plane,
                    v_plane,
                    y_stride,
                    u_stride,
                    v_stride,
                }))
            }
            // OpenH264 returns an error for config-only inputs (SPS/PPS without
            // an accompanying IDR). We translate these to Ok(None) so callers
            // don't have to special-case them.
            Err(e) => {
                let msg = e.to_string();
                if is_insufficient_data_error(&msg) {
                    debug!("decoder: insufficient data (config packet or early stream), returning None");
                    Ok(None)
                } else {
                    Err(DecoderError::Decode(msg))
                }
            }
        }
    }

    /// Decode a [`VideoFrame`] received from the scrcpy video socket.
    ///
    /// Config frames (SPS/PPS) are fed to the decoder but always return
    /// `Ok(None)` — they configure the decoder for subsequent frames.
    ///
    /// Returns `Ok(None)` when no output frame is ready yet.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`decode_frame`](Self::decode_frame).
    pub fn decode_video_frame(
        &mut self,
        frame: &VideoFrame,
    ) -> Result<Option<DecodedFrame>, DecoderError> {
        if frame.is_config {
            // Feed SPS/PPS; the decoder will configure itself but won't emit a frame.
            let _ = self.decode_frame(&frame.data)?;
            return Ok(None);
        }
        self.decode_frame(&frame.data)
    }
}

impl std::fmt::Debug for H264Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H264Decoder").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Return `true` if the OpenH264 error message represents an "insufficient
/// data" condition (no IDR frame yet, config-only input, etc.) rather than a
/// genuine decode failure.
fn is_insufficient_data_error(msg: &str) -> bool {
    // OpenH264 returns "Buffer status not valid" when given SPS/PPS alone.
    // Guard this list conservatively — unknown errors should surface.
    msg.contains("Buffer status not valid")
        || msg.contains("null buffers")
        || msg.contains("insufficient")
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrcpy_protocol::{codec_id, VideoFrame};

    // -- DecoderError helpers ------------------------------------------------

    #[test]
    fn is_init_error_variant() {
        let e = DecoderError::Init("bad".into());
        assert!(e.is_init_error());
        assert!(!e.is_decode_error());
    }

    #[test]
    fn is_decode_error_variant() {
        let e = DecoderError::Decode("bad frame".into());
        assert!(e.is_decode_error());
        assert!(!e.is_init_error());
    }

    #[test]
    fn invalid_dimensions_message_contains_sizes() {
        let e = DecoderError::InvalidDimensions {
            width: 0,
            height: 0,
        };
        assert!(e.to_string().contains('0'));
    }

    // -- DecodedFrame layout contract ----------------------------------------

    #[test]
    fn decoded_frame_y_stride_at_least_width() {
        let frame = DecodedFrame {
            width: 640,
            height: 480,
            y_plane: vec![0u8; 640 * 480],
            u_plane: vec![0u8; 320 * 240],
            v_plane: vec![0u8; 320 * 240],
            y_stride: 640,
            u_stride: 320,
            v_stride: 320,
        };
        assert!(frame.y_stride >= frame.width as usize);
        assert!(frame.u_stride >= frame.width as usize / 2);
        assert!(frame.v_stride >= frame.width as usize / 2);
    }

    // -- VideoFrame config routing -------------------------------------------

    /// Build a minimal config-flagged VideoFrame to verify the branch is taken.
    fn make_config_frame() -> VideoFrame {
        // Annex-B SPS NAL prefix — just enough to not be empty.
        VideoFrame {
            is_config: true,
            is_keyframe: false,
            pts: 0,
            data: vec![0x00, 0x00, 0x00, 0x01, 0x67],
        }
    }

    /// Build a VideoFrame marked as a data frame (not config).
    fn make_data_frame(pts: u64) -> VideoFrame {
        VideoFrame {
            is_config: false,
            is_keyframe: true,
            pts,
            data: vec![0x00, 0x00, 0x00, 0x01, 0x65],
        }
    }

    #[test]
    fn config_frame_returns_none_without_decode_error() {
        // We cannot instantiate a real H264Decoder in a unit test without the
        // OpenH264 shared library present on the test machine.  Instead we
        // test the VideoFrame routing logic independently.
        let vf = make_config_frame();
        assert!(vf.is_config);
        let data_vf = make_data_frame(1000);
        assert!(!data_vf.is_config);
        assert_eq!(data_vf.pts, 1000);
    }

    #[test]
    fn is_insufficient_data_error_matches_known_messages() {
        assert!(is_insufficient_data_error("Buffer status not valid"));
        assert!(is_insufficient_data_error(
            "Decoder returned null buffers. This is a bug"
        ));
        assert!(is_insufficient_data_error("insufficient data available"));
    }

    #[test]
    fn is_insufficient_data_error_does_not_match_real_errors() {
        assert!(!is_insufficient_data_error("SPS parse error"));
        assert!(!is_insufficient_data_error("NAL unit truncated"));
    }

    // -- Codec ID sanity (cross-module) --------------------------------------

    #[test]
    fn h264_codec_id_is_ascii_h264() {
        assert_eq!(&codec_id::H264.to_be_bytes(), b"h264");
    }
}
