//! The typed command set, parsed out of a [`Request`]'s loose argument bag.

use serde_json::Value;

use crate::message::Request;

/// Page size when `library.list` omits `limit`.
pub const DEFAULT_LIST_LIMIT: usize = 50;
/// Hard ceiling on `library.list`'s page size.
pub const MAX_LIST_LIMIT: usize = 500;

/// Every command the daemon answers. `library.export` is deliberately absent —
/// it arrives with trim support.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Status,
    Arm,
    Disarm,
    Clip,
    ConfigGet,
    ConfigSet(serde_json::Map<String, Value>),
    LibraryList { offset: usize, limit: usize },
    LibraryDelete { clip_id: String },
    LibraryRename { clip_id: String, title: String },
    LibraryFavorite { clip_id: String, favorite: bool },
    LibraryReveal { clip_id: String },
    MonitorsList,
    EncodersList,
    StatsSubscribe { enabled: bool },
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
                offset: usize_arg(req, "offset").unwrap_or(0),
                limit: usize_arg(req, "limit").unwrap_or(DEFAULT_LIST_LIMIT).min(MAX_LIST_LIMIT),
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
            "monitors.list" => Self::MonitorsList,
            "encoders.list" => Self::EncodersList,
            "stats.subscribe" => Self::StatsSubscribe { enabled: bool_arg(req, "enabled")? },
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

fn usize_arg(req: &Request, key: &str) -> Option<usize> {
    req.args.get(key).and_then(Value::as_u64).map(|n| n as usize)
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn missing_arguments_are_named_in_the_error() {
        let (_, cmd) = parse(r#"{"id":1,"cmd":"library.rename","id_":"x"}"#);
        let err = cmd.unwrap_err();
        assert!(err.contains("library.rename"), "error should name the command: {err}");
        assert!(err.contains("clip_id"), "error should name the missing argument: {err}");
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
}
