use super::{FlagStack, Flags, STACK_LEN, SetMode, wrap};

const DISAMBIGUATE: Flags = Flags::from_int(0b1);
const REPORT_EVENTS: Flags = Flags::from_int(0b10);

/// Make sure we the overflow works as expected.
#[test]
fn flag_stack_overflow() {
    let last = u8::try_from(STACK_LEN - 1).unwrap();
    assert_eq!(0, wrap(last.wrapping_add(1)));
    assert_eq!(last, wrap(0_u8.wrapping_sub(1)));

    // A ninth push evicts the oldest entry.
    let mut stack = FlagStack::default();
    for i in 0..=8 {
        stack.push(Flags::from_int(i));
    }
    assert_eq!(Flags::from_int(8), stack.current());
}

/// Its easy to get packed struct ordering wrong so this test checks.
#[test]
fn flags_int_ordering() {
    assert_eq!(
        0b1,
        Flags {
            disambiguate: true,
            ..Flags::default()
        }
        .int()
    );
    assert_eq!(
        0b10,
        Flags {
            report_events: true,
            ..Flags::default()
        }
        .int()
    );
    assert_eq!(0b1_1111, Flags::TRUE.int());
    assert_eq!(Flags::default(), Flags::DISABLED);
    for v in 0..32 {
        assert_eq!(v, Flags::from_int(v).int());
    }
}

#[test]
fn flag_stack_push_pop() {
    let mut stack = FlagStack::default();
    stack.push(DISAMBIGUATE);
    assert_eq!(DISAMBIGUATE, stack.current());

    stack.pop(1);
    assert_eq!(Flags::default(), stack.current());
}

#[test]
fn flag_stack_pop_big_number() {
    let mut stack = FlagStack::default();
    stack.pop(100);
    assert_eq!(Flags::default(), stack.current());
}

#[test]
fn flag_stack_set() {
    let mut stack = FlagStack::default();
    stack.set(SetMode::Set, DISAMBIGUATE);
    assert_eq!(DISAMBIGUATE, stack.current());

    stack.set(SetMode::Or, REPORT_EVENTS);
    assert_eq!(Flags::from_int(0b11), stack.current());

    stack.set(SetMode::Not, REPORT_EVENTS);
    assert_eq!(DISAMBIGUATE, stack.current());
}
