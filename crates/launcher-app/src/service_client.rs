//! The client side of the pipe: the launcher asks the service for the files.
//!
//! The service holds the index of the whole machine. When it is there, the
//! launcher does not read the disk and does not hold the index in its own
//! memory — it asks, and the answer comes back in a millisecond.
//!
//! When the service is not there, the launcher does everything itself, the
//! way it always did. Nothing here is a requirement: the launcher works
//! with the service and without it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use launcher_core::service::{FileHit, Request, Response, Status};

use crate::winapi::*;

/// How long the service has to be gone before the launcher believes it.
///
/// A pipe that is busy for a moment — or a service that is being restarted
/// — is not a service that is gone. The launcher believes the **whole disk**
/// when it decides to walk through the folders of the user, and that walk
/// reads more than a million files and takes three quarters of a minute.
/// A single failed probe must never be enough to start it.
const ABSENT_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// The connection to the service.
///
/// The connection is kept open: a query is sent and answered over the same
/// pipe, so that a keystroke does not have to set up a new one.
pub struct ServiceClient {
    handle: Mutex<Option<usize>>,
    /// Until when the service is not looked for.
    ///
    /// A machine without the service would otherwise open a pipe on every
    /// keystroke, and every one of those would fail.
    absent_until: Mutex<Option<std::time::Instant>>,
    /// Whether the service has answered us at least once.
    ///
    /// This is the answer that counts. A probe can fail because the pipe is
    /// busy, but a service that has answered a question **is** there.
    answered: Arc<AtomicBool>,
    /// Since when the service has not answered.
    ///
    /// The clock starts when the service stops answering and is put back to
    /// zero when it answers again.
    gone_since: Arc<Mutex<Option<std::time::Instant>>>,
    /// Whether the client is to look for the service at all.
    ///
    /// The tests use a client that never does: a machine that has the
    /// service would answer them, and a test that means to look at the work
    /// of the launcher alone would get the index of the whole machine in it.
    disabled: bool,
    /// The pipe to talk to.
    ///
    /// It is the real one, unless a test asks for another — the tests must
    /// not talk to the service that happens to run on the machine they run
    /// on.
    pipe: Option<String>,
}

impl ServiceClient {
    /// A client that has not connected yet.
    pub fn new() -> Self {
        Self {
            handle: Mutex::new(None),
            absent_until: Mutex::new(None),
            answered: Arc::new(AtomicBool::new(false)),
            gone_since: Arc::new(Mutex::new(None)),
            disabled: false,
            pipe: None,
        }
    }

    /// A client that never looks for the service.
    ///
    /// It is for the tests: a machine that has the service would otherwise
    /// answer them, and a test that means to look at the work of the
    /// launcher alone would get the index of the machine in it.
    pub fn disabled() -> Self {
        Self {
            handle: Mutex::new(None),
            absent_until: Mutex::new(Some(
                std::time::Instant::now() + std::time::Duration::from_secs(100 * 365 * 24 * 3600),
            )),
            answered: Arc::new(AtomicBool::new(false)),
            gone_since: Arc::new(Mutex::new(None)),
            disabled: true,
            pipe: None,
        }
    }

    /// A client that cannot reach the service.
    ///
    /// It is for the tests as well: they must not talk to the service that
    /// happens to run on the machine they run on, and they must be able to
    /// ask what the launcher would do when the service cannot be reached.
    pub fn unreachable() -> Self {
        Self {
            handle: Mutex::new(None),
            absent_until: Mutex::new(None),
            answered: Arc::new(AtomicBool::new(false)),
            gone_since: Arc::new(Mutex::new(None)),
            disabled: false,
            pipe: Some("\\\\.\\pipe\\IdeasLauncherNoServiceHere".to_owned()),
        }
    }

    /// Whether the service is there.
    ///
    /// The question is asked **every time**, without the cache that the
    /// queries use. The service is a program that starts with the machine, and
    /// the launcher may well be started while it is still coming up: one
    /// failed attempt would then be remembered for five seconds, and the
    /// launcher would take the whole disk into its own memory — 744 megabytes
    /// of reading and a gigabyte of memory — for an answer the service was
    /// about to give.
    pub fn is_there(&self) -> bool {
        if self.disabled {
            return false;
        }
        self.connect_now().is_some()
    }

    /// Whether the service is **provably** gone.
    ///
    /// This is the question that the walk through the folders of the user is
    /// allowed to act on, and it is a much harder question than
    /// [`is_there`](Self::is_there):
    ///
    /// * the service has to have stopped answering for
    ///   [`ABSENT_GRACE`] — a pipe that is busy for a moment is not a
    ///   service that is gone, and
    /// * the service has to be gone **now**, not a while ago.
    ///
    /// The walk reads more than a million files and takes three quarters of a
    /// minute, and it does that while the user is looking at the results.
    /// It must not start on the word of a single failed probe.
    pub fn is_definitely_gone(&self) -> bool {
        if self.disabled {
            return true;
        }
        // Ask it first: a service that is there answers, and the answer
        // settles the question.
        if self.is_there() {
            return false;
        }
        // It did not answer. Now the clock has to have run out: a pipe that
        // is busy for a moment — or a service that is being restarted — is
        // not a service that is gone, and the walk through the folders of the
        // user must not start on the word of that.
        let Ok(gone) = self.gone_since.lock() else {
            return false;
        };
        match *gone {
            Some(since) => since.elapsed() >= ABSENT_GRACE,
            None => false,
        }
    }

    /// Open the pipe, if it is not open already.
    fn connect(&self) -> Option<HANDLE> {
        // The service was looked for a moment ago and was not there.
        if let Ok(guard) = self.absent_until.lock() {
            if let Some(until) = *guard {
                if std::time::Instant::now() < until {
                    return None;
                }
            }
        }
        self.connect_now()
    }

    /// Open the pipe, whether it was looked for a moment ago or not.
    fn connect_now(&self) -> Option<HANDLE> {
        let mut guard = self.handle.lock().ok()?;
        if let Some(handle) = *guard {
            return Some(handle as HANDLE);
        }
        let name: Vec<u16> = self
            .pipe
            .clone()
            .unwrap_or_else(|| launcher_core::service::PIPE_NAME.to_owned())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                0xC000_0000, // GENERIC_READ | GENERIC_WRITE
                0,             // the pipe is not shared
                std::ptr::null(),
                3,             // OPEN_EXISTING
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            // The pipe could not be opened. **This is not a proof that the
            // service is gone**: a named pipe that has no free instance
            // answers with ERROR_PIPE_BUSY, and a service that is being
            // restarted answers with "file not found" for a moment. Both
            // look exactly like a service that is not there.
            let error = unsafe { GetLastError() };
            log::warn!("the pipe of the index service could not be opened: error {error}");
            // The service is looked for again in a few seconds — it may be
            // started in the meantime.
            if let Ok(mut absent) = self.absent_until.lock() {
                *absent = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
            }
            self.note_gone();
            return None;
        }
        *guard = Some(handle as usize);
        self.note_here();
        Some(handle)
    }

    /// Note that the service has answered.
    fn note_here(&self) {
        self.answered.store(true, Ordering::SeqCst);
        if let Ok(mut gone) = self.gone_since.lock() {
            *gone = None;
        }
    }

    /// Note that the service has stopped answering.
    fn note_gone(&self) {
        if let Ok(mut gone) = self.gone_since.lock() {
            if gone.is_none() {
                *gone = Some(std::time::Instant::now());
            }
        }
    }

    /// Forget the connection.
    ///
    /// The service may be stopped and started again while the launcher runs;
    /// the pipe that is broken is then thrown away and opened again.
    fn forget(&self, handle: HANDLE) {
        if let Ok(mut guard) = self.handle.lock() {
            if *guard == Some(handle as usize) {
                unsafe { CloseHandle(handle) };
                *guard = None;
            }
        }
    }

    /// Ask the service something.
    fn ask(&self, request: Request) -> Option<Response> {
        let handle = self.connect()?;
        let frame = request.encode();
        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(
                handle,
                frame.as_ptr().cast(),
                frame.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            self.forget(handle);
            self.note_gone();
            return None;
        }
        let Some(response) = read_frame(handle) else {
            self.forget(handle);
            self.note_gone();
            return None;
        };
        // The service answered. **This is the answer that counts** — a probe
        // that fails later does not make it go away.
        self.note_here();
        Some(response)
    }

    /// The files that match a query.
    pub fn query(&self, text: &str, limit: u32) -> Option<Vec<FileHit>> {
        match self.ask(Request::Query {
            text: text.to_string(),
            limit,
        })? {
            Response::Hits { hits } => Some(hits),
            Response::Error(_) => None,
            _ => None,
        }
    }

    /// What the service is doing.
    pub fn status(&self) -> Option<Status> {
        match self.ask(Request::Status)? {
            Response::Status(status) => Some(status),
            _ => None,
        }
    }

    /// Ask the service to read the table again.
    ///
    /// The service is the one that holds the table, so it is also the one that
    /// has to read it again.
    pub fn rebuild(&self) -> bool {
        matches!(self.ask(Request::Rebuild), Some(Response::Status(_)))
    }
}

impl Default for ServiceClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Close the pipe when the client is thrown away.
///
/// Without this the handle was simply dropped on the floor: the service
/// kept the connection open, and the thread that answers it waited for a
/// request that was never going to come. Every copy of the client that the
/// launcher made left one such thread behind in the service.
impl Drop for ServiceClient {
    fn drop(&mut self) {
        let Ok(mut guard) = self.handle.lock() else {
            return;
        };
        if let Some(handle) = guard.take() {
            unsafe { CloseHandle(handle as HANDLE) };
        }
    }
}

impl Clone for ServiceClient {
    fn clone(&self) -> Self {
        // The connection belongs to the original: the copy starts without one
        // and opens its own when it needs it.
        let mut copy = Self::new();
        // A client that is not to look for the service stays that way.
        copy.disabled = self.disabled;
        // **What is known about the service is shared.** The launcher holds
        // several clients — one for the queries, one for the check whether
        // the service is there — and they must not disagree about it: one of
        // them saying "the service is gone" is enough to start the walk
        // through the folders of the user, and that walk is expensive.
        copy.answered = self.answered.clone();
        copy.gone_since = self.gone_since.clone();
        copy.pipe = self.pipe.clone();
        copy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A service that cannot be reached is **not** a service that is gone.
    ///
    /// The launcher walks through the folders of the user when the service
    /// is not there, and that walk reads more than a million files and takes
    /// three quarters of a minute — while the user is looking at the results.
    /// A pipe that is busy for a moment looks exactly like a service that is
    /// not there, so a single failed probe must not be enough.
    #[test]
    fn a_service_that_cannot_be_reached_is_not_gone_at_once() {
        let client = ServiceClient::unreachable();
        assert!(!client.is_there());
        assert!(
            !client.is_definitely_gone(),
            "one failed probe must not be taken for a service that is gone"
        );
    }

    /// A service that has answered is there, whatever the probe says.
    #[test]
    fn a_service_that_has_answered_is_not_gone() {
        let client = ServiceClient::unreachable();
        client.note_here();
        assert!(!client.is_definitely_gone());
    }

    /// A service that has been gone for long enough **is** gone.
    #[test]
    fn a_service_that_has_been_gone_for_long_enough_is_gone() {
        let client = ServiceClient::unreachable();
        // The clock is put back by hand: the test must not wait ten seconds.
        {
            let mut gone = client.gone_since.lock().unwrap();
            *gone = Some(std::time::Instant::now() - ABSENT_GRACE - std::time::Duration::from_secs(1));
        }
        assert!(client.is_definitely_gone());
    }

    /// A client that never looks for the service is a client without a
    /// service.
    #[test]
    fn a_disabled_client_has_no_service() {
        let client = ServiceClient::disabled();
        assert!(client.is_definitely_gone());
    }
}

/// Read one frame from the pipe.
fn read_frame(handle: HANDLE) -> Option<Response> {
    let mut header = [0u8; 5];
    if !read_exact(handle, &mut header) {
        return None;
    }
    let length = u32::from_le_bytes(header[1..5].try_into().ok()?) as usize;
    if length > 64 * 1024 * 1024 {
        return None;
    }
    let mut body = vec![0u8; length];
    if length > 0 && !read_exact(handle, &mut body) {
        return None;
    }
    let mut frame = Vec::with_capacity(length + 5);
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&body);
    Response::decode(&frame)
}

/// Read until the buffer is full.
fn read_exact(handle: HANDLE, buffer: &mut [u8]) -> bool {
    let mut at = 0usize;
    while at < buffer.len() {
        let mut read = 0u32;
        let ok = unsafe {
            ReadFile(
                handle,
                buffer[at..].as_mut_ptr().cast(),
                (buffer.len() - at) as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || read == 0 {
            return false;
        }
        at += read as usize;
    }
    true
}
