//! The three things that travel over the control socket, plus the on-disk
//! clip metadata that rides inside several of them.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Longest accepted protocol line, including the newline. A client that opens
/// the pipe and never sends a `\n` must not be able to grow the daemon's read
/// buffer without bound.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Response id used when a line was too malformed to recover its real id.
/// Clients must not send requests with this id.
pub const RESERVED_ID: u64 = 0;

/// A request as it arrives on the wire. Arguments are flattened alongside
/// `id` and `cmd`: `{"id":4,"cmd":"library.list","offset":0,"limit":50}`.
///
/// Untyped on purpose — see [`crate::Command::parse`]. An unknown `cmd` has to
/// produce an error *response*, and that needs the id, so the id is recovered
/// before anything can fail on the command itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub cmd: String,
    #[serde(flatten)]
    pub args: Map<String, Value>,
}

/// Exactly one per request. `data` and `error` are mutually exclusive and the
/// unused one is omitted entirely rather than sent as null.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: u64, data: Value) -> Self {
        Self { id, ok: true, data: Some(data), error: None }
    }

    pub fn err(id: u64, error: impl Into<String>) -> Self {
        Self { id, ok: false, data: None, error: Some(error.into()) }
    }
}

/// Unsolicited. Never carries an id — an event is not a reply to anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event: String,
    pub data: Value,
}

impl Event {
    pub fn new(event: impl Into<String>, data: Value) -> Self {
        Self { event: event.into(), data }
    }
}

/// One clip's metadata: the `.json` sidecar's contents verbatim, and the
/// payload of `clip_saved` and `library.list`.
///
/// Field order here is the field order on disk — keep it matching the spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipMeta {
    pub id: String,
    pub title: String,
    /// RFC 3339 with a local UTC offset, e.g. `2026-07-26T14:30:12+03:00`.
    pub created: String,
    pub duration_ms: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub encoder: String,
    pub has_audio: bool,
    #[serde(default)]
    pub favorite: bool,
}

/// One screenshot's metadata: the payload of `shot_saved` and `shots.list`.
///
/// There is no sidecar on disk for a screenshot, unlike [`ClipMeta`]. Every
/// field here is recovered from the file itself — the id from the filename,
/// `created` from the id, `bytes` from the directory entry, and the dimensions
/// from the JPEG's own header. Nothing can therefore drift out of sync with
/// the image, because nothing is stored twice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShotMeta {
    pub id: String,
    /// `2026-08-27T14:30:12` — **no UTC offset is claimed.** The offset in
    /// force when the shot was taken is not recoverable from a filename, and
    /// stamping today's offset onto a screenshot from the other side of a DST
    /// change would be wrong twice a year. `library::created_from_id` already
    /// made this call for adopted clips; this reuses it rather than inventing
    /// a second derivation.
    pub created: String,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
}

/// Serializes one message and appends its newline.
///
/// Returns `Result` rather than panicking on purpose: the workspace builds
/// with `panic = "abort"`, so an unwrap here would take an armed replay ring
/// down with it.
pub fn encode_line<T: Serialize>(value: &T) -> serde_json::Result<String> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line)
}

/// Parses one line into a request. `Err` means the line was not a request
/// object at all and no id could be recovered; the caller answers with
/// [`RESERVED_ID`].
pub fn decode_request(line: &str) -> Result<Request, String> {
    serde_json::from_str(line).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes from spec §4.2. If these drift, every third-party UI
    /// written against the published spec breaks silently.
    #[test]
    fn response_matches_the_spec_examples_byte_for_byte() {
        let ok = Response::ok(7, serde_json::json!({"armed": true, "encoder": "NVENC H.264"}));
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#"{"id":7,"ok":true,"data":{"armed":true,"encoder":"NVENC H.264"}}"#
        );

        let err = Response::err(7, "no hardware encoder available");
        assert_eq!(
            serde_json::to_string(&err).unwrap(),
            r#"{"id":7,"ok":false,"error":"no hardware encoder available"}"#,
            "the data key must be omitted on failure, not serialized as null"
        );
    }

    /// The wire names third-party UIs read. Spec §3.2 says a UI in any language
    /// reimplements these types and is in no way second-class, so renaming a field
    /// here breaks software this repository cannot see.
    #[test]
    fn shot_meta_round_trips_and_pins_every_field_name() {
        let meta = ShotMeta {
            id: "20260827_143012".into(),
            created: "2026-08-27T14:30:12".into(),
            bytes: 412_003,
            width: 1920,
            height: 1200,
        };
        let json = serde_json::to_string(&meta).expect("serialize");
        assert_eq!(
            json,
            r#"{"id":"20260827_143012","created":"2026-08-27T14:30:12","bytes":412003,"width":1920,"height":1200}"#
        );
        assert_eq!(serde_json::from_str::<ShotMeta>(&json).expect("deserialize"), meta);
    }
}
