use crate::data::{division_key, format_date, js_float, SiteData};
use crate::html::{esc, esc_attr};
use pollywiki_schema::{Bill, Division, DivisionResult, House, Person};

pub fn avatar(person: &Person, large: bool) -> String {
    match &person.photo {
        Some(photo) => {
            let src = if large {
                photo.thumb_large.as_deref().unwrap_or(&photo.url)
            } else {
                photo.thumb.as_deref().unwrap_or(&photo.url)
            };
            format!(
                "<img class=\"{class}\" src=\"{src}\" alt=\"Portrait of {name}\" loading=\"{loading}\" decoding=\"async\" width=\"{size}\" height=\"{size}\">",
                class = if large { "avatar large" } else { "avatar" },
                src = esc_attr(src),
                name = esc_attr(&person.name),
                loading = if large { "eager" } else { "lazy" },
                size = if large { 102 } else { 48 },
            )
        }
        None => {
            let initials: String = person
                .name
                .split_whitespace()
                .filter_map(|part| part.chars().next())
                .take(2)
                .collect();
            format!(
                "<span class=\"{class}\" aria-hidden=\"true\">{initials}</span>",
                class = if large {
                    "avatar initials large"
                } else {
                    "avatar initials"
                },
                initials = esc(&initials),
            )
        }
    }
}

pub fn chamber_chip(house: House) -> String {
    match house {
        House::Senate => "<span class=\"chip senate\">Senate</span>".to_string(),
        House::Representatives => "<span class=\"chip house\">House</span>".to_string(),
    }
}

pub fn group_chip(data: &SiteData, group_slug: &str, group: &str, link: bool) -> String {
    let party = data.party_by_slug(group_slug);
    let colour = party.and_then(|p| p.colour.as_deref()).unwrap_or("#6e7b74");
    let label = party.map(|p| p.name.as_str()).unwrap_or(group);
    if link {
        format!(
            "<a class=\"group-chip\" href=\"/parties/{group_slug}/\"><span class=\"dot\" style=\"background:{colour}\" aria-hidden=\"true\"></span>{}</a>",
            esc(label),
        )
    } else {
        format!(
            "<span class=\"group-chip\"><span class=\"dot\" style=\"background:{colour}\" aria-hidden=\"true\"></span>{}</span>",
            esc(label),
        )
    }
}

pub fn vote_bar(ayes: i64, noes: i64, result: bool) -> String {
    let total = (ayes + noes).max(1);
    format!(
        "<span class=\"{class}\" role=\"img\" aria-label=\"{ayes} ayes, {noes} noes\"><span class=\"a\" style=\"width:{width}%\"></span></span>",
        class = if result { "vote-bar result" } else { "vote-bar" },
        width = js_float(ayes as f64 / total as f64 * 100.0),
    )
}

/// The question put, in the official terminology used site-wide.
pub fn result_word(result: DivisionResult) -> &'static str {
    match result {
        DivisionResult::Passed => "Carried",
        DivisionResult::Rejected => "Negatived",
    }
}

pub fn result_chip(result: DivisionResult) -> String {
    format!(
        "<span class=\"result-chip {class}\">{word}</span>",
        class = match result {
            DivisionResult::Passed => "carried",
            DivisionResult::Rejected => "negatived",
        },
        word = result_word(result),
    )
}

pub fn ledger_row(division: &Division) -> String {
    let href = format!("/divisions/{}/{}/", division.house, division_key(division));
    let chamber = match division.house {
        House::Senate => "Senate",
        House::Representatives => "House",
    };
    format!(
        "<li data-house=\"{house}\" data-text=\"{text}\"><span class=\"when\">{when}</span><span class=\"what\"><a href=\"{href}\">{name}</a></span><span class=\"tally\">{chip}{chamber} {ayes}\u{2013}{noes}{bar}</span></li>",
        house = division.house,
        text = esc_attr(&division.name.to_lowercase()),
        when = esc(&format_date(&division.date)),
        name = esc(&division.name),
        chip = result_chip(division.result),
        ayes = division.ayes,
        noes = division.noes,
        bar = vote_bar(division.ayes, division.noes, false),
    )
}

/// Month divider for the divisions ledger: mono-caps month, hairline, count.
pub fn ledger_month(month: &str, count: usize) -> String {
    format!(
        "<li class=\"ledger-month\" data-month=\"{key}\"><span class=\"m\">{label}</span><span class=\"rule\" aria-hidden=\"true\"></span><span class=\"n\">{count} division{plural}</span></li>",
        key = esc_attr(month),
        label = esc(&month_label(month)),
        plural = if count == 1 { "" } else { "s" },
    )
}

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// "2026-08" -> "August 2026". Unparseable keys pass through unchanged.
pub fn month_label(month: &str) -> String {
    let (year, m) = match month.split_once('-') {
        Some(parts) => parts,
        None => return month.to_string(),
    };
    match m.parse::<usize>() {
        Ok(n) if (1..=12).contains(&n) => format!("{} {year}", MONTH_NAMES[n - 1]),
        _ => month.to_string(),
    }
}

/// Which chamber a status line such as "Before the Senate" points at.
fn status_chamber(status_lower: &str) -> Option<House> {
    if status_lower.contains("senate") {
        Some(House::Senate)
    } else if status_lower.contains("house")
        || status_lower.contains("representatives")
        || status_lower.contains("reps")
    {
        Some(House::Representatives)
    } else {
        None
    }
}

/// Progress on the introduced -> passed 1st house -> passed 2nd house ->
/// assent path, 0 to 4. Timeline events lead where the ingest recorded them;
/// the status word fills in the rest and always wins if it is further along.
pub fn bill_stage(bill: &Bill) -> u8 {
    let mut from_timeline = 0u8;
    let mut third_readings = 0u8;
    for step in &bill.timeline {
        let event = step.event.to_lowercase();
        if event.contains("assent") {
            return 4;
        } else if event.contains("passed both houses") {
            from_timeline = from_timeline.max(3);
        } else if event.contains("third reading") {
            // The bundles do not name the chamber, so count the readings: one
            // is the originating house, two is both.
            third_readings = third_readings.saturating_add(1);
        } else if event.contains("introduced") || event.contains("first reading") {
            from_timeline = from_timeline.max(1);
        }
    }
    from_timeline = from_timeline.max(match third_readings {
        0 => 0,
        1 => 2,
        _ => 3,
    });

    let status = bill.status.to_lowercase();
    let from_status = if status.contains("assent") || status.starts_with("act") {
        4
    } else if let Some(rest) = status.strip_prefix("before ") {
        // Sitting in the other chamber means the originating house has passed it.
        match status_chamber(rest) {
            Some(house) if house != bill.chamber => 2,
            _ => 1,
        }
    } else {
        0
    };
    from_timeline.max(from_status)
}

pub const BILL_DOTS_LEGEND: &str = "<p class=\"dots-legend\">Introduced \u{b7} Passed 1st house \u{b7} Passed 2nd house \u{b7} Assent</p>";

pub fn bill_dots(bill: &Bill) -> String {
    let stage = bill_stage(bill);
    let reached = match stage {
        0 => "not yet introduced",
        1 => "introduced",
        2 => "passed originating house",
        3 => "passed both houses",
        _ => "assented",
    };
    let mut out = format!(
        "<span class=\"bill-dots\" role=\"img\" aria-label=\"Stage {stage} of 4: {reached}\">"
    );
    for step in 1..=4u8 {
        out.push_str(if step <= stage {
            "<i class=\"on\"></i>"
        } else {
            "<i class=\"off\"></i>"
        });
    }
    out.push_str("</span>");
    out
}

pub fn person_card(data: &SiteData, person: &Person) -> String {
    let seat = match person.house {
        House::Senate => format!(
            "Senator for {}",
            person.state.map(|s| s.as_str()).unwrap_or("")
        ),
        House::Representatives => person
            .electorate
            .as_deref()
            .and_then(|slug| data.electorate_by_slug(slug))
            .map(|e| e.name.clone())
            .unwrap_or_default(),
    };
    let party = data.party_by_slug(&person.group_slug);
    let sub = match party {
        Some(party) => format!("{seat} · {}", party.code.as_deref().unwrap_or(&party.name)),
        None => seat,
    };
    format!(
        "<a class=\"person-card\" href=\"/people/{slug}/\">{avatar}<span class=\"name\">{name}</span><span class=\"sub\">{sub}</span></a>",
        slug = person.slug,
        avatar = avatar(person, false),
        name = esc(&person.name),
        sub = esc(&sub),
    )
}

pub fn seat_bar(data: &SiteData, house: House) -> String {
    let mut rows: Vec<(&pollywiki_schema::Party, i64)> = data
        .parties
        .iter()
        .map(|p| (p, p.seats.as_ref().map(|s| s.get(house)).unwrap_or(0)))
        .filter(|(_, seats)| *seats > 0)
        .collect();
    rows.sort_by_key(|(_, seats)| std::cmp::Reverse(*seats));
    let total: i64 = rows.iter().map(|(_, seats)| seats).sum();
    let chamber = match house {
        House::Senate => "Senate",
        House::Representatives => "House of Representatives",
    };
    let aria = rows
        .iter()
        .map(|(p, seats)| format!("{} {seats}", p.name))
        .collect::<Vec<_>>()
        .join(", ");
    // The threshold sits on the boundary between the last minority seat and
    // the first majority one, so a segment crossing the tick holds a majority.
    let majority = total / 2 + 1;
    let tick = js_float((majority - 1).max(0) as f64 / total.max(1) as f64 * 100.0);

    let mut out = format!(
        "<div class=\"seat-bar\"><div class=\"label\"><span>{chamber} \u{b7} {total} seats</span>"
    );
    if total > 0 {
        out.push_str(&format!(
            "<span class=\"majority\">majority {majority}</span>"
        ));
    }
    out.push_str("</div><div class=\"bar-wrap\">");
    if total > 0 {
        out.push_str(&format!(
            "<span class=\"tick-label\" style=\"left:{tick}%\">{majority}</span>"
        ));
    }
    out.push_str(&format!(
        "<div class=\"bar\" role=\"img\" aria-label=\"{}\">",
        esc_attr(&format!("{chamber} composition: {aria}")),
    ));
    for (party, seats) in &rows {
        out.push_str(&format!(
            "<a href=\"/parties/{slug}/\" style=\"width:{width}%;background:{colour}\" title=\"{title}\" aria-label=\"{label}\"></a>",
            slug = party.slug,
            width = js_float(*seats as f64 / total.max(1) as f64 * 100.0),
            colour = party.colour.as_deref().unwrap_or("#6e7b74"),
            title = esc_attr(&format!("{}: {seats}", party.name)),
            label = esc_attr(&format!(
                "{}, {seats} seat{}",
                party.code.as_deref().unwrap_or(&party.name),
                if *seats == 1 { "" } else { "s" }
            )),
        ));
    }
    out.push_str("</div>");
    if total > 0 {
        out.push_str(&format!(
            "<span class=\"tick\" style=\"left:{tick}%\" aria-hidden=\"true\"></span>"
        ));
    }
    out.push_str("</div><div class=\"key\">");
    out.push_str(
        &rows
            .iter()
            .map(|(p, seats)| {
                format!(
                    "<a href=\"/parties/{}/\">{} {seats}</a>",
                    p.slug,
                    esc(p.code.as_deref().unwrap_or(&p.name))
                )
            })
            .collect::<Vec<_>>()
            .join(" \u{b7} "),
    );
    out.push_str("</div></div>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bill(json: &str) -> Bill {
        serde_json::from_str(json).expect("bill fixture")
    }

    #[test]
    fn month_labels_read_as_prose() {
        assert_eq!(month_label("2026-08"), "August 2026");
        assert_eq!(month_label("2026-01"), "January 2026");
        assert_eq!(month_label("2026-13"), "2026-13");
        assert_eq!(month_label("2026"), "2026");
    }

    #[test]
    fn result_words_use_the_question_terminology() {
        assert_eq!(result_word(DivisionResult::Passed), "Carried");
        assert_eq!(result_word(DivisionResult::Rejected), "Negatived");
        assert!(result_chip(DivisionResult::Passed).contains("result-chip carried"));
        assert!(result_chip(DivisionResult::Rejected).contains("result-chip negatived"));
    }

    #[test]
    fn stage_from_status_alone() {
        // Sitting in the chamber it was introduced in: stage 1.
        assert_eq!(
            bill_stage(&bill(
                r#"{"id":"a","title":"A","parliament":48,"chamber":"representatives","status":"Before the House of Representatives"}"#
            )),
            1
        );
        // Sitting in the other chamber, so the originating house has passed it.
        assert_eq!(
            bill_stage(&bill(
                r#"{"id":"b","title":"B","parliament":48,"chamber":"representatives","status":"Before the Senate"}"#
            )),
            2
        );
        assert_eq!(
            bill_stage(&bill(
                r#"{"id":"c","title":"C","parliament":48,"chamber":"senate","status":"Before Reps"}"#
            )),
            2
        );
        assert_eq!(
            bill_stage(&bill(
                r#"{"id":"d","title":"D","parliament":48,"chamber":"representatives","status":"Act"}"#
            )),
            4
        );
        // Finished some other way: the status word carries the rest.
        assert_eq!(
            bill_stage(&bill(
                r#"{"id":"e","title":"E","parliament":48,"chamber":"senate","status":"Not proceeding"}"#
            )),
            0
        );
    }

    #[test]
    fn timeline_events_lead_where_they_exist() {
        let introduced = bill(
            r#"{"id":"f","title":"F","parliament":48,"chamber":"senate","status":"Not proceeding",
                "timeline":[{"date":"2026-02-01","event":"Introduced"}]}"#,
        );
        assert_eq!(bill_stage(&introduced), 1);

        let one_reading = bill(
            r#"{"id":"g","title":"G","parliament":48,"chamber":"senate","status":"Discharged",
                "timeline":[{"date":"2026-02-01","event":"Introduced"},
                            {"date":"2026-03-01","event":"Third reading agreed to"}]}"#,
        );
        assert_eq!(bill_stage(&one_reading), 2);

        let both_readings = bill(
            r#"{"id":"h","title":"H","parliament":48,"chamber":"senate","status":"Discharged",
                "timeline":[{"date":"2026-02-01","event":"Third reading agreed to"},
                            {"date":"2026-03-01","event":"Third reading agreed to"}]}"#,
        );
        assert_eq!(bill_stage(&both_readings), 3);

        let assented = bill(
            r#"{"id":"i","title":"I","parliament":48,"chamber":"senate","status":"Before the House",
                "timeline":[{"date":"2026-04-01","event":"Assent"}]}"#,
        );
        assert_eq!(bill_stage(&assented), 4);
    }

    #[test]
    fn dots_carry_a_stage_label_and_fill_count() {
        let dots = bill_dots(&bill(
            r#"{"id":"j","title":"J","parliament":48,"chamber":"representatives","status":"Before the Senate"}"#,
        ));
        assert!(dots.contains("Stage 2 of 4: passed originating house"));
        assert_eq!(dots.matches("class=\"on\"").count(), 2);
        assert_eq!(dots.matches("class=\"off\"").count(), 2);
    }

    #[test]
    fn month_divider_counts_agree_with_the_run() {
        let row = ledger_month("2026-07", 1);
        assert!(row.contains("July 2026"));
        assert!(row.contains("1 division<"));
        assert!(ledger_month("2026-07", 5).contains("5 divisions<"));
    }
}
