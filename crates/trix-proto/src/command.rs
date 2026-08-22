//! The typed command set, parsed out of a [`Request`]'s loose argument bag.

use serde_json::Value;

use crate::message::Request;

/// Page size when `library.list` omits `limit`.
pub const DEFAULT_LIST_LIMIT: usize = 50;
/// Hard ceiling on `library.list`'s page size.
pub const MAX_LIST_LIMIT: usize = 500;

/// Every command the daemon answers.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Status,
    Arm,
    Disarm,
    Clip,
    ConfigGet,
    ConfigSet(serde_json::Map<String, Value>),
    LibraryList {
        offset: usize,
        limit: usize,
    },
    LibraryDelete {
        clip_id: String,
    },
    LibraryRename {
        clip_id: String,
        title: String,
    },
    LibraryFavorite {
        clip_id: String,
        favorite: bool,
    },
    LibraryReveal {
        clip_id: String,
    },
    /// Every keyframe position in a clip, in milliseconds. The trim bar draws
    /// a tick at each one and snaps the in-point to them (spec §6.3).
    LibraryKeyframes {
        clip_id: String,
    },
    /// Exports `start_ms..end_ms` of a clip as a **new clip in the library**,
    /// answering with its `ClipMeta`.
    ///
    /// No `dest`: the daemon allocates the id and the path, because an export
    /// that landed outside the clip directory would be invisible to every
    /// other `library.*` command. `mode` is on the wire so that `precise`
    /// slots in without a protocol change, but the daemon currently answers
    /// anything other than `fast` with an error.
    LibraryExport {
        clip_id: String,
        start_ms: u64,
        end_ms: u64,
        mode: String,
    },
    MonitorsList,
    EncodersList,
    StatsSubscribe {
        enabled: bool,
    },
    /// Opens the "change clips folder" dialog. Answers immediately for the
    /// same reason [`Self::SoundPick`] does, and the result arrives the same
    /// way: as a `config_changed` event carrying the new `clip_dir`.
    ///
    /// The tray has had this dialog all along; this is the settings page
    /// asking for the same one, so both end at the same handler.
    FolderPick,
    /// Opens the "choose a sound" dialog. Answers immediately: the dialog
    /// outlives the request by as long as the user takes to browse, and the
    /// result arrives as a `config_changed` event, not as this reply.
    SoundPick,
    /// Plays the configured clip sound once, so the user can hear what they
    /// just chose without saving a clip.
    SoundTest,
    /// Ends the daemon process. Answered before the process exits, so the
    /// caller can tell a clean shutdown from a crashed socket.
    Shutdown,
}

impl Command {
    /// `Err` is the text of the error response — it is shown to whoever is
    /// driving the socket, so it names the command and the missing argument.
    pub fn parse(req: &Request) -> Result<Self, String> {
        let cmd = req.cmd.as_str();
        Ok(match cmd {
            "status" => Self::Status,
            "arm" => Self::Arm,
            "disarm" => Self::Disarm,
            "clip" => Self::Clip,
            "config.get" => Self::ConfigGet,
            "config.set" => {
                let values = match req.args.get("values") {
                    Some(Value::Object(map)) => map.clone(),
                    Some(_) => return Err("config.set: \"values\" must be an object".into()),
                    // Bare form: every argument other than id/cmd is a config key.
                    None => req.args.clone(),
                };
                if values.is_empty() {
                    return Err("config.set requires at least one key to set".into());
                }
                Self::ConfigSet(values)
            }
            "library.list" => Self::LibraryList {
                offset: usize_arg(req, "offset", 0)?,
                limit: usize_arg(req, "limit", DEFAULT_LIST_LIMIT)?.min(MAX_LIST_LIMIT),
            },
            "library.delete" => Self::LibraryDelete { clip_id: str_arg(req, "clip_id")? },
            "library.rename" => Self::LibraryRename {
                clip_id: str_arg(req, "clip_id")?,
                title: str_arg(req, "title")?,
            },
            "library.favorite" => Self::LibraryFavorite {
                clip_id: str_arg(req, "clip_id")?,
                favorite: bool_arg(req, "favorite")?,
            },
            "library.reveal" => Self::LibraryReveal { clip_id: str_arg(req, "clip_id")? },
            "library.keyframes" => Self::LibraryKeyframes { clip_id: str_arg(req, "clip_id")? },
            "library.export" => Self::LibraryExport {
                clip_id: str_arg(req, "clip_id")?,
                start_ms: u64_arg(req, "start_ms")?,
                end_ms: u64_arg(req, "end_ms")?,
                mode: req.args.get("mode").and_then(Value::as_str).unwrap_or("fast").to_string(),
            },
            "monitors.list" => Self::MonitorsList,
            "encoders.list" => Self::EncodersList,
            "stats.subscribe" => Self::StatsSubscribe { enabled: bool_arg(req, "enabled")? },
            "folder.pick" => Self::FolderPick,
            "sound.pick" => Self::SoundPick,
            "sound.test" => Self::SoundTest,
            "shutdown" => Self::Shutdown,
            other => return Err(format!("unknown command {other:?}")),
        })
    }
}

fn str_arg(req: &Request, key: &str) -> Result<String, String> {
    req.args
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{} requires a string {key:?}", req.cmd))
}

fn bool_arg(req: &Request, key: &str) -> Result<bool, String> {
    req.args
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{} requires a boolean {key:?}", req.cmd))
}

fn u64_arg(req: &Request, key: &str) -> Result<u64, String> {
    req.args
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{} requires a non-negative integer {key:?}", req.cmd))
}

/// An absent argument means `default`; a present one that is not a
/// non-negative whole number is an error, matching [`str_arg`] and
/// [`bool_arg`]. Silently defaulting a bad page number would let a UI's paging
/// bug read as an empty library instead of a mistake.
///
/// `try_from` rather than `as`: on a 32-bit target `as` would wrap a huge
/// value into a small one instead of rejecting it.
fn usize_arg(req: &Request, key: &str, default: usize) -> Result<usize, String> {
    match req.args.get(key) {
        None => Ok(default),
        Some(value) => value.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| {
            format!("{} requires {key:?} to be a non-negative whole number", req.cmd)
        }),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;
    use crate::message::decode_request;

    fn parse(line: &str) -> (u64, Result<Command, String>) {
        let req = decode_request(line).expect("line should decode as a request");
        (req.id, Command::parse(&req))
    }

    #[test]
    fn parses_the_spec_example() {
        let (id, cmd) = parse(r#"{"id":7,"cmd":"arm"}"#);
        assert_eq!(id, 7);
        assert_eq!(cmd.unwrap(), Command::Arm);
    }

    /// The whole reason parsing is two-stage: an unknown command must still
    /// leave the id available for the error response.
    #[test]
    fn unknown_command_fails_but_keeps_the_id() {
        let (id, cmd) = parse(r#"{"id":42,"cmd":"launch_missiles"}"#);
        assert_eq!(id, 42);
        assert!(cmd.unwrap_err().contains("launch_missiles"));
    }

    #[test]
    fn library_list_defaults_and_clamps() {
        let (_, cmd) = parse(r#"{"id":1,"cmd":"library.list"}"#);
        assert_eq!(cmd.unwrap(), Command::LibraryList { offset: 0, limit: DEFAULT_LIST_LIMIT });

        let (_, cmd) = parse(r#"{"id":1,"cmd":"library.list","offset":40,"limit":99999}"#);
        assert_eq!(
            cmd.unwrap(),
            Command::LibraryList { offset: 40, limit: MAX_LIST_LIMIT },
            "an unbounded limit would let one request materialize the entire library"
        );
    }

    /// A wrong-type page argument must be refused, not quietly replaced by the
    /// default — otherwise a UI's paging bug looks like an empty library
    /// rather than a mistake it can see and fix.
    #[test]
    fn a_wrong_type_page_argument_is_an_error_not_a_silent_default() {
        for line in [
            r#"{"id":1,"cmd":"library.list","offset":-5}"#,
            r#"{"id":1,"cmd":"library.list","offset":1.5}"#,
            r#"{"id":1,"cmd":"library.list","limit":"ten"}"#,
            r#"{"id":1,"cmd":"library.list","limit":null}"#,
        ] {
            let (_, cmd) = parse(line);
            let err = cmd.expect_err(&format!("{line} should be refused"));
            assert!(err.contains("library.list"), "error should name the command: {err}");
        }

        let (_, cmd) = parse(r#"{"id":1,"cmd":"library.list","offset":-5}"#);
        assert!(
            cmd.unwrap_err().contains("offset"),
            "the error should name which argument was wrong"
        );
    }

    #[test]
    fn missing_arguments_are_named_in_the_error() {
        let (_, cmd) = parse(r#"{"id":1,"cmd":"library.rename","id_":"x"}"#);
        let err = cmd.unwrap_err();
        assert!(err.contains("library.rename"), "error should name the command: {err}");
        assert!(err.contains("clip_id"), "error should name the missing argument: {err}");
    }

    #[test]
    fn the_dialog_commands_parse() {
        assert_eq!(parse(r#"{"id":0,"cmd":"folder.pick"}"#).1, Ok(Command::FolderPick));
        assert_eq!(parse(r#"{"id":1,"cmd":"sound.pick"}"#).1, Ok(Command::SoundPick));
        assert_eq!(parse(r#"{"id":2,"cmd":"sound.test"}"#).1, Ok(Command::SoundTest));
    }

    #[test]
    fn stats_subscribe_carries_its_flag() {
        let (_, cmd) = parse(r#"{"id":1,"cmd":"stats.subscribe","enabled":true}"#);
        assert_eq!(cmd.unwrap(), Command::StatsSubscribe { enabled: true });
    }

    /// A request with no id cannot be answered in the normal shape at all.
    #[test]
    fn a_line_without_an_id_is_not_a_request() {
        assert!(decode_request(r#"{"cmd":"arm"}"#).is_err());
        assert!(decode_request("not json at all").is_err());
    }

    /// The app sends this before replacing trix-daemon.exe on disk. It is the
    /// only command whose success the caller confirms by watching the process
    /// leave, so it must parse from a bare request with no arguments.
    #[test]
    fn shutdown_parses_with_no_arguments() {
        let (id, cmd) = parse(r#"{"id":3,"cmd":"shutdown"}"#);
        assert_eq!(id, 3);
        assert_eq!(cmd.unwrap(), Command::Shutdown);
    }

    #[test]
    fn library_keyframes_needs_a_clip_id() {
        let req = Request {
            id: 1,
            cmd: "library.keyframes".into(),
            args: serde_json::from_str(r#"{"clip_id":"20260822_101500"}"#).unwrap(),
        };
        assert_eq!(
            Command::parse(&req),
            Ok(Command::LibraryKeyframes { clip_id: "20260822_101500".into() })
        );

        let bare = Request { id: 1, cmd: "library.keyframes".into(), args: Map::new() };
        assert!(Command::parse(&bare).is_err(), "a missing clip_id must be an error response");
    }

    #[test]
    fn library_export_defaults_mode_to_fast() {
        let req = Request {
            id: 2,
            cmd: "library.export".into(),
            args: serde_json::from_str(
                r#"{"clip_id":"20260822_101500","start_ms":1000,"end_ms":4000}"#,
            )
            .unwrap(),
        };
        assert_eq!(
            Command::parse(&req),
            Ok(Command::LibraryExport {
                clip_id: "20260822_101500".into(),
                start_ms: 1000,
                end_ms: 4000,
                mode: "fast".into(),
            }),
            "an omitted mode is fast -- the only mode that exists"
        );
    }

    #[test]
    fn library_export_requires_both_bounds() {
        let req = Request {
            id: 3,
            cmd: "library.export".into(),
            args: serde_json::from_str(r#"{"clip_id":"20260822_101500","start_ms":1000}"#).unwrap(),
        };
        let error = Command::parse(&req).expect_err("a missing end_ms must be refused");
        assert!(error.contains("end_ms"), "the message must name the missing argument: {error}");
    }
}
