//! Newline-delimited framing for the MCP stdio transport.
//!
//! A frame is the exact bytes of one message *including* its trailing `\n`
//! (and a `\r` before it, if the sender wrote one). Frames are forwarded as
//! opaque bytes; nothing here parses JSON, so nothing here can alter a message.
//!
//! Frames are buffered whole only up to [`max_frame`]. A longer frame is
//! oversized: its first part is returned as [`Frame::Oversized`] and the
//! rest is streamed in chunks with [`next_chunk`], so a hostile or broken
//! server cannot make meiose hold hundreds of megabytes. Oversized frames
//! are forwarded exactly but never parsed, trimmed, stored or recorded.

use std::io;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// Read capacity for every stream we pump. This only sets the syscall
/// granularity (and the size of an oversized frame's chunks).
pub const READ_BUF: usize = 64 * 1024;

/// Largest frame meiose buffers and processes: 16 MiB, 30 times the
/// largest real Playwright page seen (506K characters).
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// [`MAX_FRAME`]; debug builds read `MEIOSE_TEST_MAX_FRAME_KB` so tests can
/// use small frames.
pub fn max_frame() -> usize {
    #[cfg(debug_assertions)]
    if let Some(kb) = std::env::var("MEIOSE_TEST_MAX_FRAME_KB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        return kb * 1024;
    }
    MAX_FRAME
}

#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    /// A whole frame, including its line terminator (if the stream had one).
    Whole(Vec<u8>),
    /// The first bytes of a frame longer than the limit; the rest is still
    /// unread: call [`next_chunk`] until it says the frame ended.
    Oversized(Vec<u8>),
}

/// Read one frame. Returns `None` at end of stream.
///
/// If the stream ends without a trailing newline, the partial frame is
/// returned as-is so no bytes are ever lost.
pub async fn read_frame<R: AsyncBufRead + Unpin>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    match r.read_until(b'\n', &mut buf).await? {
        0 => Ok(None),
        _ => Ok(Some(buf)),
    }
}

/// Read one frame, buffering at most about `max` bytes of it.
pub async fn read_frame_limited<R: AsyncBufRead + Unpin>(
    r: &mut R,
    max: usize,
) -> io::Result<Option<Frame>> {
    let mut buf = Vec::new();
    loop {
        let (chunk, ended) = match next_chunk(r).await? {
            Some(c) => c,
            None if buf.is_empty() => return Ok(None),
            None => return Ok(Some(Frame::Whole(buf))),
        };
        buf.extend_from_slice(&chunk);
        if ended {
            return Ok(Some(Frame::Whole(buf)));
        }
        if buf.len() > max {
            return Ok(Some(Frame::Oversized(buf)));
        }
    }
}

/// The next bytes of the current frame, up to and including its newline.
/// `(chunk, true)` when the frame ended; `None` at end of stream.
pub async fn next_chunk<R: AsyncBufRead + Unpin>(r: &mut R) -> io::Result<Option<(Vec<u8>, bool)>> {
    let avail = r.fill_buf().await?;
    if avail.is_empty() {
        return Ok(None);
    }
    let (n, ended) = match avail.iter().position(|&b| b == b'\n') {
        Some(i) => (i + 1, true),
        None => (avail.len(), false),
    };
    let chunk = avail[..n].to_vec();
    r.consume(n);
    Ok(Some((chunk, ended)))
}

/// What the single stdout writer sends.
pub enum Out {
    Frame(Vec<u8>),
    /// An oversized frame, in chunks. The writer sends every chunk before
    /// anything else, so nothing can be interleaved inside it.
    Stream(mpsc::Receiver<Vec<u8>>),
}

/// Drain `rx` into `w`, flushing whenever no further frame is immediately
/// queued. This task is the *only* writer of its stream, so frames from
/// different producers can never interleave.
pub async fn write_frames<W: AsyncWrite + Unpin>(
    mut w: W,
    mut rx: mpsc::Receiver<Out>,
) -> io::Result<()> {
    while let Some(out) = rx.recv().await {
        match out {
            Out::Frame(frame) => w.write_all(&frame).await?,
            Out::Stream(mut chunks) => {
                while let Some(c) = chunks.recv().await {
                    w.write_all(&c).await?;
                }
            }
        }
        if rx.is_empty() {
            w.flush().await?;
        }
    }
    w.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, BufReader};

    async fn frames_of(input: &[u8]) -> Vec<Vec<u8>> {
        let mut r = BufReader::new(input);
        let mut out = Vec::new();
        while let Some(f) = read_frame(&mut r).await.unwrap() {
            out.push(f);
        }
        out
    }

    #[tokio::test]
    async fn splits_on_newline_and_keeps_terminators() {
        let frames = frames_of(b"{\"a\":1}\n{\"b\":2}\r\n").await;
        assert_eq!(frames, [b"{\"a\":1}\n".to_vec(), b"{\"b\":2}\r\n".to_vec()]);
    }

    #[tokio::test]
    async fn unterminated_final_frame_is_not_lost() {
        let frames = frames_of(b"{\"a\":1}\n{\"tail\":true}").await;
        assert_eq!(frames[1], b"{\"tail\":true}");
    }

    #[tokio::test]
    async fn empty_stream_yields_nothing() {
        assert!(frames_of(b"").await.is_empty());
    }

    #[tokio::test]
    async fn frames_concatenate_back_to_the_exact_input() {
        let input = b"x\n\n  \r\n{\"k\":\"\\u00e9\xc3\xa9\"}\nlast";
        assert_eq!(frames_of(input).await.concat(), input);
    }

    #[tokio::test]
    async fn five_megabyte_frame_survives_chunked_delivery() {
        let mut big = vec![b'{'];
        big.extend(std::iter::repeat_n(b'a', 5 * 1024 * 1024));
        big.extend_from_slice(b"}\n");

        // Deliver through a small pipe in odd-sized chunks.
        let (mut tx, rx) = tokio::io::duplex(1000);
        let send = big.clone();
        let writer = tokio::spawn(async move {
            for chunk in send.chunks(7919) {
                tx.write_all(chunk).await.unwrap();
            }
        });
        let mut r = BufReader::with_capacity(READ_BUF, rx);
        let got = read_frame_limited(&mut r, MAX_FRAME)
            .await
            .unwrap()
            .unwrap();
        writer.await.unwrap();
        assert!(got == Frame::Whole(big));
        assert!(read_frame(&mut r).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_oversized_frame_comes_back_in_bounded_chunks_and_whole() {
        let mut input = b"small\n".to_vec();
        let mut big = vec![b'x'; 300_000];
        big.push(b'\n');
        input.extend_from_slice(&big);
        input.extend_from_slice(b"after\nno-newline");
        let mut r = BufReader::with_capacity(4096, &input[..]);

        let small = read_frame_limited(&mut r, 10_000).await.unwrap().unwrap();
        assert_eq!(small, Frame::Whole(b"small\n".to_vec()));
        let Frame::Oversized(mut got) = read_frame_limited(&mut r, 10_000).await.unwrap().unwrap()
        else {
            panic!("expected an oversized frame")
        };
        assert!(got.len() <= 10_000 + 4096);
        loop {
            let (c, ended) = next_chunk(&mut r).await.unwrap().unwrap();
            assert!(c.len() <= 4096);
            got.extend_from_slice(&c);
            if ended {
                break;
            }
        }
        assert!(got == big);
        let after = read_frame_limited(&mut r, 10_000).await.unwrap().unwrap();
        assert_eq!(after, Frame::Whole(b"after\n".to_vec()));
        let tail = read_frame_limited(&mut r, 10_000).await.unwrap().unwrap();
        assert_eq!(tail, Frame::Whole(b"no-newline".to_vec()));
        assert!(read_frame_limited(&mut r, 10_000).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn writer_preserves_order_and_bytes() {
        let (tx, rx) = mpsc::channel(4);
        let mut sink = Vec::new();
        let producer = tokio::spawn(async move {
            for i in 0..100 {
                tx.send(Out::Frame(format!("{{\"id\":{i}}}\n").into_bytes()))
                    .await
                    .unwrap();
            }
        });
        write_frames(&mut sink, rx).await.unwrap();
        producer.await.unwrap();
        let expected: String = (0..100).map(|i| format!("{{\"id\":{i}}}\n")).collect();
        assert_eq!(String::from_utf8(sink).unwrap(), expected);
    }

    #[tokio::test]
    async fn a_stream_is_written_whole_before_the_next_frame() {
        let (tx, rx) = mpsc::channel(4);
        let (ctx, crx) = mpsc::channel(1);
        let mut sink = Vec::new();
        let producer = tokio::spawn(async move {
            tx.send(Out::Stream(crx)).await.unwrap();
            // Queued while the stream is still being written.
            tx.send(Out::Frame(b"next\n".to_vec())).await.unwrap();
            for c in [&b"big "[..], b"frame", b"\n"] {
                ctx.send(c.to_vec()).await.unwrap();
            }
        });
        write_frames(&mut sink, rx).await.unwrap();
        producer.await.unwrap();
        assert_eq!(sink, b"big frame\nnext\n");
    }
}
