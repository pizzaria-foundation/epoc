//! The one filter-as-you-type matcher, with nothing around it.
//!
//! # Why this is a module and not a method
//!
//! It was a method: [`AppPicker::matches`](crate::app_picker::AppPicker::matches) held the rule, and
//! held it behind a picker that also owns a filter string, a [`ListState`](crate::list::ListState)
//! and the geometry of its last draw. Anything else that wanted to narrow a list by what the user
//! typed — the declarative `SearchField`, a settings screen with thirty rows — could not reach the
//! rule without constructing a drawer it was never going to show, so it would have written the rule
//! again.
//!
//! That second copy is the defect this file exists to prevent, and it is not hypothetical: two
//! matchers agree on the day the second one is written and then one of them learns about accents,
//! or stops lower-casing the needle, and the same three letters find different apps in the launcher
//! than in the picker. A user reads that as the phone being wrong, and there is nothing on screen
//! that says which of the two answered.
//!
//! # What the rule is
//!
//! Case-insensitive **substring**, against the whole label, in the caller's order. Not a prefix —
//! typing `ame` must find `Camera`, because on a keypad-era list the user is recalling a name, not
//! spelling one from the start. Not a fuzzy score, because a score needs a threshold and a
//! threshold is a number nobody can defend on a list of thirty apps.
//!
//! An **empty query matches everything**, which is the behaviour a search box needs on the frame it
//! appears: the alternative — an empty query matching nothing — shows the user an empty list before
//! they have typed anything and reads as "there are no apps".
//!
//! # Indices, not items
//!
//! The caller keeps its own collection and gets back positions into it. Returning cloned items
//! would allocate a second list on every keystroke, and returning borrows would tie the result's
//! lifetime to the labels, which is exactly what a caller that wants to *sort* or *renumber* the
//! result cannot have. Indices also make the empty-query case free of any per-item work.

use alloc::string::String;
use alloc::vec::Vec;

/// Whether `label` passes `query`.
///
/// Lower-cases both sides on every call, which is a deliberate non-optimisation: the lists this runs
/// against are dozens of rows, and the version that cached the folded needle was the version where a
/// caller passed a needle it had already lower-cased and got a silent double fold. Use
/// [`matching_indices`] for a whole list — it folds the needle once.
pub fn is_match(query: &str, label: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    label.to_lowercase().contains(&query.to_lowercase())
}

/// The positions in `labels` that pass `query`, in the order they arrived.
///
/// The order is the caller's and is never touched. A picker has already sorted its apps the way the
/// user expects to see them, and a matcher that re-ordered by "relevance" would move a row out from
/// under a thumb that was already on its way down — on a device where the only way to pick is to
/// count presses, that is worse than a slightly worse ranking.
pub fn matching_indices<'a, I>(query: &str, labels: I) -> Vec<usize>
where
    I: IntoIterator<Item = &'a str>,
{
    if query.is_empty() {
        // No per-item work at all on the frame a search box opens, which is also the frame with the
        // most rows to consider.
        return labels.into_iter().enumerate().map(|(i, _)| i).collect();
    }
    let needle: String = query.to_lowercase();
    labels
        .into_iter()
        .enumerate()
        .filter(|(_, label)| label.to_lowercase().contains(needle.as_str()))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const APPS: [&str; 6] =
        ["Calculator", "Calendar", "Camera", "Maps", "Messaging", "Web"];

    fn found(query: &str) -> Vec<&'static str> {
        matching_indices(query, APPS.iter().copied()).into_iter().map(|i| APPS[i]).collect()
    }

    #[test]
    fn an_empty_query_keeps_every_row_in_its_place() {
        // The frame a search box appears on. Matching nothing here would tell the user the phone has
        // no applications installed.
        assert_eq!(matching_indices("", APPS.iter().copied()), alloc::vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_query_narrows_to_the_rows_that_contain_it() {
        assert_eq!(found("cal"), alloc::vec!["Calculator", "Calendar"]);
    }

    #[test]
    fn the_match_is_a_substring_and_not_a_prefix() {
        // The rule that makes a keypad list usable: the user is recalling a name, not spelling one
        // from its first letter. `ame` is in the middle of `Camera` and nowhere else.
        assert_eq!(found("ame"), alloc::vec!["Camera"]);
    }

    #[test]
    fn case_never_decides_a_match() {
        // Shift on a hardware keyboard is a slip, not an instruction.
        assert_eq!(found("AME"), found("ame"));
        assert_eq!(found("MaPs"), alloc::vec!["Maps"]);
    }

    #[test]
    fn a_query_nothing_contains_returns_no_rows_rather_than_all_of_them() {
        // The failure that would be invisible: an empty result and a full result are both plausible
        // screens, and only one of them is right for "zzz".
        assert!(found("zzz").is_empty());
    }

    #[test]
    fn indices_point_into_the_callers_own_list() {
        // The contract the caller depends on to draw the row: position 2 of `APPS`, not position 0
        // of some filtered copy it does not hold.
        assert_eq!(matching_indices("came", APPS.iter().copied()), alloc::vec![2]);
    }

    #[test]
    fn multibyte_labels_and_queries_match_without_slicing_anything() {
        // `to_lowercase` and `contains` are byte-safe, but the reason this test is here is the next
        // person's optimisation: a matcher that indexed by byte offset to fold or compare would
        // panic on the first accented app name, and every launcher on a Brazilian phone has one.
        let labels = ["Ação", "Configurações", "Привет", "Web"];
        let idx = |q: &str| matching_indices(q, labels.iter().copied());
        assert_eq!(idx("ção"), alloc::vec![0], "`Configurações` has `ções`, not `ção`");
        assert_eq!(idx("AÇÃO"), alloc::vec![0], "an upper-case accented query folds to the same");
        assert_eq!(idx("рив"), alloc::vec![2], "Cyrillic folds too");
        assert_eq!(idx("ç"), alloc::vec![0, 1], "a single multi-byte char is a legitimate query");
    }

    #[test]
    fn is_match_and_matching_indices_never_disagree() {
        // Two entry points, one rule. If they drifted, a screen that checks one row (a row's own
        // highlight) and a screen that filters the list would show different things.
        for q in ["", "c", "AME", "zzz", "ç"] {
            let by_list = matching_indices(q, APPS.iter().copied());
            let by_row: Vec<usize> =
                (0..APPS.len()).filter(|&i| is_match(q, APPS[i])).collect();
            assert_eq!(by_list, by_row, "query {q:?}");
        }
    }

    #[test]
    fn an_empty_label_is_only_found_by_an_empty_query() {
        let labels = ["", "Web"];
        assert_eq!(matching_indices("", labels.iter().copied()), alloc::vec![0, 1]);
        assert_eq!(matching_indices("w", labels.iter().copied()), alloc::vec![1]);
    }
}
