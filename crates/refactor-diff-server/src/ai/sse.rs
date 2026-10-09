//! Streaming response bodies as lines (NDJSON, `data:` lines) or Server-Sent Events.

use eventsource_stream::{Event, EventStreamError, Eventsource};
use futures::{Stream, StreamExt};

/// The body as lines: split on `\n`, the newline (and a `\r` before it) removed, decoded
/// lossily. A final line without a newline is still delivered.
pub fn lines(res: reqwest::Response) -> impl Stream<Item = Result<String, reqwest::Error>> + Send {
    let mut body = res.bytes_stream();
    async_stream::stream! {
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };
            buf.extend_from_slice(&chunk);
            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=nl).collect();
                yield Ok(decode(&line[..nl]));
            }
        }
        if !buf.is_empty() {
            yield Ok(decode(&buf));
        }
    }
}

fn decode(line: &[u8]) -> String {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8_lossy(line).into_owned()
}

/// The body as SSE events (`event:`/`data:` blocks separated by blank lines).
pub fn events(
    res: reqwest::Response,
) -> impl Stream<Item = Result<Event, EventStreamError<reqwest::Error>>> + Send {
    res.bytes_stream().eventsource()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_strips_a_carriage_return() {
        assert_eq!(decode(b"abc\r"), "abc");
        assert_eq!(decode(b"abc"), "abc");
        assert_eq!(decode(b"\xff"), "\u{fffd}");
    }
}
