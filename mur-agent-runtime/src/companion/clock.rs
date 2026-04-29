//! Companion-scoped clock abstraction (production = SystemClock; tests = MockClock).

use chrono::{DateTime, Local, Utc};

pub trait Clock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
    fn now_local(&self) -> DateTime<Local>;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn now_local(&self) -> DateTime<Local> {
        Local::now()
    }
}

pub struct MockClock {
    base: DateTime<Utc>,
    offset: std::sync::Mutex<chrono::Duration>,
}

impl MockClock {
    pub fn at(base: DateTime<Utc>) -> Self {
        Self {
            base,
            offset: std::sync::Mutex::new(chrono::Duration::zero()),
        }
    }
    pub fn advance(&self, d: chrono::Duration) {
        let mut o = self.offset.lock().unwrap();
        *o += d;
    }
}

impl Clock for MockClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.base + *self.offset.lock().unwrap()
    }
    fn now_local(&self) -> DateTime<Local> {
        self.now_utc().with_timezone(&Local)
    }
}
