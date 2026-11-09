//! Request cancellation closes its dedicated attachment (Firebird fb_cancel_abort).
use crate::error::PluginError;
use std::{
    cell::RefCell,
    net::{Shutdown, TcpStream},
    sync::{
        atomic::{AtomicU8, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
};

#[derive(Default)]
pub struct Control {
    reason: AtomicU8,
    socket: Mutex<Option<TcpStream>>,
}

impl Control {
    pub fn cancel(&self) {
        self.stop(1);
    }

    fn stop(&self, reason: u8) {
        if self
            .reason
            .compare_exchange(0, reason, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            if let Some(socket) = self.socket.lock().unwrap().as_ref() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
    }

    pub fn attach(&self, socket: &TcpStream) -> Result<(), PluginError> {
        *self.socket.lock().unwrap() = Some(
            socket
                .try_clone()
                .map_err(|e| PluginError::internal(e.to_string()))?,
        );
        if self.check().is_err() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        self.check()
    }

    /// A completed request must no longer be able to cancel an idle session.
    pub fn detach(&self) {
        self.socket.lock().unwrap().take();
    }

    pub fn check(&self) -> Result<(), PluginError> {
        match self.reason.load(Ordering::SeqCst) {
            0 => Ok(()),
            1 => Err(PluginError {
                code: -32800,
                message: "Request cancelled; Firebird attachment aborted".into(),
            }),
            2 => Err(PluginError {
                code: -32001,
                message: "Firebird connection timeout".into(),
            }),
            _ => Err(PluginError {
                code: -32001,
                message: "Firebird query timeout; attachment aborted".into(),
            }),
        }
    }
}

thread_local! { static CURRENT: RefCell<Option<Arc<Control>>> = const { RefCell::new(None) }; }

pub fn current() -> Arc<Control> {
    CURRENT.with(|c| c.borrow().clone().unwrap_or_default())
}

pub fn run<T>(control: Arc<Control>, action: impl FnOnce() -> T) -> T {
    CURRENT.with(|c| *c.borrow_mut() = Some(control));
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CURRENT.with(|c| *c.borrow_mut() = None);
        }
    }
    let _reset = Reset;
    action()
}

pub struct Deadline {
    done: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Deadline {
    pub fn connection(control: &Arc<Control>, seconds: u32) -> Self {
        Self::start(control, seconds, 2)
    }
    pub fn query(control: &Arc<Control>, seconds: u32) -> Self {
        Self::start(control, seconds, 3)
    }

    fn start(control: &Arc<Control>, seconds: u32, reason: u8) -> Self {
        if seconds == 0 {
            return Self {
                done: None,
                thread: None,
            };
        }
        let control = control.clone();
        let (done, receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            if receiver.recv_timeout(Duration::from_secs(seconds.into()))
                == Err(mpsc::RecvTimeoutError::Timeout)
            {
                control.stop(reason);
            }
        });
        Self {
            done: Some(done),
            thread: Some(thread),
        }
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_is_sticky_and_does_not_leak_between_requests() {
        let first = Arc::new(Control::default());
        first.cancel();
        first.stop(3);
        assert_eq!(first.check().unwrap_err().code, -32800);
        run(first, || assert!(current().check().is_err()));
        assert!(current().check().is_ok());
        let other = Arc::new(Control::default());
        {
            let _finished = Deadline::query(&other, 1);
        }
        assert!(other.check().is_ok());
    }
}
