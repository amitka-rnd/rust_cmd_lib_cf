use crate::parser::{ParseArg, Parser};
use proc_macro2::{Delimiter, Ident, Literal, Span, TokenStream, TokenTree};
use proc_macro_error::abort;
use quote::quote;

enum SepToken {
    Space,
    SemiColon,
    Or,
    Pipe,
}

#[derive(PartialEq, Clone, Debug)]
enum MarkerToken {
    Pipe,
    DollarSign,
    Ampersand,
    RedirectFd(RedirectFd),
    None,
}

#[derive(PartialEq, Clone, Debug)]
enum RedirectFd {
    Stdin,
    Stdout,
    Stderr,
    StdoutErr,
}
impl RedirectFd {
    fn id(&self) -> i32 {
        match self {
            Self::Stdin => 0,
            Self::Stdout => 1,
            Self::Stderr | Self::StdoutErr => 2,
        }
    }
}

pub struct Lexer {
    input: TokenStream,
    args: Vec<ParseArg>,

    last_marker_token: MarkerToken,
    last_arg_str: TokenStream,
    last_redirect: Option<(RedirectFd, bool)>,
}

impl Lexer {
    pub fn from(input: TokenStream) -> Self {
        Self {
            input,
            args: vec![],
            last_marker_token: MarkerToken::None,
            last_arg_str: TokenStream::new(),
            last_redirect: None,
        }
    }

    fn last_is_pipe(&self) -> bool {
        self.last_marker_token == MarkerToken::Pipe
    }

    fn last_is_dollar_sign(&self) -> bool {
        self.last_marker_token == MarkerToken::DollarSign
    }

    fn set_last_marker_token(&mut self, value: MarkerToken) {
        self.last_marker_token = value;
    }

    fn reset_last_marker_token(&mut self) {
        self.last_arg_str = TokenStream::new();
        self.last_marker_token = MarkerToken::None;
    }

    fn set_redirect(&mut self, t: TokenTree, fd: RedirectFd) {
        if let Some((last_fd, append)) = self.last_redirect.clone() {
            if append {
                abort!(t, "wrong redirect format: more than append");
            }
            if fd == RedirectFd::Stdin {
                abort!(t, "wrong input redirect format");
            }
            if self.last_marker_token == MarkerToken::None {
                abort!(t, "redirection parse error");
            }
            self.last_redirect = Some((last_fd.clone(), true));
            self.last_marker_token = MarkerToken::RedirectFd(last_fd);
        } else if self.last_marker_token == MarkerToken::Ampersand {
            if fd == RedirectFd::Stdin {
                abort!(t, "wrong input redirect format");
            }
            self.last_redirect = Some((RedirectFd::StdoutErr, false));
            self.last_marker_token = MarkerToken::RedirectFd(RedirectFd::StdoutErr);
        } else {
            self.last_redirect = Some((fd.clone(), false));
            self.last_marker_token = MarkerToken::RedirectFd(fd);
        }
    }

    fn last_arg_str_empty(&self) -> bool {
        self.last_arg_str.is_empty()
    }

    fn add_arg_with_token(&mut self, token: SepToken) {
        if let Some((fd, append)) = self.last_redirect.clone() {
            let last_arg_str = self.last_arg_str.clone();
            let fd_id = fd.id();
            self.args.push(ParseArg::ParseRedirectFile(
                fd_id,
                quote!(#last_arg_str),
                append,
            ));
            if fd == RedirectFd::StdoutErr {
                self.args
                    .push(ParseArg::ParseRedirectFile(1, quote!(#last_arg_str), true));
            }
            self.last_redirect = None;
        } else if !self.last_arg_str_empty() {
            let last_arg_str = self.last_arg_str.clone();
            let last_arg = ParseArg::ParseArgStr(quote!(#last_arg_str));
            self.args.push(last_arg);
        }
        match token {
            SepToken::Space => {}
            SepToken::SemiColon => self.args.push(ParseArg::ParseSemicolon),
            SepToken::Or => {
                self.args.pop();
                self.args.push(ParseArg::ParseOr);
            }
            SepToken::Pipe => self.args.push(ParseArg::ParsePipe),
        }
        self.reset_last_marker_token();
    }

    fn add_fd_redirect_arg(&mut self, old_fd: i32, new_fd: i32) {
        self.args.push(ParseArg::ParseRedirectFd(old_fd, new_fd));
        self.last_redirect = None;
        self.reset_last_marker_token();
    }

    fn extend_last_arg(&mut self, stream: TokenStream) {
        if self.last_arg_str_empty() {
            self.last_arg_str = quote!(String::new());
        }
        self.last_arg_str.extend(quote!(+ #stream));
        self.last_marker_token = MarkerToken::None;
    }

    pub fn scan(mut self) -> Parser {
        let mut end = 0;
        for t in self.input.clone() {
            let (_start, _end) = Self::span_location(&t.span());
            if end != 0 && end < _start {
                // new argument with spacing
                if self.last_marker_token == MarkerToken::Ampersand {
                    abort!(t, "invalid token after &, only &1, &2 or &> is supported");
                } else if !self.last_arg_str_empty() {
                    self.add_arg_with_token(SepToken::Space);
                } else if let MarkerToken::RedirectFd(ref _fd) = self.last_marker_token {
                    self.last_marker_token = MarkerToken::None;
                }
            }
            end = _end;

            let src = t.to_string();
            if self.last_is_dollar_sign() {
                if let TokenTree::Group(g) = t.clone() {
                    if g.delimiter() != Delimiter::Brace && g.delimiter() != Delimiter::Bracket {
                        abort!(
                            g,
                            "invalid grouping: found {:?}, only \"brace/bracket\" is allowed",
                            format!("{:?}", g.delimiter()).to_lowercase()
                        );
                    }
                    let mut found_var = false;
                    for tt in g.stream() {
                        if let TokenTree::Ident(ref var) = tt {
                            if found_var {
                                abort!(tt, "more than one variable in grouping");
                            }
                            if g.delimiter() == Delimiter::Brace {
                                self.extend_last_arg(quote!(&#var.to_string()));
                            } else {
                                if !self.last_arg_str_empty() {
                                    abort!(tt, "vector variable can only be used alone");
                                }
                                self.args.push(ParseArg::ParseArgVec(quote!(#var)));
                                self.reset_last_marker_token();
                            }
                            found_var = true;
                        } else {
                            abort!(tt, "invalid grouping: extra tokens");
                        }
                    }
                    continue;
                } else if let TokenTree::Ident(var) = t {
                    self.extend_last_arg(quote!(&#var.to_string()));
                    continue;
                }
            }

            if let TokenTree::Group(_) = t {
                abort!(t, "grouping is only allowed for variable");
            } else if let TokenTree::Literal(ref lit) = t {
                let s = lit.to_string();
                if s.starts_with('\"') || s.starts_with('r') {
                    self.extend_last_arg(Self::parse_str_lit(lit));
                } else {
                    if self.last_marker_token == MarkerToken::Ampersand {
                        if &s != "1" && &s != "2" {
                            abort!(t, "only &1 or &2 is allowed");
                        }
                        if let Some((fd, _)) = self.last_redirect.clone() {
                            if &s == "1" {
                                self.add_fd_redirect_arg(fd.id(), 1);
                            } else if &s == "2" {
                                self.add_fd_redirect_arg(fd.id(), 2);
                            }
                        } else {
                            abort!(t, "& is only allowed for redirect");
                        }
                        continue;
                    }
                    self.extend_last_arg(quote!(&#lit.to_string()));
                    if &s == "1" {
                        self.last_marker_token = MarkerToken::RedirectFd(RedirectFd::Stdout);
                    } else if &s == "2" {
                        self.last_marker_token = MarkerToken::RedirectFd(RedirectFd::Stderr);
                    }
                }
            } else {
                if let TokenTree::Punct(ref p) = t {
                    let ch = p.as_char();
                    if ch == '$' {
                        self.set_last_marker_token(MarkerToken::DollarSign);
                        continue;
                    } else if ch == ';' {
                        self.add_arg_with_token(SepToken::SemiColon);
                        continue;
                    } else if ch == '|' {
                        if self.last_is_pipe() {
                            self.add_arg_with_token(SepToken::Or);
                            self.set_last_marker_token(MarkerToken::None);
                        } else {
                            self.add_arg_with_token(SepToken::Pipe);
                            self.set_last_marker_token(MarkerToken::Pipe);
                        }
                        continue;
                    } else if ch == '>' {
                        let last_marker_token = self.last_marker_token.clone();
                        if last_marker_token == MarkerToken::Ampersand {
                            self.set_redirect(t, RedirectFd::StdoutErr);
                        } else if let MarkerToken::RedirectFd(fd) = last_marker_token {
                            self.set_redirect(t, fd);
                            self.reset_last_marker_token();
                        } else {
                            self.set_redirect(t, RedirectFd::Stdout);
                        }
                        continue;
                    } else if ch == '<' {
                        self.set_redirect(t, RedirectFd::Stdin);
                        continue;
                    } else if ch == '&' {
                        self.set_last_marker_token(MarkerToken::Ampersand);
                        continue;
                    }
                }

                self.extend_last_arg(quote!(&#src.to_string()));
            }
        }
        self.add_arg_with_token(SepToken::Space);
        Parser::from_args(self.args)
    }

    // helper function to get (start, end) of Span
    fn span_location(span: &Span) -> (usize, usize) {
        let s = format!("{:?}", span);
        let mut start = 0;
        let mut end = 0;
        let mut parse_second = false;
        for c in s.chars().skip(6) {
            if c == '.' {
                parse_second = true;
            } else if c.is_ascii_digit() {
                let digit = c.to_digit(10).unwrap() as usize;
                if !parse_second {
                    start = start * 10 + digit;
                } else {
                    end = end * 10 + digit;
                }
            }
        }
        (start, end)
    }

    pub fn parse_str_lit(lit: &Literal) -> TokenStream {
        let s = lit.to_string();
        if !s.starts_with('\"') {
            return quote!(#lit);
        }
        let mut iter = s[1..s.len() - 1] // To trim outside ""
            .chars()
            .peekable();
        let mut output = quote!("");
        while let Some(ch) = iter.next() {
            if ch == '$' {
                let mut with_brace = false;
                if iter.peek() == Some(&'{') {
                    with_brace = true;
                    iter.next();
                }
                let mut var = String::new();
                while let Some(&c) = iter.peek() {
                    if !c.is_ascii_alphanumeric() && c != '_' {
                        break;
                    }
                    if var.is_empty() && c.is_ascii_digit() {
                        break;
                    }
                    var.push(c);
                    iter.next();
                }
                if with_brace {
                    if iter.peek() != Some(&'}') {
                        abort!(lit.span(), "bad substitution");
                    } else {
                        iter.next();
                    }
                }
                if !var.is_empty() {
                    let var = syn::parse_str::<Ident>(&var).unwrap();
                    output.extend(quote!(+ &#var.to_string()));
                } else {
                    output.extend(quote!(+ &'$'.to_string()));
                }
            } else if ch == '\\' {
                if let Some(&ch) = iter.peek() {
                    let ec = match ch {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '0' => '\0',
                        _ => ch,
                    };
                    output.extend(quote!(+ &#ec.to_string()));
                    iter.next();
                }
            } else {
                output.extend(quote!(+ &#ch.to_string()));
            }
        }
        output
    }
}
