//! What a date is allowed to be, and what happens when it stops being allowed.
//!
//! # Why this is not inside the picker
//!
//! [`crate::grid`] states the rule this file obeys: the arithmetic that is easy to get subtly wrong
//! lives here, pure and unit-tested, and the widget on top only decides where the state lives and
//! when the children are built. A date picker is the strongest case for that rule in the tree,
//! because it has the one branch nobody writes correctly the first time — **February has 28 days,
//! except when it has 29** — and because the interesting failure is not the leap rule itself but its
//! consequence:
//!
//! ```text
//!   the model holds 31 January
//!   the user steps the month field once
//!   the model holds 31 February
//! ```
//!
//! There is no 31 February. A widget that answered that with a branch in its key handler would own
//! the rule, and the next thing that edits a date — a sync that receives one, a text field that
//! parses one, an alarm that adds a week to one — would answer it again, differently. So the rule is
//! a *value*: [`Stamp`] cannot hold 31 February, because the only ways to change one go through
//! [`Stamp::with_part`], which re-clamps the day.
//!
//! # Clamp, do not refuse, and do not report
//!
//! Three answers were available for the case above and only one of them is usable with a D-pad.
//!
//! - **Refuse the month step.** The user presses Right on the month field and nothing happens. On
//!   this device a key that does nothing is indistinguishable from a key that is broken — there is
//!   no pointer, no tooltip and no undo — and the field it happens on is chosen by whether the *day*
//!   field, three spinners away, happens to hold 31. So the same press works on the 30th and dies on
//!   the 31st, which reads as the phone being faulty.
//! - **Report an invalid date.** Every caller then has to handle a state that only exists between
//!   two keypresses, and the screen has to show something in the day field meanwhile. That is a new
//!   state in every app's `update` to buy nothing the user asked for.
//! - **Clamp the day.** 31 January + one month = 28 February (29 in a leap year). One press, one
//!   message, one valid date, and it is what the handset's own date editor does — which is the
//!   argument that settles it, because the phone has trained its user for a decade.
//!
//! The clamp is not free: stepping January → February → March from the 31st arrives at 28 March,
//! having quietly lost three days. That is the known cost, it is what every mobile date editor does,
//! and the alternative — remembering a day the date does not have — is a second hidden field that
//! disagrees with the visible one.
//!
//! # The year range is not arbitrary
//!
//! [`YEAR_MIN`] and [`YEAR_MAX`] bound a spinner rather than a calendar: the arithmetic below is
//! correct for any year, but a field the user walks one press at a time needs ends. See their own
//! notes for why these two.

/// The first year a spinner will offer.
///
/// 1900 because it covers the birthday of anybody alive to type it, and because it is a year the
/// century rule gets *wrong* if the rule is written as `year % 4 == 0` — see
/// [`is_leap_year`]. Having it inside the offered range means the mistake is reachable with the
/// D-pad instead of being a hypothetical about the year 1600.
pub const YEAR_MIN: i32 = 1900;

/// The last year a spinner will offer.
///
/// 2100 because a phone's alarms, reminders and expiry dates do not go past it, and because — like
/// [`YEAR_MIN`] — it is a century year that is *not* a leap year, so both halves of the century rule
/// are inside the range a test can walk.
pub const YEAR_MAX: i32 = 2100;

/// Whether February has 29 days in `year`.
///
/// The full Gregorian rule, all three clauses, because the two-clause version is right for the next
/// seventy-four years and wrong for 1900 — which is inside [`YEAR_MIN`]`..=`[`YEAR_MAX`] and so
/// reachable by holding Left on a year field. The symptom of the short version is a 29 February 1900
/// that the phone accepts and every other system rejects.
pub const fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// How many days `month` has in `year`, counting `month` from 1.
///
/// A month outside 1..=12 is **clamped, not rejected and never a panic**. A panic here would arrive
/// on the device as a dialog with a number in it, from inside a key handler, with the screen already
/// half drawn — and the input that gets here is a number from a model, which means it can be
/// anything a bug upstream left behind. Both clamped ends answer 31, so a nonsense month cannot make
/// a day the user is already holding suddenly illegal.
pub const fn days_in_month(year: i32, month: i32) -> i32 {
    match if month < 1 {
        1
    } else if month > 12 {
        12
    } else {
        month
    } {
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// One field of a [`Stamp`] — which spinner a key is aimed at.
///
/// An enum rather than an index because the picker has to ask two questions about whichever field
/// has the cursor — what may it hold, and what does the whole date become if it changes — and both
/// answers live here. An index would put a `match` in the widget, which is precisely the branch this
/// module exists to keep out of it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Part {
    Year,
    Month,
    /// The one whose bounds depend on the other two. See [`Stamp::bounds`].
    Day,
    Hour,
    Minute,
}

/// A date and a time of day, which cannot be an impossible one.
///
/// The fields are private and every way in clamps, which is the entire point: a `struct` with public
/// numbers is a struct that holds 31 February the moment two of them are written in the wrong order.
/// Declared in field order year → month → day → hour → minute so the derived [`Ord`] is
/// chronological, which is what a list of reminders wants and what a hand-rolled comparison gets
/// wrong once.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Stamp {
    year: i32,
    month: i32,
    day: i32,
    hour: i32,
    minute: i32,
}

impl Stamp {
    /// The nearest legal stamp to the five numbers given.
    ///
    /// Clamping rather than `Option`, for the reason the module states: this is called with numbers
    /// out of a model, on a device where the failure report is a dialog with a number in it, and a
    /// date picker that refuses to appear is worse than one showing the 28th.
    ///
    /// Order matters and is fixed here: the year and the month are clamped first, and the day is
    /// clamped against the result — so `new(2001, 2, 31, ..)` is 28 February 2001 rather than
    /// 3 March or a panic.
    pub const fn new(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> Self {
        let year = clamp(year, YEAR_MIN, YEAR_MAX);
        let month = clamp(month, 1, 12);
        Self {
            year,
            month,
            day: clamp(day, 1, days_in_month(year, month)),
            hour: clamp(hour, 0, 23),
            minute: clamp(minute, 0, 59),
        }
    }

    /// A date at midnight — what a picker with no time fields on it reports.
    pub const fn date(year: i32, month: i32, day: i32) -> Self {
        Self::new(year, month, day, 0, 0)
    }

    pub const fn year(self) -> i32 {
        self.year
    }

    pub const fn month(self) -> i32 {
        self.month
    }

    pub const fn day(self) -> i32 {
        self.day
    }

    pub const fn hour(self) -> i32 {
        self.hour
    }

    pub const fn minute(self) -> i32 {
        self.minute
    }

    /// What this stamp holds in `part` — the number a spinner shows.
    pub const fn part(self, part: Part) -> i32 {
        match part {
            Part::Year => self.year,
            Part::Month => self.month,
            Part::Day => self.day,
            Part::Hour => self.hour,
            Part::Minute => self.minute,
        }
    }

    /// The inclusive range `part` may be stepped through, *given what the other parts hold*.
    ///
    /// The day's maximum is [`days_in_month`] of this stamp's own year and month, which is the whole
    /// reason a picker cannot hardcode `1..=31`: a day field that offered the 31st in February would
    /// let the user select a date this type then silently moves, and a moved value under a cursor
    /// that did not move is the worst of the available bugs.
    pub const fn bounds(self, part: Part) -> (i32, i32) {
        match part {
            Part::Year => (YEAR_MIN, YEAR_MAX),
            Part::Month => (1, 12),
            Part::Day => (1, days_in_month(self.year, self.month)),
            Part::Hour => (0, 23),
            Part::Minute => (0, 59),
        }
    }

    /// This stamp with `part` set to `value`, and the day pulled back if that made it impossible.
    ///
    /// The one function a picker calls, and the reason the picker has no calendar knowledge in it.
    /// Setting the month or the year re-clamps the day; setting the day clamps it against the month
    /// it is in. Nothing else can move: stepping the month must not change the year, because a
    /// spinner the user is not looking at changing under a press is how a January reminder ends up
    /// filed a year out.
    pub const fn with_part(self, part: Part, value: i32) -> Self {
        match part {
            // Through `new`, so there is one clamping order in this file rather than one per arm.
            Part::Year => Self::new(value, self.month, self.day, self.hour, self.minute),
            Part::Month => Self::new(self.year, value, self.day, self.hour, self.minute),
            Part::Day => Self::new(self.year, self.month, value, self.hour, self.minute),
            Part::Hour => Self::new(self.year, self.month, self.day, value, self.minute),
            Part::Minute => Self::new(self.year, self.month, self.day, self.hour, value),
        }
    }

    /// Whether `self` is what [`Stamp::new`] would have made of the same five numbers.
    ///
    /// Always true for a stamp built by this module — it is here for the boundary, where a stamp
    /// arrives as five numbers from a sync, a parser or a stored file and the caller wants to know
    /// whether the clamp moved anything before it writes the result back.
    pub const fn is_exactly(self, year: i32, month: i32, day: i32, hour: i32, minute: i32) -> bool {
        let s = Self::new(year, month, day, hour, minute);
        s.year == year && s.month == month && s.day == day && s.hour == hour && s.minute == minute
    }
}

/// `i32::clamp` is not `const` on this toolchain, and every constructor here needs it.
const fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_century_rule_is_the_one_a_four_year_test_would_have_got_wrong() {
        // 1900 is the reachable counterexample: divisible by four, not a leap year, and inside the
        // range a year spinner offers. A two-clause rule passes every other assertion in this file.
        assert!(!is_leap_year(1900));
        assert!(is_leap_year(2000), "divisible by 400");
        assert!(!is_leap_year(2100), "the other end of the offered range");
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(2023));
        assert!(is_leap_year(1600));
        assert!(!is_leap_year(1700));
    }

    #[test]
    fn february_has_twenty_nine_days_only_in_a_leap_year() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2023, 2), 28);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
    }

    #[test]
    fn every_month_has_the_length_the_knuckles_say_it_has() {
        let expected = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for (i, want) in expected.iter().enumerate() {
            assert_eq!(days_in_month(2023, i as i32 + 1), *want, "month {}", i + 1);
        }
    }

    #[test]
    fn a_month_number_that_cannot_exist_answers_rather_than_panicking() {
        // This is called with a number out of a model, from inside a key handler. A panic here is a
        // dialog with a number in it and a dead screen; 31 is the answer that cannot invalidate a
        // day the user is already holding.
        for month in [i32::MIN, -1, 0, 13, 99, i32::MAX] {
            assert_eq!(days_in_month(2023, month), 31, "month {month}");
        }
    }

    #[test]
    fn stepping_the_month_onto_february_does_not_leave_a_thirty_first() {
        // The defect this module exists for. One press on the month field, and the date that comes
        // back is one that exists.
        let jan31 = Stamp::date(2023, 1, 31);
        let feb = jan31.with_part(Part::Month, 2);
        assert_eq!((feb.month(), feb.day()), (2, 28));
        // And in a leap year the clamp stops one day later, which is the whole point of asking the
        // year rather than the month.
        let feb = Stamp::date(2024, 1, 31).with_part(Part::Month, 2);
        assert_eq!((feb.month(), feb.day()), (2, 29));
    }

    #[test]
    fn leaving_a_leap_year_pulls_the_twenty_ninth_back_to_the_twenty_eighth() {
        // The same defect through the other field, and the one a picker that only clamped on month
        // changes would ship: 29 February 2024, one press on the year, and 2023 has no such day.
        let leap = Stamp::date(2024, 2, 29);
        assert_eq!(leap.with_part(Part::Year, 2023).day(), 28);
        assert_eq!(leap.with_part(Part::Year, 2028).day(), 29, "still a leap year, so untouched");
    }

    #[test]
    fn stepping_one_field_moves_no_other_field() {
        // A spinner the user is not looking at must not move under a press: that is how a January
        // reminder ends up filed a year out. Only the day may follow, and only when it has to.
        let s = Stamp::new(2023, 6, 15, 9, 30);
        let m = s.with_part(Part::Month, 7);
        assert_eq!((m.year(), m.day(), m.hour(), m.minute()), (2023, 15, 9, 30));
        let h = s.with_part(Part::Hour, 22);
        assert_eq!((h.year(), h.month(), h.day(), h.minute()), (2023, 6, 15, 30));
    }

    #[test]
    fn the_day_is_clamped_against_the_month_it_lands_in() {
        // The day field's own bound, and the reason a picker asks `bounds` instead of hardcoding 31.
        let feb = Stamp::date(2023, 2, 10);
        assert_eq!(feb.bounds(Part::Day), (1, 28));
        assert_eq!(feb.with_part(Part::Day, 31).day(), 28);
        assert_eq!(feb.with_part(Part::Day, 0).day(), 1);
        assert_eq!(Stamp::date(2024, 2, 10).bounds(Part::Day), (1, 29));
        assert_eq!(Stamp::date(2023, 4, 10).bounds(Part::Day), (1, 30));
    }

    #[test]
    fn a_stamp_built_out_of_nonsense_is_still_a_date() {
        // Every number here comes from a model, and a picker that panicked on a bad one would take
        // the whole screen with it.
        let s = Stamp::new(i32::MIN, i32::MIN, i32::MIN, i32::MIN, i32::MIN);
        assert_eq!((s.year(), s.month(), s.day(), s.hour(), s.minute()), (YEAR_MIN, 1, 1, 0, 0));
        let s = Stamp::new(i32::MAX, i32::MAX, i32::MAX, i32::MAX, i32::MAX);
        assert_eq!((s.year(), s.month(), s.day(), s.hour(), s.minute()), (YEAR_MAX, 12, 31, 23, 59));
        // February at the top end, where the clamp has to consult the leap rule to answer.
        assert_eq!(Stamp::new(2023, 2, 99, 0, 0).day(), 28);
    }

    #[test]
    fn the_clamp_is_reported_rather_than_hidden_from_a_caller_that_asks() {
        // For the boundary — a synced date, a parsed string — where "it was moved" is information
        // the caller wants before it writes the result back.
        assert!(Stamp::date(2023, 1, 31).is_exactly(2023, 1, 31, 0, 0));
        assert!(!Stamp::new(2023, 2, 31, 0, 0).is_exactly(2023, 2, 31, 0, 0));
        assert!(!Stamp::new(2023, 1, 31, 24, 0).is_exactly(2023, 1, 31, 24, 0));
    }

    #[test]
    fn every_part_has_a_range_a_spinner_can_walk_end_to_end() {
        // The bounds a picker hands each field. Asserted as "the ends are reachable and legal"
        // rather than as literals, so a change to the range cannot pass by editing one constant.
        let s = Stamp::new(2023, 6, 15, 9, 30);
        for part in [Part::Year, Part::Month, Part::Day, Part::Hour, Part::Minute] {
            let (lo, hi) = s.bounds(part);
            assert!(lo < hi, "{part:?} is not a range");
            assert_eq!(s.with_part(part, lo).part(part), lo, "{part:?} cannot reach its floor");
            assert_eq!(s.with_part(part, hi).part(part), hi, "{part:?} cannot reach its ceiling");
            assert_eq!(s.with_part(part, lo - 1).part(part), lo, "{part:?} past the floor");
            assert_eq!(s.with_part(part, hi + 1).part(part), hi, "{part:?} past the ceiling");
        }
    }

    #[test]
    fn stamps_compare_chronologically_because_of_the_field_order() {
        // The derived `Ord` is why the fields are declared largest-unit first. A reminder list sorts
        // on this, and the hand-rolled comparison it replaces is the one that gets December wrong.
        assert!(Stamp::date(2023, 1, 31) < Stamp::date(2023, 2, 1));
        assert!(Stamp::new(2023, 1, 1, 9, 59) < Stamp::new(2023, 1, 1, 10, 0));
        assert!(Stamp::date(1999, 12, 31) < Stamp::date(2000, 1, 1));
    }
}
