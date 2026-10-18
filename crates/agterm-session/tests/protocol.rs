//! The wire: every message and control operation round-trips, the frozen layer reads as fixed bytes and across
//! versions, and broken frames are refused.
//!
//! herdr: the framing and version tests of `src/protocol/wire.rs` at `HERDR_REV` (`build/pins.env`), licensed under
//! the Apache License 2.0, modified: postcard, the frame header and the control layer.

use std::fmt::Debug;
use std::io::{self, Read};

use agterm_session::protocol::{
    ApiCall, ApiError, ApiErrorKind, ApiReply, ApiRequest, ApiResponse, AttachClientMessage, AttachRefusal,
    AttachRequest, AttachServerMessage, CAPABILITY_HANDOFF_REQUEST, Control, ControlError, DelegatedReply,
    DelegatedRequest, ErrorCode, ExitStatus, Frame, FrameKind, FramingError, GridSize, HEADER_LEN, HandoffRequest,
    HandoffResult, Hello, JobProcess, KillOutcome, KillResult, LeadRole, MAGIC, MAX_CONTROL_SIZE, MAX_FRAME_SIZE,
    Managed, PROTOCOL_VERSION, ReplyMetadata, Rgb, Role, Screen, SessionRecord, SessionSpec, Welcome, check_hello,
    encode_control, encode_message, read_frame, welcome, write_control, write_message,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A reader that hands out at most `chunk` bytes per read, as a socket may.
struct ChunkedReader {
    data: Vec<u8>,
    at: usize,
    chunk: usize,
}

impl Read for ChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let end = (self.at + self.chunk.min(buf.len())).min(self.data.len());
        let read = end - self.at;
        buf[..read].copy_from_slice(&self.data[self.at..end]);
        self.at = end;
        Ok(read)
    }
}

fn roundtrip_message<M: Serialize + DeserializeOwned + PartialEq + Debug>(message: &M) -> TestResult {
    let mut wire = Vec::new();
    write_message(&mut wire, message)?;
    let frame = read_frame(&mut wire.as_slice())?;
    assert_eq!(frame.kind, FrameKind::Message);
    assert_eq!(frame.protocol, PROTOCOL_VERSION);
    assert_eq!(&frame.message::<M>(PROTOCOL_VERSION)?, message);
    Ok(())
}

fn roundtrip_control(control: &Control) -> TestResult {
    let mut wire = Vec::new();
    write_control(&mut wire, control)?;
    let frame = read_frame(&mut wire.as_slice())?;
    assert_eq!(frame.kind, FrameKind::Control);
    assert_eq!(&frame.control()?, control);
    Ok(())
}

/// A frame of any protocol and kind number, as another version would write it.
fn raw_frame(protocol: u32, kind: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = MAGIC.to_vec();
    frame.extend_from_slice(&protocol.to_le_bytes());
    frame.extend_from_slice(&kind.to_le_bytes());
    frame.extend_from_slice(&u32::try_from(payload.len()).unwrap_or(u32::MAX).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

fn header(protocol: u32, kind: u32, length: u32) -> Vec<u8> {
    let mut header = MAGIC.to_vec();
    header.extend_from_slice(&protocol.to_le_bytes());
    header.extend_from_slice(&kind.to_le_bytes());
    header.extend_from_slice(&length.to_le_bytes());
    header
}

fn job() -> Vec<JobProcess> {
    vec![JobProcess {
        pid: 4242,
        started: 1_234_567,
        group: 4242,
        foreground: 4242,
    }]
}

fn size() -> GridSize {
    GridSize { cols: 141, rows: 41 }
}

fn metadata() -> ReplyMetadata {
    ReplyMetadata {
        foreground: Some(Rgb { r: 1, g: 2, b: 3 }),
        background: Some(Rgb { r: 250, g: 251, b: 252 }),
        cursor: None,
        palette: vec![Rgb { r: 0, g: 0, b: 0 }; 256],
        cell_width_px: 9,
        cell_height_px: 19,
        kitty_graphics: true,
    }
}

#[test]
fn every_api_request_round_trips() {
    let requests = [
        ApiRequest::List,
        ApiRequest::Kill {
            names: vec!["one".into(), "two".into()],
            force: true,
        },
        ApiRequest::Screen {
            name: "one".into(),
            all: true,
        },
        ApiRequest::Type {
            name: "one".into(),
            data: b"echo hi\r".to_vec(),
        },
        ApiRequest::Print {
            name: "one".into(),
            data: b"\x1b]52;c;aGk=\x07".to_vec(),
        },
        ApiRequest::ForegroundJob { shell: 4242 },
        ApiRequest::JobRunning { job: job() },
        ApiRequest::EndJob { job: job() },
    ];
    for (id, request) in requests.into_iter().enumerate() {
        roundtrip_message(&ApiCall {
            id: u64::try_from(id).unwrap() + u64::from(u32::MAX),
            request,
        })
        .unwrap();
    }
}

#[test]
fn every_api_reply_round_trips() {
    let results = [
        Ok(ApiResponse::Sessions(vec![SessionRecord {
            name: "agterm-1".into(),
            clients: 2,
            leader_pid: Some(77),
            created_at: 1_760_000_000,
        }])),
        Ok(ApiResponse::Killed(vec![
            KillResult {
                name: "a".into(),
                outcome: KillOutcome::Killed,
            },
            KillResult {
                name: "b".into(),
                outcome: KillOutcome::Gone,
            },
            KillResult {
                name: "c".into(),
                outcome: KillOutcome::Failed("permission denied".into()),
            },
        ])),
        Ok(ApiResponse::Screen(Screen {
            revision: 9,
            columns: 80,
            rows: 2,
            cursor_column: 3,
            cursor_row: 1,
            alternate: true,
            text: "$ ls\nкириллица 漢字\n".into(),
        })),
        Ok(ApiResponse::Done),
        Ok(ApiResponse::ForegroundJob(Some(job()))),
        Ok(ApiResponse::ForegroundJob(None)),
        Ok(ApiResponse::JobRunning(true)),
        Err(ApiError {
            kind: ApiErrorKind::NoSuchSession,
            message: "no session named x".into(),
        }),
        Err(ApiError {
            kind: ApiErrorKind::Unsupported,
            message: "not here".into(),
        }),
        Err(ApiError {
            kind: ApiErrorKind::Failed,
            message: "it broke".into(),
        }),
    ];
    for (id, result) in results.into_iter().enumerate() {
        roundtrip_message(&ApiReply {
            id: u64::try_from(id).unwrap(),
            result,
        })
        .unwrap();
    }
}

#[test]
fn every_attach_client_message_round_trips() {
    let messages = [
        AttachClientMessage::Attach(AttachRequest {
            name: "agterm-1".into(),
            create: Some(SessionSpec {
                argv: vec!["/bin/zsh".into(), "-l".into()],
                env: vec![
                    ("TERM".into(), "xterm-ghostty".into()),
                    ("AGTERM_LIVE_SESSION".into(), "a".into()),
                ],
                cwd: Some("/home/user".into()),
                tracked_env: vec!["PATH".into()],
            }),
            size: size(),
            managed: Some(Managed {
                nonce: "0123456789abcdef0123456789abcdef".into(),
                claim: true,
            }),
            metadata: metadata(),
        }),
        AttachClientMessage::Attach(AttachRequest {
            name: "agterm-2".into(),
            create: None,
            size: size(),
            managed: None,
            metadata: ReplyMetadata::default(),
        }),
        AttachClientMessage::Input {
            data: vec![0x1b, b'[', b'A', 0xff],
        },
        AttachClientMessage::Resize { size: size() },
        AttachClientMessage::Metadata(metadata()),
        AttachClientMessage::DelegatedReply {
            id: 3,
            epoch: 2,
            reply: DelegatedReply::Clipboard { text: "copied".into() },
        },
        AttachClientMessage::DelegatedReply {
            id: 4,
            epoch: 2,
            reply: DelegatedReply::Denied,
        },
        AttachClientMessage::Detach,
    ];
    for message in &messages {
        roundtrip_message(message).unwrap();
    }
}

#[test]
fn every_attach_server_message_round_trips() {
    let mut messages = vec![
        AttachServerMessage::SnapshotBegin {
            epoch: 1,
            offset: u64::MAX - 1,
            size: size(),
        },
        AttachServerMessage::SnapshotChunk {
            epoch: 1,
            data: vec![b'x'; 64 * 1024],
        },
        AttachServerMessage::SnapshotEnd { epoch: 1 },
        AttachServerMessage::Output {
            offset: 4096,
            data: b"\x1b[31mred\x1b[m".to_vec(),
        },
        AttachServerMessage::Resized {
            offset: 5000,
            size: GridSize { cols: 80, rows: 24 },
        },
        AttachServerMessage::DelegatedRequest {
            id: 3,
            epoch: 2,
            request: DelegatedRequest::ClipboardRead { selection: "c".into() },
        },
        AttachServerMessage::Exit {
            status: ExitStatus::Exited { code: 0, signal: None },
        },
        AttachServerMessage::Exit {
            status: ExitStatus::Exited {
                code: 1,
                signal: Some("Hangup".into()),
            },
        },
        AttachServerMessage::Exit {
            status: ExitStatus::Unknown,
        },
        AttachServerMessage::Shutdown { reason: None },
        AttachServerMessage::Shutdown {
            reason: Some("update".into()),
        },
    ];
    for (generation, role) in [LeadRole::Unowned, LeadRole::Leader, LeadRole::Follower]
        .into_iter()
        .enumerate()
    {
        messages.push(AttachServerMessage::Role {
            role,
            generation: u32::try_from(generation).unwrap(),
        });
    }
    for reason in [
        AttachRefusal::Gone,
        AttachRefusal::CreateFailed,
        AttachRefusal::Unavailable,
    ] {
        messages.push(AttachServerMessage::Refused {
            reason,
            message: "no".into(),
        });
    }
    for message in &messages {
        roundtrip_message(message).unwrap();
    }
}

#[test]
fn every_control_operation_round_trips() {
    for role in [Role::Control, Role::Api, Role::Attach] {
        roundtrip_control(&Control::Hello(Hello {
            role,
            build: "b1".into(),
            capabilities: vec!["x".into()],
        }))
        .unwrap();
    }
    roundtrip_control(&Control::Welcome(Welcome {
        build: "b2".into(),
        capabilities: vec![CAPABILITY_HANDOFF_REQUEST.into()],
        pid: 77,
        error: None,
    }))
    .unwrap();
    roundtrip_control(&Control::Welcome(Welcome {
        build: "b2".into(),
        capabilities: Vec::new(),
        pid: 77,
        error: Some(ControlError::new(ErrorCode::ProtocolMismatch, "mismatch")),
    }))
    .unwrap();
    roundtrip_control(&Control::HandoffRequest(HandoffRequest {
        import_exe: "/opt/agterm/libexec/agterm-session".into(),
        expected_build: "b3".into(),
        expected_protocol: PROTOCOL_VERSION + 1,
    }))
    .unwrap();
    roundtrip_control(&Control::HandoffResult(HandoffResult(Ok(())))).unwrap();
    for code in [
        ErrorCode::ProtocolMismatch,
        ErrorCode::UnknownOp,
        ErrorCode::UnsupportedRole,
        ErrorCode::HandoffFailed,
    ] {
        roundtrip_control(&Control::HandoffResult(HandoffResult(Err(ControlError::new(
            code, "failed",
        )))))
        .unwrap();
    }
}

#[test]
fn the_frame_header_is_frozen() {
    let frame = encode_message(&ApiCall {
        id: 1,
        request: ApiRequest::List,
    })
    .unwrap();
    // id 1 as a varint, then the List variant's index
    let payload = [1_u8, 0];
    assert_eq!(frame, raw_frame(PROTOCOL_VERSION, 2, &payload));
    assert_eq!(&frame[..4], b"AGLS");
    assert_eq!(HEADER_LEN, 16);
}

#[test]
fn the_control_operations_are_frozen() {
    let cases: [(Control, &str); 4] = [
        (
            Control::Hello(Hello {
                role: Role::Control,
                build: "b".into(),
                capabilities: Vec::new(),
            }),
            r#"{"op":"hello.v1","role":"control","build":"b","capabilities":[]}"#,
        ),
        (
            Control::Welcome(Welcome {
                build: "b".into(),
                capabilities: vec![CAPABILITY_HANDOFF_REQUEST.into()],
                pid: 7,
                error: Some(ControlError::new(ErrorCode::ProtocolMismatch, "m")),
            }),
            r#"{"op":"welcome.v1","build":"b","capabilities":["handoff.request.v1"],"pid":7,"error":{"code":"protocol_mismatch","message":"m"}}"#,
        ),
        (
            Control::HandoffRequest(HandoffRequest {
                import_exe: "/x".into(),
                expected_build: "b".into(),
                expected_protocol: 9,
            }),
            r#"{"op":"handoff.request.v1","import_exe":"/x","expected_build":"b","expected_protocol":9}"#,
        ),
        (
            Control::HandoffResult(HandoffResult(Err(ControlError::new(ErrorCode::HandoffFailed, "m")))),
            r#"{"op":"handoff.result.v1","ok":false,"error":{"code":"handoff_failed","message":"m"}}"#,
        ),
    ];
    for (control, json) in cases {
        assert_eq!(
            encode_control(&control).unwrap(),
            raw_frame(PROTOCOL_VERSION, 1, json.as_bytes())
        );
        let frame = read_frame(&mut raw_frame(PROTOCOL_VERSION, 1, json.as_bytes()).as_slice()).unwrap();
        assert_eq!(frame.control().unwrap(), control);
    }
    let ok = encode_control(&Control::HandoffResult(HandoffResult(Ok(())))).unwrap();
    assert_eq!(ok[HEADER_LEN..], *br#"{"op":"handoff.result.v1","ok":true}"#);
}

#[test]
fn a_version_mismatch_is_refused_with_its_message() {
    let newer = check_hello(Role::Api, PROTOCOL_VERSION + 1).unwrap_err();
    assert_eq!(newer.code, ErrorCode::ProtocolMismatch);
    assert_eq!(
        newer.message,
        format!(
            "client protocol {} is newer than server protocol {PROTOCOL_VERSION}; the server predates this build and \
             must hand its sessions over or be restarted",
            PROTOCOL_VERSION + 1
        )
    );
    let older = check_hello(Role::Attach, PROTOCOL_VERSION - 1).unwrap_err();
    assert_eq!(older.code, ErrorCode::ProtocolMismatch);
    assert_eq!(
        older.message,
        format!(
            "client protocol {} is older than server protocol {PROTOCOL_VERSION}; use the agterm-session of the \
             server's build",
            PROTOCOL_VERSION - 1
        )
    );
    assert_eq!(check_hello(Role::Api, PROTOCOL_VERSION), Ok(()));
    assert_eq!(check_hello(Role::Attach, PROTOCOL_VERSION), Ok(()));
}

#[test]
fn a_control_connection_is_welcome_across_protocols() {
    assert_eq!(check_hello(Role::Control, PROTOCOL_VERSION + 7), Ok(()));
    assert_eq!(check_hello(Role::Control, 0), Ok(()));
    let unknown = check_hello(Role::Unknown, PROTOCOL_VERSION).unwrap_err();
    assert_eq!(unknown.code, ErrorCode::UnsupportedRole);
}

#[test]
fn a_welcome_carries_build_capabilities_and_any_refusal() {
    let hello = Hello {
        role: Role::Api,
        build: "client".into(),
        capabilities: Vec::new(),
    };
    let capabilities = vec![CAPABILITY_HANDOFF_REQUEST.to_owned()];
    let accepted = welcome(&hello, PROTOCOL_VERSION, "server", &capabilities);
    assert_eq!(accepted.error, None);
    assert_eq!(accepted.build, "server");
    assert_eq!(accepted.capabilities, capabilities);
    assert_eq!(accepted.pid, std::process::id());
    let refused = welcome(&hello, PROTOCOL_VERSION + 1, "server", &capabilities);
    assert_eq!(refused.error.map(|error| error.code), Some(ErrorCode::ProtocolMismatch));
    assert_eq!(refused.build, "server");
    assert_eq!(refused.capabilities, capabilities);
}

#[test]
fn the_envelope_of_a_newer_protocol_is_still_read() {
    let newer = PROTOCOL_VERSION + 5;
    let hello = br#"{"op":"hello.v1","role":"attach","build":"future","capabilities":["x.v9"],"window":{"a":1}}"#;
    let frame = read_frame(&mut raw_frame(newer, 1, hello).as_slice()).unwrap();
    assert_eq!(frame.protocol, newer);
    let Control::Hello(hello) = frame.control().unwrap() else {
        panic!("not a hello");
    };
    assert_eq!(hello.role, Role::Attach);
    assert_eq!(hello.build, "future");
    let refusal = welcome(&hello, frame.protocol, "server", &[]).error.unwrap();
    assert_eq!(refusal.code, ErrorCode::ProtocolMismatch);
    assert!(
        refusal.message.contains(&format!("client protocol {newer} is newer")),
        "{}",
        refusal.message
    );

    let request =
        br#"{"op":"handoff.request.v1","import_exe":"/new","expected_build":"b","expected_protocol":6,"extra":true}"#;
    let frame = read_frame(&mut raw_frame(newer, 1, request).as_slice()).unwrap();
    assert_eq!(
        frame.control().unwrap(),
        Control::HandoffRequest(HandoffRequest {
            import_exe: "/new".into(),
            expected_build: "b".into(),
            expected_protocol: 6,
        })
    );
}

#[test]
fn what_a_newer_version_adds_reads_as_unknown() {
    let read = |json: &str| read_frame(&mut raw_frame(PROTOCOL_VERSION + 1, 1, json.as_bytes()).as_slice()).unwrap();
    assert_eq!(
        read(r#"{"op":"resize.v2","cols":3}"#).control().unwrap(),
        Control::Unknown
    );
    let Control::Hello(hello) = read(r#"{"op":"hello.v1","role":"render","build":"b","capabilities":[]}"#)
        .control()
        .unwrap()
    else {
        panic!("not a hello");
    };
    assert_eq!(hello.role, Role::Unknown);
    let Control::Welcome(welcome) =
        read(r#"{"op":"welcome.v1","build":"b","capabilities":[],"pid":1,"error":{"code":"melted","message":"m"}}"#)
            .control()
            .unwrap()
    else {
        panic!("not a welcome");
    };
    assert_eq!(welcome.error.map(|error| error.code), Some(ErrorCode::Other));
}

#[test]
fn an_unknown_operation_is_never_sent() {
    assert!(matches!(
        encode_control(&Control::Unknown),
        Err(FramingError::Unencodable(_))
    ));
}

#[test]
fn a_contradictory_handoff_result_is_refused() {
    for json in [
        r#"{"op":"handoff.result.v1","ok":true,"error":{"code":"handoff_failed","message":"m"}}"#,
        r#"{"op":"handoff.result.v1","ok":false}"#,
    ] {
        let frame = read_frame(&mut raw_frame(PROTOCOL_VERSION, 1, json.as_bytes()).as_slice()).unwrap();
        assert!(matches!(frame.control(), Err(FramingError::Malformed(_))), "{json}");
    }
}

#[test]
fn a_truncated_frame_is_refused() {
    let mut payload_cut = header(PROTOCOL_VERSION, 2, 100);
    payload_cut.extend_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]);
    assert!(matches!(
        read_frame(&mut payload_cut.as_slice()),
        Err(FramingError::UnexpectedEof)
    ));
    let header_cut = &header(PROTOCOL_VERSION, 2, 1)[..9];
    assert!(matches!(
        read_frame(&mut &header_cut[..]),
        Err(FramingError::UnexpectedEof)
    ));
    assert!(matches!(read_frame(&mut io::empty()), Err(FramingError::Closed)));
}

#[test]
fn an_oversized_length_is_refused_before_the_payload_is_read() {
    // only the header is there: a reader that tried to fill the payload would report the end of the stream
    let huge = header(PROTOCOL_VERSION, 2, u32::MAX);
    match read_frame(&mut huge.as_slice()) {
        Err(FramingError::Oversized { claimed, max }) => {
            assert_eq!(claimed, usize::try_from(u32::MAX).unwrap());
            assert_eq!(max, MAX_FRAME_SIZE);
        }
        other => panic!("expected Oversized, got {other:?}"),
    }
    let control = header(PROTOCOL_VERSION, 1, u32::try_from(MAX_CONTROL_SIZE + 1).unwrap());
    assert!(matches!(
        read_frame(&mut control.as_slice()),
        Err(FramingError::Oversized { max, .. }) if max == MAX_CONTROL_SIZE
    ));
    let largest = header(PROTOCOL_VERSION, 2, u32::try_from(MAX_FRAME_SIZE).unwrap());
    assert!(matches!(
        read_frame(&mut largest.as_slice()),
        Err(FramingError::UnexpectedEof)
    ));
}

#[test]
fn an_oversized_message_is_not_written() {
    let message = AttachServerMessage::Output {
        offset: 0,
        data: vec![0; MAX_FRAME_SIZE],
    };
    assert!(matches!(encode_message(&message), Err(FramingError::Oversized { max, .. }) if max == MAX_FRAME_SIZE));
}

#[test]
fn a_frame_of_another_protocol_or_kind_is_refused() {
    let mut bad_magic = raw_frame(PROTOCOL_VERSION, 2, &[0]);
    bad_magic[0] = b'X';
    assert!(matches!(read_frame(&mut bad_magic.as_slice()), Err(FramingError::BadMagic(magic)) if magic == *b"XGLS"));
    let unknown_kind = raw_frame(PROTOCOL_VERSION, 3, &[0]);
    assert!(matches!(
        read_frame(&mut unknown_kind.as_slice()),
        Err(FramingError::UnknownKind(3))
    ));

    let frame = read_frame(&mut encode_message(&AttachClientMessage::Detach).unwrap().as_slice()).unwrap();
    assert!(matches!(
        frame.message::<AttachClientMessage>(PROTOCOL_VERSION + 1),
        Err(FramingError::ProtocolChanged { negotiated, got }) if negotiated == PROTOCOL_VERSION + 1 && got == PROTOCOL_VERSION
    ));
    assert!(matches!(
        frame.control(),
        Err(FramingError::WrongKind {
            expected: FrameKind::Control,
            got: FrameKind::Message
        })
    ));
    let control = Frame {
        protocol: PROTOCOL_VERSION,
        kind: FrameKind::Control,
        payload: b"{}".to_vec(),
    };
    assert!(matches!(
        control.message::<AttachClientMessage>(PROTOCOL_VERSION),
        Err(FramingError::WrongKind { .. })
    ));
}

#[test]
fn trailing_bytes_are_refused() {
    let mut payload = encode_message(&AttachClientMessage::Detach).unwrap()[HEADER_LEN..].to_vec();
    let decoded = payload.len();
    payload.push(0xde);
    let frame = read_frame(&mut raw_frame(PROTOCOL_VERSION, 2, &payload).as_slice()).unwrap();
    match frame.message::<AttachClientMessage>(PROTOCOL_VERSION) {
        Err(FramingError::Malformed(message)) => {
            assert!(message.contains("trailing bytes"), "{message}");
            assert!(message.contains(&format!("decoded {decoded}")), "{message}");
        }
        other => panic!("expected trailing bytes refused, got {other:?}"),
    }
    let json = br#"{"op":"hello.v1","role":"api","build":"b","capabilities":[]} x"#;
    let frame = read_frame(&mut raw_frame(PROTOCOL_VERSION, 1, json).as_slice()).unwrap();
    assert!(matches!(frame.control(), Err(FramingError::Malformed(_))));
    let spaced = b"{\"op\":\"hello.v1\",\"role\":\"api\",\"build\":\"b\",\"capabilities\":[]}\n ";
    let frame = read_frame(&mut raw_frame(PROTOCOL_VERSION, 1, spaced).as_slice()).unwrap();
    assert!(matches!(frame.control(), Ok(Control::Hello(_))));
}

#[test]
fn a_malformed_payload_is_refused() {
    let frame = read_frame(&mut raw_frame(PROTOCOL_VERSION, 2, &[0xde, 0xad, 0xbe, 0xef, 1, 2]).as_slice()).unwrap();
    assert!(matches!(
        frame.message::<ApiCall>(PROTOCOL_VERSION),
        Err(FramingError::Malformed(_))
    ));
    let frame = read_frame(&mut raw_frame(PROTOCOL_VERSION, 1, b"not json").as_slice()).unwrap();
    assert!(matches!(frame.control(), Err(FramingError::Malformed(_))));
}

#[test]
fn partial_reads_are_reassembled() {
    let message = AttachClientMessage::Input { data: vec![42; 500] };
    let mut wire = encode_message(&message).unwrap();
    wire.extend(encode_control(&Control::HandoffResult(HandoffResult(Ok(())))).unwrap());
    let mut chunked = ChunkedReader {
        data: wire,
        at: 0,
        chunk: 7,
    };
    let first = read_frame(&mut chunked).unwrap();
    assert_eq!(first.message::<AttachClientMessage>(PROTOCOL_VERSION).unwrap(), message);
    let second = read_frame(&mut chunked).unwrap();
    assert_eq!(second.control().unwrap(), Control::HandoffResult(HandoffResult(Ok(()))));
    assert!(matches!(read_frame(&mut chunked), Err(FramingError::Closed)));
}
