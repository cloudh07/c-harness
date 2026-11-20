//! The runner's stdout, shared by every thread that writes to the client.

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

use ch_protocol::runner::RunnerFrame;

#[derive(Clone)]
pub struct Out {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    writer: Box<dyn Write + Send>,
    broken: bool,
    /// Whether omp's lines may still go out. Cleared by the frame that reports omp ended, so
    /// nothing omp wrote can follow that frame.
    omp_open: bool,
}

impl Out {
    pub fn new(writer: impl Write + Send + 'static) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                writer: Box::new(writer),
                broken: false,
                omp_open: false,
            })),
        }
    }

    pub fn frame(&self, frame: &RunnerFrame) {
        self.lock().write_line(&frame.to_line());
    }

    pub fn omp_line(&self, line: &str) {
        let mut inner = self.lock();
        if inner.omp_open {
            inner.write_line(line);
        }
    }

    pub fn open_omp(&self) {
        self.lock().omp_open = true;
    }

    /// Writes `frame` and lets no omp line out after it.
    pub fn close_omp_with(&self, frame: &RunnerFrame) {
        let mut inner = self.lock();
        inner.omp_open = false;
        inner.write_line(&frame.to_line());
    }

    pub fn close_omp(&self) {
        self.lock().omp_open = false;
    }

    /// Whether the client has stopped reading.
    pub fn is_broken(&self) -> bool {
        self.lock().broken
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Inner {
    fn write_line(&mut self, line: &str) {
        if self.broken {
            return;
        }
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        if self
            .writer
            .write_all(&bytes)
            .and_then(|()| self.writer.flush())
            .is_err()
        {
            self.broken = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[derive(Clone, Default)]
    struct Shared(Arc<StdMutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn omp_lines_pass_only_while_omp_is_open() {
        let shared = Shared::default();
        let out = Out::new(shared.clone());
        out.omp_line("{\"type\":\"early\"}");
        out.open_omp();
        out.omp_line("{\"type\":\"ready\"}");
        out.close_omp_with(&RunnerFrame::Stopped {
            id: "s".into(),
            exit_code: Some(0),
        });
        out.omp_line("{\"type\":\"late\"}");
        let text = String::from_utf8(shared.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(lines[0], "{\"type\":\"ready\"}");
        assert!(lines[1].contains("ch_stopped"));
    }

    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn notices_a_client_that_stopped_reading() {
        let out = Out::new(Closed);
        assert!(!out.is_broken());
        out.frame(&RunnerFrame::Notice {
            kind: "k".into(),
            message: "m".into(),
        });
        assert!(out.is_broken());
    }
}
