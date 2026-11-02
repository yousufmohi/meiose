//! Newline-delimited framing for the MCP stdio transport.
//!
//! A frame is the exact bytes of one message *including* its trailing `\n`
//! (and a `\r` before it, if the sender wrote one). Frames are forwarded as
//! opaque bytes; nothing here parses JSON, so nothing here can alter a message.

use std::io;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// Read capacity for every stream we pump. Messages larger than this still
/// work (the frame buffer grows); this only sets the syscall granularity.
pub const READ_BUF: usize = 64 * 1024;

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

/// Drain `rx` into `w`, flushing whenever no further frame is immediately
/// queued. This task is the *only* writer of its stream, so frames from
/// different producers can never interleave.
pub async fn write_frames<W: AsyncWrite + Unpin>(
    mut w: W,
    mut rx: mpsc::Receiver<Vec<u8>>,
) -> io::Result<()> {
    while let Some(frame) = rx.recv().await {
        w.write_all(&frame).await?;
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
        let got = read_frame(&mut r).await.unwrap().unwrap();
        writer.await.unwrap();
        assert_eq!(got.len(), big.len());
        assert!(got == big);
        assert!(read_frame(&mut r).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn writer_preserves_order_and_bytes() {
        let (tx, rx) = mpsc::channel(4);
        let mut sink = Vec::new();
        let producer = tokio::spawn(async move {
            for i in 0..100 {
                tx.send(format!("{{\"id\":{i}}}\n").into_bytes())
                    .await
                    .unwrap();
            }
        });
        write_frames(&mut sink, rx).await.unwrap();
        producer.await.unwrap();
        let expected: String = (0..100).map(|i| format!("{{\"id\":{i}}}\n")).collect();
        assert_eq!(String::from_utf8(sink).unwrap(), expected);
    }
}
