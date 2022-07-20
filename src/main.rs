use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;

pub struct Notify(Condvar, Mutex<()>);
impl Notify {
    fn new() -> Self {
        Self(Condvar::new(), Mutex::new(()))
    }
    fn wait(&self) {
        drop(self.0.wait(self.1.lock().unwrap()).unwrap());
    }
    fn notify_all(&self) {
        self.0.notify_all();
    }
}

pub struct BroadcastPipe {
    buffer: Arc<RwLock<Vec<u8>>>,
    ready: Arc<Notify>,
    read_pos: usize,
    complete: Arc<AtomicBool>,
}
impl BroadcastPipe {
    fn new() -> Self {
        Self {
            buffer: Arc::new(RwLock::new(Vec::new())),
            ready: Arc::new(Notify::new()),
            read_pos: 0,
            complete: Arc::new(AtomicBool::new(false)),
        }
    }
}
impl Clone for BroadcastPipe {
    fn clone(&self) -> Self {
        Self {
            buffer: self.buffer.clone(),
            ready: self.ready.clone(),
            read_pos: 0,
            complete: self.complete.clone(),
        }
    }
}
impl Write for BroadcastPipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let res = self.buffer.write().unwrap().write(buf)?;
        if res > 0 {
            self.ready.notify_all();
        }
        Ok(res)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.buffer.write().unwrap().flush()
    }
}
impl Read for BroadcastPipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.len() == 0 {
            return Ok(0);
        }
        loop {
            let internal_buffer = self.buffer.read().unwrap();
            let count = std::cmp::min(buf.len(), internal_buffer.len() - self.read_pos);
            if count == 0 && !self.complete.load(Ordering::SeqCst) {
                drop(internal_buffer);
                self.ready.wait();
                continue;
            }
            let range = self.read_pos..(self.read_pos + count);
            buf[range.clone()].clone_from_slice(&internal_buffer[range]);
            self.read_pos += count;
            return Ok(count);
        }
    }
}

fn main() {
    let app = clap::App::new("nc-broadcast")
        .arg(clap::Arg::new("bind").required(true))
        .arg(clap::Arg::new("tee").long("tee"));
    let args = app.get_matches();
    let bind: SocketAddr = args.value_of("bind").unwrap().parse().unwrap();
    let tee = args.is_present("tee");
    let mut pipe = BroadcastPipe::new();
    let read_pipe = pipe.clone();
    thread::spawn(move || {
        std::io::copy(&mut std::io::stdin(), &mut pipe).unwrap();
        pipe.complete.store(true, Ordering::SeqCst);
        pipe.ready.notify_all();
    });
    if tee {
        let mut thread_read_pipe = read_pipe.clone();
        thread::spawn(move || {
            std::io::copy(&mut thread_read_pipe, &mut std::io::stdout()).unwrap();
        });
    }
    let listener = TcpListener::bind(bind).unwrap();
    loop {
        let (mut conn, ip) = listener.accept().unwrap();
        let mut thread_read_pipe = read_pipe.clone();
        thread::spawn(move || {
            std::io::copy(&mut thread_read_pipe, &mut conn).unwrap();
            conn.shutdown(Shutdown::Both).unwrap();
            eprintln!("Finished sending file to {}", ip);
        });
    }
}
