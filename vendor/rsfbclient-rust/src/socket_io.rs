//! Bounded receive waits let cancellation interrupt Windows blocking reads too.
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

pub(crate) struct SocketIo {
    socket: TcpStream,
    cancelled: Option<Arc<AtomicBool>>,
}

impl SocketIo {
    pub fn new(socket: TcpStream, cancelled: Option<Arc<AtomicBool>>) -> io::Result<Self> {
        if cancelled.is_some() {
            let interval = Some(Duration::from_millis(100));
            socket.set_read_timeout(interval)?;
        }
        Ok(Self { socket, cancelled })
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.socket.peer_addr()
    }

    fn check(&self) -> io::Result<()> {
        if self
            .cancelled
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
        {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "Firebird request aborted",
            ))
        } else {
            Ok(())
        }
    }

    fn retry(&self, error: &io::Error) -> bool {
        self.cancelled.is_some()
            && matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )
    }
}

impl Read for SocketIo {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            match self.socket.read(bytes) {
                Err(error) if self.retry(&error) => continue,
                result => return result,
            }
        }
    }
}

impl Write for SocketIo {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.check()?;
        self.socket.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, sync::mpsc, thread, time::Instant};

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        (socket, listener.accept().unwrap().0)
    }

    #[test]
    fn cancellation_interrupts_a_read_without_socket_shutdown() {
        let (socket, _peer) = pair();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut stream = SocketIo::new(socket, Some(cancelled.clone())).unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || sender.send(stream.read(&mut [0])).unwrap());
        thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        cancelled.store(true, Ordering::SeqCst);
        let error = receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
        assert!(started.elapsed() < Duration::from_secs(2));
        reader.join().unwrap();
    }

    #[test]
    fn idle_timeouts_do_not_fail_an_uncancelled_stream() {
        let (socket, mut peer) = pair();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut stream = SocketIo::new(socket, Some(cancelled)).unwrap();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(450));
            peer.write_all(&[42]).unwrap();
        });
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [42]);
        writer.join().unwrap();
    }
}
