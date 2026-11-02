//! A sans-IO SMTP session: feed it bytes, it appends replies to a buffer.
//!
//! Keeping the protocol free of sockets means every edge case (pipelining,
//! a terminator split across reads, oversized lines) is testable directly.

pub mod server;

/// RFC 5321 allows 512 bytes for a command line, but extensions push it higher.
const MAX_COMMAND_LINE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Waiting for HELO or EHLO.
    Connected,
    /// Greeted, waiting for MAIL FROM.
    Ready,
    /// MAIL accepted, waiting for RCPT TO.
    Mail,
    /// At least one RCPT accepted, DATA now allowed.
    Rcpt,
    /// Reading the message body.
    Data,
    Closed,
}

/// How far through the `\r\n.\r\n` terminator the body currently is.
/// A message starts as if a CRLF had just been seen, so an empty body works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Terminator {
    None,
    Cr,
    CrLf,
    CrLfDot,
    CrLfDotCr,
}

#[derive(Debug)]
pub struct Session {
    state: State,
    terminator: Terminator,
    line: Vec<u8>,
    line_overflowed: bool,
    ehlo_reply: Vec<u8>,
    helo_reply: Vec<u8>,
    pub messages: u64,
    pub body_bytes: u64,
}

impl Session {
    pub fn new(hostname: &str) -> Self {
        Session {
            state: State::Connected,
            terminator: Terminator::CrLf,
            line: Vec::with_capacity(256),
            line_overflowed: false,
            ehlo_reply: format!(
                "250-{hostname}\r\n250-PIPELINING\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n"
            )
            .into_bytes(),
            helo_reply: format!("250 {hostname}\r\n").into_bytes(),
            messages: 0,
            body_bytes: 0,
        }
    }

    pub fn greeting(hostname: &str) -> Vec<u8> {
        format!("220 {hostname} ESMTP rsmtp-sink\r\n").into_bytes()
    }

    pub fn is_closed(&self) -> bool {
        self.state == State::Closed
    }

    /// Consume `input`, appending any replies to `out`. Pipelined commands in
    /// a single read produce a single batch of replies.
    pub fn feed(&mut self, mut input: &[u8], out: &mut Vec<u8>) {
        while !input.is_empty() && self.state != State::Closed {
            if self.state == State::Data {
                input = self.consume_body(input, out);
            } else {
                input = self.consume_command_bytes(input, out);
            }
        }
    }

    fn consume_body<'a>(&mut self, input: &'a [u8], out: &mut Vec<u8>) -> &'a [u8] {
        for (i, &b) in input.iter().enumerate() {
            self.terminator = match (self.terminator, b) {
                (Terminator::CrLfDotCr, b'\n') => {
                    self.body_bytes += i as u64 + 1;
                    self.messages += 1;
                    self.state = State::Ready;
                    self.terminator = Terminator::CrLf;
                    out.extend_from_slice(b"250 2.0.0 Ok: queued\r\n");
                    return &input[i + 1..];
                }
                (Terminator::CrLf, b'.') => Terminator::CrLfDot,
                (Terminator::CrLfDot, b'\r') => Terminator::CrLfDotCr,
                (Terminator::Cr, b'\n') => Terminator::CrLf,
                (_, b'\r') => Terminator::Cr,
                _ => Terminator::None,
            };
        }
        self.body_bytes += input.len() as u64;
        &[]
    }

    fn consume_command_bytes<'a>(&mut self, input: &'a [u8], out: &mut Vec<u8>) -> &'a [u8] {
        let Some(newline) = input.iter().position(|&b| b == b'\n') else {
            self.buffer_line(input);
            return &[];
        };
        let (head, rest) = input.split_at(newline + 1);
        if self.line.is_empty() && !self.line_overflowed {
            let (state, reply) = dispatch(self.state, head);
            self.apply(state, reply, out);
        } else {
            self.buffer_line(head);
            if self.line_overflowed {
                self.line.clear();
                self.line_overflowed = false;
                out.extend_from_slice(b"500 5.5.2 Line too long\r\n");
            } else {
                let line = std::mem::take(&mut self.line);
                let (state, reply) = dispatch(self.state, &line);
                self.apply(state, reply, out);
                self.line = line;
                self.line.clear();
            }
        }
        rest
    }

    fn buffer_line(&mut self, bytes: &[u8]) {
        if self.line_overflowed {
            return;
        }
        if self.line.len() + bytes.len() > MAX_COMMAND_LINE {
            self.line.clear();
            self.line_overflowed = true;
        } else {
            self.line.extend_from_slice(bytes);
        }
    }

    fn apply(&mut self, state: State, reply: Reply, out: &mut Vec<u8>) {
        self.state = state;
        match reply {
            Reply::Static(bytes) => out.extend_from_slice(bytes),
            Reply::Ehlo => out.extend_from_slice(&self.ehlo_reply),
            Reply::Helo => out.extend_from_slice(&self.helo_reply),
        }
        if state == State::Data {
            self.terminator = Terminator::CrLf;
        }
    }
}

enum Reply {
    Static(&'static [u8]),
    Ehlo,
    Helo,
}

const OK: Reply = Reply::Static(b"250 2.0.0 Ok\r\n");
const BAD_SEQUENCE: &[u8] = b"503 5.5.1 Bad sequence of commands\r\n";

fn dispatch(state: State, line: &[u8]) -> (State, Reply) {
    let verb = line.get(..4).unwrap_or(line);
    let has_args_or_ends = |line: &[u8]| matches!(line.get(4), Some(b' ' | b'\r' | b'\n'));

    if !has_args_or_ends(line) {
        return (state, Reply::Static(b"500 5.5.2 Unknown command\r\n"));
    }

    if verb.eq_ignore_ascii_case(b"MAIL") {
        if state != State::Ready {
            return (state, Reply::Static(BAD_SEQUENCE));
        }
        if !starts_with_ignore_case(&line[5..], b"FROM:") {
            return (state, Reply::Static(b"501 5.5.4 Syntax: MAIL FROM:<address>\r\n"));
        }
        (State::Mail, OK)
    } else if verb.eq_ignore_ascii_case(b"RCPT") {
        if state != State::Mail && state != State::Rcpt {
            return (state, Reply::Static(BAD_SEQUENCE));
        }
        if !starts_with_ignore_case(&line[5..], b"TO:") {
            return (state, Reply::Static(b"501 5.5.4 Syntax: RCPT TO:<address>\r\n"));
        }
        (State::Rcpt, OK)
    } else if verb.eq_ignore_ascii_case(b"DATA") {
        if state != State::Rcpt {
            return (state, Reply::Static(BAD_SEQUENCE));
        }
        (State::Data, Reply::Static(b"354 End data with <CR><LF>.<CR><LF>\r\n"))
    } else if verb.eq_ignore_ascii_case(b"EHLO") {
        (State::Ready, Reply::Ehlo)
    } else if verb.eq_ignore_ascii_case(b"HELO") {
        (State::Ready, Reply::Helo)
    } else if verb.eq_ignore_ascii_case(b"RSET") {
        let next = if state == State::Connected { state } else { State::Ready };
        (next, OK)
    } else if verb.eq_ignore_ascii_case(b"NOOP") {
        (state, OK)
    } else if verb.eq_ignore_ascii_case(b"VRFY") {
        (state, Reply::Static(b"252 2.1.5 Cannot verify\r\n"))
    } else if verb.eq_ignore_ascii_case(b"QUIT") {
        (State::Closed, Reply::Static(b"221 2.0.0 Bye\r\n"))
    } else {
        (state, Reply::Static(b"500 5.5.2 Unknown command\r\n"))
    }
}

fn starts_with_ignore_case(haystack: &[u8], prefix: &[u8]) -> bool {
    haystack
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&[u8]]) -> (Session, String) {
        let mut session = Session::new("test");
        let mut out = Vec::new();
        for chunk in chunks {
            session.feed(chunk, &mut out);
        }
        (session, String::from_utf8(out).unwrap())
    }

    const ENVELOPE: &[u8] = b"EHLO c\r\nMAIL FROM:<a@b>\r\nRCPT TO:<c@d>\r\nDATA\r\n";

    #[test]
    fn full_transaction() {
        let (s, out) = run(&[ENVELOPE, b"hello\r\n.\r\n", b"QUIT\r\n"]);
        assert_eq!(s.messages, 1);
        assert!(s.is_closed());
        assert!(out.contains("250-PIPELINING"));
        assert!(out.contains("354 "));
        assert!(out.contains("queued"));
        assert!(out.ends_with("221 2.0.0 Bye\r\n"));
    }

    #[test]
    fn pipelined_commands_each_get_a_reply_in_order() {
        let (s, out) = run(&[
            b"HELO c\r\nMAIL FROM:<a>\r\nRCPT TO:<b>\r\nDATA\r\nx\r\n.\r\nQUIT\r\n",
        ]);
        let codes: Vec<&str> = out.lines().map(|l| &l[..3]).collect();
        assert_eq!(codes, ["250", "250", "250", "354", "250", "221"]);
        assert_eq!(s.messages, 1);
    }

    #[test]
    fn terminator_split_across_every_boundary() {
        let whole = b"body\r\n.\r\n";
        for split in 0..whole.len() {
            let (s, _) = run(&[ENVELOPE, &whole[..split], &whole[split..]]);
            assert_eq!(s.messages, 1, "split at {split}");
        }
    }

    #[test]
    fn command_split_byte_by_byte() {
        let script = b"EHLO c\r\nMAIL FROM:<a>\r\nQUIT\r\n";
        let chunks: Vec<&[u8]> = script.chunks(1).collect();
        let (s, out) = run(&chunks);
        assert!(s.is_closed());
        assert!(out.ends_with("221 2.0.0 Bye\r\n"));
    }

    #[test]
    fn dot_stuffed_line_does_not_end_message() {
        let (s, _) = run(&[ENVELOPE, b"..not the end\r\n.x\r\nstill body\r\n"]);
        assert_eq!(s.messages, 0);
    }

    #[test]
    fn dot_inside_a_line_does_not_end_message() {
        let (s, _) = run(&[ENVELOPE, b"a.\r\nb\r\n"]);
        assert_eq!(s.messages, 0);
    }

    #[test]
    fn empty_body_is_accepted() {
        let (s, _) = run(&[ENVELOPE, b".\r\n"]);
        assert_eq!(s.messages, 1);
    }

    #[test]
    fn cr_repeated_before_terminator_is_handled() {
        let (s, _) = run(&[ENVELOPE, b"a\r\r\n.\r\n"]);
        assert_eq!(s.messages, 1);
    }

    #[test]
    fn several_messages_on_one_connection() {
        let (s, _) = run(&[
            ENVELOPE,
            b"one\r\n.\r\nMAIL FROM:<a>\r\nRCPT TO:<b>\r\nDATA\r\ntwo\r\n.\r\n",
        ]);
        assert_eq!(s.messages, 2);
    }

    #[test]
    fn out_of_order_commands_are_rejected() {
        let (_, out) = run(&[b"MAIL FROM:<a>\r\n"]);
        assert!(out.starts_with("503"));
        let (_, out) = run(&[b"EHLO c\r\nDATA\r\n"]);
        assert!(out.ends_with("503 5.5.1 Bad sequence of commands\r\n"));
        let (_, out) = run(&[b"EHLO c\r\nRCPT TO:<a>\r\n"]);
        assert!(out.ends_with("503 5.5.1 Bad sequence of commands\r\n"));
    }

    #[test]
    fn rset_clears_the_envelope() {
        let (_, out) = run(&[b"EHLO c\r\nMAIL FROM:<a>\r\nRSET\r\nDATA\r\n"]);
        assert!(out.ends_with("503 5.5.1 Bad sequence of commands\r\n"));
    }

    #[test]
    fn commands_are_case_insensitive() {
        let (_, out) = run(&[b"ehlo c\r\nmail from:<a>\r\nrcpt to:<b>\r\ndata\r\n"]);
        assert!(out.ends_with("354 End data with <CR><LF>.<CR><LF>\r\n"));
    }

    #[test]
    fn malformed_input_never_panics() {
        for input in [
            &b"\r\n"[..],
            b"\n",
            b"MAIL\r\n",
            b"RCPT\r\n",
            b"MAIL\n",
            b"M\r\n",
            b"\xff\xfe\r\n",
            b"EHLOX\r\n",
        ] {
            let (s, out) = run(&[b"EHLO c\r\n", input]);
            assert!(!s.is_closed());
            assert!(out.lines().count() >= 4, "{input:?}");
        }
    }

    #[test]
    fn oversized_line_is_rejected_and_session_recovers() {
        let huge = vec![b'A'; MAX_COMMAND_LINE * 3];
        let (_, out) = run(&[&huge, b"\r\n", b"NOOP\r\n"]);
        assert!(out.starts_with("500 5.5.2 Line too long"));
        assert!(out.ends_with("250 2.0.0 Ok\r\n"));
    }

    #[test]
    fn bytes_after_quit_are_ignored() {
        let (s, out) = run(&[b"QUIT\r\nNOOP\r\n"]);
        assert!(s.is_closed());
        assert_eq!(out, "221 2.0.0 Bye\r\n");
    }
}
