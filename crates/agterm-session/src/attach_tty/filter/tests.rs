use super::*;

fn filtered(bytes: &[u8]) -> Vec<u8> {
    let mut filter = OutputFilter::new();
    let mut out = Vec::new();
    filter.feed(bytes, &mut out);
    out
}

/// `bytes` fed in two reads cut at every byte, and one byte at a time: the same output as whole.
fn assert_split_safe(bytes: &[u8], expected: &[u8]) {
    assert_eq!(filtered(bytes), expected, "whole: {:?}", String::from_utf8_lossy(bytes));
    for cut in 1..bytes.len() {
        let mut filter = OutputFilter::new();
        let mut out = Vec::new();
        filter.feed(&bytes[..cut], &mut out);
        filter.feed(&bytes[cut..], &mut out);
        assert_eq!(out, expected, "cut at {cut}: {:?}", String::from_utf8_lossy(bytes));
    }
    let mut filter = OutputFilter::new();
    let mut out = Vec::new();
    for byte in bytes {
        filter.feed(std::slice::from_ref(byte), &mut out);
    }
    assert_eq!(out, expected, "byte by byte");
}

#[test]
fn the_queries_the_server_answers_are_dropped_wherever_the_reads_cut_them() {
    let queries: [&[u8]; 21] = [
        b"\x1b[c",
        b"\x1b[0c",
        b"\x1b[>c",
        b"\x1b[>0c",
        b"\x1b[=c",
        b"\x1b[5n",
        b"\x1b[6n",
        b"\x1b[?6n",
        b"\x1b[?996n",
        b"\x1b[?2026$p",
        b"\x1b[4$p",
        b"\x1b[>q",
        b"\x1b[?u",
        b"\x1b[14t",
        b"\x1b[16t",
        b"\x1b[18t",
        b"\x1b[21t",
        b"\x1bP$qm\x1b\\",
        b"\x1bP+q544e\x1b\\",
        b"\x1bZ",
        b"\x05",
    ];
    for query in queries {
        let mut stream = b"ab".to_vec();
        stream.extend_from_slice(query);
        stream.extend_from_slice(b"cd");
        assert_split_safe(&stream, b"abcd");
    }
}

#[test]
fn the_colour_and_clipboard_queries_are_dropped_with_either_terminator() {
    let queries: [&[u8]; 9] = [
        b"\x1b]10;?\x07",
        b"\x1b]11;?\x1b\\",
        b"\x1b]12;?\x07",
        b"\x1b]4;1;?\x07",
        b"\x1b]4;1;?;2;?\x1b\\",
        b"\x1b]21;foreground=?;background=?\x1b\\",
        b"\x1b]52;c;?\x07",
        b"\x1b]52;;?\x1b\\",
        b"\x1b]5522;type=read;dGV4dC9wbGFpbg==\x1b\\",
    ];
    for query in queries {
        let mut stream = b"ab".to_vec();
        stream.extend_from_slice(query);
        stream.extend_from_slice(b"cd");
        assert_split_safe(&stream, b"abcd");
    }
}

#[test]
fn everything_else_passes_unchanged() {
    let kept: [&[u8]; 16] = [
        b"plain text \xe2\x82\xac\r\n",
        b"\x1b[1;31mred\x1b[0m",
        b"\x1b[2J\x1b[H",
        b"\x1b[?1049h\x1b[?25l",
        b"\x1b[22;0t",
        b"\x1b[>1u\x1b[<u",
        b"\x1b[u",
        b"\x1b]0;title\x07",
        b"\x1b]2;title\x1b\\",
        b"\x1b]10;#ffffff\x07",
        b"\x1b]4;1;rgb:ff/00/00\x07",
        b"\x1b]52;c;aGVsbG8=\x07",
        b"\x1b]133;B\x07",
        b"\x1bPq#0;2;0;0;0#1~~@@\x1b\\",
        b"\x1b_Ga=T,f=100;AAAA\x1b\\",
        b"\x1b(B\x1b7\x1b8",
    ];
    for bytes in kept {
        assert_split_safe(bytes, bytes);
    }
}

#[test]
fn an_osc_4_keeps_its_settings_and_loses_its_queries() {
    assert_split_safe(b"\x1b]4;1;red;2;?;3;blue\x07", b"\x1b]4;1;red;3;blue\x07");
}

#[test]
fn decset_loses_the_modes_the_server_reports_and_win32_input() {
    assert_split_safe(b"\x1b[?2031h", b"");
    assert_split_safe(b"\x1b[?2048l", b"");
    assert_split_safe(b"\x1b[?1049;2031;2004h", b"\x1b[?1049;2004h");
    // what a Windows session's pseudoconsole asks of its terminal as it starts
    assert_split_safe(b"\x1b[?9001h\x1b[?1004h", b"\x1b[?1004h");
    assert_split_safe(b"\x1b[?9001;1004l", b"\x1b[?1004l");
    assert_split_safe(b"\x1b[?1000h", b"\x1b[?1000h");
}

#[test]
fn a_prompt_mark_gets_redraw_0() {
    assert_split_safe(b"\x1b]133;A\x07", b"\x1b]133;A;redraw=0\x07");
    assert_split_safe(b"\x1b]133;A\x1b\\", b"\x1b]133;A;redraw=0\x1b\\");
    assert_split_safe(
        b"\x1b]133;A;aid=14;cl=line\x07",
        b"\x1b]133;A;aid=14;cl=line;redraw=0\x07",
    );
    assert_split_safe(b"\x1b]133;A;redraw=1\x07", b"\x1b]133;A;redraw=0\x07");
    assert_split_safe(b"\x1b]133;A;redraw=0\x07", b"\x1b]133;A;redraw=0\x07");
}

#[test]
fn a_long_string_passes_as_it_comes() {
    let mut write = b"\x1b]52;c;".to_vec();
    write.extend(std::iter::repeat_n(b'A', STRING_LIMIT * 2));
    write.extend_from_slice(b"\x07");
    assert_eq!(filtered(&write), write);
    let mut filter = OutputFilter::new();
    let mut out = Vec::new();
    filter.feed(&write[..STRING_LIMIT + 100], &mut out);
    assert!(out.len() > STRING_LIMIT, "a long OSC is passed on before its end");
}

#[test]
fn a_c1_byte_in_ground_is_text_as_ghostty_decodes_it() {
    assert_split_safe(b"a\x9b>cb", b"a\x9b>cb");
}

#[test]
fn an_abandoned_sequence_passes_as_the_terminal_abandons_it() {
    assert_split_safe(b"\x1b[1\x18x", b"\x1b[1\x18x");
    assert_split_safe(b"\x1b]11;?\x18x", b"\x1b]11;?\x18x");
}

#[test]
fn the_ground_places_are_where_no_sequence_or_character_is_cut() {
    let stream = b"\x1b[1;31mA\x1b]0;title\x1b\\B\x1bPq#0\x1b\\C\xe2\x82\xacD";
    for cut in 1..stream.len() {
        let mut filter = OutputFilter::new();
        let mut out = Vec::new();
        filter.feed(&stream[..cut], &mut out);
        let at = out.len();
        let mut marked = out.clone();
        if filter.at_ground() {
            filter.feed(&stream[cut..], &mut marked);
        } else {
            let taken = filter.feed_until_ground(&stream[cut..], &mut marked);
            assert!(filter.at_ground());
            let mut fresh = OutputFilter::new();
            let mut ignored = Vec::new();
            fresh.feed(&stream[..cut + taken], &mut ignored);
            assert!(fresh.at_ground(), "cut {cut} + {taken}");
            filter.feed(&stream[cut + taken..], &mut marked);
        }
        assert_eq!(marked, stream, "cut at {cut} (output {at})");
    }
}

#[test]
fn cancel_ends_the_sequence_held() {
    let mut filter = OutputFilter::new();
    let mut out = Vec::new();
    filter.feed(b"\x1b[3", &mut out);
    assert!(!filter.at_ground());
    filter.cancel(&mut out);
    assert!(filter.at_ground());
    assert_eq!(out, b"\x1b[3\x18");
    filter.feed(b"1mx", &mut out);
    assert_eq!(out, b"\x1b[3\x181mx");
}

#[test]
fn a_dropped_string_takes_its_st_and_nothing_after() {
    assert_split_safe(b"\x1b]11;?\x1b[1m", b"\x1b[1m");
    assert_split_safe(b"\x1bP$qm\x1b[2m", b"\x1b[2m");
}
