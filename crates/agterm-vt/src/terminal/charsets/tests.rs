use super::{Charset, table};

/// Every charset, as Zig's test iterates the enum's fields.
const ALL: [Charset; 4] = [Charset::Utf8, Charset::Ascii, Charset::British, Charset::DecSpecial];

#[test]
fn charset_tables() {
    for field in ALL {
        // utf8 has no table
        if field == Charset::Utf8 {
            continue;
        }
        let tbl = table(field);

        // Yes, I could use `table_len` here, but I want to explicitly use a hardcoded constant so that if there are
        // miscompilations or a comptime issue, we catch it.
        assert_eq!(256_usize, tbl.len());
    }
}

#[test]
fn the_tables_map_what_they_change() {
    assert_eq!(table(Charset::Ascii)[usize::from(b'#')], u16::from(b'#'));
    assert_eq!(table(Charset::British)[usize::from(b'#')], 0x00a3);
    assert_eq!(table(Charset::DecSpecial)[usize::from(b'q')], 0x2500);
    assert_eq!(table(Charset::DecSpecial)[usize::from(b'A')], u16::from(b'A'));
}
