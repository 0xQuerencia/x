//! Incremental frames owned by the supervisor. A blocked stdout or stderr
//! pipe never requires a detached reader thread.

use std::io::{self, BufRead, BufReader, Read};

use crate::desktop::capture::{OutputPipe, prepare_pipe, read_ready};

const MAX_FRAME_BYTES: usize = 1024 * 1024;

pub(super) struct PollPipe<P>(P);

impl<P: OutputPipe> PollPipe<P> {
    pub(super) fn new(pipe: P) -> io::Result<Self> {
        prepare_pipe(&pipe)?;
        Ok(Self(pipe))
    }
}

impl<P: OutputPipe> Read for PollPipe<P> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        read_ready(&mut self.0, buffer)
    }
}

pub(super) enum Frame {
    Line(String),
    Progress,
    Pending,
    Closed,
}

pub(super) struct Frames<R> {
    reader: BufReader<R>,
    partial: Vec<u8>,
}

impl<R: Read> Frames<R> {
    pub(super) fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            partial: Vec::new(),
        }
    }

    fn poll(&mut self) -> io::Result<Frame> {
        let available = match self.reader.fill_buf() {
            Ok(data) => data,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(Frame::Pending);
            }
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return if self.partial.is_empty() {
                Ok(Frame::Closed)
            } else {
                self.finish()
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if self.partial.len() + consumed > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Harness output frame exceeds limit",
            ));
        }
        self.partial.extend_from_slice(&available[..consumed]);
        self.reader.consume(consumed);
        if newline.is_some() {
            self.finish()
        } else {
            Ok(Frame::Progress)
        }
    }

    fn finish(&mut self) -> io::Result<Frame> {
        let bytes = std::mem::take(&mut self.partial);
        String::from_utf8(bytes)
            .map(Frame::Line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

pub(super) fn read_frame<R: Read>(stream: &mut Option<Frames<R>>) -> io::Result<Frame> {
    let Some(reader) = stream.as_mut() else {
        return Ok(Frame::Closed);
    };
    let frame = reader.poll()?;
    if matches!(frame, Frame::Closed) {
        stream.take();
    }
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn dropping_pending_frames_closes_the_pipe_with_a_live_writer() {
        use std::io::Write;

        let (read, mut write) = std::io::pipe().unwrap();
        let mut frames = Some(Frames::new(PollPipe::new(read).unwrap()));
        assert!(matches!(read_frame(&mut frames).unwrap(), Frame::Pending));
        drop(frames);
        assert_eq!(
            write.write_all(b"after owner returns").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn frames_refuse_oversized_input_before_retaining_more_than_the_limit() {
        let mut reader = Frames::new(Cursor::new(vec![b'a'; MAX_FRAME_BYTES + 1]));
        loop {
            match reader.poll() {
                Ok(Frame::Progress) => {}
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                    assert_eq!(reader.partial.len(), MAX_FRAME_BYTES);
                    break;
                }
                _ => panic!("an oversized unterminated frame must never be delivered"),
            }
        }
    }

    #[test]
    fn frames_preserve_partial_final_lines_and_reject_invalid_utf8() {
        let mut input = Some(Frames::new(Cursor::new(b"one\ntwo")));
        assert!(matches!(read_frame(&mut input).unwrap(), Frame::Line(text) if text == "one\n"));
        assert!(matches!(read_frame(&mut input).unwrap(), Frame::Progress));
        assert!(matches!(read_frame(&mut input).unwrap(), Frame::Line(text) if text == "two"));
        assert!(matches!(read_frame(&mut input).unwrap(), Frame::Closed));
        assert!(input.is_none());
        let mut invalid = Frames::new(Cursor::new([0xff, b'\n']));
        assert_eq!(
            invalid.poll().err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
