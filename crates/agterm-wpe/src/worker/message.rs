//! What the app and the worker say to each other, and how each message becomes a [`Packet`]: the body as JSON, with
//! the bytes it describes in the blob (an SHM frame's pixels, a scheme answer's data) and a dmabuf's planes as the
//! packet's descriptors.

use std::os::fd::OwnedFd;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::wire::{Packet, WireError};
use crate::config::HostConfig;
use crate::event::{Event, SchemeResponse, SchemeToken};
use crate::frames::{BufferId, DmaBuf, Frame, FrameContent, Plane, Rect, ShmRegion};
use crate::host::{Command, Evaluation, StartError};
use crate::spec::ViewId;

/// The protocol's version: the app and its worker are one executable, so a mismatch means the file on disk was
/// replaced by another build while the app ran.
pub(crate) const PROTOCOL: u32 = 1;

/// From the app to the worker.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ToWorker {
    /// The engine's configuration, once, after the worker's [`FromWorker::Hello`].
    Start(HostConfig),
    /// Any command but an evaluation and a scheme's bytes (`Command::Quit` included).
    Command(Command),
    /// [`crate::Host::evaluate`], answered by [`FromWorker::Evaluation`] with its `id`.
    Evaluate {
        id: u64,
        view: ViewId,
        script: String,
        world: Option<String>,
    },
    /// [`SchemeResponse::Bytes`], its data in the blob.
    SchemeBytes { token: SchemeToken, mime: String },
}

/// From the worker to the app.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum FromWorker {
    /// The worker is up: its protocol, its build and its pid (in the app's pid namespace).
    Hello {
        protocol: u32,
        version: String,
        pid: u32,
    },
    /// How the engine's start went.
    Started(Result<(), StartFailure>),
    /// Any event but a frame.
    Event(Event),
    /// A dmabuf frame; the planes are the packet's descriptors, in order.
    DmaBufFrame {
        frame: FrameHeader,
        dmabuf: DmaBufHeader,
    },
    /// An SHM frame; the regions' pixels follow each other in the blob.
    ShmFrame {
        frame: FrameHeader,
        regions: Vec<Rect>,
    },
    Evaluation {
        id: u64,
        result: Evaluation,
    },
    /// The engine closed everything after `Command::Quit`; the worker ends.
    Stopped,
}

/// Why the worker's engine did not start, as [`StartError`] tells it.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum StartFailure {
    Display(String),
    Ended,
    Other(String),
}

impl From<&StartError> for StartFailure {
    fn from(error: &StartError) -> Self {
        match error {
            StartError::Display(message) => Self::Display(message.clone()),
            StartError::Ended => Self::Ended,
            other => Self::Other(other.to_string()),
        }
    }
}

impl From<StartFailure> for StartError {
    fn from(failure: StartFailure) -> Self {
        match failure {
            StartFailure::Display(message) => Self::Display(message),
            StartFailure::Ended => Self::Ended,
            StartFailure::Other(message) => Self::Worker(message),
        }
    }
}

/// A frame without its content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FrameHeader {
    view: ViewId,
    buffer: BufferId,
    width: u32,
    height: u32,
    damage: Vec<Rect>,
}

/// A dmabuf without its descriptors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DmaBufHeader {
    width: u32,
    height: u32,
    fourcc: u32,
    modifier: u64,
    /// Each plane's offset and stride.
    planes: Vec<(u32, u32)>,
}

fn json(body: &impl Serialize) -> Result<Vec<u8>, WireError> {
    serde_json::to_vec(body).map_err(|err| WireError::Broken(format!("not serializable: {err}")))
}

/// A body read back from its packet's JSON.
pub(crate) fn body<T: for<'de> Deserialize<'de>>(packet: &Packet) -> Result<T, WireError> {
    serde_json::from_slice(&packet.body).map_err(|err| WireError::Broken(format!("an unreadable body: {err}")))
}

impl ToWorker {
    /// `command` as the message that carries it, and the bytes that go with it.
    pub(crate) fn command(command: Command) -> (Self, Option<Vec<u8>>) {
        match command {
            Command::Scheme(token, SchemeResponse::Bytes { data, mime }) => {
                (Self::SchemeBytes { token, mime }, Some(data))
            }
            command => (Self::Command(command), None),
        }
    }

    pub(crate) fn packet(&self, blob: Option<Vec<u8>>) -> Result<Packet, WireError> {
        Ok(Packet {
            body: json(self)?,
            blob,
            fds: Vec::new(),
        })
    }
}

impl FromWorker {
    pub(crate) fn packet(&self) -> Result<Packet, WireError> {
        Ok(Packet {
            body: json(self)?,
            blob: None,
            fds: Vec::new(),
        })
    }

    /// An event of the engine as its packet: a frame's content travels beside its body.
    pub(crate) fn event_packet(event: Event) -> Result<Packet, WireError> {
        let Event::Frame(frame) = event else {
            return Self::Event(event).packet();
        };
        let header = FrameHeader {
            view: frame.view,
            buffer: frame.buffer,
            width: frame.width,
            height: frame.height,
            damage: frame.damage,
        };
        match frame.content {
            FrameContent::DmaBuf(dmabuf) => {
                let fds = dmabuf
                    .planes
                    .iter()
                    .map(|plane| plane.fd.try_clone())
                    .collect::<std::io::Result<Vec<OwnedFd>>>()?;
                let body = Self::DmaBufFrame {
                    frame: header,
                    dmabuf: DmaBufHeader {
                        width: dmabuf.width,
                        height: dmabuf.height,
                        fourcc: dmabuf.fourcc,
                        modifier: dmabuf.modifier,
                        planes: dmabuf.planes.iter().map(|plane| (plane.offset, plane.stride)).collect(),
                    },
                };
                Ok(Packet {
                    body: json(&body)?,
                    blob: None,
                    fds,
                })
            }
            FrameContent::Shm(regions) => {
                let mut pixels = Vec::with_capacity(regions.iter().map(|region| region.pixels.len()).sum());
                for region in &regions {
                    pixels.extend_from_slice(&region.pixels);
                }
                let body = Self::ShmFrame {
                    frame: header,
                    regions: regions.iter().map(|region| region.rect).collect(),
                };
                Ok(Packet {
                    body: json(&body)?,
                    blob: Some(pixels),
                    fds: Vec::new(),
                })
            }
        }
    }
}

/// The frame a [`FromWorker::DmaBufFrame`] packet carries; `known` is the buffer's dmabuf as an earlier frame brought
/// it (one `Arc` per buffer, as the engine hands them out), and the planes this packet brought again are closed.
pub(crate) fn dmabuf_frame(
    frame: FrameHeader,
    dmabuf: DmaBufHeader,
    fds: Vec<OwnedFd>,
    known: Option<Arc<DmaBuf>>,
) -> Result<Frame, WireError> {
    if fds.len() != dmabuf.planes.len() {
        return Err(WireError::Broken(format!(
            "{} plane descriptors for {} planes",
            fds.len(),
            dmabuf.planes.len()
        )));
    }
    let dmabuf = known.unwrap_or_else(|| {
        Arc::new(DmaBuf {
            width: dmabuf.width,
            height: dmabuf.height,
            fourcc: dmabuf.fourcc,
            modifier: dmabuf.modifier,
            planes: fds
                .into_iter()
                .zip(dmabuf.planes)
                .map(|(fd, (offset, stride))| Plane { fd, offset, stride })
                .collect(),
        })
    });
    Ok(Frame {
        view: frame.view,
        buffer: frame.buffer,
        width: frame.width,
        height: frame.height,
        damage: frame.damage,
        content: FrameContent::DmaBuf(dmabuf),
    })
}

/// The frame a [`FromWorker::ShmFrame`] packet carries, its regions cut from the blob.
pub(crate) fn shm_frame(frame: FrameHeader, regions: Vec<Rect>, blob: Option<Vec<u8>>) -> Result<Frame, WireError> {
    let pixels = blob.unwrap_or_default();
    let mut at = 0usize;
    let mut cut = Vec::with_capacity(regions.len());
    for rect in regions {
        let len = usize::try_from(u64::from(rect.width) * u64::from(rect.height) * 4)
            .map_err(|_| WireError::Broken("a region too large".to_owned()))?;
        let end = at
            .checked_add(len)
            .filter(|end| *end <= pixels.len())
            .ok_or_else(|| WireError::Broken("an SHM frame's regions outrun its pixels".to_owned()))?;
        cut.push(ShmRegion {
            rect,
            pixels: pixels[at..end].to_vec(),
        });
        at = end;
    }
    if at != pixels.len() {
        return Err(WireError::Broken("an SHM frame's pixels outrun its regions".to_owned()));
    }
    Ok(Frame {
        view: frame.view,
        buffer: frame.buffer,
        width: frame.width,
        height: frame.height,
        damage: frame.damage,
        content: FrameContent::Shm(cut),
    })
}

impl FrameHeader {
    pub(crate) fn buffer(&self) -> BufferId {
        self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(width: u32, height: u32) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    #[test]
    fn an_shm_frame_comes_back_region_by_region() {
        let event = Event::Frame(Frame {
            view: ViewId(3),
            buffer: BufferId(9),
            width: 4,
            height: 4,
            damage: vec![rect(4, 4)],
            content: FrameContent::Shm(vec![
                ShmRegion {
                    rect: rect(1, 1),
                    pixels: vec![1, 2, 3, 4],
                },
                ShmRegion {
                    rect: rect(2, 1),
                    pixels: vec![5, 6, 7, 8, 9, 10, 11, 12],
                },
            ]),
        });
        let packet = FromWorker::event_packet(event).unwrap();
        let FromWorker::ShmFrame { frame, regions } = body(&packet).unwrap() else {
            panic!("not an SHM frame");
        };
        let frame = shm_frame(frame, regions, packet.blob).unwrap();
        let FrameContent::Shm(regions) = frame.content else {
            panic!("not SHM content");
        };
        assert_eq!(regions[0].pixels, [1, 2, 3, 4]);
        assert_eq!(regions[1].pixels, [5, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!((frame.view, frame.buffer), (ViewId(3), BufferId(9)));
        // pixels that do not add up to the regions are a broken packet
        assert!(
            shm_frame(
                FrameHeader {
                    view: ViewId(3),
                    buffer: BufferId(9),
                    width: 4,
                    height: 4,
                    damage: Vec::new(),
                },
                vec![rect(2, 2)],
                Some(vec![0; 15]),
            )
            .is_err()
        );
    }

    #[test]
    fn a_scheme_answer_s_bytes_travel_in_the_blob() {
        let (message, blob) = ToWorker::command(Command::Scheme(
            SchemeToken(4),
            SchemeResponse::Bytes {
                data: vec![0; 70_000],
                mime: "image/png".to_owned(),
            },
        ));
        assert!(matches!(message, ToWorker::SchemeBytes { token: SchemeToken(4), ref mime } if mime == "image/png"));
        assert_eq!(blob.map(|blob| blob.len()), Some(70_000));
        let (message, blob) = ToWorker::command(Command::Clipboard("x".to_owned()));
        assert!(matches!(message, ToWorker::Command(Command::Clipboard(_))) && blob.is_none());
    }

    #[test]
    fn a_frame_never_goes_through_the_event_s_json() {
        let frame = Event::Frame(Frame {
            view: ViewId(1),
            buffer: BufferId(1),
            width: 1,
            height: 1,
            damage: Vec::new(),
            content: FrameContent::Shm(Vec::new()),
        });
        assert!(serde_json::to_vec(&FromWorker::Event(frame)).is_err());
    }
}
