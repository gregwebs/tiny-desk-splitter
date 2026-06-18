use std::collections::{HashMap, HashSet};

use crate::sync::YearMonth;

fn render_month_divider(ym: &YearMonth, show_sync: bool) -> String {
    let label = ym.display_label();
    let sync_button = if show_sync {
        format!(
            " <button hx-post='/sync/{}/{}' hx-disabled-elt='this'>Sync</button>",
            ym.year, ym.month
        )
    } else {
        String::new()
    };
    format!(
        "<div class='month-divider'><span class='month-label'>{}</span>{}</div>",
        label, sync_button
    )
}

pub fn build_month_items(
    current: &YearMonth,
    earliest_date: Option<&str>,
    synced: &HashSet<YearMonth>,
    mut by_month: HashMap<YearMonth, Vec<String>>,
    no_date_rows: Vec<String>,
    hide_empty_months: bool,
) -> Vec<String> {
    let earliest_ym = earliest_date
        .and_then(YearMonth::from_date_str)
        .unwrap_or_else(|| current.clone());
    let stop_at = earliest_ym.previous();
    tracing::debug!(
        "month walk: current={:?} earliest={:?} stop_at={:?}",
        current,
        earliest_ym,
        stop_at
    );

    let mut items: Vec<String> = Vec::new();
    let mut ym = current.clone();
    loop {
        let rows = by_month.remove(&ym);
        let has_rows = rows.as_ref().is_some_and(|r| !r.is_empty());
        if !hide_empty_months || has_rows {
            let show_sync = ym == *current || !synced.contains(&ym);
            items.push(render_month_divider(&ym, show_sync));
            if let Some(rows) = rows {
                items.extend(rows);
            }
        }
        if ym == stop_at {
            break;
        }
        ym = ym.previous();
    }

    if !no_date_rows.is_empty() {
        items.push(
            r#"<div class="month-divider"><span class="month-label">Unknown date</span></div>"#
                .to_string(),
        );
        items.extend(no_date_rows);
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ym(year: i32, month: u32) -> YearMonth {
        YearMonth { year, month }
    }

    fn count_dividers(items: &[String]) -> usize {
        items.iter().filter(|s| s.contains("month-divider")).count()
    }

    fn has_sync_button(items: &[String], year: i32, month: u32) -> bool {
        items.iter().any(|s| {
            s.contains("month-divider")
                && s.contains(&format!("hx-post='/sync/{}/{}'", year, month))
        })
    }

    #[test]
    fn basic_walk_produces_correct_dividers() {
        let current = ym(2026, 5);
        let items = build_month_items(
            &current,
            Some("2026-03-01"),
            &HashSet::new(),
            HashMap::new(),
            Vec::new(),
            false,
        );
        // May, Apr, Mar, Feb (stop_at = Feb = earliest.previous())
        assert_eq!(count_dividers(&items), 4);
    }

    #[test]
    fn single_month_when_current_equals_earliest() {
        let current = ym(2026, 5);
        let items = build_month_items(
            &current,
            Some("2026-05-15"),
            &HashSet::new(),
            HashMap::new(),
            Vec::new(),
            false,
        );
        // May, Apr (stop_at = Apr = earliest.previous())
        assert_eq!(count_dividers(&items), 2);
    }

    #[test]
    fn no_earliest_defaults_to_current() {
        let current = ym(2026, 5);
        let items = build_month_items(
            &current,
            None,
            &HashSet::new(),
            HashMap::new(),
            Vec::new(),
            false,
        );
        assert_eq!(count_dividers(&items), 2);
    }

    #[test]
    fn rows_interleaved_under_their_month() {
        let current = ym(2026, 3);
        let mut by_month = HashMap::new();
        by_month.insert(ym(2026, 3), vec!["row-march".to_string()]);
        by_month.insert(
            ym(2026, 2),
            vec!["row-feb-1".to_string(), "row-feb-2".to_string()],
        );

        let items = build_month_items(
            &current,
            Some("2026-02-01"),
            &HashSet::new(),
            by_month,
            Vec::new(),
            false,
        );

        let march_div = items.iter().position(|s| s.contains("March 2026")).unwrap();
        let row_march = items.iter().position(|s| s == "row-march").unwrap();
        assert!(row_march == march_div + 1);

        let feb_div = items
            .iter()
            .position(|s| s.contains("February 2026"))
            .unwrap();
        let row_feb1 = items.iter().position(|s| s == "row-feb-1").unwrap();
        assert!(row_feb1 == feb_div + 1);
    }

    #[test]
    fn no_date_rows_appended_at_end() {
        let current = ym(2026, 5);
        let no_date = vec!["undated-row".to_string()];
        let items = build_month_items(
            &current,
            Some("2026-05-01"),
            &HashSet::new(),
            HashMap::new(),
            no_date,
            false,
        );

        let unknown_div = items
            .iter()
            .position(|s| s.contains("Unknown date"))
            .expect("should have Unknown date divider");
        let undated = items.iter().position(|s| s == "undated-row").unwrap();
        assert_eq!(undated, unknown_div + 1);
        assert_eq!(undated, items.len() - 1);
    }

    #[test]
    fn no_unknown_date_section_when_empty() {
        let current = ym(2026, 5);
        let items = build_month_items(
            &current,
            None,
            &HashSet::new(),
            HashMap::new(),
            Vec::new(),
            false,
        );
        assert!(!items.iter().any(|s| s.contains("Unknown date")));
    }

    #[test]
    fn synced_months_omit_sync_button_except_current() {
        let current = ym(2026, 5);
        let mut synced = HashSet::new();
        synced.insert(ym(2026, 4));
        synced.insert(ym(2026, 5));

        let items = build_month_items(
            &current,
            Some("2026-04-01"),
            &synced,
            HashMap::new(),
            Vec::new(),
            false,
        );

        // Current month always shows sync button even if synced
        assert!(has_sync_button(&items, 2026, 5));
        // April is synced → no sync button
        assert!(!has_sync_button(&items, 2026, 4));
        // March (stop_at) is not synced → has sync button
        assert!(has_sync_button(&items, 2026, 3));
    }

    #[test]
    fn long_span_completes() {
        let current = ym(2026, 5);
        let items = build_month_items(
            &current,
            Some("2000-01-01"),
            &HashSet::new(),
            HashMap::new(),
            Vec::new(),
            false,
        );
        // 2026-05 to 1999-12 inclusive = 318 months
        assert_eq!(count_dividers(&items), 318);
    }

    #[test]
    fn hide_empty_months_skips_dividers_with_no_rows() {
        let current = ym(2026, 5);
        let mut by_month = HashMap::new();
        by_month.insert(ym(2026, 3), vec!["row-march".to_string()]);

        let items = build_month_items(
            &current,
            Some("2026-01-01"),
            &HashSet::new(),
            by_month,
            Vec::new(),
            true,
        );

        // Only March has rows; May, Apr, Feb, Jan should be skipped entirely.
        assert_eq!(count_dividers(&items), 1);
        assert!(items.iter().any(|s| s.contains("March 2026")));
        assert!(items.iter().any(|s| s == "row-march"));
    }

    #[test]
    fn hide_empty_months_keeps_dividers_with_rows() {
        let current = ym(2026, 3);
        let mut by_month = HashMap::new();
        by_month.insert(ym(2026, 3), vec!["row-march".to_string()]);
        by_month.insert(
            ym(2026, 1),
            vec!["row-jan-1".to_string(), "row-jan-2".to_string()],
        );

        let items = build_month_items(
            &current,
            Some("2026-01-01"),
            &HashSet::new(),
            by_month,
            Vec::new(),
            true,
        );

        // March and January have rows; February is empty and should be skipped.
        assert_eq!(count_dividers(&items), 2);
        assert!(items.iter().any(|s| s.contains("March 2026")));
        assert!(items.iter().any(|s| s.contains("January 2026")));
        assert!(!items.iter().any(|s| s.contains("February 2026")));
    }

    #[test]
    fn hide_empty_months_omits_current_month_when_empty() {
        let current = ym(2026, 5);
        let mut by_month = HashMap::new();
        by_month.insert(ym(2026, 3), vec!["row-march".to_string()]);

        let items = build_month_items(
            &current,
            Some("2026-03-01"),
            &HashSet::new(),
            by_month,
            Vec::new(),
            true,
        );

        assert!(!items.iter().any(|s| s.contains("May 2026")));
        assert!(items.iter().any(|s| s.contains("March 2026")));
    }

    #[test]
    fn hide_empty_months_still_appends_no_date_rows() {
        let current = ym(2026, 5);
        let no_date = vec!["undated-row".to_string()];

        let items = build_month_items(
            &current,
            Some("2026-05-01"),
            &HashSet::new(),
            HashMap::new(),
            no_date,
            true,
        );

        // No month has rows, so no month dividers should appear; the only
        // divider-class element left is the "Unknown date" section, which is
        // independent of hide_empty_months.
        assert_eq!(count_dividers(&items), 1);
        assert!(items.iter().any(|s| s.contains("Unknown date")));
        assert!(items.iter().any(|s| s == "undated-row"));
    }
}
