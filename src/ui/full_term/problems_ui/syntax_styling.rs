use ratatui::style::Color;
use rustc_ap_rustc_lexer::LiteralKind;
use rustc_ap_rustc_lexer::TokenKind;

pub(super) fn colour_for_token_kind(kind: TokenKind, token_text: &str) -> Option<Color> {
    match kind {
        TokenKind::LineComment { .. } | TokenKind::BlockComment { .. } => Some(Color::Green),
        TokenKind::Ident | TokenKind::RawIdent => {
            if is_keyword(token_text) {
                Some(Color::Blue)
            } else {
                Some(Color::LightGreen)
            }
        }
        TokenKind::Literal {
            kind:
                LiteralKind::Str { .. }
                | LiteralKind::ByteStr { .. }
                | LiteralKind::RawByteStr { .. }
                | LiteralKind::RawStr { .. },
            ..
        } => Some(Color::Yellow),
        TokenKind::Lifetime { .. } => Some(Color::Blue),
        TokenKind::OpenParen | TokenKind::CloseParen => Some(Color::Blue),
        TokenKind::OpenBrace | TokenKind::CloseBrace => Some(Color::Magenta),
        TokenKind::OpenBracket | TokenKind::CloseBracket => Some(Color::Magenta),
        TokenKind::Question => Some(Color::Yellow),
        _ => None,
    }
}

const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
];

fn is_keyword(token_text: &str) -> bool {
    KEYWORDS.contains(&token_text)
}
