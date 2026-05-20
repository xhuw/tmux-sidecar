use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event as CrosstermEvent, KeyEvent, MouseEvent};

#[derive(Debug, Clone)]
pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Tick,
}

impl From<CrosstermEvent> for AppEvent {
    fn from(value: CrosstermEvent) -> Self {
        match value {
            CrosstermEvent::Key(key) => Self::Key(key),
            CrosstermEvent::Mouse(mouse) => Self::Mouse(mouse),
            CrosstermEvent::Resize(width, height) => Self::Resize(width, height),
            _ => Self::Tick,
        }
    }
}

pub fn poll_next(timeout: Duration) -> Result<AppEvent> {
    if event::poll(timeout)? {
        Ok(AppEvent::from(event::read()?))
    } else {
        Ok(AppEvent::Tick)
    }
}
