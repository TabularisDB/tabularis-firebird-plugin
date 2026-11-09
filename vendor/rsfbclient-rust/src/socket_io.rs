//! Cancellable socket waits, including Windows reads and writes under backpressure.
#[cfg(any(not(windows), test))]
use std::time::Duration;
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

pub(crate) struct SocketIo {
    socket: TcpStream,
    cancelled: Option<Arc<AtomicBool>>,
}

impl SocketIo {
    pub fn new(socket: TcpStream, cancelled: Option<Arc<AtomicBool>>) -> io::Result<Self> {
        if cancelled.is_some() {
            #[cfg(windows)]
            socket.set_nonblocking(true)?;
            #[cfg(not(windows))]
            {
                let interval = Some(Duration::from_millis(100));
                socket.set_read_timeout(interval)?;
            }
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
            && (error.kind() == io::ErrorKind::WouldBlock
                || (cfg!(not(windows)) && error.kind() == io::ErrorKind::TimedOut))
    }

    #[cfg(windows)]
    fn wait(&self, events: i16) -> io::Result<()> {
        use std::os::windows::io::AsRawSocket;
        loop {
            self.check()?;
            if self.cancelled.is_none() {
                return Ok(());
            }
            if windows::ready(self.socket.as_raw_socket() as usize, events)? {
                return self.check();
            }
        }
    }
}

impl Read for SocketIo {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            #[cfg(windows)]
            self.wait(windows::READ)?;
            match self.socket.read(bytes) {
                Err(error) if self.retry(&error) => continue,
                result => return result,
            }
        }
    }
}

impl Write for SocketIo {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        #[cfg(windows)]
        loop {
            self.wait(windows::WRITE)?;
            match self.socket.write(bytes) {
                Err(error)
                    if self.cancelled.is_some() && error.kind() == io::ErrorKind::WouldBlock =>
                {
                    continue
                }
                result => return result,
            }
        }
        #[cfg(not(windows))]
        {
            self.check()?;
            self.socket.write(bytes)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check()
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    pub const READ: i16 = 0x0100; // POLLRDNORM
    pub const WRITE: i16 = 0x0010; // POLLWRNORM

    // WSAPOLLFD from winsock2.h: SOCKET, SHORT, SHORT.
    #[repr(C)]
    struct PollFd {
        socket: usize,
        events: i16,
        returned: i16,
    }
    #[link(name = "Ws2_32")]
    extern "system" {
        fn WSAPoll(descriptors: *mut PollFd, count: u32, timeout: i32) -> i32;
        fn WSAGetLastError() -> i32;
    }
    pub fn ready(socket: usize, events: i16) -> io::Result<bool> {
        let mut descriptor = PollFd {
            socket,
            events,
            returned: 0,
        };
        // The descriptor remains valid and exclusively borrowed for this synchronous call.
        let result = unsafe { WSAPoll(&mut descriptor, 1, 100) };
        if result < 0 {
            // Called immediately on the same thread as the failed Winsock operation.
            Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
        } else {
            Ok(result > 0)
        }
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

    #[test]
    fn shutdown_interrupts_a_write_stalled_by_backpressure() {
        use std::sync::atomic::AtomicUsize;
        let (socket, peer) = pair();
        // Only a failure-cleanup bound; successful cancellation must beat it.
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let abort = socket.try_clone().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut stream = SocketIo::new(socket, Some(cancelled.clone())).unwrap();
        let written = Arc::new(AtomicUsize::new(0));
        let progress = written.clone();
        let (sender, receiver) = mpsc::channel();
        let writer = thread::spawn(move || {
            let bytes = [0; 65536];
            let error = loop {
                match stream.write(&bytes) {
                    Ok(0) => panic!("unexpected zero-length write"),
                    Ok(count) => {
                        progress.fetch_add(count, Ordering::SeqCst);
                    }
                    Err(error) => break error,
                }
            };
            sender.send(error).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut previous = 0;
        let mut stalled = false;
        while Instant::now() < deadline {
            thread::sleep(Duration::from_millis(200));
            let count = written.load(Ordering::SeqCst);
            if count > 0 && count == previous {
                stalled = true;
                break;
            }
            previous = count;
        }
        cancelled.store(true, Ordering::SeqCst);
        abort.shutdown(std::net::Shutdown::Both).unwrap();
        let result = receiver.recv_timeout(Duration::from_secs(2));
        drop(peer);
        writer.join().unwrap();
        assert!(
            stalled,
            "peer must stop accepting write progress before cancellation"
        );
        let error = result.expect("socket shutdown did not interrupt a blocked write");
        assert!(
            !matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            "write only ended at its safety timeout: {error}"
        );
    }
}
