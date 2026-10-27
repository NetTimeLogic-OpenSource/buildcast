use std::io::{self, Read};
use std::sync::mpsc;

use crate::Error;
use crate::disks::AlignedBuf;
use crate::stream::fill;

const DEPTH: usize = 3;

/// Decompresses on its own thread so decoding overlaps device I/O.
///
/// Buffers rotate through a pool channel. Dropping the decoder stops the
/// thread.
pub(super) struct Decoder {
    data: mpsc::Receiver<io::Result<Chunk>>,
    pool: mpsc::SyncSender<AlignedBuf>,
}

/// Decoded data. A `len` of 0 marks the end of the stream.
pub(super) struct Chunk {
    pub(super) buf: AlignedBuf,
    pub(super) len: usize,
}

impl Decoder {
    pub(super) fn next(&self) -> Result<Chunk, Error> {
        match self.data.recv() {
            Ok(chunk) => Ok(chunk?),
            Err(_) => Err(Error::DecoderStopped),
        }
    }

    pub(super) fn recycle(&self, buf: AlignedBuf) {
        let _ = self.pool.try_send(buf);
    }

    pub(super) fn spawn(mut reader: Box<dyn Read + Send>, buf_bytes: usize) -> Decoder {
        let (data_tx, data) = mpsc::sync_channel::<io::Result<Chunk>>(DEPTH);
        let (pool, pool_rx) = mpsc::sync_channel::<AlignedBuf>(DEPTH);
        for _ in 0..DEPTH {
            pool.send(AlignedBuf::new(buf_bytes)).unwrap();
        }
        std::thread::spawn(move || {
            while let Ok(mut buf) = pool_rx.recv() {
                let (msg, more) = match fill(&mut reader, buf.as_mut_slice()) {
                    Ok(0) => (Ok(Chunk { buf, len: 0 }), false),
                    Ok(len) => (Ok(Chunk { buf, len }), true),
                    Err(err) => (Err(err), false),
                };
                if data_tx.send(msg).is_err() || !more {
                    break;
                }
            }
        });
        Decoder { data, pool }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn chunks_then_eof() {
        let data = vec![3u8; 10 * 1024];
        let dec = Decoder::spawn(Box::new(Cursor::new(data)), 4096);
        let mut lens = Vec::new();
        loop {
            let c = dec.next().unwrap();
            if c.len == 0 {
                break;
            }
            if c.len == 2048 {
                assert_eq!(&c.buf.as_slice()[..8], &[3u8; 8]);
            }
            lens.push(c.len);
            // the decoder needs a free buffer to detect EOF
            dec.recycle(c.buf);
        }
        assert_eq!(lens, vec![4096, 4096, 2048]);
        assert!(matches!(dec.next(), Err(Error::DecoderStopped)));
    }

    #[test]
    fn propagates_error() {
        struct Bad;

        impl Read for Bad {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("boom"))
            }
        }

        let dec = Decoder::spawn(Box::new(Bad), 4096);
        let Err(err) = dec.next() else {
            panic!("expected error");
        };
        assert!(err.to_string().contains("boom"));
    }

    #[test]
    fn pool_recycles() {
        let data: Vec<u8> = (0..20480u32).map(|i| i as u8).collect();
        let dec = Decoder::spawn(Box::new(Cursor::new(data.clone())), 4096);
        let mut out = Vec::new();
        loop {
            let ch = dec.next().unwrap();
            if ch.len == 0 {
                break;
            }
            out.extend_from_slice(&ch.buf.as_slice()[..ch.len]);
            dec.recycle(ch.buf);
        }
        assert_eq!(out, data);
    }
}
