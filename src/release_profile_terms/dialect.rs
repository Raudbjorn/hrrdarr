//! Independent bounded adapter for release-term delimiters and capture semantics.
//! Captures become unnamed engine slots; no generated name can collide with user names.
use super::TermError;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Default)]
struct Options {
    explicit: bool,
    whitespace: bool,
    multiline: bool,
}
#[derive(Debug)]
enum Piece {
    Text(String),
    Reference(String, bool),
}
#[derive(Debug)]
struct Capture {
    name: Option<String>,
    slot: usize,
}

pub(super) enum Term<'a> {
    Literal(&'a str),
    Regex { pattern: &'a str, flags: &'a str },
}

// The source form is an unanchored, line-local, greedy delimiter recognition.
// Escaping a slash changes the regex body, not which final delimiter is recognized.
pub(super) fn recognize(term: &str) -> Term<'_> {
    for line in term.split('\n') {
        if let (Some(first), Some(last)) = (line.find('/'), line.rfind('/')) {
            if first != last {
                let suffix = &line[last + 1..];
                let end = suffix.bytes().take_while(u8::is_ascii_lowercase).count();
                return Term::Regex {
                    pattern: &line[first + 1..last],
                    flags: &suffix[..end],
                };
            }
        }
    }
    Term::Literal(term)
}
fn text(pieces: &mut Vec<Piece>, value: &str) {
    if let Some(Piece::Text(prior)) = pieces.last_mut() {
        prior.push_str(value);
    } else {
        pieces.push(Piece::Text(value.to_owned()));
    }
}
fn space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 12)
}
fn name(value: &str) -> Result<(), TermError> {
    if value.is_empty() {
        return Err(TermError::InvalidSyntax);
    }
    // Explicit-number/duplicate/balancing captures require capture-stack semantics.
    if value.bytes().all(|b| b.is_ascii_digit()) || value.contains('-') {
        return Err(TermError::UnsupportedDialect);
    }
    if !value
        .chars()
        .enumerate()
        .all(|(i, c)| c == '_' || c.is_alphanumeric() && (i != 0 || !c.is_numeric()))
    {
        return Err(TermError::InvalidSyntax);
    }
    Ok(())
}
fn reference(value: &str) -> Result<String, TermError> {
    if value.is_empty() || value.starts_with(['+', '-']) {
        return Err(TermError::InvalidSyntax);
    }
    Ok(value.to_owned())
}

// Shared escape handling for classes and ordinary pattern text. Returns None only
// for numbered/named references, which need the completed capture table.
fn escape(
    pattern: &str,
    start: usize,
    in_class: bool,
) -> Result<Option<(String, usize)>, TermError> {
    let bytes = pattern.as_bytes();
    let code = *bytes.get(start + 1).ok_or(TermError::InvalidSyntax)?;
    let mut end = start + 2;
    if matches!(
        code,
        b'p' | b'P' | b'g' | b'K' | b'R' | b'X' | b'h' | b'H' | b'V' | b'N' | b'O' | b'U'
    ) {
        return Err(TermError::UnsupportedDialect);
    }
    if code == b'c' {
        let letter = bytes
            .get(end)
            .copied()
            .ok_or(TermError::InvalidSyntax)?
            .to_ascii_uppercase();
        if !(b'@'..=b'_').contains(&letter) {
            return Err(TermError::InvalidSyntax);
        }
        return Ok(Some((format!(r"\x{:02x}", letter - b'@'), end + 1)));
    }
    if code == b'0' || in_class && (b'1'..=b'7').contains(&code) {
        let mut value = u16::from(code - b'0');
        for _ in 0..2 {
            if let Some(next @ b'0'..=b'7') = bytes.get(end).copied() {
                value = value * 8 + u16::from(next - b'0');
                end += 1;
            } else {
                break;
            }
        }
        return Ok(Some((format!(r"\x{:02x}", value & 255), end)));
    }
    if in_class {
        match code {
            b'b' => return Ok(Some((r"\x08".into(), end))),
            b'A' | b'z' | b'Z' | b'G' | b'B' | b'k' | b'8' | b'9' => {
                return Err(TermError::InvalidSyntax);
            }
            _ => {}
        }
    } else {
        if code == b'Z' {
            return Ok(Some((r"(?=\n?\z)".into(), end)));
        }
        if code == b'k' || (b'1'..=b'9').contains(&code) {
            return Ok(None);
        }
    }
    if matches!(code, b'<' | b'>') {
        return Ok(Some((format!(r"\x{code:02x}"), end)));
    }
    if matches!(code, b'x' | b'u') {
        if bytes.get(end) == Some(&b'{') {
            return Err(TermError::UnsupportedDialect);
        }
        let length = if code == b'x' { 2 } else { 4 };
        let hex = bytes
            .get(end..end + length)
            .ok_or(TermError::InvalidSyntax)?;
        if !hex.iter().all(u8::is_ascii_hexdigit) {
            return Err(TermError::InvalidSyntax);
        }
        let value = u32::from_str_radix(&pattern[end..end + length], 16)
            .map_err(|_| TermError::InvalidSyntax)?;
        if (0xd800..=0xdfff).contains(&value) {
            return Err(TermError::UnsupportedDialect);
        }
        end += length;
    } else {
        if code.is_ascii_alphabetic() && !b"abefnrtvdDwWsSAzGbB".contains(&code) || code == b'_' {
            return Err(TermError::InvalidSyntax);
        }
        end = start
            + 1
            + pattern[start + 1..]
                .chars()
                .next()
                .ok_or(TermError::InvalidSyntax)?
                .len_utf8();
    }
    Ok(Some((pattern[start..end].to_owned(), end)))
}

pub(super) fn normalize(pattern: &str, flags: &str) -> Result<String, TermError> {
    let mut options = Options::default();
    let mut prefix = String::new();
    for flag in flags.bytes() {
        match flag {
            b'n' => options.explicit = true,
            b'x' => options.whitespace = true,
            b'm' => {
                options.multiline = true;
                prefix.push('m');
            }
            b'i' | b's' => prefix.push(char::from(flag)),
            _ => return Err(TermError::InvalidSyntax),
        }
    }
    let mut pieces = Vec::new();
    if !prefix.is_empty() {
        text(&mut pieces, &format!("(?{prefix})"));
    }
    let mut captures = Vec::new();
    let mut scopes = Vec::new();
    let mut condition_ends = Vec::new();
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if options.whitespace && space(bytes[i]) {
            i += 1;
            continue;
        }
        if options.whitespace && bytes[i] == b'#' {
            i = pattern[i..].find('\n').map_or(bytes.len(), |n| i + n + 1);
            continue;
        }
        match bytes[i] {
            b'[' => {
                let mut class = String::from("[");
                i += 1;
                if bytes.get(i) == Some(&b'^') {
                    class.push('^');
                    i += 1;
                }
                if bytes.get(i) == Some(&b']') {
                    class.push_str(r"\]");
                    i += 1;
                }
                loop {
                    match bytes.get(i) {
                        None => return Err(TermError::InvalidSyntax),
                        Some(b'\\') => {
                            let (escaped, end) =
                                escape(pattern, i, true)?.ok_or(TermError::InvalidSyntax)?;
                            class.push_str(&escaped);
                            i = end;
                        }
                        Some(b'-') if matches!(bytes.get(i + 1), Some(b'[' | b'-')) => {
                            return Err(TermError::UnsupportedDialect);
                        }
                        Some(b']') => {
                            class.push(']');
                            i += 1;
                            break;
                        }
                        Some(b'[' | b'&' | b'~') => {
                            class.push('\\');
                            class.push(char::from(bytes[i]));
                            i += 1;
                        }
                        Some(_) => {
                            let width = pattern[i..]
                                .chars()
                                .next()
                                .ok_or(TermError::InvalidSyntax)?
                                .len_utf8();
                            class.push_str(&pattern[i..i + width]);
                            i += width;
                        }
                    }
                }
                // x is implemented by this scanner, so class whitespace remains significant.
                // Rust class-set operators are escaped: .NET treats those characters literally.
                text(&mut pieces, &class);
            }
            b'\\' => {
                if let Some((escaped, end)) = escape(pattern, i, false)? {
                    text(&mut pieces, &escaped);
                    i = end;
                    continue;
                }
                let start = i;
                i += 1;
                let Some(&next) = bytes.get(i) else {
                    return Err(TermError::InvalidSyntax);
                };
                if (b'1'..=b'9').contains(&next) {
                    let n = i;
                    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                    }
                    pieces.push(Piece::Reference(pattern[n..i].to_owned(), false));
                } else if next == b'k' {
                    i += 1;
                    let close = match bytes.get(i) {
                        Some(b'<') => b'>',
                        Some(b'\'') => b'\'',
                        _ => return Err(TermError::InvalidSyntax),
                    };
                    i += 1;
                    let n = i;
                    while bytes.get(i).is_some_and(|b| *b != close) {
                        i += 1;
                    }
                    if i == bytes.len() {
                        return Err(TermError::InvalidSyntax);
                    }
                    pieces.push(Piece::Reference(reference(&pattern[n..i])?, false));
                    i += 1;
                } else {
                    // Reject engine-specific constructs rather than admitting another dialect.
                    if matches!(next, b'g' | b'K' | b'R' | b'X' | b'h' | b'H' | b'V' | b'N') {
                        return Err(TermError::UnsupportedDialect);
                    }
                    i += pattern[i..]
                        .chars()
                        .next()
                        .ok_or(TermError::InvalidSyntax)?
                        .len_utf8();
                    text(&mut pieces, &pattern[start..i]);
                }
            }
            b'(' => {
                if pattern[i..].starts_with("(?#") {
                    let end = pattern[i + 3..].find(')').ok_or(TermError::InvalidSyntax)?;
                    i += 4 + end;
                    continue;
                }
                if scopes.len() >= super::MAX_AST_DEPTH {
                    return Err(TermError::LimitExceeded);
                }
                if !pattern[i..].starts_with("(?") {
                    scopes.push(options);
                    if options.explicit {
                        text(&mut pieces, "(?:");
                    } else {
                        captures.push(Capture {
                            name: None,
                            slot: captures.len() + 1,
                        });
                        text(&mut pieces, "(");
                    }
                    i += 1;
                    continue;
                }
                if pattern[i..].starts_with("(?(") {
                    scopes.push(options);
                    if pattern[i + 3..].starts_with('?') {
                        // Keep the condition lookaround as an independently scoped group.
                        text(&mut pieces, "(?(");
                        condition_ends.push(scopes.len() + 1);
                        i += 2;
                    } else {
                        let start = i + 3;
                        let end =
                            pattern[start..].find(')').ok_or(TermError::InvalidSyntax)? + start;
                        let raw = &pattern[start..end];
                        let raw = raw
                            .strip_prefix('<')
                            .and_then(|v| v.strip_suffix('>'))
                            .or_else(|| raw.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                            .unwrap_or(raw);
                        pieces.push(Piece::Reference(reference(raw)?, true));
                        i = end + 1;
                    }
                    continue;
                }
                let look = ["(?<=", "(?<!", "(?=", "(?!", "(?:", "(?>"]
                    .into_iter()
                    .find(|v| pattern[i..].starts_with(v));
                if let Some(open) = look {
                    scopes.push(options);
                    text(&mut pieces, open);
                    i += open.len();
                    continue;
                }
                if pattern[i..].starts_with("(?<") || pattern[i..].starts_with("(?'") {
                    let close = if bytes[i + 2] == b'<' { '>' } else { '\'' };
                    let start = i + 3;
                    let end = pattern[start..]
                        .find(close)
                        .ok_or(TermError::InvalidSyntax)?
                        + start;
                    let capture_name = &pattern[start..end];
                    name(capture_name)?;
                    if captures
                        .iter()
                        .any(|c: &Capture| c.name.as_deref() == Some(capture_name))
                    {
                        return Err(TermError::UnsupportedDialect);
                    }
                    captures.push(Capture {
                        name: Some(capture_name.to_owned()),
                        slot: captures.len() + 1,
                    });
                    scopes.push(options);
                    text(&mut pieces, "(");
                    i = end + 1;
                    continue;
                }
                let mut next_options = options;
                let mut on = String::new();
                let mut off = String::new();
                let mut negate = false;
                let mut saw_flag = false;
                let mut j = i + 2;
                loop {
                    match bytes.get(j).copied() {
                        Some(b'-') if !negate => negate = true,
                        Some(flag @ (b'i' | b'm' | b's' | b'x' | b'n')) => {
                            saw_flag = true;
                            match flag {
                                b'n' => next_options.explicit = !negate,
                                b'x' => next_options.whitespace = !negate,
                                b'm' => {
                                    next_options.multiline = !negate;
                                    if negate {
                                        off.push('m');
                                    } else {
                                        on.push('m');
                                    }
                                }
                                _ => {
                                    if negate {
                                        off.push(char::from(flag));
                                    } else {
                                        on.push(char::from(flag));
                                    }
                                }
                            }
                        }
                        Some(end @ (b':' | b')')) => {
                            if !saw_flag {
                                return Err(TermError::InvalidSyntax);
                            }
                            if end == b':' {
                                scopes.push(options);
                            }
                            options = next_options;
                            if !on.is_empty() || !off.is_empty() {
                                let minus = if off.is_empty() {
                                    String::new()
                                } else {
                                    format!("-{off}")
                                };
                                text(&mut pieces, &format!("(?{on}{minus}{}", char::from(end)));
                            } else if end == b':' {
                                text(&mut pieces, "(?:");
                            }
                            i = j + 1;
                            break;
                        }
                        Some(_) => return Err(TermError::UnsupportedDialect),
                        None => return Err(TermError::InvalidSyntax),
                    }
                    j += 1;
                }
            }
            b'$' if !options.multiline => {
                // .NET's non-multiline dollar also permits one terminal LF.
                text(&mut pieces, r"(?=\n?\z)");
                i += 1;
            }
            b')' => {
                let closed_depth = scopes.len();
                options = scopes.pop().ok_or(TermError::InvalidSyntax)?;
                text(&mut pieces, ")");
                if condition_ends.last() == Some(&closed_depth) {
                    condition_ends.pop();
                    text(&mut pieces, ")");
                }
                i += 1;
            }
            _ => {
                let width = pattern[i..]
                    .chars()
                    .next()
                    .ok_or(TermError::InvalidSyntax)?
                    .len_utf8();
                text(&mut pieces, &pattern[i..i + width]);
                i += width;
            }
        }
    }
    if !scopes.is_empty() {
        return Err(TermError::InvalidSyntax);
    }
    let mut by_number = BTreeMap::new();
    let mut by_name = BTreeMap::new();
    // .NET numbers unnamed captures first, then named captures, regardless of location.
    for capture in captures
        .iter()
        .filter(|c| c.name.is_none())
        .chain(captures.iter().filter(|c| c.name.is_some()))
    {
        by_number.insert(by_number.len() + 1, capture.slot);
        if let Some(name) = &capture.name {
            by_name.insert(name.as_str(), capture.slot);
        }
    }
    let mut result = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(value) => result.push_str(&value),
            Piece::Reference(value, conditional) => {
                let number = value.parse::<usize>().ok();
                let slot = number
                    .and_then(|n| by_number.get(&n))
                    .or_else(|| by_name.get(value.as_str()));
                let Some(slot) = slot else {
                    // Multi-digit undefined escapes can be .NET octal; never reinterpret them as another engine's backref.
                    return Err(if number.is_some_and(|n| n >= 10) {
                        TermError::UnsupportedDialect
                    } else {
                        TermError::InvalidSyntax
                    });
                };
                if conditional {
                    result.push_str(&format!("(?({slot})"));
                } else {
                    result.push_str(&format!("\\k<{slot}>"));
                }
            }
        }
    }
    Ok(result)
}
