use std::sync::{Mutex, MutexGuard};

static IO_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub struct IoGuard {
    _g: Option<MutexGuard<'static, ()>>,
}

impl IoGuard {
    fn new() -> Self {
        let depth = DEPTH.with(|d| d.get());
        let g = if depth == 0 {
            Some(IO_LOCK.lock().unwrap_or_else(|e| e.into_inner()))
        } else {
            None
        };
        DEPTH.with(|d| d.set(depth + 1));
        IoGuard { _g: g }
    }
}

impl Drop for IoGuard {
    fn drop(&mut self) {
        let remaining = DEPTH.with(|d| d.get()).saturating_sub(1);
        DEPTH.with(|d| d.set(remaining));
        drop(self._g.take());
    }
}

pub fn io_lock_guard() -> IoGuard {
    IoGuard::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_guards_do_not_deadlock() {
        let _a = io_lock_guard();
        let _b = io_lock_guard();
        // both guards on same thread should not deadlock; second is reentrant
        assert!(true);
    }
}
