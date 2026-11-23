//! The wire between the launcher and the service that keeps the index.
//!
//! Everything is a frame: a four byte header with the kind of the message
//! and the length of the body, and then the body itself. The bodies are
//! written by hand, without a serialization library — the launcher answers a
//! query on every keystroke, and the framing should not be the slow part.
//!
//! The service is the part of the launcher that runs as a Windows service:
//! it holds the index of the whole machine, follows the change journal of
//! the volumes, and answers the queries that come in through the pipe. The
//! window itself stays a small program that asks the service for the files.

/// The pipe the service listens on.
///
/// The name says what it is for, so that two copies of the launcher do not
/// talk to each other by accident.
pub const PIPE_NAME: &str = r"\\.\pipe\IdeasLauncherService";

/// The kind of a frame.
const KIND_REQUEST: u8 = 1;
const KIND_RESPONSE: u8 = 2;

/// What the client asks the service for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The hits of a query.
    Query {
        /// The text the user typed.
        text: String,
        /// How many hits are wanted.
        limit: u32,
    },
    /// What the service is doing.
    Status,
    /// The service should read the table again.
    Rebuild,
    /// The service should stop.
    Stop,
}

/// What the service answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// The hits of a query.
    Hits {
        /// The files that matched.
        hits: Vec<FileHit>,
    },
    /// The state of the service.
    Status(Status),
    /// Something went wrong.
    Error(String),
}

/// One file that matched a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHit {
    /// The name of the file.
    pub name: String,
    /// The full path of the file.
    pub path: String,
    /// True for folders.
    pub is_dir: bool,
    /// The size of the content, when it is known.
    pub size: Option<u64>,
    /// When the content changed, when it is known.
    pub modified: Option<u64>,
    /// How well the file matches the query.
    pub score: i32,
}

/// What the service is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Status {
    /// How many files the index holds.
    pub entries: u64,
    /// When the index was built, in seconds since the Unix epoch.
    pub built_at: u64,
    /// Whether the change journal is being followed.
    pub journal: bool,
    /// Whether the index is being built right now.
    pub building: bool,
    /// How many changes the journal has brought in.
    pub changes: u64,
}

impl Request {
    /// The frame of the request.
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        match self {
            Request::Query { text, limit } => {
                body.push(1);
                put_u32(&mut body, *limit);
                put_str(&mut body, text);
            }
            Request::Status => body.push(2),
            Request::Rebuild => body.push(4),
            Request::Stop => body.push(3),
        }
        frame(KIND_REQUEST, &body)
    }

    /// Read a request out of a frame.
    pub fn decode(frame: &[u8]) -> Option<Request> {
        let (kind, body) = read_frame(frame)?;
        if kind != KIND_REQUEST {
            return None;
        }
        let mut reader = Reader { bytes: body, at: 0 };
        match reader.byte()? {
            1 => {
                let limit = reader.u32()?;
                let text = reader.string()?;
                Some(Request::Query { text, limit })
            }
            2 => Some(Request::Status),
            3 => Some(Request::Stop),
            4 => Some(Request::Rebuild),
            _ => None,
        }
    }
}

impl Response {
    /// The frame of the response.
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        match self {
            Response::Hits { hits } => {
                // The body is written in one go: each hit is roughly a
                // hundred bytes (two strings plus the numbers), and a
                // `Vec` that grows from nothing reallocates a thousand
                // times for a full answer. The estimate only has to be in
                // the right ballpark — a little too much is released at
                // the end, a little too little costs one realloc.
                body.reserve(1 + 4 + hits.len() * 110);
                body.push(1);
                put_u32(&mut body, hits.len() as u32);
                for hit in hits {
                    put_str(&mut body, &hit.name);
                    put_str(&mut body, &hit.path);
                    body.push(u8::from(hit.is_dir));
                    put_u64(&mut body, hit.size.unwrap_or(u64::MAX));
                    put_u64(&mut body, hit.modified.unwrap_or(u64::MAX));
                    put_u32(&mut body, hit.score as u32);
                }
            }
            Response::Status(status) => {
                body.push(2);
                put_u64(&mut body, status.entries);
                put_u64(&mut body, status.built_at);
                body.push(u8::from(status.journal));
                body.push(u8::from(status.building));
                put_u64(&mut body, status.changes);
            }
            Response::Error(text) => {
                body.push(3);
                put_str(&mut body, text);
            }
        }
        frame(KIND_RESPONSE, &body)
    }

    /// Read a response out of a frame.
    pub fn decode(frame: &[u8]) -> Option<Response> {
        let (kind, body) = read_frame(frame)?;
        if kind != KIND_RESPONSE {
            return None;
        }
        let mut reader = Reader { bytes: body, at: 0 };
        match reader.byte()? {
            1 => {
                let count = reader.u32()? as usize;
                let mut hits = Vec::with_capacity(count.min(4096));
                for _ in 0..count {
                    let name = reader.string()?;
                    let path = reader.string()?;
                    let is_dir = reader.byte()? != 0;
                    let size = reader.u64()?;
                    let modified = reader.u64()?;
                    let score = reader.u32()? as i32;
                    hits.push(FileHit {
                        name,
                        path,
                        is_dir,
                        size: (size != u64::MAX).then_some(size),
                        modified: (modified != u64::MAX).then_some(modified),
                        score,
                    });
                }
                Some(Response::Hits { hits })
            }
            2 => Some(Response::Status(Status {
                entries: reader.u64()?,
                built_at: reader.u64()?,
                journal: reader.byte()? != 0,
                building: reader.byte()? != 0,
                changes: reader.u64()?,
            })),
            3 => Some(Response::Error(reader.string()?)),
            _ => None,
        }
    }
}

/// A frame: the kind, the length, and then the body.
fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    out.push(kind);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// The kind and the body of a frame.
fn read_frame(bytes: &[u8]) -> Option<(u8, &[u8])> {
    if bytes.len() < 5 {
        return None;
    }
    let kind = bytes[0];
    let length = u32::from_le_bytes(bytes[1..5].try_into().ok()?) as usize;
    let body = bytes.get(5..5 + length)?;
    Some((kind, body))
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    let bytes = text.as_bytes();
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Reads the pieces out of a body.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> Option<&[u8]> {
        let piece = self.bytes.get(self.at..self.at + count)?;
        self.at += count;
        Some(piece)
    }

    fn byte(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<String> {
        let length = self.u32()? as usize;
        let bytes = self.take(length)?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_survives_the_round_trip() {
        let request = Request::Query {
            text: "文档 doc".to_string(),
            limit: 500,
        };
        let decoded = Request::decode(&request.encode()).expect("the request is read back");
        assert_eq!(decoded, request);
    }

    #[test]
    fn the_hits_survive_the_round_trip() {
        let response = Response::Hits {
            hits: vec![
                FileHit {
                    name: "报告.pdf".to_string(),
                    path: "C:\\Users\\Sisyphus\\报告.pdf".to_string(),
                    is_dir: false,
                    size: Some(1234),
                    modified: Some(1_700_000_000),
                    score: -3,
                },
                FileHit {
                    name: "文件夹".to_string(),
                    path: "C:\\Users\\Sisyphus\\文件夹".to_string(),
                    is_dir: true,
                    size: None,
                    modified: None,
                    score: 7,
                },
            ],
        };
        let decoded = Response::decode(&response.encode()).expect("the response is read back");
        assert_eq!(decoded, response);
    }

    #[test]
    fn the_status_survives_the_round_trip() {
        let response = Response::Status(Status {
            entries: 9_190_042,
            built_at: 1_700_000_000,
            journal: true,
            building: false,
            changes: 1234,
        });
        let decoded = Response::decode(&response.encode()).expect("the response is read back");
        assert_eq!(decoded, response);
    }

    #[test]
    fn a_frame_of_another_kind_is_not_read() {
        let request = Request::Status;
        assert!(Response::decode(&request.encode()).is_none());
    }

    #[test]
    fn a_short_frame_is_not_read() {
        assert!(Request::decode(&[1, 2, 3]).is_none());
    }
}
