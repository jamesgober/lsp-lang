//! The front half of a session: the router that receives messages from the
//! transport, reads their parameters, and queues them as [`Job`]s.
//!
//! Nothing in a router handler touches the language. Each message becomes a
//! job in arrival order, and the session runs the queue when the input has
//! nothing more to hand over (see [`Server::run`](crate::Server::run)). That
//! gap between receiving a request and running it is where
//! `$/cancelRequest` does its work: the transport answers a cancelled request
//! with `REQUEST_CANCELLED` the moment the cancellation is read, and the job
//! finds its request cancelled and never calls the handler.

use std::collections::VecDeque;

use jsonx_lib::Value;
use rpc_io::{Event, Responder, Router};

use crate::{
    context::MessageType,
    json::{opt_object, req_i32, req_object, req_str},
    position::{Encoding, Position, Range, document_position, document_uri},
    response::ResponseError,
    session::clip_to,
};

/// One edit of a `didChange`: replaces `range`, or the whole text without one.
#[derive(Debug)]
pub(crate) struct Edit {
    pub(crate) range: Option<Range>,
    pub(crate) text: String,
}

/// A typed request, read from its parameters.
#[derive(Debug)]
pub(crate) enum Feature {
    Hover(String, Position),
    Definition(String, Position),
    Completion(String, Position),
    Symbols(String),
    SemanticFull(String),
    SemanticDelta { uri: String, previous: String },
}

/// A message waiting to be handled.
pub(crate) enum Job {
    Initialize {
        params: Value<'static>,
        encoding: Encoding,
        responder: Responder,
    },
    Shutdown {
        responder: Responder,
    },
    Exit,
    Open {
        uri: String,
        version: i32,
        text: String,
    },
    Change {
        uri: String,
        version: i32,
        edits: Vec<Edit>,
    },
    Close {
        uri: String,
    },
    Notification {
        method: String,
        params: Value<'static>,
    },
    Request {
        method: String,
        params: Value<'static>,
        responder: Responder,
    },
    Feature {
        method: &'static str,
        request: Result<Feature, ResponseError>,
        responder: Responder,
    },
    /// A notification whose parameters could not be read; reported in order.
    Malformed {
        method: String,
        why: String,
    },
    /// Something the transport dealt with that the client may want to know,
    /// logged in order.
    Log {
        kind: MessageType,
        message: String,
    },
}

/// The router's state: the queue, and two flags the transport loop reads.
#[derive(Default)]
pub(crate) struct Frontend {
    pub(crate) jobs: VecDeque<Job>,
    /// Run the queue before reading the next message: set by `initialize`,
    /// whose answer opens the gate that the messages behind it wait at.
    pub(crate) eager: bool,
    /// `exit` was received: nothing after it is read.
    pub(crate) exit: bool,
    /// A handler ran, or an event other than an answer was reported, since the
    /// flag was last cleared: the message just handled was not an answer to
    /// one of the server's own requests.
    pub(crate) activity: bool,
}

impl Frontend {
    /// Queues a job; the message that made it was not an answer.
    pub(crate) fn push(&mut self, job: Job) {
        self.activity = true;
        self.jobs.push_back(job);
    }
}

/// Notifications forwarded to [`Handler::notification`](crate::Handler::notification).
const FORWARDED: &[&str] = &[
    "initialized",
    "textDocument/didSave",
    "textDocument/willSave",
    "workspace/didChangeConfiguration",
    "workspace/didChangeWatchedFiles",
    "workspace/didChangeWorkspaceFolders",
    "$/setTrace",
    "window/workDoneProgress/cancel",
];

/// Builds the router: the lifecycle, document synchronization, the typed
/// features, the forwarded notifications, and the host's custom methods.
pub(crate) fn router(
    custom_requests: &[Box<str>],
    custom_notifications: &[Box<str>],
) -> Router<Frontend> {
    let mut router = Router::new();
    // Custom methods first: a built-in method of the same name wins.
    for name in custom_requests {
        let method: String = name.to_string();
        router =
            router.deferred(
                name,
                move |front: &mut Frontend, params, _, responder| match params.parse() {
                    Ok(value) => front.push(Job::Request {
                        method: method.clone(),
                        params: value.into_owned(),
                        responder,
                    }),
                    Err(error) => refuse(responder, error.into()),
                },
            );
    }
    for name in FORWARDED
        .iter()
        .copied()
        .chain(custom_notifications.iter().map(|n| &**n))
    {
        let method = name.to_owned();
        router = router.notification(name, move |front: &mut Frontend, params, _| {
            let job = match params.parse() {
                Ok(value) => Job::Notification {
                    method: method.clone(),
                    params: value.into_owned(),
                },
                Err(error) => Job::Malformed {
                    method: method.clone(),
                    why: error.message().to_owned(),
                },
            };
            front.push(job);
        });
    }
    router = router
        .on_event(report)
        .deferred(
            "initialize",
            |front: &mut Frontend, params, _, responder| {
                let read = params
                    .parse()
                    .map_err(ResponseError::from)
                    .and_then(|value| {
                        let encoding = read_initialize(&value)?;
                        Ok((value.into_owned(), encoding))
                    });
                match read {
                    Ok((params, encoding)) => {
                        front.eager = true;
                        front.push(Job::Initialize {
                            params,
                            encoding,
                            responder,
                        });
                    }
                    Err(error) => refuse(responder, error),
                }
            },
        )
        .deferred("shutdown", |front: &mut Frontend, _, _, responder| {
            front.push(Job::Shutdown { responder });
        })
        .notification("exit", |front: &mut Frontend, _, _| {
            front.exit = true;
            front.push(Job::Exit);
        })
        .notification("textDocument/didOpen", |front: &mut Frontend, params, _| {
            let job = notification_job("textDocument/didOpen", &params, read_open);
            front.push(job);
        })
        .notification(
            "textDocument/didChange",
            |front: &mut Frontend, params, _| {
                let job = notification_job("textDocument/didChange", &params, read_change);
                front.push(job);
            },
        )
        .notification(
            "textDocument/didClose",
            |front: &mut Frontend, params, _| {
                let job = notification_job("textDocument/didClose", &params, read_close);
                front.push(job);
            },
        );
    router = feature(router, "textDocument/hover", |v| {
        let (uri, at) = document_position(v)?;
        Ok(Feature::Hover(uri, at))
    });
    router = feature(router, "textDocument/definition", |v| {
        let (uri, at) = document_position(v)?;
        Ok(Feature::Definition(uri, at))
    });
    router = feature(router, "textDocument/completion", |v| {
        let (uri, at) = document_position(v)?;
        Ok(Feature::Completion(uri, at))
    });
    router = feature(router, "textDocument/documentSymbol", |v| {
        Ok(Feature::Symbols(document_uri(v)?))
    });
    router = feature(router, "textDocument/semanticTokens/full", |v| {
        Ok(Feature::SemanticFull(document_uri(v)?))
    });
    feature(router, "textDocument/semanticTokens/full/delta", |v| {
        Ok(Feature::SemanticDelta {
            uri: document_uri(v)?,
            previous: req_str(v, "previousResultId")?.to_owned(),
        })
    })
}

/// Turns what the transport dealt with on its own into log lines for the
/// client: answers that match no request of ours, and answers too broken to
/// read. (A malformed *request* was already answered with an error.)
fn report(front: &mut Frontend, event: &Event<'_>) {
    let (kind, message) = match event {
        Event::Unmatched { id } => (
            MessageType::Warning,
            format!(
                "ignored a response with id {}, which answers no request the server is waiting on",
                clip_to(&id.to_string(), 64)
            ),
        ),
        Event::Invalid {
            error,
            answered: false,
        } => (
            MessageType::Warning,
            format!(
                "ignored a response that is not valid: {}",
                clip_to(error.message(), 128)
            ),
        ),
        Event::PeerError { error } => (
            MessageType::Warning,
            format!(
                "the client could not read a message of the server's: {}",
                clip_to(error.message(), 128)
            ),
        ),
        // Things the transport handled that were never answers.
        Event::UnknownNotification { .. }
        | Event::Dropped { .. }
        | Event::Cancelled { .. }
        | Event::Panicked { .. }
        | Event::Oversized { .. } => {
            front.activity = true;
            return;
        }
        _ => return,
    };
    front.push(Job::Log { kind, message });
}

/// Registers a typed request: `read` turns its parameters into a [`Feature`]
/// (or the `INVALID_PARAMS` error to answer with, once the lifecycle allows).
fn feature(
    router: Router<Frontend>,
    method: &'static str,
    read: fn(&Value<'_>) -> Result<Feature, ResponseError>,
) -> Router<Frontend> {
    router.deferred(method, move |front: &mut Frontend, params, _, responder| {
        let request = params
            .parse()
            .map_err(ResponseError::from)
            .and_then(|value| read(&value));
        front.push(Job::Feature {
            method,
            request,
            responder,
        });
    })
}

/// Answers a request that could not be read. The connection having closed is
/// the only way this fails, and the session loop notices that itself.
fn refuse(responder: Responder, error: ResponseError) {
    let _closed_is_noticed_elsewhere = responder.err(error.into());
}

/// Reads a document-synchronization notification into its job, or into the
/// report of why it could not be read.
fn notification_job(
    method: &str,
    params: &rpc_io::Params<'_>,
    read: fn(Value<'_>) -> Result<Job, ResponseError>,
) -> Job {
    match params.parse().map_err(ResponseError::from).and_then(read) {
        Ok(job) => job,
        Err(error) => Job::Malformed {
            method: method.to_owned(),
            why: error.message().to_owned(),
        },
    }
}

/// `InitializeParams`: only the client's offered position encodings matter to
/// the server. Every level is optional and `null` reads as absent; a level of
/// the wrong type is an error.
fn read_initialize(params: &Value<'_>) -> Result<Encoding, ResponseError> {
    if !matches!(params, Value::Null | Value::Object(_)) {
        return Err(ResponseError::invalid_params(format!(
            "invalid params: `initialize` takes an object, not {}",
            params.kind()
        )));
    }
    let general = match opt_object(params, "capabilities")? {
        Some(capabilities) => opt_object(capabilities, "general")?,
        None => None,
    };
    let offered = match general {
        Some(general) => &general["positionEncodings"],
        None => &Value::Null,
    };
    match offered {
        Value::Null => Ok(Encoding::negotiate([])),
        Value::Array(names) => {
            let mut strings = Vec::with_capacity(names.len());
            for name in names.iter() {
                let Some(name) = name.as_str() else {
                    return Err(ResponseError::invalid_params(
                        "invalid params: `positionEncodings` must be an array of strings",
                    ));
                };
                strings.push(name);
            }
            Ok(Encoding::negotiate(strings))
        }
        other => Err(ResponseError::invalid_params(format!(
            "invalid params: `positionEncodings` must be an array, not {}",
            other.kind()
        ))),
    }
}

/// `textDocument/didOpen`. The text is moved out of the parsed tree, so a text
/// with escapes (any with a newline) is copied once, by the JSON decoder.
fn read_open(mut params: Value<'_>) -> Result<Job, ResponseError> {
    let document = params.get_mut("textDocument").ok_or_else(|| {
        ResponseError::invalid_params("invalid params: `textDocument` is required")
    })?;
    let uri = req_str(document, "uri")?.to_owned();
    let version = req_i32(document, "version")?;
    let text = take_string(document, "text")?;
    Ok(Job::Open { uri, version, text })
}

/// `textDocument/didChange`.
fn read_change(mut params: Value<'_>) -> Result<Job, ResponseError> {
    let document = req_object(&params, "textDocument")?;
    let uri = req_str(document, "uri")?.to_owned();
    let version = req_i32(document, "version")?;
    let changes = params.get_mut("contentChanges");
    let Some(Value::Array(changes)) = changes else {
        return Err(ResponseError::invalid_params(
            "invalid params: `contentChanges` must be an array",
        ));
    };
    let mut edits = Vec::with_capacity(changes.len());
    for change in changes.iter_mut() {
        if !matches!(change, Value::Object(_)) {
            return Err(ResponseError::invalid_params(
                "invalid params: each content change must be an object",
            ));
        }
        let range = match &change["range"] {
            Value::Null => None,
            range => Some(Range::from_json(range)?),
        };
        edits.push(Edit {
            range,
            text: take_string(change, "text")?,
        });
    }
    Ok(Job::Change {
        uri,
        version,
        edits,
    })
}

/// `textDocument/didClose`.
fn read_close(params: Value<'_>) -> Result<Job, ResponseError> {
    Ok(Job::Close {
        uri: req_str(req_object(&params, "textDocument")?, "uri")?.to_owned(),
    })
}

/// Moves the string member `name` out of `object`.
fn take_string(object: &mut Value<'_>, name: &str) -> Result<String, ResponseError> {
    match object.get_mut(name).map(Value::take) {
        Some(Value::String(text)) => Ok(text.into_owned()),
        Some(Value::Null) | None => Err(ResponseError::invalid_params(format!(
            "invalid params: `{name}` is required"
        ))),
        Some(other) => Err(ResponseError::invalid_params(format!(
            "invalid params: `{name}` must be a string, not {}",
            other.kind()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use jsonx_lib::json;

    use super::*;

    #[test]
    fn test_initialize_params_tolerate_missing_and_null_levels() {
        for params in [
            json!(null),
            json!({}),
            json!({ "capabilities": null }),
            json!({ "capabilities": { "general": null } }),
            json!({ "capabilities": { "general": { "positionEncodings": null } } }),
        ] {
            assert_eq!(
                read_initialize(&params).ok(),
                Some(Encoding::Utf16),
                "{params}"
            );
        }
        let utf8 =
            json!({ "capabilities": { "general": { "positionEncodings": ["utf-16", "utf-8"] } } });
        assert_eq!(read_initialize(&utf8).ok(), Some(Encoding::Utf8));
    }

    #[test]
    fn test_initialize_params_of_the_wrong_type_are_refused() {
        for params in [
            json!([1]),
            json!({ "capabilities": 5 }),
            json!({ "capabilities": { "general": [] } }),
            json!({ "capabilities": { "general": { "positionEncodings": "utf-8" } } }),
            json!({ "capabilities": { "general": { "positionEncodings": [8] } } }),
        ] {
            let err = read_initialize(&params).err();
            assert_eq!(
                err.map(|e| e.code()),
                Some(ResponseError::INVALID_PARAMS),
                "{params}"
            );
        }
    }

    #[test]
    fn test_open_moves_the_text_out_of_the_message() {
        let params = json!({ "textDocument": { "uri": "u", "version": 3, "text": "a\nb" } });
        let job = read_open(params);
        assert!(matches!(
            job,
            Ok(Job::Open { ref uri, version: 3, ref text }) if uri == "u" && text == "a\nb"
        ));
        for bad in [
            json!({}),
            json!({ "textDocument": { "uri": "u", "version": 3 } }),
            json!({ "textDocument": { "uri": "u", "version": "3", "text": "" } }),
            json!({ "textDocument": { "uri": 1, "version": 3, "text": "" } }),
            json!({ "textDocument": { "uri": "u", "version": 3, "text": 4 } }),
        ] {
            assert!(read_open(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn test_change_reads_ranged_and_whole_edits() {
        let params = json!({
            "textDocument": { "uri": "u", "version": 3 },
            "contentChanges": [
                { "range": { "start": { "line": 0, "character": 1 }, "end": { "line": 0, "character": 1 } }, "text": "x" },
                { "text": "all" },
            ],
        });
        let read = read_change(params).ok();
        let Some(Job::Change { edits, version, .. }) = read else {
            return assert!(read.is_some(), "didChange params did not read");
        };
        assert_eq!(version, 3);
        assert_eq!(edits.len(), 2);
        assert!(edits[0].range.is_some() && edits[0].text == "x");
        assert!(edits[1].range.is_none() && edits[1].text == "all");
        for bad in [
            json!({ "textDocument": { "uri": "u", "version": 1 } }),
            json!({ "textDocument": { "uri": "u", "version": 1 }, "contentChanges": {} }),
            json!({ "textDocument": { "uri": "u", "version": 1 }, "contentChanges": [5] }),
            json!({ "textDocument": { "uri": "u", "version": 1 }, "contentChanges": [{}] }),
            json!({ "textDocument": { "uri": "u", "version": 1 }, "contentChanges": [{ "range": 1, "text": "" }] }),
            json!({ "textDocument": { "uri": "u" }, "contentChanges": [] }),
        ] {
            assert!(read_change(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn test_close_reads_the_uri() {
        assert!(matches!(
            read_close(json!({ "textDocument": { "uri": "u" } })),
            Ok(Job::Close { ref uri }) if uri == "u"
        ));
        assert!(read_close(json!({})).is_err());
    }
}
