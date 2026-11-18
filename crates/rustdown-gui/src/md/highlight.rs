#![forbid(unsafe_code)]
//! Colour-coded code blocks in the preview: a small built-in highlighter
//! for common languages (keywords, strings, comments, numbers, types and
//! function names), so no syntax library is loaded.  A block is scanned
//! when it first shows and then kept in egui's per-frame cache while it
//! stays on screen.

use egui::{
    Color32, FontId, TextFormat,
    cache::{ComputerMut, FrameCache},
    text::{LayoutJob, LayoutSection},
};

/// Token colours: VS Code's default Light+ and Dark+ themes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CodeColors {
    plain: Color32,
    keyword: Color32,
    string: Color32,
    comment: Color32,
    number: Color32,
    kind: Color32,
    function: Color32,
}

impl CodeColors {
    const fn new(dark: bool, plain: Color32) -> Self {
        if dark {
            Self {
                plain,
                keyword: Color32::from_rgb(0x56, 0x9C, 0xD6),
                string: Color32::from_rgb(0xCE, 0x91, 0x78),
                comment: Color32::from_rgb(0x6A, 0x99, 0x55),
                number: Color32::from_rgb(0xB5, 0xCE, 0xA8),
                kind: Color32::from_rgb(0x4E, 0xC9, 0xB0),
                function: Color32::from_rgb(0xDC, 0xDC, 0xAA),
            }
        } else {
            Self {
                plain,
                keyword: Color32::from_rgb(0x00, 0x00, 0xFF),
                string: Color32::from_rgb(0xA3, 0x15, 0x15),
                comment: Color32::from_rgb(0x00, 0x80, 0x00),
                number: Color32::from_rgb(0x09, 0x86, 0x58),
                kind: Color32::from_rgb(0x26, 0x7F, 0x99),
                function: Color32::from_rgb(0x79, 0x5E, 0x26),
            }
        }
    }
}

/// What the highlighter knows about a language.
struct Lang {
    keywords: &'static [&'static str],
    /// Built-in type names (coloured as types).
    types: &'static [&'static str],
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// String delimiters, longest first (`"""` before `"`).
    quotes: &'static [&'static str],
    /// Identifiers starting with a capital letter are types.
    capitalized_types: bool,
    /// Keywords match in any case (SQL).
    case_insensitive: bool,
}

const RUST: Lang = Lang {
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true",
        "type", "unsafe", "use", "where", "while",
    ],
    types: &[
        "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32",
        "i64", "i128", "isize", "f32", "f64",
    ],
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    // No `'`: it also starts lifetimes.
    quotes: &["\""],
    capitalized_types: true,
    case_insensitive: false,
};

const PYTHON: Lang = Lang {
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def",
        "del", "elif", "else", "except", "False", "finally", "for", "from", "global", "if",
        "import", "in", "is", "lambda", "match", "None", "nonlocal", "not", "or", "pass", "raise",
        "return", "self", "True", "try", "while", "with", "yield",
    ],
    types: &[
        "int", "float", "str", "bool", "list", "dict", "set", "tuple", "bytes", "object",
    ],
    line_comments: &["#"],
    block_comment: None,
    quotes: &["\"\"\"", "'''", "\"", "'"],
    capitalized_types: true,
    case_insensitive: false,
};

const JAVASCRIPT: Lang = Lang {
    keywords: &[
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "of",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "type",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    types: &[
        "string", "number", "boolean", "any", "unknown", "never", "object", "bigint", "symbol",
    ],
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &["\"", "'", "`"],
    capitalized_types: true,
    case_insensitive: false,
};

const JSON: Lang = Lang {
    keywords: &["true", "false", "null"],
    types: &[],
    line_comments: &[],
    block_comment: None,
    quotes: &["\""],
    capitalized_types: false,
    case_insensitive: false,
};

/// C, C++, C#, Java, Go, Kotlin, Swift, Dart: one shared keyword list.
const C_LIKE: Lang = Lang {
    keywords: &[
        "abstract",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "defer",
        "delete",
        "do",
        "else",
        "enum",
        "extends",
        "extern",
        "false",
        "final",
        "finally",
        "fn",
        "for",
        "func",
        "fun",
        "go",
        "goto",
        "if",
        "implements",
        "import",
        "in",
        "interface",
        "internal",
        "is",
        "let",
        "namespace",
        "new",
        "nil",
        "null",
        "nullptr",
        "object",
        "operator",
        "override",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "sealed",
        "self",
        "sizeof",
        "static",
        "struct",
        "super",
        "switch",
        "template",
        "this",
        "throw",
        "throws",
        "true",
        "try",
        "typedef",
        "typename",
        "union",
        "using",
        "val",
        "var",
        "virtual",
        "void",
        "volatile",
        "when",
        "where",
        "while",
        "#include",
        "#define",
        "#ifdef",
        "#ifndef",
        "#endif",
        "#pragma",
    ],
    types: &[
        "int", "long", "short", "char", "float", "double", "bool", "boolean", "byte", "string",
        "unsigned", "signed", "auto", "size_t", "uint", "int32", "int64", "uint8", "uint32",
        "uint64", "float64", "error", "rune",
    ],
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &["\"", "'", "`"],
    capitalized_types: true,
    case_insensitive: false,
};

const SHELL: Lang = Lang {
    keywords: &[
        "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
        "in", "function", "return", "local", "export", "exit", "echo", "cd", "set", "unset",
        "source", "param", "foreach", "elseif", "switch", "try", "catch", "finally", "throw",
    ],
    types: &[],
    line_comments: &["#"],
    block_comment: Some(("<#", "#>")),
    quotes: &["\"", "'"],
    capitalized_types: false,
    case_insensitive: true,
};

const SQL: Lang = Lang {
    keywords: &[
        "select",
        "from",
        "where",
        "insert",
        "into",
        "values",
        "update",
        "set",
        "delete",
        "create",
        "table",
        "drop",
        "alter",
        "add",
        "join",
        "left",
        "right",
        "inner",
        "outer",
        "full",
        "on",
        "group",
        "by",
        "order",
        "having",
        "limit",
        "offset",
        "and",
        "or",
        "not",
        "null",
        "is",
        "as",
        "distinct",
        "union",
        "all",
        "case",
        "when",
        "then",
        "else",
        "end",
        "primary",
        "key",
        "foreign",
        "references",
        "index",
        "view",
        "with",
        "in",
        "exists",
        "between",
        "like",
        "asc",
        "desc",
        "true",
        "false",
        "default",
        "unique",
    ],
    types: &[
        "int",
        "integer",
        "bigint",
        "smallint",
        "text",
        "varchar",
        "char",
        "boolean",
        "date",
        "timestamp",
        "real",
        "float",
        "numeric",
        "decimal",
        "blob",
    ],
    line_comments: &["--"],
    block_comment: Some(("/*", "*/")),
    quotes: &["'", "\""],
    capitalized_types: false,
    case_insensitive: true,
};

/// TOML, YAML, INI.
const CONFIG: Lang = Lang {
    keywords: &["true", "false", "null", "yes", "no", "on", "off"],
    types: &[],
    line_comments: &["#", ";"],
    block_comment: None,
    quotes: &["\"\"\"", "'''", "\"", "'"],
    capitalized_types: false,
    case_insensitive: false,
};

const CSS: Lang = Lang {
    keywords: &[
        "important",
        "media",
        "import",
        "from",
        "to",
        "and",
        "not",
        "only",
    ],
    types: &[],
    line_comments: &[],
    block_comment: Some(("/*", "*/")),
    quotes: &["\"", "'"],
    capitalized_types: false,
    case_insensitive: false,
};

/// The language of a fenced block's info string (`rust`, `py`,
/// `ts,ignore`…), `None` when the highlighter does not know it.
fn language(info: &str) -> Option<&'static Lang> {
    let name = info
        .split(|c: char| c == ',' || c == '{' || c.is_whitespace())
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    Some(match name.as_str() {
        "rust" | "rs" => &RUST,
        "python" | "py" | "python3" => &PYTHON,
        "javascript" | "js" | "jsx" | "mjs" | "cjs" | "typescript" | "ts" | "tsx" => &JAVASCRIPT,
        "json" | "jsonc" | "json5" => &JSON,
        "c" | "h" | "cpp" | "c++" | "cc" | "cxx" | "hpp" | "cs" | "csharp" | "c#" | "java"
        | "go" | "golang" | "kotlin" | "kt" | "swift" | "dart" => &C_LIKE,
        "sh" | "bash" | "shell" | "zsh" | "console" | "powershell" | "ps1" | "pwsh" | "ps"
        | "bat" | "cmd" => &SHELL,
        "sql" => &SQL,
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" => &CONFIG,
        "css" | "scss" | "less" => &CSS,
        _ => return None,
    })
}

/// Byte length of the longest prefix of `text` whose characters all pass
/// `keep`.
fn prefix_len(text: &str, keep: impl Fn(char) -> bool) -> usize {
    text.char_indices()
        .find(|&(_, c)| !keep(c))
        .map_or(text.len(), |(at, _)| at)
}

/// Byte length of the string literal at the start of `text`, which starts
/// with `quote`: up to the closing `quote` (a backslash escapes the next
/// character), or the end of the line for a one-character quote other
/// than a backtick, or the end of `text`.
fn string_len(text: &str, quote: &str) -> usize {
    let multiline = quote.len() > 1 || quote == "`";
    let body = &text[quote.len()..];
    let mut escaped = false;
    for (at, c) in body.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '\n' && !multiline {
            return quote.len() + at;
        } else if body[at..].starts_with(quote) {
            return quote.len() + at + quote.len();
        }
    }
    text.len()
}

/// `code` split into coloured byte ranges (adjacent ranges of one colour
/// merged).
fn color_ranges(code: &str, lang: &Lang, colors: CodeColors) -> Vec<(usize, usize, Color32)> {
    let mut ranges: Vec<(usize, usize, Color32)> = Vec::new();
    let mut push = |start: usize, end: usize, color: Color32| match ranges.last_mut() {
        Some(last) if last.2 == color && last.1 == start => last.1 = end,
        _ => ranges.push((start, end, color)),
    };
    let mut at = 0;
    while at < code.len() {
        let rest = &code[at..];
        let Some(first) = rest.chars().next() else {
            break;
        };
        let (len, color) = if lang.line_comments.iter().any(|m| rest.starts_with(m)) {
            (rest.find('\n').unwrap_or(rest.len()), colors.comment)
        } else if let Some((open, close)) = lang.block_comment
            && rest.starts_with(open)
        {
            let len = rest[open.len()..]
                .find(close)
                .map_or(rest.len(), |end| open.len() + end + close.len());
            (len, colors.comment)
        } else if let Some(quote) = lang.quotes.iter().find(|q| rest.starts_with(**q)) {
            (string_len(rest, quote), colors.string)
        } else if first.is_ascii_digit() {
            let len = prefix_len(rest, |c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
            (len, colors.number)
        } else if first.is_alphabetic()
            || first == '_'
            || (first == '#' && lang.keywords.iter().any(|k| k.starts_with('#')))
        {
            let len = first.len_utf8()
                + prefix_len(&rest[first.len_utf8()..], |c| {
                    c.is_alphanumeric() || c == '_'
                });
            let word = &rest[..len];
            let after = rest[len..].trim_start_matches([' ', '\t']);
            let is = |list: &[&str]| {
                if lang.case_insensitive {
                    list.iter().any(|k| k.eq_ignore_ascii_case(word))
                } else {
                    list.contains(&word)
                }
            };
            let color = if is(lang.keywords) {
                colors.keyword
            } else if is(lang.types) || (lang.capitalized_types && first.is_uppercase()) {
                colors.kind
            } else if after.starts_with('(') || after.starts_with('!') {
                // A call, or a Rust macro.
                colors.function
            } else {
                colors.plain
            };
            (len, color)
        } else {
            (first.len_utf8(), colors.plain)
        };
        // Always advance (a zero length would loop forever).
        let len = len.max(first.len_utf8());
        push(at, at + len, color);
        at += len;
    }
    ranges
}

/// The colour-coded layout of `code` in the language of the info string
/// `info`, with `plain` for everything that is not a token.
fn layout_job(
    code: &str,
    info: &str,
    font_size: f32,
    dark: bool,
    plain: Color32,
) -> Option<LayoutJob> {
    let lang = language(info)?;
    let colors = CodeColors::new(dark, plain);
    let font_id = FontId::monospace(font_size);
    let mut job = LayoutJob {
        text: code.to_owned(),
        ..LayoutJob::default()
    };
    for (start, end, color) in color_ranges(code, lang, colors) {
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: start..end,
            format: TextFormat::simple(font_id.clone(), color),
        });
    }
    Some(job)
}

#[derive(Default)]
struct Highlighter;

/// Key: code, info string, font size bits, dark theme, plain colour.
type Key<'a> = (&'a str, &'a str, u32, bool, Color32);

impl ComputerMut<Key<'_>, Option<LayoutJob>> for Highlighter {
    fn compute(&mut self, (code, info, size_bits, dark, plain): Key<'_>) -> Option<LayoutJob> {
        layout_job(code, info, f32::from_bits(size_bits), dark, plain)
    }
}

type HighlightCache = FrameCache<Option<LayoutJob>, Highlighter>;

/// The colour-coded layout of the code block `code` with the info string
/// `info` (its language), at `font_size` in the monospace font, or `None`
/// when the language is unknown (then it is shown in one colour).
pub(crate) fn highlighted(
    ctx: &egui::Context,
    code: &str,
    info: &str,
    font_size: f32,
    plain: Color32,
) -> Option<LayoutJob> {
    let dark = ctx.style().visuals.dark_mode;
    let key: Key<'_> = (code, info, font_size.to_bits(), dark, plain);
    ctx.memory_mut(|memory| memory.caches.cache::<HighlightCache>().get(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text of each range with its colour.
    fn tokens<'a>(code: &'a str, info: &str, dark: bool) -> Vec<(&'a str, Color32)> {
        let lang = language(info).unwrap_or(&RUST);
        color_ranges(code, lang, CodeColors::new(dark, Color32::GRAY))
            .into_iter()
            .map(|(start, end, color)| (&code[start..end], color))
            .collect()
    }

    fn color_of(code: &str, info: &str, needle: &str) -> Option<Color32> {
        tokens(code, info, false)
            .into_iter()
            .find(|(text, _)| text.contains(needle))
            .map(|(_, color)| color)
    }

    #[test]
    fn rust_tokens_get_their_colours() {
        let code =
            "/// Add.\nfn add(a: i32) -> Option<i32> { let s = \"x\\\"y\"; println!(\"{s}\"); 42 }";
        let light = CodeColors::new(false, Color32::GRAY);
        assert_eq!(color_of(code, "rust", "/// Add."), Some(light.comment));
        assert_eq!(color_of(code, "rust", "fn"), Some(light.keyword));
        assert_eq!(color_of(code, "rust", "add"), Some(light.function));
        assert_eq!(color_of(code, "rust", "i32"), Some(light.kind));
        assert_eq!(color_of(code, "rust", "Option"), Some(light.kind));
        assert_eq!(
            color_of(code, "rust", "x\\\"y"),
            Some(light.string),
            "escaped quote"
        );
        assert_eq!(
            color_of(code, "rust", "println"),
            Some(light.function),
            "macro"
        );
        assert_eq!(color_of(code, "rust", "42"), Some(light.number));
    }

    #[test]
    fn ranges_cover_the_code_exactly_once() {
        for (code, info) in [
            (
                "def f(x):\n    return f\"{x}\"  # done\n'''doc\nmore'''",
                "python",
            ),
            ("SELECT name FROM users WHERE id = 'a''b'; -- note", "sql"),
            ("const s = `multi\nline`; /* open", "ts"),
            ("héllo wörld \"ünïcode\" 3.14e5", "rust"),
            ("", "rust"),
        ] {
            let lang = language(info).unwrap_or(&RUST);
            let ranges = color_ranges(code, lang, CodeColors::new(true, Color32::GRAY));
            let mut at = 0;
            for (start, end, _) in &ranges {
                assert_eq!(*start, at, "{code:?}");
                assert!(end > start && code.is_char_boundary(*end), "{code:?}");
                at = *end;
            }
            assert_eq!(at, code.len(), "{code:?}");
        }
    }

    #[test]
    fn sql_keywords_match_any_case_and_unknown_languages_are_plain() {
        let light = CodeColors::new(false, Color32::GRAY);
        assert_eq!(
            color_of("select x from t", "SQL", "select"),
            Some(light.keyword)
        );
        assert_eq!(color_of("SELECT x", "sql", "SELECT"), Some(light.keyword));
        assert!(language("brainfuck").is_none());
        assert!(language("").is_none());
        assert!(language("rust,ignore").is_some());
        assert!(layout_job("x", "nope", 14.0, false, Color32::GRAY).is_none());
        let job = layout_job("let x = 1;", "rs", 14.0, true, Color32::GRAY);
        assert!(job.is_some_and(|job| job.sections.len() > 1 && job.text == "let x = 1;"));
    }
}
