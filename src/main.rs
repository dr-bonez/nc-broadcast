use std::fs::File;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    complete: Arc<AtomicUsize>,
}
impl BroadcastPipe {
    fn new() -> Self {
        Self {
            buffer: Arc::new(RwLock::new(Vec::new())),
            ready: Arc::new(Notify::new()),
            read_pos: 0,
            complete: Arc::new(AtomicUsize::new(0)),
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
            if count == 0 && self.complete.load(Ordering::SeqCst) > 0 {
                drop(internal_buffer);
                self.ready.wait();
                continue;
            }
            buf[0..count]
                .clone_from_slice(&internal_buffer[self.read_pos..(self.read_pos + count)]);
            self.read_pos += count;
            return Ok(count);
        }
    }
}

fn main() {
    let app = clap::App::new("nc-broadcast")
        .arg(clap::Arg::new("bind").required(true))
        .arg(clap::Arg::new("tee").long("tee"))
        .arg(
            clap::Arg::new("input")
                .long("input")
                .takes_value(true)
                .action(clap::ArgAction::Append),
        );
    let args = app.get_matches();
    let bind: SocketAddr = args.value_of("bind").unwrap().parse().unwrap();
    let tee = args.is_present("tee");
    let mut pipe = BroadcastPipe::new();
    pipe.complete.fetch_add(1, Ordering::SeqCst);
    let read_pipe = pipe.clone();
    thread::spawn(move || {
        std::io::copy(&mut std::io::stdin(), &mut pipe).unwrap();
        pipe.complete.fetch_sub(1, Ordering::SeqCst);
        pipe.ready.notify_all();
    });

    for input in args
        .values_of("input")
        .into_iter()
        .flatten()
        .map(|s| s.to_owned())
    {
        let mut pipe = read_pipe.clone();
        pipe.complete.fetch_add(1, Ordering::SeqCst);
        thread::spawn(move || {
            let mut buffer;
            let mut notify = inotify::Inotify::init().unwrap();
            let input_path = Path::new(&input);
            let wd = notify
                .add_watch(
                    input_path.parent().unwrap(),
                    inotify::WatchMask::CREATE | inotify::WatchMask::ONLYDIR,
                )
                .unwrap();
            if !input_path.exists() {
                buffer = vec![0; inotify::get_buffer_size(input_path.parent().unwrap()).unwrap()];
                for event in notify.read_events_blocking(&mut buffer).unwrap() {
                    if event.mask == inotify::EventMask::CREATE
                        && event.name == input_path.file_name()
                    {
                        break;
                    }
                }
            }
            notify.rm_watch(wd).unwrap();
            let mut file = File::open(&input_path).unwrap();
            buffer = vec![0; inotify::get_buffer_size(input_path).unwrap()];
            notify
                .add_watch(&input_path, inotify::WatchMask::MODIFY)
                .unwrap();
            loop {
                std::io::copy(&mut file, &mut pipe).unwrap();
                notify.read_events_blocking(&mut buffer).unwrap().next();
            }
        });
    }
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
