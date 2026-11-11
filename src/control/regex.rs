//! The little regular-expression dialect the controller profiles' `match` fields use.
//!
//! Swift matches a port name with `String.range(of:options:[.regularExpression,
//! .caseInsensitive])`, i.e. an unanchored ICU search. The shipped profiles only need
//! literals, `.`, `\s`, the postfix `*` / `+` / `?` and a negative lookahead
//! (`apc.*mini.*mk2(?!.*notes)`), so that is all this implements, with a backtracking
//! matcher: pulling in `regex` would cost the engine its `no_std` build. Anything else
//! in a pattern is refused at parse time rather than silently mismatching.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Char(char),
    Any,
    Space,
    Star(Box<Node>),
    Plus(Box<Node>),
    Opt(Box<Node>),
    NotAhead(Vec<Node>),
}

/// A compiled `match` pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct PortPattern {
    source: String,
    nodes: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternError(pub String);

impl core::fmt::Display for PatternError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "unsupported port pattern: {}", self.0)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PatternError {}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

impl PortPattern {
    pub fn new(source: &str) -> Result<PortPattern, PatternError> {
        let chars: Vec<char> = source.chars().collect();
        let mut i = 0;
        let nodes = parse_seq(&chars, &mut i, false)?;
        Ok(PortPattern {
            source: source.to_string(),
            nodes,
        })
    }

    /// A pattern that matches `text` as written (case-insensitively, anywhere in the name):
    /// no character of it is special. For a port that has no profile and is known by name.
    pub fn literal(text: &str) -> PortPattern {
        PortPattern {
            source: text.to_string(),
            nodes: text.chars().map(|c| Node::Char(lower(c))).collect(),
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Unanchored, case-insensitive search.
    pub fn is_match(&self, text: &str) -> bool {
        let t: Vec<char> = text.chars().map(lower).collect();
        (0..=t.len()).any(|start| match_here(&self.nodes, &t, start))
    }
}

fn parse_seq(chars: &[char], i: &mut usize, in_group: bool) -> Result<Vec<Node>, PatternError> {
    let mut out: Vec<Node> = Vec::new();
    while *i < chars.len() {
        let c = chars[*i];
        *i += 1;
        let atom = match c {
            ')' if in_group => return Ok(out),
            '.' => Node::Any,
            '\\' => {
                let n = *chars
                    .get(*i)
                    .ok_or_else(|| PatternError("trailing backslash".to_string()))?;
                *i += 1;
                match n {
                    's' => Node::Space,
                    '.' | '\\' | '(' | ')' | '*' | '+' | '?' | '-' | '_' => Node::Char(n),
                    other => {
                        return Err(PatternError(alloc::format!("\\{other}")));
                    }
                }
            }
            '(' => {
                if chars.get(*i) == Some(&'?') && chars.get(*i + 1) == Some(&'!') {
                    *i += 2;
                    let inner = parse_seq(chars, i, true)?;
                    Node::NotAhead(inner)
                } else {
                    return Err(PatternError("group".to_string()));
                }
            }
            '*' | '+' | '?' => {
                let prev = out
                    .pop()
                    .ok_or_else(|| PatternError("quantifier with nothing before it".to_string()))?;
                match c {
                    '*' => Node::Star(Box::new(prev)),
                    '+' => Node::Plus(Box::new(prev)),
                    _ => Node::Opt(Box::new(prev)),
                }
            }
            '[' | ']' | '{' | '}' | '|' | '^' | '$' => {
                return Err(PatternError(alloc::format!("{c:?}")));
            }
            other => Node::Char(lower(other)),
        };
        out.push(atom);
    }
    if in_group {
        return Err(PatternError("unclosed group".to_string()));
    }
    Ok(out)
}

fn one(node: &Node, c: char) -> bool {
    match node {
        Node::Char(x) => *x == c,
        Node::Any => c != '\n',
        Node::Space => c.is_whitespace(),
        _ => false,
    }
}

fn match_here(nodes: &[Node], t: &[char], pos: usize) -> bool {
    let Some((first, rest)) = nodes.split_first() else {
        return true;
    };
    match first {
        Node::NotAhead(inner) => !match_here(inner, t, pos) && match_here(rest, t, pos),
        Node::Star(n) | Node::Plus(n) => {
            let min = usize::from(matches!(first, Node::Plus(_)));
            let mut end = pos;
            while end < t.len() && one(n, t[end]) {
                end += 1;
            }
            let mut k = end;
            loop {
                if k - pos >= min && match_here(rest, t, k) {
                    return true;
                }
                if k == pos {
                    return false;
                }
                k -= 1;
            }
        }
        Node::Opt(n) => {
            (pos < t.len() && one(n, t[pos]) && match_here(rest, t, pos + 1))
                || match_here(rest, t, pos)
        }
        n => pos < t.len() && one(n, t[pos]) && match_here(rest, t, pos + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_patterns() {
        let apc = PortPattern::new("apc.*mini.*mk2(?!.*notes)").unwrap();
        assert!(apc.is_match("APC mini mk2 Control"));
        assert!(!apc.is_match("APC mini mk2 Notes"));
        assert!(!apc.is_match("APC mini"));
        let ip = PortPattern::new("inpulse\\s*200").unwrap();
        assert!(ip.is_match("DJControl Inpulse 200 Mk3"));
        assert!(ip.is_match("Inpulse200"));
        assert!(!ip.is_match("btw Bus 1"));
        let mpk = PortPattern::new("mpk.*mini").unwrap();
        assert!(mpk.is_match("MPK Mini Mk II") && mpk.is_match("MPKmini2"));
    }

    #[test]
    fn a_literal_pattern_has_no_special_characters() {
        let p = PortPattern::literal("Foo (USB) 2.0 [a|b]*");
        assert!(p.is_match("Foo (USB) 2.0 [a|b]*"));
        assert!(p.is_match("xx foo (usb) 2.0 [A|B]* 1"));
        assert!(!p.is_match("Foo (USB) 2x0 [a|b]*"));
        assert!(!p.is_match("Foo"));
    }

    #[test]
    fn unsupported_syntax_is_refused() {
        assert!(PortPattern::new("a|b").is_err());
        assert!(PortPattern::new("[ab]").is_err());
        assert!(PortPattern::new("(ab)").is_err());
    }
}
