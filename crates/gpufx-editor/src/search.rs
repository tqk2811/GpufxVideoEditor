//! The one text search every list uses: the editor's search boxes and the agent's list tools.
//!
//! A filter is split into words. An item matches when every word is, ignoring case and accents
//! (`đ` reads as `d`), a part of one of its fields. Matching items come best first: the name equal
//! to the filter, then a name starting with it, then words found at the start of a word of the name,
//! inside the name, in a tag or category, and last in the other fields. Items that score the same
//! keep their order, and an empty filter keeps every item in its order.
//!
//! An item may have more names than one: the translations its pack brings (see
//! [`crate::i18n::pack`]). Every name, English or translated, of every language the pack brings, is
//! matched as a name, the best one counting, so a word typed in any of them ranks alike whatever the
//! language of the editor. Translated tags and categories are tags, translated descriptions other
//! fields.

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// `text` lower-cased, without accents, with `đ` as `d`, for comparing.
pub fn fold(text: &str) -> String {
    text.nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .map(|c| if c == 'đ' { 'd' } else { c })
        .collect()
}

/// The fields of one item a filter is matched against.
#[derive(Debug, Clone, Default)]
pub struct SearchFields<'a> {
    pub name: &'a str,
    /// More names: the translations of the name.
    pub names: Vec<&'a str>,
    /// Tags and categories.
    pub tags: Vec<&'a str>,
    /// Ids, keys, pack names and descriptions.
    pub other: Vec<&'a str>,
}

impl<'a> SearchFields<'a> {
    pub fn new(name: &'a str) -> Self {
        Self { name, ..Self::default() }
    }

    pub fn tags(mut self, tags: impl IntoIterator<Item = &'a str>) -> Self {
        self.tags.extend(tags);
        self
    }

    pub fn other(mut self, other: impl IntoIterator<Item = &'a str>) -> Self {
        self.other.extend(other);
        self
    }
}

/// A filter, folded and split into words once.
#[derive(Debug, Clone)]
pub struct Query {
    whole: String,
    words: Vec<String>,
}

impl Query {
    pub fn new(filter: &str) -> Self {
        let words: Vec<String> = fold(filter).split_whitespace().map(str::to_owned).collect();
        Self { whole: words.join(" "), words }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// How well `fields` match, higher being better, or `None` when a word is in no field.
    pub fn score(&self, fields: &SearchFields) -> Option<u32> {
        if self.is_empty() {
            return Some(0);
        }
        let names: Vec<String> = std::iter::once(fields.name).chain(fields.names.iter().copied()).map(fold).collect();
        let tags: Vec<String> = fields.tags.iter().map(|tag| fold(tag)).collect();
        let other: Vec<String> = fields.other.iter().map(|text| fold(text)).collect();
        let mut score = 0;
        for word in &self.words {
            let in_name = names.iter().flat_map(|name| name.match_indices(word.as_str()).map(|(at, _)| if starts_word(name, at) { 30 } else { 20 })).max();
            let best = in_name
                .or_else(|| tags.iter().any(|tag| tag.contains(word.as_str())).then_some(10))
                .or_else(|| other.iter().any(|text| text.contains(word.as_str())).then_some(5))?;
            score += best;
        }
        let whole = |name: &String| name.split_whitespace().collect::<Vec<_>>().join(" ");
        if names.iter().map(whole).any(|name| name == self.whole) {
            score += 2000;
        } else if names.iter().map(whole).any(|name| name.starts_with(&self.whole)) {
            score += 1000;
        }
        Some(score)
    }
}

/// `at` is the start of `text` or follows a character that is no letter or digit.
fn starts_word(text: &str, at: usize) -> bool {
    text[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric())
}

/// The items of `items` matching `filter`, best first, ties in their order.
pub fn rank<T>(items: impl IntoIterator<Item = T>, filter: &str, fields: impl Fn(&T) -> SearchFields<'_>) -> Vec<T> {
    let query = Query::new(filter);
    if query.is_empty() {
        return items.into_iter().collect();
    }
    let mut scored: Vec<(u32, T)> = items.into_iter().filter_map(|item| query.score(&fields(&item)).map(|score| (score, item))).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, item)| item).collect()
}

/// At most `limit` of `items`; all of them when `None`.
pub fn take_limit<T>(mut items: Vec<T>, limit: Option<usize>) -> Vec<T> {
    if let Some(limit) = limit {
        items.truncate(limit);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names<'a>(items: &[&'a str], filter: &str) -> Vec<&'a str> {
        rank(items.iter().copied(), filter, |name| SearchFields::new(name))
    }

    #[test]
    fn folding_drops_case_and_vietnamese_accents() {
        assert_eq!(fold("Đường Phố Hà Nội"), "duong pho ha noi");
        assert_eq!(fold("ĐĂNG KÝ"), "dang ky");
        assert_eq!(fold("İstanbul"), "istanbul");
        assert_eq!(names(&["Mưa rơi", "Nắng"], "mua"), ["Mưa rơi"]);
        assert_eq!(names(&["Mua rơi"], "MƯA"), ["Mua rơi"]);
        assert_eq!(names(&["Đêm trăng"], "dem"), ["Đêm trăng"]);
        assert_eq!(names(&["Dem trang"], "đêm"), ["Dem trang"]);
    }

    #[test]
    fn every_word_must_match_some_field() {
        fn fields<'r>(item: &'r (&str, &str, &str)) -> SearchFields<'r> {
            SearchFields::new(item.0).tags([item.1]).other([item.2])
        }
        let items = [("Snow Fall", "capcut", "packages/gpufx/capcut/snow.zip"), ("Box Blur", "blur", "packages/gpufx/blur/box.zip")];
        let names = |filter| rank(items, filter, fields).into_iter().map(|item| item.0).collect::<Vec<_>>();
        assert_eq!(names("fall capcut"), ["Snow Fall"]);
        assert_eq!(names("box.zip blur"), ["Box Blur"]);
        assert!(names("snow blur").is_empty());
        assert!(names("rain").is_empty());
    }

    #[test]
    fn an_empty_filter_keeps_everything_in_order() {
        assert_eq!(names(&["b", "a", "c"], ""), ["b", "a", "c"]);
        assert_eq!(names(&["b", "a", "c"], "   "), ["b", "a", "c"]);
    }

    #[test]
    fn better_matches_come_first_and_ties_keep_their_order() {
        fn fields<'r>(item: &'r (&str, &str, &str)) -> SearchFields<'r> {
            SearchFields::new(item.0).tags([item.1]).other([item.2])
        }
        let items = [("Other", "", "blur id"), ("Tagged", "blur", ""), ("Motionblur", "", ""), ("Radial Blur", "", ""), ("Blur Strong", "", ""), ("Blur", "", ""), ("Zoom Blur", "", "")];
        let ranked: Vec<&str> = rank(items, "blur", fields).into_iter().map(|item| item.0).collect();
        assert_eq!(ranked, ["Blur", "Blur Strong", "Radial Blur", "Zoom Blur", "Motionblur", "Tagged", "Other"]);
    }

    #[test]
    fn a_multi_word_name_equal_to_or_starting_with_the_filter_comes_first() {
        let items = ["Blur Zoom Strong", "Zoom Blur Strong", "Zoom  Blur", "Strong Zoom Blur"];
        assert_eq!(names(&items, "zoom blur"), ["Zoom  Blur", "Zoom Blur Strong", "Blur Zoom Strong", "Strong Zoom Blur"]);
        assert_eq!(names(&items, "ZOOM BLUR STRONG"), ["Zoom Blur Strong", "Blur Zoom Strong", "Strong Zoom Blur"]);
    }

    #[test]
    fn a_translated_name_ranks_as_a_name() {
        let items = [("Pop", "Bật ra"), ("Bounce", ""), ("Other", "")];
        fn fields<'r>(item: &'r (&str, &str)) -> SearchFields<'r> {
            SearchFields { name: item.0, names: vec![item.1], tags: vec![], other: vec![] }
        }
        let ranked: Vec<&str> = rank(items, "bat ra", fields).into_iter().map(|item| item.0).collect();
        assert_eq!(ranked, ["Pop"]);
        let score = |item: (&'static str, &'static str), filter| Query::new(filter).score(&fields(&item)).unwrap();
        assert_eq!(score(("Pop", "Bật ra"), "bật ra"), score(("Bật ra", ""), "bat ra"), "a translated name scores as the name does");
    }

    #[test]
    fn a_limit_keeps_the_first_items() {
        assert_eq!(take_limit(vec![1, 2, 3], Some(2)), [1, 2]);
        assert_eq!(take_limit(vec![1, 2, 3], None), [1, 2, 3]);
    }
}
