use super::option;
use super::{Click, Command, PromptKind, Redraw};
use crate::os::string_encoding::DecodeError;
use crate::terminal::osc::{Command as OscCommand, Parser};

fn semantic_prompt<'c, 'p>(cmd: &'c OscCommand<'p>) -> &'c Command<'p> {
    match cmd {
        OscCommand::SemanticPrompt(sp) => sp,
        _ => unreachable!(),
    }
}

#[test]
fn osc_133_end_input_start_output() {
    let mut p: Parser = Parser::new(false);

    let input = "133;C";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);
    assert!(semantic_prompt(&cmd).read_option(option::AID).is_none());
    assert!(semantic_prompt(&cmd).read_option(option::CL).is_none());
}

#[test]
fn osc_133_end_input_start_output_extra_contents() {
    let mut p: Parser = Parser::new(false);
    let input = "133;Cextra";
    for ch in input.bytes() {
        p.next(ch);
    }
    assert!(p.end(None).is_none());
}

#[test]
fn osc_133_end_input_start_output_with_options() {
    let mut p: Parser = Parser::new(false);
    let input = "133;C;aid=foo";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);
    assert_eq!(
        b"foo".as_slice(),
        semantic_prompt(&cmd).read_option(option::AID).unwrap()
    );
}

#[test]
fn osc_133_end_input_start_output_with_cmdline() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline=echo bobr kurwa";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);

    semantic_prompt(&cmd).write_command_line(&mut w).unwrap();
    assert_eq!(b"echo bobr kurwa".as_slice(), w.as_slice());
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_3() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline=echo bobr\\nkurwa";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));

    semantic_prompt(&cmd).write_command_line(&mut w).unwrap();
    assert_eq!(b"echo bobr\nkurwa".as_slice(), w.as_slice());
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_4() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline=$'echo bobr kurwa'";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);

    semantic_prompt(&cmd).write_command_line(&mut w).unwrap();
    assert_eq!(b"echo bobr kurwa".as_slice(), w.as_slice());
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_5() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline='echo bobr kurwa'";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);

    semantic_prompt(&cmd).write_command_line(&mut w).unwrap();
    assert_eq!(b"echo bobr kurwa".as_slice(), w.as_slice());
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_6() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline='echo bobr kurwa";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);
    assert!(matches!(
        semantic_prompt(&cmd).write_command_line(&mut w),
        Err(DecodeError::Decode)
    ));
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_7() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline=$'echo bobr kurwa";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);

    assert!(matches!(
        semantic_prompt(&cmd).write_command_line(&mut w),
        Err(DecodeError::Decode)
    ));
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_8() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline=$'";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);
    assert!(matches!(
        semantic_prompt(&cmd).write_command_line(&mut w),
        Err(DecodeError::Decode)
    ));
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_9() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline=";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);

    semantic_prompt(&cmd).write_command_line(&mut w).unwrap();
    assert_eq!(b"".as_slice(), w.as_slice());
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_url_1() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline_url=echo bobr kurwa";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);

    semantic_prompt(&cmd).write_command_line(&mut w).unwrap();
    assert_eq!(b"echo bobr kurwa".as_slice(), w.as_slice());
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_url_2() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline_url=echo bobr%20kurwa";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);

    semantic_prompt(&cmd).write_command_line(&mut w).unwrap();
    assert_eq!(b"echo bobr kurwa".as_slice(), w.as_slice());
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_url_3() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline_url=echo bobr%3bkurwa";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);

    semantic_prompt(&cmd).write_command_line(&mut w).unwrap();
    assert_eq!(b"echo bobr;kurwa".as_slice(), w.as_slice());
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_url_4() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline_url=echo bobr%3kurwa";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);
    assert!(matches!(
        semantic_prompt(&cmd).write_command_line(&mut w),
        Err(DecodeError::Decode)
    ));
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_url_5() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline_url=echo bobr%kurwa";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);
    assert!(matches!(
        semantic_prompt(&cmd).write_command_line(&mut w),
        Err(DecodeError::Decode)
    ));
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_url_6() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline_url=echo bobr kurwa%20";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);

    semantic_prompt(&cmd).write_command_line(&mut w).unwrap();
    assert_eq!(b"echo bobr kurwa ".as_slice(), w.as_slice());
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_url_7() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline_url=echo bobr kurwa%2";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);
    assert!(matches!(
        semantic_prompt(&cmd).write_command_line(&mut w),
        Err(DecodeError::Decode)
    ));
}

#[test]
fn osc_133_end_input_start_output_with_cmdline_url_8() {
    let mut w = Vec::<u8>::new();

    let mut p: Parser = Parser::new(false);
    let input = "133;C;cmdline_url=echo bobr kurwa%";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndInputStartOutput);
    assert!(matches!(
        semantic_prompt(&cmd).write_command_line(&mut w),
        Err(DecodeError::Decode)
    ));
}

#[test]
fn osc_133_fresh_line() {
    let mut p: Parser = Parser::new(false);

    let input = "133;L";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLine);
}

#[test]
fn osc_133_fresh_line_extra_contents() {
    // Random
    {
        let mut p: Parser = Parser::new(false);
        let input = "133;Lol";
        for ch in input.bytes() {
            p.next(ch);
        }
        assert!(p.end(None).is_none());
    }

    // Options
    {
        let mut p: Parser = Parser::new(false);
        let input = "133;L;aid=foo";
        for ch in input.bytes() {
            p.next(ch);
        }
        assert!(p.end(None).is_none());
    }
}

#[test]
fn osc_133_fresh_line_new_prompt() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
    assert!(semantic_prompt(&cmd).read_option(option::AID).is_none());
    assert!(semantic_prompt(&cmd).read_option(option::CL).is_none());
}

#[test]
fn osc_133_fresh_line_new_prompt_with_aid() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A;aid=14";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
    assert_eq!(
        b"14".as_slice(),
        semantic_prompt(&cmd).read_option(option::AID).unwrap()
    );
}

#[test]
fn osc_133_fresh_line_new_prompt_with_eq_in_aid() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A;aid=a=b";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
    assert_eq!(
        b"a=b".as_slice(),
        semantic_prompt(&cmd).read_option(option::AID).unwrap()
    );
}

#[test]
fn osc_133_fresh_line_new_prompt_with_cl_line() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A;cl=line";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
    assert_eq!(semantic_prompt(&cmd).read_option(option::CL), Some(Click::Line));
}

#[test]
fn osc_133_fresh_line_new_prompt_with_cl_m() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A;cl=m";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
    assert_eq!(semantic_prompt(&cmd).read_option(option::CL), Some(Click::Multiple));
}

#[test]
fn osc_133_fresh_line_new_prompt_with_invalid_cl() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A;cl=invalid";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
    assert!(semantic_prompt(&cmd).read_option(option::CL).is_none());
}

#[test]
fn osc_133_fresh_line_new_prompt_with_trailing_semicolon() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A;";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
}

#[test]
fn osc_133_fresh_line_new_prompt_with_bare_key() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A;barekey";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
    assert!(semantic_prompt(&cmd).read_option(option::AID).is_none());
    assert!(semantic_prompt(&cmd).read_option(option::CL).is_none());
}

#[test]
fn osc_133_fresh_line_new_prompt_with_multiple_options() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A;aid=foo;cl=line";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
    assert_eq!(
        b"foo".as_slice(),
        semantic_prompt(&cmd).read_option(option::AID).unwrap()
    );
    assert_eq!(semantic_prompt(&cmd).read_option(option::CL), Some(Click::Line));
}

#[test]
fn osc_133_fresh_line_new_prompt_default_redraw() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
    assert!(semantic_prompt(&cmd).read_option(option::REDRAW).is_none());
}

#[test]
fn osc_133_fresh_line_new_prompt_with_redraw_0() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A;redraw=0";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
    assert_eq!(
        semantic_prompt(&cmd).read_option(option::REDRAW).unwrap(),
        Redraw::False
    );
}

#[test]
fn osc_133_fresh_line_new_prompt_with_redraw_1() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A;redraw=1";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
    assert_eq!(semantic_prompt(&cmd).read_option(option::REDRAW).unwrap(), Redraw::True);
}

#[test]
fn osc_133_fresh_line_new_prompt_with_invalid_redraw() {
    let mut p: Parser = Parser::new(false);

    let input = "133;A;redraw=x";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::FreshLineNewPrompt);
    assert!(semantic_prompt(&cmd).read_option(option::REDRAW).is_none());
}

#[test]
fn osc_133_prompt_start() {
    let mut p: Parser = Parser::new(false);

    let input = "133;P";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::PromptStart);
    assert!(semantic_prompt(&cmd).read_option(option::PROMPT_KIND).is_none());
}

#[test]
fn osc_133_prompt_start_with_k_i() {
    let mut p: Parser = Parser::new(false);

    let input = "133;P;k=i";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::PromptStart);
    assert_eq!(
        semantic_prompt(&cmd).read_option(option::PROMPT_KIND),
        Some(PromptKind::Initial)
    );
}

#[test]
fn osc_133_prompt_start_with_k_r() {
    let mut p: Parser = Parser::new(false);

    let input = "133;P;k=r";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::PromptStart);
    assert_eq!(
        semantic_prompt(&cmd).read_option(option::PROMPT_KIND),
        Some(PromptKind::Right)
    );
}

#[test]
fn osc_133_prompt_start_with_k_c() {
    let mut p: Parser = Parser::new(false);

    let input = "133;P;k=c";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::PromptStart);
    assert_eq!(
        semantic_prompt(&cmd).read_option(option::PROMPT_KIND),
        Some(PromptKind::Continuation)
    );
}

#[test]
fn osc_133_prompt_start_with_k_s() {
    let mut p: Parser = Parser::new(false);

    let input = "133;P;k=s";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::PromptStart);
    assert_eq!(
        semantic_prompt(&cmd).read_option(option::PROMPT_KIND),
        Some(PromptKind::Secondary)
    );
}

#[test]
fn osc_133_prompt_start_with_invalid_k() {
    let mut p: Parser = Parser::new(false);

    let input = "133;P;k=x";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::PromptStart);
    assert!(semantic_prompt(&cmd).read_option(option::PROMPT_KIND).is_none());
}

#[test]
fn osc_133_prompt_start_extra_contents() {
    let mut p: Parser = Parser::new(false);
    let input = "133;Pextra";
    for ch in input.bytes() {
        p.next(ch);
    }
    assert!(p.end(None).is_none());
}

#[test]
fn osc_133_new_command() {
    let mut p: Parser = Parser::new(false);

    let input = "133;N";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::NewCommand);
    assert!(semantic_prompt(&cmd).read_option(option::AID).is_none());
    assert!(semantic_prompt(&cmd).read_option(option::CL).is_none());
}

#[test]
fn osc_133_new_command_with_aid() {
    let mut p: Parser = Parser::new(false);

    let input = "133;N;aid=foo";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::NewCommand);
    assert_eq!(
        b"foo".as_slice(),
        semantic_prompt(&cmd).read_option(option::AID).unwrap()
    );
}

#[test]
fn osc_133_new_command_with_cl_line() {
    let mut p: Parser = Parser::new(false);

    let input = "133;N;cl=line";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::NewCommand);
    assert_eq!(semantic_prompt(&cmd).read_option(option::CL), Some(Click::Line));
}

#[test]
fn osc_133_new_command_with_multiple_options() {
    let mut p: Parser = Parser::new(false);

    let input = "133;N;aid=foo;cl=line";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::NewCommand);
    assert_eq!(
        b"foo".as_slice(),
        semantic_prompt(&cmd).read_option(option::AID).unwrap()
    );
    assert_eq!(semantic_prompt(&cmd).read_option(option::CL), Some(Click::Line));
}

#[test]
fn osc_133_new_command_extra_contents() {
    let mut p: Parser = Parser::new(false);
    let input = "133;Nextra";
    for ch in input.bytes() {
        p.next(ch);
    }
    assert!(p.end(None).is_none());
}

#[test]
fn osc_133_end_prompt_start_input() {
    let mut p: Parser = Parser::new(false);

    let input = "133;B";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndPromptStartInput);
}

#[test]
fn osc_133_end_prompt_start_input_extra_contents() {
    let mut p: Parser = Parser::new(false);
    let input = "133;Bextra";
    for ch in input.bytes() {
        p.next(ch);
    }
    assert!(p.end(None).is_none());
}

#[test]
fn osc_133_end_prompt_start_input_with_options() {
    let mut p: Parser = Parser::new(false);
    let input = "133;B;aid=foo";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndPromptStartInput);
    assert_eq!(
        b"foo".as_slice(),
        semantic_prompt(&cmd).read_option(option::AID).unwrap()
    );
}

#[test]
fn osc_133_end_prompt_start_input_terminate_eol() {
    let mut p: Parser = Parser::new(false);

    let input = "133;I";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(
        semantic_prompt(&cmd).action,
        super::Action::EndPromptStartInputTerminateEol
    );
}

#[test]
fn osc_133_end_prompt_start_input_terminate_eol_extra_contents() {
    let mut p: Parser = Parser::new(false);
    let input = "133;Iextra";
    for ch in input.bytes() {
        p.next(ch);
    }
    assert!(p.end(None).is_none());
}

#[test]
fn osc_133_end_prompt_start_input_terminate_eol_with_options() {
    let mut p: Parser = Parser::new(false);
    let input = "133;I;aid=foo";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(
        semantic_prompt(&cmd).action,
        super::Action::EndPromptStartInputTerminateEol
    );
    assert_eq!(
        b"foo".as_slice(),
        semantic_prompt(&cmd).read_option(option::AID).unwrap()
    );
}

#[test]
fn osc_133_end_command() {
    let mut p: Parser = Parser::new(false);

    let input = "133;D";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndCommand);
    assert!(semantic_prompt(&cmd).read_option(option::EXIT_CODE).is_none());
    assert!(semantic_prompt(&cmd).read_option(option::AID).is_none());
    assert!(semantic_prompt(&cmd).read_option(option::ERR).is_none());
}

#[test]
fn osc_133_end_command_extra_contents() {
    let mut p: Parser = Parser::new(false);
    let input = "133;Dextra";
    for ch in input.bytes() {
        p.next(ch);
    }
    assert!(p.end(None).is_none());
}

#[test]
fn osc_133_end_command_with_exit_code_0() {
    let mut p: Parser = Parser::new(false);

    let input = "133;D;0";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndCommand);
    assert_eq!(semantic_prompt(&cmd).read_option(option::EXIT_CODE), Some(0));
}

#[test]
fn osc_133_end_command_with_exit_code_and_aid() {
    let mut p: Parser = Parser::new(false);

    let input = "133;D;12;aid=foo";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(None).unwrap();
    assert!(matches!(cmd, OscCommand::SemanticPrompt(_)));
    assert_eq!(semantic_prompt(&cmd).action, super::Action::EndCommand);
    assert_eq!(
        b"foo".as_slice(),
        semantic_prompt(&cmd).read_option(option::AID).unwrap()
    );
    assert_eq!(semantic_prompt(&cmd).read_option(option::EXIT_CODE), Some(12));
}

#[test]
fn option_read_aid() {
    assert_eq!(b"test123".as_slice(), option::AID.read(b"aid=test123").unwrap());
    assert_eq!(b"myaid".as_slice(), option::AID.read(b"cl=line;aid=myaid;k=i").unwrap());
    assert!(option::AID.read(b"cl=line;k=i").is_none());
    assert_eq!(b"".as_slice(), option::AID.read(b"aid=").unwrap());
    assert_eq!(b"last".as_slice(), option::AID.read(b"k=i;aid=last").unwrap());
    assert_eq!(b"first".as_slice(), option::AID.read(b"aid=first;k=i").unwrap());
    assert!(option::AID.read(b"").is_none());
    assert!(option::AID.read(b"aid").is_none());
    assert_eq!(b"value".as_slice(), option::AID.read(b";;aid=value;;").unwrap());
}

#[test]
fn option_read_cl() {
    assert_eq!(option::CL.read(b"cl=line").unwrap(), Click::Line);
    assert_eq!(option::CL.read(b"cl=m").unwrap(), Click::Multiple);
    assert_eq!(option::CL.read(b"cl=v").unwrap(), Click::ConservativeVertical);
    assert_eq!(option::CL.read(b"cl=w").unwrap(), Click::SmartVertical);
    assert!(option::CL.read(b"cl=invalid").is_none());
    assert!(option::CL.read(b"aid=foo").is_none());
}

#[test]
fn option_read_prompt_kind() {
    assert_eq!(option::PROMPT_KIND.read(b"k=i").unwrap(), PromptKind::Initial);
    assert_eq!(option::PROMPT_KIND.read(b"k=r").unwrap(), PromptKind::Right);
    assert_eq!(option::PROMPT_KIND.read(b"k=c").unwrap(), PromptKind::Continuation);
    assert_eq!(option::PROMPT_KIND.read(b"k=s").unwrap(), PromptKind::Secondary);
    assert!(option::PROMPT_KIND.read(b"k=x").is_none());
    assert!(option::PROMPT_KIND.read(b"k=ii").is_none());
    assert!(option::PROMPT_KIND.read(b"k=").is_none());
}

#[test]
fn option_read_err() {
    assert_eq!(b"some_error".as_slice(), option::ERR.read(b"err=some_error").unwrap());
    assert!(option::ERR.read(b"aid=foo").is_none());
}

#[test]
fn option_read_redraw() {
    assert_eq!(option::REDRAW.read(b"redraw=1").unwrap(), Redraw::True);
    assert_eq!(option::REDRAW.read(b"redraw=0").unwrap(), Redraw::False);
    assert_eq!(option::REDRAW.read(b"redraw=last").unwrap(), Redraw::Last);
    assert!(option::REDRAW.read(b"redraw=2").is_none());
    assert!(option::REDRAW.read(b"redraw=10").is_none());
    assert!(option::REDRAW.read(b"redraw=").is_none());
}

#[test]
fn option_read_special_key() {
    assert!(option::SPECIAL_KEY.read(b"special_key=1").unwrap());
    assert!(!option::SPECIAL_KEY.read(b"special_key=0").unwrap());
    assert!(option::SPECIAL_KEY.read(b"special_key=x").is_none());
}

#[test]
fn option_read_click_events() {
    use super::ClickEvents;
    assert!(option::CLICK_EVENTS.read(b"click_events=yes").is_none());
    assert!(option::CLICK_EVENTS.read(b"click_events=0").is_none());
    assert_eq!(
        option::CLICK_EVENTS.read(b"click_events=1").unwrap(),
        ClickEvents::Absolute
    );
    assert_eq!(
        option::CLICK_EVENTS.read(b"click_events=2").unwrap(),
        ClickEvents::Relative
    );
}

#[test]
fn option_read_exit_code() {
    assert_eq!(option::EXIT_CODE.read(b"42").unwrap(), 42);
    assert_eq!(option::EXIT_CODE.read(b"0").unwrap(), 0);
    assert_eq!(option::EXIT_CODE.read(b"-1").unwrap(), -1);
    assert!(option::EXIT_CODE.read(b"abc").is_none());
    assert_eq!(option::EXIT_CODE.read(b"127;aid=foo").unwrap(), 127);
}

// No Ghostty test. The exit code is parsed with
// `lib.parseInt(i32, full, 10)`, which accepts a leading sign (the type is
// signed) and, since `73768913b`, refuses `_` digit separators (the
// `std.fmt.parseInt` it replaced read `1_2` as 12).
#[test]
fn option_read_exit_code_follows_zig_parse_int_but_refuses_digit_separators() {
    assert_eq!(Some(12), option::EXIT_CODE.read(b"+12"));
    assert_eq!(Some(-12), option::EXIT_CODE.read(b"-12;aid=foo"));
    assert_eq!(None, option::EXIT_CODE.read(b"1_2"));
    assert_eq!(None, option::EXIT_CODE.read(b"-1_2;aid=foo"));
    assert_eq!(None, option::EXIT_CODE.read(b"_12"));
    assert_eq!(None, option::EXIT_CODE.read(b"12_"));
}

#[test]
fn osc_133_exit_codes_reject_digit_separators() {
    let mut p: Parser = Parser::new(false);
    for ch in "133;D;-4_2;aid=foo".bytes() {
        p.next(ch);
    }
    let cmd = p.end(None).unwrap();
    assert_eq!(None, semantic_prompt(&cmd).read_option(option::EXIT_CODE));
}
