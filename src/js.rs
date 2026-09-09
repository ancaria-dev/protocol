//! The agent's JavaScript handled as text: what the build strips out of it,
//! and what it reads out of it.
//!
//! `compact` is the whole of the minification. It removes comments, indentation
//! and blank lines and collapses runs of spaces, and it does nothing else: no
//! renaming, no reordering, no dropping of anything that was declared. Every
//! agent module shares one scope and QuickJS is the engine, so a minifier that
//! decided a top-level function was unused, or that two helpers could share a
//! name, would produce a bundle that loads and then silently does less than it
//! says. Line breaks between statements are kept for the same reason: automatic
//! semicolon insertion is part of the language, and joining lines changes what
//! the parser sees.
//!
//! `sites` reads the hook names back out. Every site is installed through a call
//! that takes its own name first and its address second, `hook("goldDelta",
//! RVA.goldDelta, ...)`, so the list comes from the code rather than from a
//! table kept beside it: a table is one more thing to forget, and a hook missing
//! from the list is a hook nobody can switch off.

/// Where the scanner is. Comments are the only thing it drops; the other states
/// exist so that a `//` inside a string, or a quote inside a regular
/// expression, is not mistaken for one.
#[derive(Clone, Copy, PartialEq)]
enum State {
    Code,
    Line,
    Block,
    /// A string, holding the quote that ends it.
    Text(char),
    Template,
    Regex,
}

/// Minifies one module: comments and layout out, code untouched.
pub fn compact(source: &str) -> String {
    scan(source, true)
}

/// Removes the comments and leaves the layout alone. What `sites` reads, so
/// that an example written in a comment is not offered as a hook.
pub fn strip_comments(source: &str) -> String {
    scan(source, false)
}

fn scan(source: &str, tight: bool) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut state = State::Code;
    // Whether anything but whitespace has been written on this line yet, and
    // whether a space is owed before the next code character. Together they are
    // what turns indentation and blank lines into nothing.
    let mut code_on_line = false;
    let mut owed = false;
    // The last code character, which is how a regular expression is told from a
    // division: `/` after a value divides, `/` after an operator or a comma
    // opens a literal.
    let mut prev = '\0';
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied().unwrap_or('\0');
        match state {
            State::Code => {
                if c == '/' && next == '/' {
                    state = State::Line;
                    i += 2;
                    continue;
                }
                if c == '/' && next == '*' {
                    state = State::Block;
                    i += 2;
                    continue;
                }
                if c == '\n' {
                    line(&mut out, &mut code_on_line, &mut owed, tight);
                    i += 1;
                    continue;
                }
                if c == ' ' || c == '\t' || c == '\r' {
                    if tight {
                        // Leading whitespace is dropped rather than owed, so an
                        // indented line arrives at the margin.
                        owed = code_on_line;
                    } else {
                        out.push(c);
                    }
                    i += 1;
                    continue;
                }
                emit(&mut out, c, &mut code_on_line, &mut owed, tight);
                state = match c {
                    '"' | '\'' => State::Text(c),
                    '`' => State::Template,
                    '/' if opens_regex(prev) => State::Regex,
                    _ => State::Code,
                };
                prev = c;
                i += 1;
            }
            State::Line => {
                // The newline itself is left to Code, which decides whether the
                // line it ends had anything on it.
                if c == '\n' {
                    state = State::Code;
                    continue;
                }
                i += 1;
            }
            State::Block => {
                if c == '*' && next == '/' {
                    state = State::Code;
                    i += 2;
                    continue;
                }
                // A comment that spans lines must not glue the code on either
                // side of it together.
                if c == '\n' {
                    line(&mut out, &mut code_on_line, &mut owed, tight);
                }
                i += 1;
            }
            State::Text(quote) => {
                if c == '\\' {
                    emit(&mut out, c, &mut code_on_line, &mut owed, tight);
                    if i + 1 < chars.len() {
                        emit(&mut out, next, &mut code_on_line, &mut owed, tight);
                    }
                    i += 2;
                    continue;
                }
                emit(&mut out, c, &mut code_on_line, &mut owed, tight);
                if c == quote || c == '\n' {
                    // A raw newline inside a string is a syntax error, so the
                    // string was never one: going back to Code keeps a typo
                    // from swallowing the rest of the file.
                    state = State::Code;
                    prev = quote;
                }
                i += 1;
            }
            State::Template => {
                if c == '\\' {
                    emit(&mut out, c, &mut code_on_line, &mut owed, tight);
                    if i + 1 < chars.len() {
                        emit(&mut out, next, &mut code_on_line, &mut owed, tight);
                    }
                    i += 2;
                    continue;
                }
                // Template literals may span lines, so this one is written out
                // as it stands rather than reflowed.
                emit(&mut out, c, &mut code_on_line, &mut owed, tight);
                if c == '`' {
                    state = State::Code;
                    prev = '`';
                }
                i += 1;
            }
            State::Regex => {
                if c == '\\' {
                    emit(&mut out, c, &mut code_on_line, &mut owed, tight);
                    if i + 1 < chars.len() {
                        emit(&mut out, next, &mut code_on_line, &mut owed, tight);
                    }
                    i += 2;
                    continue;
                }
                if c == '[' {
                    // A `/` inside a character class does not end the literal.
                    let (consumed, text) = char_class(&chars[i..]);
                    for ch in text.chars() {
                        emit(&mut out, ch, &mut code_on_line, &mut owed, tight);
                    }
                    i += consumed;
                    continue;
                }
                if c == '\n' {
                    // No regular expression spans a line, so this `/` was a
                    // division after all. Recovering here rather than reading
                    // on keeps one guess from desynchronising the whole file.
                    state = State::Code;
                    continue;
                }
                emit(&mut out, c, &mut code_on_line, &mut owed, tight);
                if c == '/' {
                    state = State::Code;
                    prev = '/';
                }
                i += 1;
            }
        }
    }
    out
}

fn emit(out: &mut String, c: char, code_on_line: &mut bool, owed: &mut bool, tight: bool) {
    if tight && *owed {
        out.push(' ');
        *owed = false;
    }
    out.push(c);
    *code_on_line = true;
}

fn line(out: &mut String, code_on_line: &mut bool, owed: &mut bool, tight: bool) {
    if !tight || *code_on_line {
        out.push('\n');
    }
    *code_on_line = false;
    *owed = false;
}

/// Reads to the end of a character class, returning how many characters that
/// took and the text of it.
fn char_class(rest: &[char]) -> (usize, String) {
    let mut text = String::new();
    let mut i = 0;
    while i < rest.len() {
        let c = rest[i];
        text.push(c);
        if c == '\\' && i + 1 < rest.len() {
            text.push(rest[i + 1]);
            i += 2;
            continue;
        }
        i += 1;
        if c == ']' || c == '\n' {
            break;
        }
    }
    (i, text)
}

/// Whether a `/` here opens a regular expression. After a value (a name, a
/// number, a closing bracket, the end of a string) it is a division; after
/// anything else it is a literal.
fn opens_regex(prev: char) -> bool {
    !(prev.is_alphanumeric()
        || prev == '_'
        || prev == '$'
        || prev == ')'
        || prev == ']'
        || prev == '}'
        || prev == '"'
        || prev == '\''
        || prev == '`'
        || prev == '/')
}

/// The hook sites one module installs, in the order it installs them.
///
/// Matched on the shape of the call rather than on a list of helper names, so
/// that a module wrapping `hook` in something of its own (health has one) does
/// not quietly drop its sites out of the list.
pub fn sites(source: &str) -> Vec<String> {
    let text: Vec<char> = strip_comments(source).chars().collect();
    let mut found: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 4 <= text.len() {
        if text[i] == 'R' && text[i + 1] == 'V' && text[i + 2] == 'A' && text[i + 3] == '.' {
            if let Some(name) = named_before(&text, i) {
                if !found.contains(&name) {
                    found.push(name);
                }
            }
            i += 4;
            continue;
        }
        i += 1;
    }
    found
}

/// Walks back from `RVA.` over `, "name"` and the call that opened it.
fn named_before(text: &[char], at: usize) -> Option<String> {
    let mut i = skip_back(text, at.checked_sub(1)?)?;
    if text[i] != ',' {
        return None;
    }
    i = skip_back(text, i.checked_sub(1)?)?;
    if text[i] != '"' {
        return None;
    }
    let end = i;
    let mut start = i.checked_sub(1)?;
    while text[start] != '"' {
        start = start.checked_sub(1)?;
    }
    let name: String = text[start + 1..end].iter().collect();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        || name.starts_with(|c: char| c.is_ascii_digit())
    {
        return None;
    }
    // Whatever opened the call has to be a name: this is an argument list, not
    // a bare pair of values.
    let paren = skip_back(text, start.checked_sub(1)?)?;
    if text[paren] != '(' {
        return None;
    }
    let before = paren.checked_sub(1)?;
    if !(text[before].is_ascii_alphanumeric() || text[before] == '_') {
        return None;
    }
    Some(name)
}

fn skip_back(text: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while text[i].is_whitespace() {
        i = i.checked_sub(1)?;
    }
    Some(i)
}

#[cfg(test)]
mod tests {
    #[test]
    fn comments_go_and_code_stays() {
        let source = "// a note\nvar a = 1;  // trailing\n\n/* block\n   spanning */\nvar b = 2;\n";
        assert_eq!(super::compact(source), "var a = 1;\nvar b = 2;\n");
    }

    #[test]
    fn a_slash_in_a_string_is_not_a_comment() {
        let source = "var url = \"https://ancaria.dev\"; // real\n";
        assert_eq!(super::compact(source), "var url = \"https://ancaria.dev\";\n");
    }

    #[test]
    fn a_regex_survives_with_its_slashes() {
        let source = "var shape = /^TYPE_[A-Z0-9_/]+$/; // note\nvar half = 4 / 2;\n";
        assert_eq!(
            super::compact(source),
            "var shape = /^TYPE_[A-Z0-9_/]+$/;\nvar half = 4 / 2;\n"
        );
    }

    /// Every statement keeps its own line. Joining them would leave automatic
    /// semicolon insertion deciding where one ends, which is a different
    /// program.
    #[test]
    fn lines_are_not_joined() {
        let source = "var a = 1\nvar b = 2\n";
        assert_eq!(super::compact(source), "var a = 1\nvar b = 2\n");
    }

    #[test]
    fn indentation_goes_but_spacing_between_tokens_stays() {
        let source = "function f() {\n        return 1 + 2;\n}\n";
        assert_eq!(super::compact(source), "function f() {\nreturn 1 + 2;\n}\n");
    }

    #[test]
    fn sites_are_read_off_the_calls() {
        let source = "hook(\"goldDelta\", RVA.goldDelta, {});\n\
                      attachHp(\"hpDamage\",\n    RVA.hpDamage, {});\n\
                      hook(\"goldDelta\", RVA.goldDelta, {});\n";
        assert_eq!(super::sites(source), vec!["goldDelta", "hpDamage"]);
    }

    /// A site named in a comment is not a site.
    #[test]
    fn sites_ignore_comments() {
        let source = "// hook(\"example\", RVA.example, ...)\nhook(\"real\", RVA.real, {});\n";
        assert_eq!(super::sites(source), vec!["real"]);
    }

    /// An address used without a name is not a site either: `at(RVA.x)` is a
    /// lookup, not an attach.
    #[test]
    fn a_bare_address_is_not_a_site() {
        let source = "var fn = new NativeFunction(at(RVA.typeNameFn), \"pointer\");\n";
        assert!(super::sites(source).is_empty());
    }
}
