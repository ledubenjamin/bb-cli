//! Remove terminal control sequences before indexing or rendering remote content.

#[derive(Clone, Copy)]
enum State {
    Text,
    Escape,
    Csi,
    String,
    StringEscape,
}

pub(super) fn clean(bytes: &[u8]) -> String {
    after(bytes, 0)
}

pub(super) fn after(bytes: &[u8], offset: usize) -> String {
    let mut output = String::new();
    sanitized(bytes, |ch, position| {
        if position >= offset {
            output.push(ch);
        }
    });
    output
}

/// Maps a byte position in sanitized UTF-8 back to the original log bytes.
/// ANSI sequences and invalid UTF-8 therefore do not shift search navigation.
pub(super) fn raw_offset(bytes: &[u8], offset: usize) -> Option<usize> {
    let mut cleaned = 0;
    let mut found = None;
    sanitized(bytes, |ch, position| {
        if cleaned == offset {
            found = Some(position);
        }
        cleaned += ch.len_utf8();
    });
    found
}

/// Markers cannot collide with indexed text: sanitization removes both controls.
pub(super) fn highlights(marked: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut offset = 0;
    let mut start = None;
    for ch in marked.chars() {
        match ch {
            '\u{1e}' => start = Some(offset),
            '\u{1f}' => {
                if let Some(start) = start.take() {
                    ranges.push(start..offset);
                }
            }
            _ => offset += ch.len_utf8(),
        }
    }
    ranges
}

fn sanitized(bytes: &[u8], mut emit: impl FnMut(char, usize)) {
    let mut state = State::Text;
    let mut raw_offset = 0;
    let mut character = |ch, position| {
        state = match state {
            State::Text => match ch {
                '\u{1b}' => State::Escape,
                '\u{9b}' => State::Csi,
                '\u{9d}' | '\u{90}' | '\u{9e}' | '\u{9f}' => State::String,
                _ => {
                    if !ch.is_control() || matches!(ch, '\n' | '\t') {
                        emit(ch, position);
                    }
                    State::Text
                }
            },
            State::Escape => match ch {
                '[' => State::Csi,
                ']' | 'P' | '^' | '_' => State::String,
                _ => State::Text,
            },
            State::Csi => {
                if ('@'..='~').contains(&ch) {
                    State::Text
                } else {
                    State::Csi
                }
            }
            State::String => match ch {
                '\u{7}' | '\u{9c}' => State::Text,
                '\u{1b}' => State::StringEscape,
                _ => State::String,
            },
            State::StringEscape => {
                if ch == '\\' {
                    State::Text
                } else {
                    State::String
                }
            }
        };
    };
    for chunk in bytes.utf8_chunks() {
        for (position, ch) in chunk.valid().char_indices() {
            character(ch, raw_offset + position);
        }
        raw_offset += chunk.valid().len();
        if !chunk.invalid().is_empty() {
            character(char::REPLACEMENT_CHARACTER, raw_offset);
            raw_offset += chunk.invalid().len();
        }
    }
}

pub(super) fn lines(bytes: &[u8], offset: u64) -> Vec<super::LogLine> {
    let mut result = Vec::new();
    let mut cursor = offset;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        let mut start = 0;
        while start < line.len() {
            let end = utf8_boundary(line, (start + super::BLOCK_SIZE).min(line.len()));
            result.push(super::LogLine {
                start: cursor + start as u64,
                end: cursor + end as u64,
                text: clean(&line[start..end]).trim_end_matches('\n').to_owned(),
                highlights: Vec::new(),
            });
            if end == line.len() {
                break;
            }
            // Overlap long-line fragments so every supported query crosses a fragment boundary.
            start = utf8_boundary(line, end - 4096);
        }
        cursor += line.len() as u64;
    }
    result
}

/// Back up at most one UTF-8 sequence; malformed bytes still make forward progress.
fn utf8_boundary(bytes: &[u8], mut offset: usize) -> usize {
    let floor = offset.saturating_sub(3);
    while offset < bytes.len() && offset > floor && bytes[offset] & 0xc0 == 0x80 {
        offset -= 1;
    }
    offset
}
