//! The spike's control socket: one line per connection in, one line out. The command runs on the UI thread; the
//! socket thread waits for its reply.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

pub struct Request {
    pub line: String,
    pub reply: mpsc::Sender<Result<String, String>>,
}

pub fn serve(path: &Path, tx: async_channel::Sender<Request>) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    thread::Builder::new().name("control".into()).spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let tx = tx.clone();
            thread::spawn(move || {
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    return;
                }
                let (reply_tx, reply_rx) = mpsc::channel();
                let request = Request {
                    line: line.trim_end().to_owned(),
                    reply: reply_tx,
                };
                let answer = if tx.send_blocking(request).is_err() {
                    "err app gone".to_owned()
                } else {
                    match reply_rx.recv() {
                        Ok(Ok(text)) if text.is_empty() => "ok".to_owned(),
                        Ok(Ok(text)) => format!("ok {text}"),
                        Ok(Err(error)) => format!("err {error}"),
                        Err(_) => "err no reply".to_owned(),
                    }
                };
                let _ = stream.write_all(answer.as_bytes());
                let _ = stream.write_all(b"\n");
            });
        }
    })?;
    Ok(())
}
