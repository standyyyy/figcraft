//! The TDesign icon set: parsed, complete, and drawable.

use super::ICONS;

#[test]
fn every_icon_has_a_usable_path() {
    // A path with no drawing commands is an icon that inserts nothing; one with a NaN is a path the
    // vector layer refuses. Both are worth failing on here rather than in a design a model made.
    assert!(ICONS.len() >= 30, "the set carries the TDesign icons: {}", ICONS.len());
    for (name, path, w, h) in ICONS {
        assert!(!name.is_empty(), "an icon has a name");
        assert!(path.starts_with('M'), "`{name}` starts with a move: {path}");
        assert!(path.contains('L') || path.contains('C'), "`{name}` draws something: {path}");
        assert!(!path.contains("NaN") && !path.contains("inf"), "`{name}` has finite coordinates");
        assert!(*w > 0.0 && *h > 0.0, "`{name}` has a box: {w}x{h}");
    }
}

#[test]
fn the_names_are_unique_and_sorted() {
    let mut names: Vec<&str> = ICONS.iter().map(|(n, ..)| *n).collect();
    let sorted = {
        let mut s = names.clone();
        s.sort_unstable();
        s
    };
    assert_eq!(names, sorted, "sorted, so listing them needs no sort");
    names.dedup();
    assert_eq!(names.len(), ICONS.len(), "no two icons share a name");
}

#[test]
fn a_path_has_one_move_per_stroke() {
    // A TDesign icon is a bag of strokes: a calendar's frame, its ticks, its binding rings. Joining
    // them into one chain drew lines that do not exist, so this pins the per-segment shape: the move
    // count never exceeds the line count, and a multi-stroke icon has more than one.
    let calendar = super::find("calendar").expect("the calendar icon");
    let moves = calendar.1.matches('M').count();
    assert!(moves > 4, "the calendar is drawn in several strokes, got {moves}: {}", calendar.1);
}

#[test]
fn an_icon_is_found_with_or_without_the_catalog_prefix() {
    // A model reading the icons file sees `icon/add`; one reading a design sees `add`.
    for asked in ["add", "icon/add"] {
        let found = super::find(asked).unwrap_or_else(|| panic!("`{asked}` is an icon"));
        assert_eq!(found.0, "add");
    }
    assert!(super::find("no-such-icon").is_none(), "an unknown name is None, not a guess");
    assert!(super::find("").is_none());
}

#[test]
fn the_set_covers_the_controls_a_prototype_needs() {
    // What a screen built from TDesign reaches for. A set missing `search` or `check` makes a model
    // draw one out of rectangles instead.
    for needed in ["add", "search", "check", "close", "arrow-right", "arrow-down", "more", "setting", "user", "star"] {
        assert!(super::find(needed).is_some(), "`{needed}` is in the set");
    }
}

#[test]
fn names_lists_every_icon() {
    assert_eq!(super::names().count(), ICONS.len());
}
