//! Frames and the two ways to carry them.
//!
//! The simulation only ever sees a `Peer`: a pair of queues of encoded frames. Whether the far end
//! is a window in the same process or a socket is decided when the peer is made, and nothing after
//! that can tell. Sending never blocks, so one slow client cannot stall a server tick: a client too
//! far behind is cut off instead.
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{self, BufWriter, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};

/// An encoded message. Shared, so a broadcast costs one encode however many are listening.
pub type Frame = Arc<[u8]>;

/// No message is bigger than this.
pub const MAX_FRAME: usize = 32 * 1024 * 1024;
/// A peer with this much unsent is too far behind to catch up, and gets disconnected.
const MAX_QUEUED: usize = 64 * 1024 * 1024;

pub fn encode<T: Serialize>(msg: &T) -> Frame {
    postcard::to_stdvec(msg).expect("messages always serialise").into()
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    postcard::from_bytes(bytes).map_err(|e| e.to_string())
}

/// One end of a connection.
pub struct Peer {
    tx: Sender<Frame>,
    rx: Mutex<Receiver<Frame>>,
    /// Bytes this end has sent that have not gone out, or been read, yet.
    queued: Arc<AtomicUsize>,
    /// On a memory peer: the other end's `queued`, which reading a frame here pays down.
    far_queued: Option<Arc<AtomicUsize>>,
    closed: Arc<AtomicBool>,
    /// The socket, to unblock its reader when this end hangs up.
    socket: Option<TcpStream>,
}

impl Peer {
    /// Queues a frame. Never blocks. Returns false once the connection is gone.
    pub fn send_frame(&self, frame: Frame) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let size = frame.len();
        if self.queued.fetch_add(size, Ordering::AcqRel) + size > MAX_QUEUED {
            self.close();
            return false;
        }
        if self.tx.send(frame).is_err() {
            self.closed.store(true, Ordering::Release);
            return false;
        }
        true
    }

    pub fn send<T: Serialize>(&self, msg: &T) -> bool {
        self.send_frame(encode(msg))
    }

    /// The next frame that has arrived, if any.
    pub fn try_recv_frame(&self) -> Option<Frame> {
        let rx = self.rx.lock().unwrap_or_else(|e| e.into_inner());
        match rx.try_recv() {
            Ok(frame) => {
                self.received(&frame);
                Some(frame)
            }
            Err(TryRecvError::Disconnected) => {
                self.closed.store(true, Ordering::Release);
                None
            }
            Err(TryRecvError::Empty) => None,
        }
    }

    /// The next frame, waiting up to `timeout` for one.
    pub fn recv_frame_timeout(&self, timeout: std::time::Duration) -> Option<Frame> {
        let rx = self.rx.lock().unwrap_or_else(|e| e.into_inner());
        match rx.recv_timeout(timeout) {
            Ok(frame) => {
                self.received(&frame);
                Some(frame)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.closed.store(true, Ordering::Release);
                None
            }
            Err(mpsc::RecvTimeoutError::Timeout) => None,
        }
    }

    fn received(&self, frame: &Frame) {
        if let Some(far) = &self.far_queued {
            far.fetch_sub(frame.len(), Ordering::AcqRel);
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// How much this end has queued that has not gone out yet.
    pub fn queued_bytes(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(s) = &self.socket {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.close();
    }
}

/// Two ends of a connection inside one process. Each pays down the other's backlog by reading.
pub fn memory_pair() -> (Peer, Peer) {
    let (a_tx, a_rx) = mpsc::channel::<Frame>();
    let (b_tx, b_rx) = mpsc::channel::<Frame>();
    let (a_queued, b_queued) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let make = |tx, rx, queued, far_queued| Peer {
        tx,
        rx: Mutex::new(rx),
        queued,
        far_queued: Some(far_queued),
        closed: Arc::new(AtomicBool::new(false)),
        socket: None,
    };
    (make(a_tx, b_rx, a_queued.clone(), b_queued.clone()), make(b_tx, a_rx, b_queued, a_queued))
}

/// Wraps a connected socket in a peer: one thread reads frames, one writes them.
pub fn tcp_peer(stream: TcpStream) -> io::Result<Peer> {
    stream.set_nodelay(true)?;
    let reader = stream.try_clone()?;
    let writer = stream.try_clone()?;
    let (out_tx, out_rx) = mpsc::channel::<Frame>();
    let (in_tx, in_rx) = mpsc::channel::<Frame>();
    let queued = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicBool::new(false));

    {
        let closed = closed.clone();
        std::thread::Builder::new().name("net-read".into()).spawn(move || {
            let _ = read_frames(reader, &in_tx);
            closed.store(true, Ordering::Release);
        })?;
    }
    {
        let closed = closed.clone();
        let queued = queued.clone();
        std::thread::Builder::new().name("net-write".into()).spawn(move || {
            let _ = write_frames(writer, &out_rx, &queued);
            closed.store(true, Ordering::Release);
        })?;
    }
    Ok(Peer { tx: out_tx, rx: Mutex::new(in_rx), queued, far_queued: None, closed, socket: Some(stream) })
}

fn read_frames(mut s: TcpStream, tx: &Sender<Frame>) -> io::Result<()> {
    let mut size = [0u8; 4];
    loop {
        s.read_exact(&mut size)?;
        let len = u32::from_le_bytes(size) as usize;
        if len > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
        }
        let mut body = vec![0u8; len];
        s.read_exact(&mut body)?;
        if tx.send(body.into()).is_err() {
            return Ok(());
        }
    }
}

fn write_frames(s: TcpStream, rx: &Receiver<Frame>, queued: &AtomicUsize) -> io::Result<()> {
    let mut w = BufWriter::with_capacity(64 * 1024, s);
    while let Ok(mut frame) = rx.recv() {
        // Everything already waiting goes out in one write, then one flush.
        loop {
            w.write_all(&(frame.len() as u32).to_le_bytes())?;
            w.write_all(&frame)?;
            queued.fetch_sub(frame.len(), Ordering::AcqRel);
            match rx.try_recv() {
                Ok(next) => frame = next,
                Err(_) => break,
            }
        }
        w.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::*;
    use std::net::TcpListener;
    use std::time::Duration;

    fn wait_for(peer: &Peer) -> Frame {
        peer.recv_frame_timeout(Duration::from_secs(5)).expect("a frame arrives")
    }

    #[test]
    fn memory_peers_carry_frames_in_order_both_ways() {
        let (a, b) = memory_pair();
        for i in 0..100u32 {
            assert!(a.send(&C2S::Pong(i)));
        }
        for i in 0..100u32 {
            assert_eq!(decode::<C2S>(&wait_for(&b)).expect("decodes"), C2S::Pong(i));
        }
        assert!(b.send(&S2C::Ping(1)));
        assert_eq!(decode::<S2C>(&wait_for(&a)).expect("decodes"), S2C::Ping(1));
    }

    #[test]
    fn reading_pays_down_a_memory_peers_backlog() {
        let (a, b) = memory_pair();
        let frame: Frame = vec![0u8; 1024 * 1024].into();
        // Far more than the backlog limit in total, but never more than a little of it at once.
        for _ in 0..(MAX_QUEUED / frame.len()) * 3 {
            assert!(a.send_frame(frame.clone()), "cut off although the other end keeps reading");
            assert!(b.try_recv_frame().is_some());
        }
        assert_eq!(a.queued_bytes(), 0);
    }

    #[test]
    fn dropping_one_end_closes_the_other() {
        let (a, b) = memory_pair();
        drop(a);
        assert!(b.try_recv_frame().is_none());
        assert!(b.is_closed());
        assert!(!b.send(&C2S::Respawn));
    }

    #[test]
    fn tcp_peers_carry_the_same_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let client = tcp_peer(TcpStream::connect(addr).expect("connect")).expect("peer");
        let server = tcp_peer(listener.accept().expect("accept").0).expect("peer");
        let big = C2S::Chat("x".repeat(200_000));
        assert!(client.send(&big));
        assert_eq!(decode::<C2S>(&wait_for(&server)).expect("decodes"), big);
        assert!(server.send(&S2C::Ping(4)));
        assert_eq!(decode::<S2C>(&wait_for(&client)).expect("decodes"), S2C::Ping(4));
    }

    #[test]
    fn a_frame_claiming_to_be_huge_cuts_the_connection_without_allocating() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let mut raw = TcpStream::connect(addr).expect("connect");
        let server = tcp_peer(listener.accept().expect("accept").0).expect("peer");
        raw.write_all(&u32::MAX.to_le_bytes()).expect("write");
        assert!(server.recv_frame_timeout(Duration::from_secs(5)).is_none());
        assert!(server.is_closed());
    }

    #[test]
    fn a_peer_that_stopped_reading_is_cut_off_rather_than_buffered_forever() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let _stalled = TcpStream::connect(addr).expect("connect");
        let server = tcp_peer(listener.accept().expect("accept").0).expect("peer");
        let frame: Frame = vec![0u8; 1024 * 1024].into();
        let mut sent = 0usize;
        let started = std::time::Instant::now();
        while server.send_frame(frame.clone()) {
            sent += 1;
            assert!(sent < 1000, "still accepting after a gigabyte");
        }
        assert!(sent as u64 * 1024 * 1024 >= MAX_QUEUED as u64 / 2);
        assert!(started.elapsed() < Duration::from_secs(5), "sending must never block");
    }
}
