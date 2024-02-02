#[derive(Debug, Eq, PartialEq)]
pub enum TerminalOutput {
    SetCursorPos { x: usize, y: usize },
    ClearForwards,
    ClearBackwards,
    ClearAll,
    Data(Vec<u8>),
    Invalid,
}

enum CsiParserState {
    Params(Vec<u8>),
    Finished(u8),
    Invalid,
}

fn finalize_csi_buf(buf: &[u8]) -> Option<usize> {
    if buf.is_empty() {
        return None;
    }
    Some(
        std::str::from_utf8(buf)
            .expect("Ascii digits should always result in a valid utf8 string")
            .parse()
            .expect("Digits should always parse as usize"),
    )
}

fn is_csi_terminator(b: u8) -> bool {
    matches!(
        b,
        b'A' | b'B' | b'C' | b'D' | b'E' | b'F' | b'G' | b'H' | b'J' | b'K' | b'S' | b'T' | b'f'
    )
}

struct CsiParser {
    state: CsiParserState,
    params: Vec<Option<usize>>,
}

impl CsiParser {
    fn new() -> CsiParser {
        CsiParser {
            state: CsiParserState::Params(Vec::new()),
            params: Vec::new(),
        }
    }

    fn push(&mut self, b: u8) {
        if let CsiParserState::Finished(_) | CsiParserState::Invalid = &self.state {
            panic!("CsiParser should not be pushed to once finished");
        }

        match &mut self.state {
            CsiParserState::Params(buf) => {
                if is_csi_terminator(b) {
                    self.params.push(finalize_csi_buf(buf));
                    self.state = CsiParserState::Finished(b);
                } else if b == b';' {
                    self.params.push(finalize_csi_buf(buf));
                    buf.clear();
                } else if b.is_ascii_digit() {
                    buf.push(b);
                } else {
                    self.state = CsiParserState::Invalid
                }
            }
            CsiParserState::Finished(_) | CsiParserState::Invalid => {
                unreachable!();
            }
        }
    }

    fn extract_param(&self, i: usize) -> Option<usize> {
        self.params.get(i).copied().flatten()
    }
}

enum AnsiParserInner {
    Empty,
    Escape,
    Csi(CsiParser),
}

pub struct AnsiParser {
    inner: AnsiParserInner,
}

impl AnsiParser {
    pub fn new() -> AnsiParser {
        AnsiParser {
            inner: AnsiParserInner::Empty,
        }
    }

    pub fn push(&mut self, incoming: &[u8]) -> Vec<TerminalOutput> {
        let mut output = Vec::new();
        let mut data_output = Vec::new();
        for b in incoming {
            match &mut self.inner {
                AnsiParserInner::Empty => {
                    if *b == b'\x1b' {
                        self.inner = AnsiParserInner::Escape;
                        continue;
                    }

                    data_output.push(*b);
                }
                AnsiParserInner::Escape => {
                    if !data_output.is_empty() {
                        output.push(TerminalOutput::Data(std::mem::take(&mut data_output)));
                    }

                    match b {
                        b'[' => {
                            self.inner = AnsiParserInner::Csi(CsiParser::new());
                        }
                        _ => {
                            let b_utf8 = std::char::from_u32(*b as u32);
                            println!("Unhandled escape sequence {b_utf8:?} {b:x}");
                            self.inner = AnsiParserInner::Empty;
                        }
                    }
                }
                AnsiParserInner::Csi(parser) => {
                    parser.push(*b);
                    match parser.state {
                        CsiParserState::Finished(b'H') => {
                            output.push(TerminalOutput::SetCursorPos {
                                x: parser.extract_param(0).unwrap_or(1),
                                y: parser.extract_param(1).unwrap_or(1),
                            });
                            self.inner = AnsiParserInner::Empty;
                        }
                        CsiParserState::Finished(b'J') => {
                            let ret = match parser.extract_param(0).unwrap_or(0) {
                                0 => TerminalOutput::ClearForwards,
                                1 => TerminalOutput::ClearBackwards,
                                2 | 3 => TerminalOutput::ClearAll,
                                _ => TerminalOutput::Invalid,
                            };
                            output.push(ret);
                            self.inner = AnsiParserInner::Empty;
                        }
                        CsiParserState::Finished(esc) => {
                            println!(
                                "Unhandled csi code: {:?} {esc:x}",
                                std::char::from_u32(esc as u32)
                            );
                            self.inner = AnsiParserInner::Empty;
                        }
                        CsiParserState::Invalid => {
                            output.push(TerminalOutput::Invalid);
                            self.inner = AnsiParserInner::Empty;
                        }
                        _ => {}
                    }
                }
            }
        }

        if !data_output.is_empty() {
            output.push(TerminalOutput::Data(data_output));
        }

        output
    }
}

#[cfg(test)]
mod test {
    use super::{AnsiParser, TerminalOutput};

    #[test]
    fn test_set_cursor_position() {
        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[32;15H");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            TerminalOutput::SetCursorPos { x: 32, y: 15 }
        ));

        let parsed = output_buffer.push(b"\x1b[;32H");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            TerminalOutput::SetCursorPos { x: 1, y: 32 }
        ));

        let parsed = output_buffer.push(b"\x1b[32H");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            TerminalOutput::SetCursorPos { x: 32, y: 1 }
        ));

        let parsed = output_buffer.push(b"\x1b[32;H");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            TerminalOutput::SetCursorPos { x: 32, y: 1 }
        ));

        let parsed = output_buffer.push(b"\x1b[H");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            TerminalOutput::SetCursorPos { x: 1, y: 1 }
        ));

        let parsed = output_buffer.push(b"\x1b[;H");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            TerminalOutput::SetCursorPos { x: 1, y: 1 }
        ));
    }

    #[test]
    fn test_clear() {
        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[J");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], TerminalOutput::ClearForwards,));

        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[0J");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], TerminalOutput::ClearForwards,));

        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[1J");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], TerminalOutput::ClearBackwards,));

        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[2J");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], TerminalOutput::ClearAll,));
    }

    #[test]
    fn test_invalid_clear() {
        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[8J");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], TerminalOutput::Invalid,));
    }

    #[test]
    fn test_invalid_csi() {
        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[-23;H");
        assert!(matches!(parsed[0], TerminalOutput::Invalid));

        let mut output_buffer = AnsiParser::new();
        let parsed = output_buffer.push(b"\x1b[asdf");
        assert!(matches!(parsed[0], TerminalOutput::Invalid));
    }
}
