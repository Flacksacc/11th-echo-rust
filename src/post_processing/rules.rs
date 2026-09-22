//! Conservative English written-form rules. Recognized compound expressions
//! are consumed even when disabled, so cardinal conversion cannot partially
//! rewrite a disabled money/date/identifier category.
use crate::settings::PostProcessingSettings;
use regex::Regex;
use std::sync::LazyLock;
use text2num::{replace_numbers_in_text, Language};

#[derive(Clone, Debug)]
pub(super) struct Atom {
    pub text: String,
    pub protected: bool,
    pub case_protected: bool,
    /// Literal content without source sentence marks. Never trim generated
    /// replacements or structured values when rebuilding punctuation.
    pub content: String,
}

impl Atom {
    fn source(text: String, protected: bool) -> Self {
        let content = if let Some(acronym) = acronym(&text) {
            acronym.to_string()
        } else {
            core(&text).into()
        };
        Self {
            text,
            protected,
            case_protected: protected,
            content,
        }
    }
    fn written(content: String, suffix: &str) -> Self {
        Self {
            text: format!("{content}{suffix}"),
            content,
            protected: true,
            case_protected: true,
        }
    }
    fn allow_case(mut self) -> Self {
        self.case_protected = false;
        self
    }
}

static ACRONYM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(?:[A-Z]\.){2,}|(?i:[ap]\.m\.|e\.g\.|i\.e\.|mr\.|mrs\.|ms\.|dr\.|prof\.))$")
        .unwrap()
});
fn acronym(s: &str) -> Option<&str> {
    let candidate = s.trim_matches(|c| edge_punctuation(c) && c != '.');
    ACRONYM.is_match(candidate).then_some(candidate)
}

static NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-?\d+(?:\.\d+)?(?:st|nd|rd|th)?$").unwrap());
static STRUCTURED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(?i:\S+@\S+\.\S+|(?:https?://|www\.)\S+|[\w-]+\.(?:com|org|net|io|edu|gov|co)(?:/\S*)?)|[-+$€£]?\d[\d,.:/%-]*[\w%]*|(?:[A-Z]\.){2,}|[A-Z][A-Z0-9-]+)$").unwrap()
});

pub(super) fn edge_punctuation(c: char) -> bool {
    matches!(
        c,
        '.' | ','
            | '?'
            | '!'
            | ';'
            | ':'
            | '…'
            | '。'
            | '，'
            | '？'
            | '！'
            | '“'
            | '”'
            | '"'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '—'
            | '–'
    )
}
fn core(s: &str) -> &str {
    s.trim_matches(edge_punctuation)
}
fn key(s: &str) -> String {
    core(s).to_lowercase()
}
fn trailing(s: &str) -> &str {
    &s[s.trim_end_matches(edge_punctuation).len()..]
}
fn joined(words: &[&str]) -> String {
    words.join(" ")
}

pub fn validate(settings: &PostProcessingSettings) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for line in &settings.custom_replacements {
        let Some((from, to)) = line.split_once("=>") else {
            return Err("Each replacement must use spoken => written.".into());
        };
        if from.trim().is_empty() || to.trim().is_empty() {
            return Err("Replacement forms cannot be blank.".into());
        }
        if !seen.insert(
            from.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase(),
        ) {
            return Err("Two replacements have the same spoken form.".into());
        }
        if settings
            .protected_phrases
            .iter()
            .any(|p| p.eq_ignore_ascii_case(from.trim()))
        {
            return Err("A replacement conflicts with a protected phrase.".into());
        }
    }
    Ok(())
}

fn phrase_len(words: &[&str], phrase: &str) -> Option<usize> {
    let parts: Vec<_> = phrase.split_whitespace().collect();
    (!parts.is_empty()
        && words.len() >= parts.len()
        && parts
            .iter()
            .zip(words)
            .all(|(a, b)| core(a).eq_ignore_ascii_case(core(b))))
    .then_some(parts.len())
}

fn number(words: &[&str]) -> Option<(usize, String)> {
    let mut best = None;
    for n in 1..=words.len().min(16) {
        // Do not combine two independently punctuated numeric phrases.
        if n > 1 && words[n - 2].ends_with(edge_punctuation) {
            break;
        }
        let source = words[..n]
            .iter()
            .map(|w| core(w).to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        let (sign, source) = source
            .strip_prefix("minus ")
            .or_else(|| source.strip_prefix("negative "))
            .map_or(("", source.as_str()), |s| ("-", s));
        let value = replace_numbers_in_text(source, &Language::english(), 0.0);
        if NUMBER.is_match(&value) {
            best = Some((n, format!("{sign}{value}")));
        }
    }
    best
}

fn integer(s: &str) -> Option<u32> {
    s.trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .parse()
        .ok()
}
fn money(words: &[&str]) -> Option<(usize, String)> {
    let (n, value) = number(words)?;
    if value.ends_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let unit = key(words.get(n)?);
    let symbol = match unit.as_str() {
        "dollar" | "dollars" => "$",
        "euro" | "euros" => "€",
        "pound" | "pounds" if words.get(n + 1).is_some_and(|w| key(w) == "sterling") => "£",
        "cent" | "cents" => "¢",
        _ => return None,
    };
    let mut end = n + 1 + usize::from(symbol == "£");
    if symbol == "¢" {
        return Some((end, format!("{value}¢")));
    }
    let offset = end + usize::from(words.get(end).is_some_and(|w| key(w) == "and"));
    if let Some((k, cents)) = number(words.get(offset..).unwrap_or_default()) {
        if words
            .get(offset + k)
            .is_some_and(|w| matches!(key(w).as_str(), "cent" | "cents"))
            && cents.parse::<u8>().is_ok_and(|c| c < 100)
            && !value.contains('.')
        {
            end = offset + k + 1;
            return Some((
                end,
                format!("{symbol}{value}.{:02}", cents.parse::<u8>().ok()?),
            ));
        }
    }
    Some((end, format!("{symbol}{value}")))
}

fn measurement(words: &[&str]) -> Option<(usize, String)> {
    let (n, value) = number(words)?;
    if value.ends_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let unit = match key(words.get(n)?).as_str() {
        "kilogram" | "kilograms" => "kg",
        "gram" | "grams" => "g",
        "milligram" | "milligrams" => "mg",
        "pound" | "pounds" => "lb",
        "ounce" | "ounces" => "oz",
        "meter" | "meters" | "metre" | "metres" => "m",
        "kilometer" | "kilometers" => "km",
        "centimeter" | "centimeters" => "cm",
        "millimeter" | "millimeters" => "mm",
        "foot" | "feet" => "ft",
        "inch" | "inches" => "in",
        "mile" | "miles" => "mi",
        "liter" | "liters" | "litre" | "litres" => "L",
        "milliliter" | "milliliters" => "mL",
        "percent" | "percentage" => "%",
        "degree" | "degrees" => "°",
        _ => return None,
    };
    let mut n = n + 1;
    let mut unit = unit.to_string();
    if unit == "°" {
        if let Some(w) = words.get(n) {
            match key(w).as_str() {
                "celsius" => {
                    unit.push('C');
                    n += 1;
                }
                "fahrenheit" => {
                    unit.push('F');
                    n += 1;
                }
                _ => {}
            }
        }
    }
    let gap = if unit == "%" || unit.starts_with('°') {
        ""
    } else {
        " "
    };
    Some((n, format!("{value}{gap}{unit}")))
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];
fn date(words: &[&str]) -> Option<(usize, String)> {
    let first = key(words.first()?);
    let month_index = MONTHS.iter().position(|m| *m == first);
    let (mut n, day, month) = if let Some(m) = month_index {
        let offset = 1 + usize::from(words.get(1).is_some_and(|w| key(w) == "the"));
        // Limit the day parse; a following year must not merge into the day.
        let (k, d) = number(&words[offset..words.len().min(offset + 2)])?;
        (offset + k, integer(&d)?, m)
    } else {
        let offset = usize::from(words.first().is_some_and(|w| key(w) == "the"));
        let (k, d) = number(&words[offset..])?;
        let of = offset + k;
        if words.get(of).map(|w| key(w)) != Some("of".into()) {
            return None;
        }
        let m = MONTHS
            .iter()
            .position(|m| words.get(of + 1).is_some_and(|w| *m == key(w)))?;
        (of + 2, integer(&d)?, m)
    };
    let mut year = None;
    if let Some((k, y)) = number(&words[n..]) {
        if let Some(y) = integer(&y).filter(|y| (1000..=2999).contains(y)) {
            year = Some(y);
            n += k;
        }
    }
    if year.is_none()
        && words
            .get(n)
            .is_some_and(|w| matches!(key(w).as_str(), "nineteen" | "twenty"))
    {
        if let Some((k, tail)) = number(&words[n + 1..]) {
            if let Some(tail) = integer(&tail).filter(|x| *x < 100) {
                year = Some(
                    if key(words[n]) == "nineteen" {
                        1900
                    } else {
                        2000
                    } + tail,
                );
                n += k + 1;
            }
        }
    }
    chrono::NaiveDate::from_ymd_opt(year.unwrap_or(2000) as i32, month as u32 + 1, day)?;
    let mut name = MONTHS[month].to_string();
    name[..1].make_ascii_uppercase();
    Some((
        n,
        format!(
            "{name} {day}{}",
            year.map_or(String::new(), |y| format!(", {y}"))
        ),
    ))
}

fn time(words: &[&str]) -> Option<(usize, String)> {
    // Only explicit AM/PM or o'clock; never guess that an ordinary pair of
    // numeric quantities is a clock time.
    let marker = words.iter().take(7).position(|w| {
        matches!(
            key(w).replace('.', "").as_str(),
            "am" | "pm" | "o'clock" | "oclock"
        )
    })?;
    if marker == 0 {
        return None;
    }
    let mut parsed = None;
    for split in 1..=marker {
        let (hn, h) = number(&words[..split])?;
        if hn != split {
            continue;
        }
        let h = integer(&h)?;
        let minutes = if split == marker {
            Some(0)
        } else {
            let rest = &words[split..marker];
            let rest = if rest.first().is_some_and(|w| key(w) == "oh") {
                &rest[1..]
            } else {
                rest
            };
            number(rest)
                .filter(|(k, _)| *k == rest.len())
                .and_then(|(_, m)| integer(&m))
        };
        if (1..=12).contains(&h) {
            if let Some(m) = minutes.filter(|m| *m < 60) {
                parsed = Some((h, m));
                break;
            }
        }
    }
    let (h, m) = parsed?;
    let suffix = key(words[marker]).replace('.', "");
    Some((
        marker + 1,
        format!(
            "{h}:{m:02}{}",
            if suffix == "am" {
                " AM"
            } else if suffix == "pm" {
                " PM"
            } else {
                ""
            }
        ),
    ))
}

fn digit(word: &str) -> Option<char> {
    match key(word).as_str() {
        "zero" | "oh" => Some('0'),
        "one" => Some('1'),
        "two" => Some('2'),
        "three" => Some('3'),
        "four" => Some('4'),
        "five" => Some('5'),
        "six" => Some('6'),
        "seven" => Some('7'),
        "eight" => Some('8'),
        "nine" => Some('9'),
        s if s.len() == 1 && s.as_bytes()[0].is_ascii_digit() => s.chars().next(),
        _ => None,
    }
}
fn identifier(words: &[&str]) -> Option<(usize, String)> {
    let cue = key(words.first()?);
    if !matches!(
        cue.as_str(),
        "phone" | "telephone" | "code" | "pin" | "id" | "identifier" | "serial"
    ) {
        return None;
    }
    let start = 1 + usize::from(words.get(1).is_some_and(|w| key(w) == "number"));
    let start = start + usize::from(words.get(start).is_some_and(|w| key(w) == "is"));
    let mut end = start;
    let mut value = String::new();
    while let Some(w) = words.get(end) {
        if let Some(d) = digit(w) {
            value.push(d);
        } else if core(w).chars().all(|c| c.is_ascii_digit()) && !core(w).is_empty() {
            value.push_str(core(w));
        } else if matches!(cue.as_str(), "code" | "id" | "identifier" | "serial")
            && core(w).len() == 1
            && core(w).chars().all(|c| c.is_ascii_alphabetic())
        {
            value.push_str(&core(w).to_uppercase());
        } else {
            break;
        }
        end += 1;
        if w.ends_with(edge_punctuation) {
            break;
        }
    }
    if value.len() < 2 {
        return None;
    }
    Some((end, format!("{} {value}", joined(&words[..start]))))
}

const EMAIL_TLDS: &[&str] = &[
    "com", "org", "net", "edu", "gov", "io", "co", "uk", "us", "ca", "au", "de", "fr", "jp", "dev",
    "ai", "app", "info", "biz", "me", "tv", "online", "site", "tech", "cloud",
];

fn email_separator(word: &str) -> Option<char> {
    match key(word).as_str() {
        "dot" => Some('.'),
        "underscore" => Some('_'),
        "dash" | "hyphen" => Some('-'),
        "plus" => Some('+'),
        _ => None,
    }
}

fn email_prefix(word: &str) -> bool {
    matches!(
        key(word).as_str(),
        "email" | "e-mail" | "address" | "contact" | "send" | "to" | "please" | "my" | "is"
    )
}

/// Parse a spoken address as one protected atom. Unlike web addresses, email
/// local-parts and domain labels are commonly dictated as separate words:
/// `john smith at north wind dot co dot uk` becomes
/// `johnsmith@northwind.co.uk`. At ordinary sentence positions, introductory
/// words are deliberately rejected so the main scanner can advance to the
/// actual address instead of swallowing "please email" into the local part.
fn spoken_email(words: &[&str]) -> Option<(usize, String)> {
    let at = words.iter().take(9).position(|word| key(word) == "at")?;
    if at == 0 || words[..at].iter().any(|word| email_prefix(word)) {
        return None;
    }

    let mut local = String::new();
    let mut last_was_separator = false;
    for word in &words[..at] {
        if let Some(separator) = email_separator(word) {
            if local.is_empty() || last_was_separator {
                return None;
            }
            local.push(separator);
            last_was_separator = true;
            continue;
        }
        let part = key(word);
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        local.push_str(&part);
        last_was_separator = false;
    }
    if local.is_empty() || last_was_separator {
        return None;
    }

    let mut labels = Vec::<String>::new();
    let mut label = String::new();
    let mut end = at + 1;
    let mut best = None;
    while let Some(word) = words.get(end) {
        let part = key(word);
        if part == "dot" {
            if label.is_empty() {
                break;
            }
            labels.push(std::mem::take(&mut label));
            end += 1;
            continue;
        }
        // A speech engine may have already written one domain token, for
        // example `gmail.com`, while leaving the `at` spoken.
        if labels.is_empty()
            && label.is_empty()
            && part.contains('.')
            && part.split('.').all(|label| {
                !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
        {
            let written_labels = part.split('.').map(str::to_owned).collect::<Vec<_>>();
            if written_labels.len() >= 2 && EMAIL_TLDS.contains(&written_labels.last()?.as_str()) {
                return Some((end + 1, format!("{local}@{}", written_labels.join("."))));
            }
            break;
        }
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            break;
        }
        label.push_str(&part);
        end += 1;
        if EMAIL_TLDS.contains(&label.as_str()) && !labels.is_empty() {
            best = Some((end, format!("{local}@{}.{}", labels.join("."), label)));
            // Continue only for a compound ending such as co.uk.
            if words.get(end).is_none_or(|word| key(word) != "dot") {
                break;
            }
        }
    }
    best
}

fn electronic(words: &[&str]) -> Option<(usize, String)> {
    // Consume a single address grammar, never a free-form sentence around it.
    // Names consist of words/letters linked with dot, underscore, dash or plus.
    if words.len() >= 4
        && matches!(key(words[0]).as_str(), "http" | "https")
        && key(words[1]) == "colon"
        && key(words[2]) == "slash"
        && key(words[3]) == "slash"
    {
        let (n, address) = electronic(&words[4..])?;
        if address.contains('@') {
            return None;
        }
        return Some((n + 4, format!("{}://{address}", key(words[0]))));
    }
    let mut n = 0;
    let mut out = String::new();
    let mut at = false;
    let mut dot = false;
    let mut expect_part = true;
    while n < words.len().min(32) {
        let w = key(words[n]);
        if expect_part {
            // ASR often already writes the domain, but leaves "at" spoken.
            if at && w.contains('.') && !w.contains('@') && STRUCTURED.is_match(&w) {
                out.push_str(&w);
                return Some((n + 1, out));
            }
            if w.is_empty()
                || !w.chars().all(|c| c.is_ascii_alphanumeric())
                || matches!(w.as_str(), "at" | "dot" | "underscore" | "dash" | "plus")
            {
                return None;
            }
            out.push_str(&w);
            n += 1;
            expect_part = false;
            // A spelled-out local part (j o h n) has unambiguous letter boundaries.
            if w.len() == 1 {
                while let Some(part) = words.get(n) {
                    let part = key(part);
                    if part.len() == 1 && part.chars().all(|c| c.is_ascii_alphanumeric()) {
                        out.push_str(&part);
                        n += 1;
                    } else {
                        break;
                    }
                }
            }
        } else {
            match w.as_str() {
                "dot" => {
                    out.push('.');
                    dot = true;
                }
                "at" if !at => {
                    out.push('@');
                    at = true;
                    dot = false;
                }
                "underscore" => out.push('_'),
                "dash" | "hyphen" => out.push('-'),
                "plus" if !at => out.push('+'),
                _ => break,
            }
            n += 1;
            expect_part = true;
        }
        if !expect_part
            && dot
            && matches!(
                w.as_str(),
                "com" | "org" | "net" | "edu" | "gov" | "io" | "co" | "uk" | "us" | "dev" | "ai"
            )
        {
            if words.get(n).is_some_and(|w| key(w) == "dot") {
                continue;
            }
            if !at {
                while words.get(n).is_some_and(|w| key(w) == "slash") {
                    let Some(part) = words.get(n + 1) else {
                        break;
                    };
                    let part = core(part);
                    if part.is_empty()
                        || !part
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                    {
                        break;
                    }
                    out.push('/');
                    out.push_str(part);
                    n += 2;
                }
            }
            return Some((n, out));
        }
    }
    None
}

/// Remove ASR sentence marks before number parsing, without touching internal
/// symbols in explicit protected phrases, custom spoken forms, or addresses.
pub(super) fn strip_sentence_punctuation(input: &str, s: &PostProcessingSettings) -> String {
    let words: Vec<_> = input.split_whitespace().collect();
    let mut output = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let rest = &words[i..];
        let protected = s
            .protected_phrases
            .iter()
            .filter_map(|p| phrase_len(rest, p))
            .chain(
                s.custom_replacements
                    .iter()
                    .filter_map(|r| r.split_once("=>"))
                    .filter_map(|(from, _)| phrase_len(rest, from)),
            )
            .max();
        if let Some(n) = protected {
            output.push(Atom::source(joined(&rest[..n]), true).content);
            i += n;
            continue;
        }
        if STRUCTURED.is_match(core(rest[0])) || acronym(rest[0]).is_some() {
            output.push(Atom::source(rest[0].into(), true).content);
        } else {
            output.extend(
                rest[0]
                    .split(edge_punctuation)
                    .filter(|p| !p.is_empty())
                    .map(str::to_string),
            );
        }
        i += 1;
    }
    output.join(" ")
}

pub(super) fn apply(input: &str, s: &PostProcessingSettings) -> Vec<Atom> {
    let words: Vec<_> = input.split_whitespace().collect();
    let mut result = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let rest = &words[i..];
        let protection = s
            .protected_phrases
            .iter()
            .filter_map(|p| phrase_len(rest, p))
            .chain(phrase_len(rest, "one of a kind"))
            .max();
        if let Some(n) = protection {
            result.push(Atom::source(joined(&rest[..n]), true));
            i += n;
            continue;
        }
        let replacement = s
            .custom_replacements
            .iter()
            .filter_map(|r| r.split_once("=>"))
            .filter_map(|(a, b)| phrase_len(rest, a.trim()).map(|n| (n, b.trim())))
            .max_by_key(|(n, _)| *n);
        if let Some((n, text)) = replacement {
            result.push(Atom::written(text.into(), trailing(rest[n - 1])));
            i += n;
            continue;
        }
        let structured = STRUCTURED.is_match(core(rest[0])) || acronym(rest[0]).is_some();
        let compound = spoken_email(rest)
            .or_else(|| electronic(rest))
            .map(|v| (v, s.urls_emails))
            .or_else(|| identifier(rest).map(|v| (v, s.telephone_alphanumeric)))
            .or_else(|| date(rest).map(|v| (v, s.dates)))
            .or_else(|| time(rest).map(|v| (v, s.times)))
            .or_else(|| money(rest).map(|v| (v, s.money)))
            .or_else(|| measurement(rest).map(|v| (v, s.measurements)));
        if let Some(((n, text), enabled)) = compound {
            let suffix = trailing(rest[n - 1]);
            result.push(if enabled {
                let atom = Atom::written(text, suffix);
                if matches!(
                    key(rest[0]).as_str(),
                    "phone" | "telephone" | "code" | "pin" | "id" | "identifier" | "serial"
                ) {
                    atom.allow_case()
                } else {
                    atom
                }
            } else {
                Atom::source(joined(&rest[..n]), true).allow_case()
            });
            i += n;
            continue;
        }
        // Try compound rules first: uppercase cues such as PIN and ID must
        // still work, while unrelated acronyms and addresses stay protected.
        if structured && !NUMBER.is_match(core(rest[0])) {
            result.push(Atom::source(rest[0].into(), true));
            i += 1;
            continue;
        }
        if let Some((n, value)) = number(rest) {
            let ordinal = value.ends_with(|c: char| c.is_ascii_alphabetic());
            let decimal = value.contains('.') || value.starts_with('-');
            let enabled = s.format_numbers
                && if ordinal {
                    s.ordinals
                } else if decimal {
                    s.decimals_quantities
                } else {
                    s.whole_numbers
                };
            let small = value.parse::<u32>().is_ok_and(|n| n < 13);
            result.push(if enabled && (s.prefer_digits || !small || n > 1) {
                Atom::written(value, trailing(rest[n - 1]))
            } else {
                Atom::source(joined(&rest[..n]), true).allow_case()
            });
            i += n;
            continue;
        }
        // Sentence punctuation can occur without whitespace (hello,world).
        // Split only ordinary words; structured/protected spans bypass this.
        for part in rest[0].split_inclusive(edge_punctuation) {
            if !part.is_empty() {
                result.push(Atom::source(part.into(), false));
            }
        }
        i += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn format(text: &str, s: &PostProcessingSettings) -> String {
        apply(text, s)
            .iter()
            .map(|a| a.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
    #[test]
    fn supported_categories() {
        let s = PostProcessingSettings::default();
        for (input, expected) in [
            ("three items", "3 items"),
            ("twenty first place", "21st place"),
            ("minus twenty three", "-23"),
            ("three point five", "3.5"),
            ("twelve dollars and five cents", "$12.05"),
            ("25 dollars", "$25"),
            ("twelve dollars five cents", "$12.05"),
            ("three euros", "€3"),
            ("five pounds sterling", "£5"),
            ("fifty cents", "50¢"),
            ("twenty five pounds", "25 lb"),
            ("three kilograms", "3 kg"),
            ("fifty percent", "50%"),
            ("january fifth", "January 5"),
            ("the fifth of january", "January 5"),
            ("january fifth twenty twenty six", "January 5, 2026"),
            ("three thirty pm", "3:30 PM"),
            ("nine oh five am", "9:05 AM"),
            ("phone number zero one two three four", "phone number 01234"),
            ("code a zero zero seven", "code A007"),
            ("PIN zero zero seven", "PIN 007"),
            ("JOHN at example dot com", "john@example.com"),
            ("phone number 012 345 6789", "phone number 0123456789"),
            ("j o h n at example dot com", "john@example.com"),
            (
                "john smith at north wind dot co dot uk",
                "johnsmith@northwind.co.uk",
            ),
            (
                "mary jane watson at research lab dot example dot com",
                "maryjanewatson@researchlab.example.com",
            ),
            (
                "please email john smith at north wind dot co dot uk today",
                "please email johnsmith@northwind.co.uk today",
            ),
            (
                "contact jane doe at support team dot example dot com",
                "contact janedoe@supportteam.example.com",
            ),
            ("john at gmail.com", "john@gmail.com"),
            (
                "https colon slash slash example dot com slash billing",
                "https://example.com/billing",
            ),
            (
                "email john dot smith at example dot com please",
                "email john.smith@example.com please",
            ),
            ("visit example dot com today", "visit example.com today"),
            ("one of a kind", "one of a kind"),
            (
                "https://example.com/a?b=3&c=4",
                "https://example.com/a?b=3&c=4",
            ),
        ] {
            assert_eq!(format(input, &s), expected, "{input}");
        }
    }
    #[test]
    fn categories_are_independent() {
        let s = PostProcessingSettings {
            money: false,
            dates: false,
            times: false,
            measurements: false,
            urls_emails: false,
            telephone_alphanumeric: false,
            ..Default::default()
        };
        for input in [
            "twelve dollars and five cents",
            "three kilograms",
            "january fifth",
            "three thirty pm",
            "code zero zero seven",
            "john at example dot com",
        ] {
            assert_eq!(format(input, &s), input);
        }
        let s = PostProcessingSettings {
            format_numbers: false,
            ..Default::default()
        };
        assert_eq!(
            format("three items and twelve dollars", &s),
            "three items and $12"
        );
        let s = PostProcessingSettings {
            whole_numbers: false,
            ..Default::default()
        };
        assert_eq!(
            format("three items and third place", &s),
            "three items and 3rd place"
        );
        let s = PostProcessingSettings {
            ordinals: false,
            decimals_quantities: false,
            ..Default::default()
        };
        for input in ["twenty first", "three point five", "minus twenty three"] {
            assert_eq!(format(input, &s), input);
        }
        assert_eq!(format("three items", &s), "3 items");
        let s = PostProcessingSettings {
            prefer_digits: false,
            ..Default::default()
        };
        assert_eq!(
            format("three items and twenty three boxes", &s),
            "three items and 23 boxes"
        );
    }
    #[test]
    fn protected_and_replacements_are_boundaries_not_substrings() {
        let s = PostProcessingSettings {
            protected_phrases: vec!["twenty five".into()],
            custom_replacements: vec!["acme => ACME".into()],
            ..Default::default()
        };
        assert_eq!(
            format("Twenty five acme acmeology", &s),
            "Twenty five ACME acmeology"
        );
        assert!(validate(&s).is_ok());
        let s = PostProcessingSettings {
            custom_replacements: vec!["a=>b".into(), "A=>c".into()],
            ..s
        };
        assert!(validate(&s).is_err());
    }
}
