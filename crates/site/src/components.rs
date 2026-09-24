use crate::data::{
    division_key, division_stage, first_sentence, format_date, js_float, plain_text, short_date,
    DivisionSeries, SiteData,
};
use crate::html::{esc, esc_attr};
use pollywiki_schema::{Bill, Division, DivisionResult, House, Person, SummaryKind};

pub fn avatar(person: &Person, large: bool) -> String {
    match &person.photo {
        Some(photo) => {
            let src = if large {
                photo.thumb_large.as_deref().unwrap_or(&photo.url)
            } else {
                photo.thumb.as_deref().unwrap_or(&photo.url)
            };
            // A card avatar sits beside the name in the same link, so it is
            // decorative; the profile portrait stands alone and keeps its text
            // alternative.
            format!(
                "<img class=\"{class}\" src=\"{src}\" alt=\"{alt}\" loading=\"{loading}\" decoding=\"async\" width=\"{size}\" height=\"{size}\">",
                class = if large { "avatar large" } else { "avatar" },
                src = esc_attr(src),
                alt = if large {
                    format!("Portrait of {}", esc_attr(&person.name))
                } else {
                    String::new()
                },
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
                "<span class=\"{class}\" aria-hidden=\"true\" data-pagefind-ignore>{initials}</span>",
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

/// The swatch for a group the reference data gives no colour: a neutral grey,
/// so an unknown group never borrows another party's hue.
pub const GROUP_FALLBACK_COLOUR: &str = "#6e7b74";

pub fn group_chip(data: &SiteData, group_slug: &str, group: &str, link: bool) -> String {
    let party = data.party_by_slug(group_slug);
    let colour = party
        .and_then(|p| p.colour.as_deref())
        .unwrap_or(GROUP_FALLBACK_COLOUR);
    let label = party.map(|p| p.name.as_str()).unwrap_or(group);
    // A group with no sitting members has no page, so the chip stays plain.
    if link && party.is_some() {
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

/// A ledger row where nothing above it gives the date: home, a bill page.
pub fn ledger_row(division: &Division) -> String {
    ledger_row_labelled(division, &division.name)
}

/// A dated ledger row whose title column says only what the page around it
/// leaves unsaid, such as the stage alone under the bill's own heading. The
/// link still leads to the division and its full official name.
pub fn ledger_row_labelled(division: &Division, label: &str) -> String {
    ledger_li(division, &esc(&format_date(&division.date)), label, false)
}

/// A ledger row under a month divider, which already carries the year: the
/// date drops it, and the full date stays machine-readable in the <time>.
pub fn ledger_row_in_month(division: &Division) -> String {
    ledger_li(
        division,
        &format!(
            "<time datetime=\"{}\">{}</time>",
            esc_attr(&division.date),
            esc(&short_date(&division.date))
        ),
        &division.name,
        false,
    )
}

/// A row in a division's sitting day, where every row shares the date: the
/// division number takes the date column instead, so the day reads in the
/// order it was voted. The division being read is in the list, marked
/// current and not linked to itself.
pub fn sitting_day_row(division: &Division, current: bool) -> String {
    ledger_li(
        division,
        &format!("Division {}", division.number),
        &division.name,
        current,
    )
}

/// The tally splits into outcome, chamber, figures and bar so each lines up
/// down the ledger as a column. The spaces between the spans, the date, title
/// and tally's included, cost nothing in the grid; they keep the words apart
/// when read aloud, copied or quoted in a search excerpt. The row
/// carries no lower-cased copy of its title for the filter: the script reads
/// the title itself, which spares the index a fifth of its weight. The label
/// is what the title column shows; a current row shows it unlinked.
fn ledger_li(division: &Division, when: &str, label: &str, current: bool) -> String {
    let chamber = match division.house {
        House::Senate => "Senate",
        House::Representatives => "House",
    };
    let (attrs, what) = if current {
        (" aria-current=\"true\"", esc(label))
    } else {
        (
            "",
            format!(
                "<a href=\"/divisions/{}/{}/\">{}</a>",
                division.house,
                division_key(division),
                esc(label)
            ),
        )
    };
    format!(
        "<li data-house=\"{house}\"{attrs}><span class=\"when\">{when}</span> <span class=\"what\">{what}</span> <span class=\"tally\">{chip} <span class=\"ch\">{chamber}</span> <span class=\"fig\">{ayes}\u{2013}{noes}</span>{bar}</span></li>",
        house = division.house,
        chip = result_chip(division.result),
        ayes = division.ayes,
        noes = division.noes,
        bar = vote_bar(division.ayes, division.noes, false),
    )
}

/// One ledger entry per series. A lone division renders as the plain row. A
/// run of them folds into the matter, its outcomes in the order they were
/// decided, and one step per question, so a bill amended and put again reads
/// as one story instead of ten identical lines.
pub fn series_row(data: &SiteData, series: &DivisionSeries) -> String {
    match series.divisions.as_slice() {
        [single] => ledger_row(single),
        _ => series_li(data, series, &esc(&format_date(series.date))),
    }
}

/// A series under a month divider: the date drops its year, as a plain row
/// does there, and the full date stays machine-readable in the <time>.
pub fn series_row_in_month(data: &SiteData, series: &DivisionSeries) -> String {
    match series.divisions.as_slice() {
        [single] => ledger_row_in_month(single),
        _ => series_li(
            data,
            series,
            &format!(
                "<time datetime=\"{}\">{}</time>",
                esc_attr(series.date),
                esc(&short_date(series.date))
            ),
        ),
    }
}

/// The folded row. It says how many divisions it stands for in data-count,
/// so an index filter can count divisions rather than rows. Like a plain
/// row it carries no lower-cased copy of its text: a filter reads the title
/// and the step labels from the row itself.
fn series_li(data: &SiteData, series: &DivisionSeries, when: &str) -> String {
    let chamber = match series.house {
        House::Senate => "Senate",
        House::Representatives => "House",
    };
    let carried = series
        .divisions
        .iter()
        .filter(|d| d.result == DivisionResult::Passed)
        .count();
    let negatived = series.divisions.len() - carried;

    // A stage every division shares belongs in the title; only a stage that
    // varies tells the steps apart.
    let stages: Vec<Option<&str>> = series
        .divisions
        .iter()
        .map(|d| division_stage(&d.name))
        .collect();
    let shared_stage = stages
        .windows(2)
        .all(|pair| pair[0] == pair[1])
        .then(|| stages[0])
        .flatten();
    let title = match shared_stage {
        Some(stage) => format!("{}; {}", series.matter, stage),
        None => series.matter.to_string(),
    };
    // The summary is a pure toggle: a link inside it would be a second control
    // in one, and a click on the title would leave the page instead of opening
    // the row. When the whole series is about one bill the register has a page
    // for, the link opens with the steps, under the bill's own title.
    let bill = series
        .divisions
        .iter()
        .map(|d| d.bill_ids.as_slice())
        .reduce(|a, b| if a == b { a } else { &[] })
        .and_then(|ids| match ids {
            [id] => data.bill_by_id(id),
            _ => None,
        });
    let bill_line = match bill {
        Some(bill) => format!(
            "<p class=\"series-bill\">Bill: <a href=\"/bills/{}/\">{}</a></p>",
            esc_attr(&bill.id),
            esc(&bill.title)
        ),
        None => String::new(),
    };

    let mut steps = String::new();
    let (mut machine_written, mut volunteer_written) = (false, false);
    for d in &series.divisions {
        let href = esc_attr(&format!("/divisions/{}/{}/", d.house, division_key(d)));
        let stage = if shared_stage.is_some() {
            None
        } else {
            division_stage(&d.name)
        };
        let question = step_question(d);
        let ai_mark = match question {
            Some((_, true)) => {
                machine_written = true;
                "<span class=\"ai-mark\" title=\"AI-generated\">AI</span>"
            }
            Some((_, false)) => {
                volunteer_written = true;
                ""
            }
            None => "",
        };
        let question = question.map(|(text, _)| text);
        // A stage is the rest of the division's official name; a question is
        // a description of it. The stage link says which it is, so the index
        // filter can match a series on its divisions' names alone, as it
        // matches a plain row, and never on a written description.
        let (label, class, note) = match (stage, question) {
            (Some(stage), question) => (Some(stage.to_string()), " class=\"stage\"", question),
            (None, Some(question)) => (Some(question), "", None),
            (None, None) => (None, "", None),
        };
        // The number identifies the step; the link sits on whatever describes
        // it, and on the number only when nothing does.
        let (number, what) = match label {
            Some(label) => (
                format!("Division {}", d.number),
                format!(
                    "<a{class} href=\"{href}\">{}</a>{}",
                    esc(&label),
                    match note {
                        Some(note) => format!("<span class=\"q\">{}{ai_mark}</span>", esc(&note)),
                        None => ai_mark.to_string(),
                    }
                ),
            ),
            None => (
                format!("<a href=\"{href}\">Division {}</a>", d.number),
                String::new(),
            ),
        };
        steps.push_str(&format!(
            "<li><span class=\"n\">{number}</span><span class=\"what\">{what}</span><span class=\"tally\">{chip} <span class=\"fig\">{ayes}\u{2013}{noes}</span>{bar}</span></li>",
            chip = result_chip(d.result),
            ayes = d.ayes,
            noes = d.noes,
            bar = vote_bar(d.ayes, d.noes, false),
        ));
    }
    let mut credit = String::new();
    if machine_written {
        credit.push_str("<span class=\"ai-tag\">AI-generated</span> Descriptions marked AI are written by AI from the official record and may contain errors; the record is authoritative. <a href=\"/about/methodology/\">How this works.</a>");
    }
    if volunteer_written {
        if machine_written {
            credit.push(' ');
        }
        credit.push_str("Other descriptions are the first sentence of context written by <a href=\"https://theyvoteforyou.org.au\">They Vote For You</a> volunteers (ODbL).");
    }
    let credit = if credit.is_empty() {
        String::new()
    } else {
        format!("<p class=\"series-credit\">{credit}</p>")
    };

    format!(
        "<li class=\"ledger-series\" data-house=\"{house}\" data-count=\"{n}\"><details><summary><span class=\"when\">{when}</span><span class=\"what\"><span class=\"matter\">{title}</span><span class=\"series-note\">{n} divisions \u{b7} {carried} carried \u{b7} {negatived} negatived</span></span><span class=\"tally\">{strip} <span class=\"ch\">{chamber}</span></span></summary>{bill_line}<ol class=\"series-steps\">{steps}</ol>{credit}</details></li>",
        house = series.house,
        title = esc(&title),
        n = series.divisions.len(),
        strip = outcome_strip(&series.divisions),
    )
}

/// One sentence on what a division decided, and whether a machine wrote it.
/// They Vote For You's written context comes first; where their field holds a
/// Hansard excerpt instead, the machine-written note stands in.
fn step_question(division: &Division) -> Option<(String, bool)> {
    match (
        &division.summary,
        division.summary_kind,
        &division.ai_summary,
    ) {
        (Some(summary), kind, _) if kind != Some(SummaryKind::Transcript) => {
            let text = plain_text(summary);
            (!text.is_empty()).then(|| (first_sentence(&text).to_string(), false))
        }
        (_, _, Some(ai)) => Some((first_sentence(&ai.text).to_string(), true)),
        _ => None,
    }
}

/// The outcomes of a series as a row of marks, oldest first: filled for
/// carried, hollow for negatived, so the sequence reads without colour.
fn outcome_strip(divisions: &[&Division]) -> String {
    let label: Vec<&str> = divisions.iter().map(|d| result_word(d.result)).collect();
    let marks: String = divisions
        .iter()
        .map(|d| match d.result {
            DivisionResult::Passed => "<i class=\"carried\"></i>",
            DivisionResult::Rejected => "<i class=\"negatived\"></i>",
        })
        .collect();
    format!(
        "<span class=\"outcome-strip\" role=\"img\" aria-label=\"In order: {}\">{marks}</span>",
        esc_attr(&label.join(", "))
    )
}

/// Month divider for a dated index: mono-caps month, hairline, count. The
/// month is a heading, so a long index can be walked a month at a time, and
/// its id is the target the month strip links to. The noun names what is
/// being counted ("division", "bill"); it is pluralised by appending an s.
pub fn ledger_month(month: &str, count: usize, noun: &str) -> String {
    format!(
        "<li class=\"ledger-month\" id=\"m-{key}\" data-month=\"{key}\"><h2 class=\"m\">{label}</h2><span class=\"rule\" aria-hidden=\"true\"></span><span class=\"n\">{count} {noun}{plural}</span></li>",
        key = esc_attr(month),
        label = esc(&month_label(month)),
        noun = esc(noun),
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
    match parse_month(month) {
        Some((year, n)) => format!("{} {year}", MONTH_NAMES[n - 1]),
        None => month.to_string(),
    }
}

/// "2026-08" -> "Aug 2026", for the month strip, where sixteen full names
/// would run to several lines on a phone. Unparseable keys pass through.
pub fn month_short(month: &str) -> String {
    match parse_month(month) {
        Some((year, n)) => format!("{} {year}", &MONTH_NAMES[n - 1][..3]),
        None => month.to_string(),
    }
}

fn parse_month(month: &str) -> Option<(&str, usize)> {
    let (year, m) = month.split_once('-')?;
    let n = m.parse::<usize>().ok()?;
    (1..=12).contains(&n).then_some((year, n))
}

/// A strip of links to the month dividers of a dated index, newest first, so
/// a reader can reach March without scrolling past every row since. Each
/// link counts what its month holds; the filter hides and recounts them with
/// the dividers. The noun is spelt out for a screen reader only, since the
/// strip sits right under the filter that names what is listed.
pub fn month_jump(months: &[(&str, usize)], noun: &str) -> String {
    let mut out =
        String::from("<nav class=\"month-jump\" id=\"month-jump\" aria-label=\"Jump to month\">");
    for (month, count) in months {
        out.push_str(&format!(
            "<a href=\"#m-{key}\" data-month=\"{key}\">{label} <span class=\"n\">{count}</span><span class=\"visually-hidden\"> {noun}{plural}</span></a>",
            key = esc_attr(month),
            label = esc(&month_short(month)),
            noun = esc(noun),
            plural = if *count == 1 { "" } else { "s" },
        ));
    }
    out.push_str("</nav>");
    out
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

/// The key to the progress dots: each stage beside the dots a bill at that
/// stage shows, filled as far as it has got. The plain spaces between entries
/// only keep copied text apart; the layout spaces them with a gap.
pub const BILL_DOTS_LEGEND: &str = "<p class=\"dots-legend\"><span><span class=\"bill-dots\" aria-hidden=\"true\"><i class=\"on\"></i><i class=\"off\"></i><i class=\"off\"></i><i class=\"off\"></i></span>Introduced</span> <span><span class=\"bill-dots\" aria-hidden=\"true\"><i class=\"on\"></i><i class=\"on\"></i><i class=\"off\"></i><i class=\"off\"></i></span>Passed 1st house</span> <span><span class=\"bill-dots\" aria-hidden=\"true\"><i class=\"on\"></i><i class=\"on\"></i><i class=\"on\"></i><i class=\"off\"></i></span>Passed 2nd house</span> <span><span class=\"bill-dots\" aria-hidden=\"true\"><i class=\"on\"></i><i class=\"on\"></i><i class=\"on\"></i><i class=\"on\"></i></span>Assent</span></p>";

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
        "<span class=\"bill-dots\" role=\"img\" aria-label=\"Stage {stage} of 4: {reached}\" title=\"Stage {stage} of 4: {reached}\">"
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
            // A seat abolished at a redistribution leaves the bundle behind.
            .unwrap_or_else(|| crate::data::title_from_slug(person.electorate.as_deref())),
    };
    let party = data.party_by_slug(&person.group_slug);
    let sub = match party {
        Some(party) => format!("{seat} · {}", party.code.as_deref().unwrap_or(&party.name)),
        None => seat,
    };
    format!(
        "<a class=\"person-card\" href=\"/people/{slug}/\">{avatar}<span class=\"name\">{name}</span> <span class=\"sub\">{sub}</span></a>",
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
    // The label above repeats the majority in words, so the figure over the
    // tick is for the eye only.
    if total > 0 {
        out.push_str(&format!(
            "<span class=\"tick-label\" style=\"left:{tick}%\" aria-hidden=\"true\">{majority}</span>"
        ));
    }
    // One picture with one text alternative. The segments are not links: a
    // one-seat party is a few pixels wide, too narrow to tap or to show a
    // focus ring, and the key underneath links every party anyway.
    out.push_str(&format!(
        "<div class=\"bar\" role=\"img\" aria-label=\"{}\">",
        esc_attr(&format!("{chamber} composition: {aria}")),
    ));
    for (party, seats) in &rows {
        out.push_str(&format!(
            "<span style=\"width:{width}%;background:{colour}\" title=\"{title}\"></span>",
            width = js_float(*seats as f64 / total.max(1) as f64 * 100.0),
            colour = party.colour.as_deref().unwrap_or(GROUP_FALLBACK_COLOUR),
            title = esc_attr(&format!("{}: {seats}", party.name)),
        ));
    }
    out.push_str("</div>");
    if total > 0 {
        out.push_str(&format!(
            "<span class=\"tick\" style=\"left:{tick}%\" aria-hidden=\"true\"></span>"
        ));
    }
    // The key is a row of flex items spaced by a gap, so no separator can be
    // left dangling at the start of a wrapped line. The plain space between
    // links only keeps copied or extracted text apart; the layout drops it.
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
            .join(" "),
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
    fn short_month_labels_keep_the_year() {
        assert_eq!(month_short("2026-09"), "Sep 2026");
        assert_eq!(month_short("2025-05"), "May 2025");
        assert_eq!(month_short("2026-13"), "2026-13");
        assert_eq!(month_short("undated"), "undated");
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
        let row = ledger_month("2026-07", 1, "division");
        assert!(
            row.starts_with("<li class=\"ledger-month\" id=\"m-2026-07\" data-month=\"2026-07\">")
        );
        assert!(row.contains("July 2026"));
        assert!(row.contains("1 division<"));
        assert!(ledger_month("2026-07", 5, "division").contains("5 divisions<"));
        assert!(ledger_month("2026-07", 5, "bill").contains("5 bills<"));
    }
}
