//! A text to show before it is put into words: a key of a language file and its arguments, and the
//! small formatter that fills a template with them.
//!
//! Template syntax: `{name}` is replaced by the argument `name`; `{{` and `}}` give a literal brace.
//! A placeholder with no argument stays as it is written, so a mistake shows instead of vanishing.

use std::borrow::Cow;

/// The value of a placeholder.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    /// Shown as it is: a file name, a path, detail from FFmpeg.
    Plain(String),
    /// A number. It also chooses the plural form when it is the text's count.
    Count(i64),
    /// Another text, put into words in the same language first.
    Text(Text),
}

/// What to show, in no language yet.
#[derive(Debug, Clone, PartialEq)]
pub enum Text {
    /// Not translated.
    Raw(String),
    Key {
        key: Cow<'static, str>,
        args: Vec<(&'static str, Arg)>,
        /// The argument whose count picks `<key>.one` or `<key>.other`.
        plural: Option<&'static str>,
        /// Shown when no language has the key, instead of the key.
        fallback: Option<String>,
    },
}

impl Text {
    pub fn key(key: impl Into<Cow<'static, str>>) -> Self {
        Self::Key { key: key.into(), args: Vec::new(), plural: None, fallback: None }
    }

    pub fn raw(text: impl Into<String>) -> Self {
        Self::Raw(text.into())
    }

    /// Adds the argument `name`.
    pub fn arg(mut self, name: &'static str, value: impl Into<Arg>) -> Self {
        if let Self::Key { args, .. } = &mut self {
            args.push((name, value.into()));
        }
        self
    }

    /// Adds the number `name`, which also picks the plural form.
    pub fn count(mut self, name: &'static str, count: i64) -> Self {
        if let Self::Key { args, plural, .. } = &mut self {
            args.push((name, Arg::Count(count)));
            *plural = Some(name);
        }
        self
    }

    /// What to show when no language has the key.
    pub fn or_raw(mut self, text: impl Into<String>) -> Self {
        if let Self::Key { fallback, .. } = &mut self {
            *fallback = Some(text.into());
        }
        self
    }

    /// The count that picks the plural form, if the text has one.
    pub fn plural_count(&self) -> Option<i64> {
        let Self::Key { args, plural: Some(name), .. } = self else { return None };
        args.iter().find(|(arg, _)| arg == name).and_then(|(_, value)| match value {
            Arg::Count(count) => Some(*count),
            _ => None,
        })
    }
}

impl From<Text> for Arg {
    fn from(text: Text) -> Self {
        Self::Text(text)
    }
}

impl From<String> for Arg {
    fn from(text: String) -> Self {
        Self::Plain(text)
    }
}

impl From<&String> for Arg {
    fn from(text: &String) -> Self {
        Self::Plain(text.clone())
    }
}

impl From<&str> for Arg {
    fn from(text: &str) -> Self {
        Self::Plain(text.to_owned())
    }
}

macro_rules! count_args {
    ($($number:ty),*) => {$(
        impl From<$number> for Arg {
            fn from(number: $number) -> Self {
                Self::Count(number as i64)
            }
        }
        impl From<&$number> for Arg {
            fn from(number: &$number) -> Self {
                Self::Count(*number as i64)
            }
        }
    )*};
}
count_args!(usize, u32, u64, i64);

/// The plural form a count takes. Every language goes by this rule; one with no plural, such as
/// Vietnamese, writes only the `other` form, and a missing form falls back to the other one.
pub fn plural_forms(count: i64) -> [&'static str; 2] {
    if count == 1 { ["one", "other"] } else { ["other", "one"] }
}

/// `template` with each `{name}` replaced by `value(name)`.
pub fn format(template: &str, value: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(at) = rest.find(['{', '}']) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
        } else if tail.starts_with('}') {
            out.push('}');
            rest = &tail[1..];
        } else {
            match tail[1..].find(['{', '}']).filter(|end| tail.as_bytes()[1 + end] == b'}') {
                Some(end) => {
                    let name = &tail[1..1 + end];
                    match value(name) {
                        Some(value) => out.push_str(&value),
                        None => out.push_str(&tail[..end + 2]),
                    }
                    rest = &tail[end + 2..];
                }
                None => {
                    out.push('{');
                    rest = &tail[1..];
                }
            }
        }
    }
    out.push_str(rest);
    out
}

/// The placeholder names of `template`, in order, without repeats.
pub fn placeholders(template: &str) -> Vec<String> {
    let names = std::cell::RefCell::new(Vec::<String>::new());
    format(template, |name| {
        let mut names = names.borrow_mut();
        if !names.iter().any(|known| known == name) {
            names.push(name.to_owned());
        }
        Some(String::new())
    });
    names.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(template: &str) -> String {
        format(template, |name| match name {
            "name" => Some("clip.mp4".to_owned()),
            "count" => Some("3".to_owned()),
            _ => None,
        })
    }

    #[test]
    fn placeholders_are_filled() {
        assert_eq!(fill("Added {name}"), "Added clip.mp4");
        assert_eq!(fill("{count} of {name}, {count}"), "3 of clip.mp4, 3");
        assert_eq!(fill("no placeholder"), "no placeholder");
        assert_eq!(fill(""), "");
        assert_eq!(fill("Đã thêm {name}…"), "Đã thêm clip.mp4…");
    }

    #[test]
    fn doubled_braces_are_literal() {
        assert_eq!(fill("{{name}} is {name}"), "{name} is clip.mp4");
        assert_eq!(fill("a }} b {{"), "a } b {");
    }

    #[test]
    fn a_missing_argument_stays_visible() {
        assert_eq!(fill("Hello {who}"), "Hello {who}");
    }

    #[test]
    fn stray_braces_are_kept() {
        assert_eq!(fill("a { b"), "a { b");
        assert_eq!(fill("a } b"), "a } b");
        assert_eq!(fill("{ {name}"), "{ clip.mp4");
        assert_eq!(fill("trailing {"), "trailing {");
    }

    #[test]
    fn placeholder_names_are_listed_once() {
        assert_eq!(placeholders("{a} {b} {a} {{c}}"), vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn plural_forms_prefer_one_only_for_one() {
        assert_eq!(plural_forms(1), ["one", "other"]);
        assert_eq!(plural_forms(0), ["other", "one"]);
        assert_eq!(plural_forms(2), ["other", "one"]);
    }

    #[test]
    fn the_count_argument_is_found() {
        assert_eq!(Text::key("k").arg("name", "x").count("count", 4).plural_count(), Some(4));
        assert_eq!(Text::key("k").arg("count", 4usize).plural_count(), None);
        assert_eq!(Text::raw("x").count("count", 4).plural_count(), None);
    }
}
