//! The update handoff on Unix: the endpoint and the PTYs pass as descriptors over the channel (`SCM_RIGHTS`).
//!
//! herdr: `src/server/handoff.rs` (`send_fds`, `send_fd_batch`, `recv_fds`, `recv_fd_batch`, `FDS_PER_MESSAGE`) at
//! `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified: the listening socket and the
//! namespace's locked lock file go first, then one PTY master per pane; every descriptor received is made
//! close-on-exec; the pid of the channel's peer is checked where the OS reports it.

use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::time::Duration;

use ngterm_pty::{Child, Master};

use super::{Deadline, PaneState, Support};
use crate::endpoint::{Endpoint, Listener, Stream};
use crate::server::pane::Pane;
use crate::server::spawn::Spawned;

/// Descriptors per `SCM_RIGHTS` message (herdr's `FDS_PER_MESSAGE`): a message holds at most 253 on Linux and 254 on
/// macOS, so the batches stay well below, and the panes are unbounded.
const FDS_PER_MESSAGE: usize = 64;

/// Unix hands over everywhere.
pub(super) fn support() -> Support {
    Support::Supported
}

/// The pid of the channel's peer, where the OS reports it (Linux; macOS's peer credentials carry none).
pub(super) fn peer_pid(stream: &Stream) -> Option<u32> {
    use interprocess::local_socket::traits::StreamCommon as _;
    let pid = stream.peer_creds().ok()?.pid()?;
    u32::try_from(pid).ok()
}

/// What the importer receives: the endpoint's two descriptors and every pane's PTY.
pub(super) struct Handles {
    pub(super) endpoint: [OwnedFd; 2],
    pub(super) ptys: Vec<(Master, Child)>,
}

/// Step 6 of the exporter: the listening socket and the locked lock file, then each pane's master.
pub(super) fn send_handles(
    stream: &Stream,
    listener: &Listener,
    panes: &[Arc<Pane>],
    _replacement: &Spawned,
) -> io::Result<()> {
    let mut fds: Vec<OwnedFd> = listener.handoff_fds()?.into();
    for pane in panes {
        fds.push(pane.master()?);
    }
    let raw: Vec<RawFd> = fds.iter().map(AsRawFd::as_raw_fd).collect();
    send_fds(stream, &raw)
    // the duplicates close here: the replacement holds its own
}

/// Step 6 of the importer: the endpoint's descriptors and the masters of `panes`, each pane's program adopted by the
/// pid the manifest names.
pub(super) fn receive_handles(stream: &Arc<Stream>, panes: &[PaneState], timeout: Duration) -> io::Result<Handles> {
    let _deadline = Deadline::start(stream, timeout);
    let mut fds = recv_fds(stream, 2 + panes.len())?.into_iter();
    let (Some(listener), Some(lock)) = (fds.next(), fds.next()) else {
        return Err(io::Error::other("the handoff sent no endpoint"));
    };
    let ptys = fds
        .zip(panes)
        .map(|(master, pane)| (Master::from_fd(master), Child::adopt(pane.pid)))
        .collect();
    Ok(Handles {
        endpoint: [listener, lock],
        ptys,
    })
}

/// The importer's endpoint: the old server's listener adopted.
pub(super) fn take_endpoint(endpoint: &Endpoint, [listener, lock]: [OwnedFd; 2]) -> io::Result<Listener> {
    endpoint.adopt(listener, lock)
}

fn send_fds(stream: &Stream, fds: &[RawFd]) -> io::Result<()> {
    let socket = stream_fd(stream);
    for batch in fds.chunks(FDS_PER_MESSAGE) {
        send_fd_batch(socket, batch)?;
    }
    Ok(())
}

fn stream_fd(stream: &Stream) -> RawFd {
    let Stream::UdSocket(socket) = stream;
    socket.as_fd().as_raw_fd()
}

/// The control buffer for `count` descriptors.
fn control_buffer(count: usize) -> io::Result<Vec<u8>> {
    let bytes = u32::try_from(count * size_of::<RawFd>()).map_err(io::Error::other)?;
    // SAFETY: CMSG_SPACE is pure arithmetic on its argument.
    let space = unsafe { libc::CMSG_SPACE(bytes) };
    Ok(vec![0; usize::try_from(space).map_err(io::Error::other)?])
}

/// `value` as the type of a `msghdr` or `cmsghdr` length field (`size_t` on Linux, `socklen_t` elsewhere).
fn fit<S, T: TryFrom<S>>(value: S) -> io::Result<T> {
    T::try_from(value).map_err(|_| io::Error::other("a length does not fit its control message field"))
}

fn send_fd_batch(socket: RawFd, fds: &[RawFd]) -> io::Result<()> {
    if fds.is_empty() {
        return Ok(());
    }
    let byte = *b"F";
    let mut iov = [libc::iovec {
        iov_base: byte.as_ptr().cast_mut().cast(),
        iov_len: byte.len(),
    }];
    let mut control = control_buffer(fds.len())?;
    let fd_bytes = u32::try_from(size_of_val(fds)).map_err(io::Error::other)?;
    // SAFETY: `msghdr` is plain data, for which all zero bytes is a valid (empty) value.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = iov.as_mut_ptr();
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = fit(control.len())?;
    // SAFETY: the control buffer has room for one header with `fds`; CMSG_FIRSTHDR reads the lengths set above.
    let header = unsafe { libc::CMSG_FIRSTHDR(&raw const message) };
    if header.is_null() {
        return Err(io::Error::other("no room for the descriptors' control message"));
    }
    // SAFETY: CMSG_LEN is pure arithmetic on its argument.
    let length = fit(unsafe { libc::CMSG_LEN(fd_bytes) })?;
    // SAFETY: `header` points into `control`, which holds a whole header (CMSG_SPACE above), and nothing else
    // borrows it.
    let header = unsafe { &mut *header };
    header.cmsg_level = libc::SOL_SOCKET;
    header.cmsg_type = libc::SCM_RIGHTS;
    header.cmsg_len = length;
    // SAFETY: CMSG_DATA of the header just filled: its payload, which has room for `fds` (CMSG_SPACE above).
    let data = unsafe { libc::CMSG_DATA(header) };
    // SAFETY: the payload has room for the slice's bytes, which the copy reads exactly; the two do not overlap.
    unsafe { std::ptr::copy_nonoverlapping(fds.as_ptr().cast::<u8>(), data, size_of_val(fds)) };
    // SAFETY: every pointer in `message` points at live buffers of the lengths it records.
    if unsafe { libc::sendmsg(socket, &raw const message, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn recv_fds(stream: &Stream, expected: usize) -> io::Result<Vec<OwnedFd>> {
    let socket = stream_fd(stream);
    let mut out: Vec<OwnedFd> = Vec::with_capacity(expected);
    while out.len() < expected {
        let wanted = (expected - out.len()).min(FDS_PER_MESSAGE);
        let batch = recv_fd_batch(socket, wanted)?;
        if batch.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "the handoff channel closed after {} of {expected} descriptors",
                    out.len()
                ),
            ));
        }
        out.extend(batch);
    }
    Ok(out)
}

fn recv_fd_batch(socket: RawFd, wanted: usize) -> io::Result<Vec<OwnedFd>> {
    let mut byte = [0_u8; 1];
    let mut iov = [libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: byte.len(),
    }];
    let mut control = control_buffer(wanted)?;
    // SAFETY: `msghdr` is plain data, for which all zero bytes is a valid (empty) value.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = iov.as_mut_ptr();
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = fit(control.len())?;
    let read = loop {
        // SAFETY: every pointer in `message` points at live buffers of the lengths it records.
        let read = unsafe { libc::recvmsg(socket, &raw mut message, 0) };
        if read >= 0 {
            break read;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    };
    let received = take_descriptors(&message, &control);
    // truncation means the kernel closed what did not fit: the batch is lost
    if message.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::other("the handoff's descriptor message was truncated"));
    }
    if read == 0 {
        return Ok(Vec::new());
    }
    if received.len() > wanted {
        return Err(io::Error::other(format!(
            "the handoff's descriptor message carried {} descriptors, at most {wanted} were due",
            received.len()
        )));
    }
    if received.is_empty() {
        return Err(io::Error::other("the handoff's message carried no descriptors"));
    }
    for fd in &received {
        set_cloexec(fd)?;
    }
    Ok(received)
}

/// The descriptors in `message`'s `SCM_RIGHTS` headers, owned (so they close when dropped, on every path).
fn take_descriptors(message: &libc::msghdr, control: &[u8]) -> Vec<OwnedFd> {
    let mut out = Vec::new();
    let control_end = control.as_ptr() as usize + fit::<_, usize>(message.msg_controllen).unwrap_or(0);
    // SAFETY: the message's control fields point into `control`, which the kernel filled up to `msg_controllen`.
    let mut header = unsafe { libc::CMSG_FIRSTHDR(message) };
    while !header.is_null() {
        // SAFETY: `header` is a header within the control buffer CMSG_FIRSTHDR/CMSG_NXTHDR walked to.
        let cmsg = unsafe { &*header };
        let (level, kind, length) = (cmsg.cmsg_level, cmsg.cmsg_type, cmsg.cmsg_len);
        if level == libc::SOL_SOCKET && kind == libc::SCM_RIGHTS {
            // SAFETY: CMSG_DATA of a header within the buffer.
            let data = unsafe { libc::CMSG_DATA(header) };
            // SAFETY: CMSG_LEN is pure arithmetic.
            let empty = unsafe { libc::CMSG_LEN(0) } as usize;
            // bounded by the header's own length and the bytes the kernel wrote, so the reads stay in the buffer
            let available = control_end.saturating_sub(data as usize);
            let bytes = fit::<_, usize>(length)
                .unwrap_or(0)
                .saturating_sub(empty)
                .min(available);
            for index in 0..bytes / size_of::<RawFd>() {
                // SAFETY: `index` stays within the payload's bytes.
                let at = unsafe { data.cast::<RawFd>().add(index) };
                // SAFETY: `at` lies within the payload the kernel wrote; the payload may be unaligned.
                let fd = unsafe { at.read_unaligned() };
                // SAFETY: the kernel installed this descriptor for this process with the message; nothing else owns it.
                out.push(unsafe { OwnedFd::from_raw_fd(fd) });
            }
        }
        // SAFETY: walks to the next header within the buffer, or null past its end.
        header = unsafe { libc::CMSG_NXTHDR(message, header) };
    }
    out
}

fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: plain `fcntl` on a descriptor owned here.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    use super::*;

    /// Descriptors pass in batches across a socket pair, more than one batch's worth, each received close-on-exec and
    /// naming the same open file.
    #[test]
    fn descriptors_pass_in_batches_and_arrive_close_on_exec() {
        let (left, right) = UnixStream::pair().unwrap();
        let (left, right) = (OwnedFd::from(left), OwnedFd::from(right));
        let files: Vec<std::fs::File> = (0..FDS_PER_MESSAGE + 3)
            .map(|_| tempfile::tempfile().unwrap())
            .collect();
        let raw: Vec<RawFd> = files.iter().map(AsRawFd::as_raw_fd).collect();
        for batch in raw.chunks(FDS_PER_MESSAGE) {
            send_fd_batch(left.as_raw_fd(), batch).unwrap();
        }
        let mut received = Vec::new();
        while received.len() < files.len() {
            received.extend(recv_fd_batch(right.as_raw_fd(), FDS_PER_MESSAGE).unwrap());
        }
        assert_eq!(received.len(), files.len());
        for (index, (sent, got)) in files.iter().zip(&received).enumerate() {
            // SAFETY: plain `fcntl` on a descriptor owned here.
            let flags = unsafe { libc::fcntl(got.as_raw_fd(), libc::F_GETFD) };
            assert_ne!(flags & libc::FD_CLOEXEC, 0);
            let mut sent = sent;
            sent.write_all(format!("{index}").as_bytes()).unwrap();
            let mut text = String::new();
            let mut got = std::fs::File::from(got.try_clone().unwrap());
            std::io::Seek::rewind(&mut got).unwrap();
            got.read_to_string(&mut text).unwrap();
            assert_eq!(text, index.to_string());
        }
    }

    #[test]
    fn a_closed_channel_gives_no_descriptors() {
        let (left, right) = UnixStream::pair().unwrap();
        drop(left);
        assert!(recv_fd_batch(right.as_raw_fd(), 4).unwrap().is_empty());
    }
}
