use super::capability::parse_numeric_params;
use super::*;

fn parse_two_numeric_params(params: &str) -> Option<(u32, u32)> {
    let (first, second) = params.split_once(';')?;
    let first = first.parse::<u32>().ok()?;
    let second = second.parse::<u32>().ok()?;
    Some((first, second))
}

/// Parses a terminal query response sequence.
///
/// This mirrors CC Ink's response parsing for DECRPM, DA1/DA2, Kitty keyboard
/// flags, DECXCPR cursor position, OSC responses, and XTVERSION. Returns
/// `None` when the sequence is not a recognized terminal response and should be
/// treated as ordinary input by higher-level parsers.
pub fn parse_terminal_response(sequence: &str) -> Option<TerminalResponse> {
    if let Some(rest) = sequence.strip_prefix("\x1b[?") {
        if let Some(body) = rest.strip_suffix("$y") {
            let (mode, status) = parse_two_numeric_params(body)?;
            return Some(TerminalResponse::Decrpm { mode, status });
        }

        if let Some(body) = rest.strip_suffix('c') {
            return Some(TerminalResponse::Da1 {
                params: parse_numeric_params(body)?,
            });
        }

        if let Some(body) = rest.strip_suffix('u') {
            return Some(TerminalResponse::KittyKeyboard {
                flags: body.parse::<u32>().ok()?,
            });
        }

        if let Some(body) = rest.strip_suffix('R') {
            let (row, col) = parse_two_numeric_params(body)?;
            return Some(TerminalResponse::CursorPosition { row, col });
        }
    }

    if let Some(body) = sequence
        .strip_prefix("\x1b[>")
        .and_then(|rest| rest.strip_suffix('c'))
    {
        return Some(TerminalResponse::Da2 {
            params: parse_numeric_params(body)?,
        });
    }

    if let Some(body) = sequence.strip_prefix("\x1b]") {
        let body = if let Some(body) = body.strip_suffix("\x1b\\") {
            body
        } else {
            body.strip_suffix('\x07')?
        };
        let (code, data) = body.split_once(';')?;
        return Some(TerminalResponse::Osc {
            code: code.parse::<u32>().ok()?,
            data: data.to_string(),
        });
    }

    parse_xtversion_response(sequence).map(|name| TerminalResponse::Xtversion {
        name: name.to_string(),
    })
}

/// Incremental parser for terminal query response sequences.
///
/// This is a small Rust counterpart to CC Ink's termio tokenizer plus
/// `parseTerminalResponse(...)` path. It can be used by custom frontends that
/// read raw terminal input: feed chunks as they arrive, and the parser emits
/// recognized [`TerminalResponse`] values while buffering incomplete CSI, OSC,
/// DCS, APC, PM, and SOS string sequences across chunk boundaries.
#[derive(Clone, Debug, Default)]
pub struct TerminalResponseParser {
    buffer: String,
}

impl TerminalResponseParser {
    /// Creates an empty response parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a raw input chunk and returns any recognized terminal responses.
    ///
    /// Non-response input is ignored. Incomplete escape sequences are retained
    /// until a future call completes them.
    pub fn feed(&mut self, input: &str) -> Vec<TerminalResponse> {
        self.buffer.push_str(input);
        self.drain_responses()
    }

    /// Feeds raw input bytes after applying CC Ink's `inputToString(Buffer)` rules.
    pub fn feed_bytes(&mut self, input: &[u8]) -> Vec<TerminalResponse> {
        self.feed(&terminal_input_bytes_to_string(input))
    }

    /// Feeds a raw input chunk and wraps recognized responses as terminal events.
    pub fn feed_events(&mut self, input: &str) -> Vec<TerminalEvent> {
        self.feed(input)
            .into_iter()
            .map(TerminalEvent::Response)
            .collect()
    }

    /// Feeds raw input bytes and wraps recognized responses as terminal events.
    pub fn feed_bytes_events(&mut self, input: &[u8]) -> Vec<TerminalEvent> {
        self.feed_bytes(input)
            .into_iter()
            .map(TerminalEvent::Response)
            .collect()
    }

    /// Returns the currently buffered incomplete sequence, if any.
    pub fn buffered(&self) -> &str {
        &self.buffer
    }

    /// Clears any buffered incomplete input.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    fn drain_responses(&mut self) -> Vec<TerminalResponse> {
        let mut responses = Vec::new();

        loop {
            let Some(start) = self
                .buffer
                .as_bytes()
                .iter()
                .position(|byte| *byte == b'\x1b')
            else {
                self.buffer.clear();
                break;
            };
            if start > 0 {
                self.buffer.drain(..start);
            }

            let bytes = self.buffer.as_bytes();
            if bytes.len() < 2 {
                break;
            }

            match bytes[1] {
                b'[' => {
                    let Some(end) = bytes.iter().enumerate().skip(2).find_map(|(index, byte)| {
                        (0x40..=0x7e).contains(byte).then_some(index + 1)
                    }) else {
                        break;
                    };
                    let sequence = self.buffer[..end].to_string();
                    self.buffer.drain(..end);
                    if let Some(response) = parse_terminal_response(&sequence) {
                        responses.push(response);
                    }
                }
                b']' | b'P' | b'_' | b'^' | b'X' => {
                    let mut end = None;
                    let mut index = 2usize;
                    while index < bytes.len() {
                        match bytes[index] {
                            b'\x07' => {
                                end = Some(index + 1);
                                break;
                            }
                            b'\x1b' if index + 1 >= bytes.len() => break,
                            b'\x1b' if bytes[index + 1] == b'\\' => {
                                end = Some(index + 2);
                                break;
                            }
                            _ => {}
                        }
                        index += 1;
                    }

                    let Some(end) = end else {
                        break;
                    };
                    let sequence = self.buffer[..end].to_string();
                    self.buffer.drain(..end);
                    if let Some(response) = parse_terminal_response(&sequence) {
                        responses.push(response);
                    }
                }
                _ => {
                    // Not a response sequence we recognize. Drop this ESC and
                    // keep scanning so mixed key/text input does not block later
                    // query replies in the same chunk.
                    self.buffer.drain(..1);
                }
            }
        }

        responses
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TerminalQueryMatcher {
    Decrpm(u32),
    Da1,
    Da2,
    KittyKeyboard,
    CursorPosition,
    Osc(u32),
    Xtversion,
}

impl TerminalQueryMatcher {
    fn matches(&self, response: &TerminalResponse) -> bool {
        match (self, response) {
            (Self::Decrpm(expected), TerminalResponse::Decrpm { mode, .. }) => expected == mode,
            (Self::Da1, TerminalResponse::Da1 { .. }) => true,
            (Self::Da2, TerminalResponse::Da2 { .. }) => true,
            (Self::KittyKeyboard, TerminalResponse::KittyKeyboard { .. }) => true,
            (Self::CursorPosition, TerminalResponse::CursorPosition { .. }) => true,
            (Self::Osc(expected), TerminalResponse::Osc { code, .. }) => expected == code,
            (Self::Xtversion, TerminalResponse::Xtversion { .. }) => true,
            _ => false,
        }
    }
}

/// A terminal query request paired with the response kind that should satisfy it.
///
/// This is the Rust counterpart to CC Ink's `TerminalQuery<T>` shape from
/// `terminal-querier.ts`: callers write [`Self::request`] to stdout and feed
/// parsed [`TerminalResponse`] values back into [`TerminalQuerier::on_response`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalQuery {
    request: String,
    pub(super) matcher: TerminalQueryMatcher,
}

impl TerminalQuery {
    /// Builds a DECRQM query for a DEC private mode.
    pub fn decrqm(mode: u32) -> Self {
        Self {
            request: decrqm_query_sequence(mode),
            matcher: TerminalQueryMatcher::Decrpm(mode),
        }
    }

    /// Builds a DA1 primary device-attributes query.
    pub fn da1() -> Self {
        Self {
            request: da1_query_sequence().to_string(),
            matcher: TerminalQueryMatcher::Da1,
        }
    }

    /// Builds a DA2 secondary device-attributes query.
    pub fn da2() -> Self {
        Self {
            request: da2_query_sequence().to_string(),
            matcher: TerminalQueryMatcher::Da2,
        }
    }

    /// Builds a Kitty keyboard flags query.
    pub fn kitty_keyboard() -> Self {
        Self {
            request: kitty_keyboard_query_sequence().to_string(),
            matcher: TerminalQueryMatcher::KittyKeyboard,
        }
    }

    /// Builds a DECXCPR cursor-position query.
    pub fn cursor_position() -> Self {
        Self {
            request: cursor_position_query_sequence().to_string(),
            matcher: TerminalQueryMatcher::CursorPosition,
        }
    }

    /// Builds an OSC dynamic color query, such as OSC 10 or OSC 11.
    pub fn osc_color(code: u32) -> Self {
        Self {
            request: osc_color_query_sequence(code),
            matcher: TerminalQueryMatcher::Osc(code),
        }
    }

    /// Builds an XTVERSION terminal name/version query.
    pub fn xtversion() -> Self {
        Self {
            request: xtversion_query_sequence().to_string(),
            matcher: TerminalQueryMatcher::Xtversion,
        }
    }

    /// Returns the escape sequence to write to stdout for this query.
    pub fn request(&self) -> &str {
        &self.request
    }
}

/// Future returned by [`TerminalQuerier::send`].
///
/// It resolves to `Some(response)` when a matching terminal response arrives,
/// or `None` when a DA1 flush sentinel proves that the terminal ignored the
/// query.
pub struct PendingTerminalQuery {
    pub(super) receiver: oneshot::Receiver<Option<TerminalResponse>>,
}

impl Future for PendingTerminalQuery {
    type Output = Option<TerminalResponse>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.get_mut().receiver).poll(cx) {
            Poll::Ready(Ok(response)) => Poll::Ready(response),
            Poll::Ready(Err(_)) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Future returned by [`TerminalQuerier::flush`].
pub struct PendingTerminalFlush {
    pub(super) receiver: oneshot::Receiver<()>,
}

impl Future for PendingTerminalFlush {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.get_mut().receiver).poll(cx) {
            Poll::Ready(_) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(super) enum PendingTerminalRequest {
    Query {
        matcher: TerminalQueryMatcher,
        sender: oneshot::Sender<Option<TerminalResponse>>,
    },
    Sentinel {
        sender: oneshot::Sender<()>,
    },
}

pub(super) fn dispatch_terminal_query_response(
    queue: &mut VecDeque<PendingTerminalRequest>,
    response: TerminalResponse,
) {
    if let Some(index) = queue.iter().position(|pending| match pending {
        PendingTerminalRequest::Query { matcher, .. } => matcher.matches(&response),
        PendingTerminalRequest::Sentinel { .. } => false,
    }) {
        if let Some(PendingTerminalRequest::Query { sender, .. }) = queue.remove(index) {
            let _ = sender.send(Some(response));
        }
        return;
    }

    if !matches!(response, TerminalResponse::Da1 { .. }) {
        return;
    }

    let Some(sentinel_index) = queue
        .iter()
        .position(|pending| matches!(pending, PendingTerminalRequest::Sentinel { .. }))
    else {
        return;
    };

    for _ in 0..=sentinel_index {
        match queue.pop_front() {
            Some(PendingTerminalRequest::Query { sender, .. }) => {
                let _ = sender.send(None);
            }
            Some(PendingTerminalRequest::Sentinel { sender }) => {
                let _ = sender.send(());
            }
            None => break,
        }
    }
}

/// Timeout-free terminal query coordinator.
///
/// This mirrors CC Ink's `TerminalQuerier`: queries and DA1 sentinels are queued
/// in write order, responses are delivered with [`Self::on_response`], and a
/// DA1 sentinel resolves earlier unanswered queries as unsupported instead of
/// relying on wall-clock timeouts.
pub struct TerminalQuerier<W> {
    output: W,
    queue: VecDeque<PendingTerminalRequest>,
}

impl<W: Write> TerminalQuerier<W> {
    /// Creates a new terminal querier that writes requests to `output`.
    pub fn new(output: W) -> Self {
        Self {
            output,
            queue: VecDeque::new(),
        }
    }

    /// Returns an immutable reference to the wrapped output writer.
    pub fn output_ref(&self) -> &W {
        &self.output
    }

    /// Returns a mutable reference to the wrapped output writer.
    pub fn output_mut(&mut self) -> &mut W {
        &mut self.output
    }

    /// Consumes the querier and returns the wrapped output writer.
    pub fn into_output(self) -> W {
        self.output
    }

    /// Sends a query and returns a future for its response.
    ///
    /// The future resolves to `None` when a later [`Self::flush`] sentinel
    /// arrives first, matching CC Ink's no-timeout unsupported-query behavior.
    pub fn send(&mut self, query: TerminalQuery) -> io::Result<PendingTerminalQuery> {
        let (sender, receiver) = oneshot::channel();
        self.queue.push_back(PendingTerminalRequest::Query {
            matcher: query.matcher.clone(),
            sender,
        });
        self.output.write_all(query.request.as_bytes())?;
        Ok(PendingTerminalQuery { receiver })
    }

    /// Sends the DA1 sentinel and returns a future that resolves when DA1 arrives.
    ///
    /// All unanswered queries queued before this sentinel resolve to `None` when
    /// the sentinel response is observed.
    pub fn flush(&mut self) -> io::Result<PendingTerminalFlush> {
        let (sender, receiver) = oneshot::channel();
        self.queue
            .push_back(PendingTerminalRequest::Sentinel { sender });
        self.output.write_all(da1_query_sequence().as_bytes())?;
        Ok(PendingTerminalFlush { receiver })
    }

    /// Dispatches a parsed terminal response event to the queued query batch.
    ///
    /// Returns `true` when `event` was a [`TerminalEvent::Response`] and was
    /// therefore handled by this querier.
    pub fn on_event(&mut self, event: &TerminalEvent) -> bool {
        if let TerminalEvent::Response(response) = event {
            self.on_response(response.clone());
            true
        } else {
            false
        }
    }

    /// Dispatches a parsed terminal response to the queued query batch.
    ///
    /// First matching query wins. If nothing matches and the response is DA1,
    /// the first pending sentinel is completed and all earlier queries resolve
    /// as unsupported.
    pub fn on_response(&mut self, response: TerminalResponse) {
        dispatch_terminal_query_response(&mut self.queue, response);
    }
}
