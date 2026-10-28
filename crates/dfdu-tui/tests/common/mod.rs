//! What the run-loop tests share: a `TestBackend` that reports every flush.

use std::convert::Infallible;
use std::io;

use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Size};

/// `TestBackend`'s results as the run loop's backend error type; it never fails.
fn never<T>(r: Result<T, Infallible>) -> io::Result<T> {
    r.map_err(|e| match e {})
}

/// `TestBackend` that passes the buffer to `on_flush` each time it is flushed; ratatui flushes
/// once per `Terminal::draw`.
pub struct Recording<F: FnMut(&Buffer)> {
    inner: TestBackend,
    on_flush: F,
}

impl<F: FnMut(&Buffer)> Recording<F> {
    pub fn new(width: u16, height: u16, on_flush: F) -> Self {
        Recording {
            inner: TestBackend::new(width, height),
            on_flush,
        }
    }
}

impl<F: FnMut(&Buffer)> Backend for Recording<F> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        never(self.inner.draw(content))
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        never(self.inner.hide_cursor())
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        never(self.inner.show_cursor())
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        never(self.inner.get_cursor_position())
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, p: P) -> io::Result<()> {
        never(self.inner.set_cursor_position(p))
    }
    fn clear(&mut self) -> io::Result<()> {
        never(self.inner.clear())
    }
    fn clear_region(&mut self, t: ClearType) -> io::Result<()> {
        never(self.inner.clear_region(t))
    }
    fn size(&self) -> io::Result<Size> {
        never(self.inner.size())
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        never(self.inner.window_size())
    }
    fn flush(&mut self) -> io::Result<()> {
        never(self.inner.flush())?;
        (self.on_flush)(self.inner.buffer());
        Ok(())
    }
}
