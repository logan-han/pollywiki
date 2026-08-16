use crate::data::{division_key, format_date, js_float, SiteData};
use crate::html::{esc, esc_attr};
use pollywiki_schema::{Division, House, Person};

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

pub fn ledger_row(division: &Division) -> String {
    let href = format!("/divisions/{}/{}/", division.house, division_key(division));
    let chamber = match division.house {
        House::Senate => "Senate",
        House::Representatives => "House",
    };
    format!(
        "<li data-house=\"{house}\"><span class=\"when\">{when}</span><span class=\"what\"><a href=\"{href}\">{name}</a></span><span class=\"tally\">{chamber} {ayes}\u{2013}{noes}{bar}</span></li>",
        house = division.house,
        when = esc(&format_date(&division.date)),
        name = esc(&division.name),
        ayes = division.ayes,
        noes = division.noes,
        bar = vote_bar(division.ayes, division.noes, false),
    )
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
    let mut out = format!(
        "<div class=\"seat-bar\"><div class=\"label\">{chamber} · {total} seats</div><div class=\"bar\" role=\"img\" aria-label=\"{}\">",
        esc_attr(&format!("{chamber} composition: {aria}")),
    );
    for (party, seats) in &rows {
        out.push_str(&format!(
            "<span style=\"width:{width}%;background:{colour}\" title=\"{title}\"></span>",
            width = js_float(*seats as f64 / total.max(1) as f64 * 100.0),
            colour = party.colour.as_deref().unwrap_or("#6e7b74"),
            title = esc_attr(&format!("{}: {seats}", party.name)),
        ));
    }
    out.push_str("</div><div class=\"key\">");
    out.push_str(&esc(&rows
        .iter()
        .map(|(p, seats)| format!("{} {seats}", p.code.as_deref().unwrap_or(&p.name)))
        .collect::<Vec<_>>()
        .join(" · ")));
    out.push_str("</div></div>");
    out
}
