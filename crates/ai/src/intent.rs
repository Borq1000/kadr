//! Local natural-language intent recognition (Russian + English).
//!
//! AI UX does not mean every sentence goes to an LLM. Common, unambiguous
//! requests are recognised here deterministically, for $0, offline, in
//! microseconds. Only what this parser cannot understand is routed to a
//! (budget- and privacy-gated) model.

use kadr_core::Time;

#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    /// "Удали паузы длиннее N секунд" / "remove pauses longer than N s".
    RemovePauses { min: Time },
    /// "Разрежь здесь" / "split here".
    SplitAtPlayhead,
    /// "Удали последние N секунд".
    DeleteLast { duration: Time },
    /// "Удали первые N секунд".
    DeleteFirst { duration: Time },
    /// "Верни" / "отмени" / "undo".
    Undo,
    Redo,
    /// "Поставь маркер [название]".
    AddMarker { name: String },
    /// Not recognised locally.
    Unknown,
}

/// Default when "remove pauses" has no number.
pub const DEFAULT_PAUSE: Time = Time::from_secs(2);

fn normalize(s: &str) -> String {
    s.to_lowercase().replace('ё', "е")
}

/// Russian number words in the cases they appear after "длиннее", "последние"…
fn word_number(w: &str) -> Option<f64> {
    Some(match w {
        "ноль" | "нуля" | "zero" => 0.0,
        "пол" | "половину" | "половины" | "полсекунды" | "half" => 0.5,
        "один" | "одна" | "одну" | "одной" | "одного" | "секунду" | "секунды" | "one" | "a" => 1.0,
        "полторы" | "полутора" | "полтора" => 1.5,
        "два" | "две" | "двух" | "two" => 2.0,
        "три" | "трех" | "three" => 3.0,
        "четыре" | "четырех" | "four" => 4.0,
        "пять" | "пяти" | "five" => 5.0,
        "шесть" | "шести" | "six" => 6.0,
        "семь" | "семи" | "seven" => 7.0,
        "восемь" | "восьми" | "eight" => 8.0,
        "девять" | "девяти" | "nine" => 9.0,
        "десять" | "десяти" | "ten" => 10.0,
        "пятнадцать" | "пятнадцати" | "fifteen" => 15.0,
        "двадцать" | "двадцати" | "twenty" => 20.0,
        "тридцать" | "тридцати" | "thirty" => 30.0,
        _ => return None,
    })
}

fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '.' || c == ','))
        .map(|t| t.trim_matches(|c| c == '.' || c == ','))
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Unit multiplier (seconds) if `w` is a time unit.
fn unit(w: &str) -> Option<f64> {
    if w.starts_with("миллисек") || w == "мс" || w == "ms" || w.starts_with("millisec") {
        Some(0.001)
    } else if w.starts_with("сек") || w == "с" || w == "s" || w.starts_with("sec") || w == "secs" {
        Some(1.0)
    } else if w.starts_with("мин") || w == "m" || w.starts_with("min") {
        Some(60.0)
    } else {
        None
    }
}

/// Finds a duration like "2", "2.5 с", "двух секунд", "полторы минуты", "3s".
fn find_duration(toks: &[String]) -> Option<Time> {
    for (i, t) in toks.iter().enumerate() {
        // "3s", "2.5сек", "500мс"
        let split = t.find(|c: char| c.is_alphabetic()).unwrap_or(t.len());
        let (num, suffix) = t.split_at(split);
        let n = if !num.is_empty() {
            num.replace(',', ".").parse::<f64>().ok()
        } else {
            word_number(t)
        };
        let Some(n) = n else { continue };
        let mult = if !suffix.is_empty() && !num.is_empty() {
            unit(suffix).unwrap_or(1.0)
        } else if word_number(t).is_some() && unit(t).is_some() {
            // The token itself is "секунду"/"секунды" = 1 second.
            1.0
        } else {
            toks.get(i + 1).and_then(|u| unit(u)).unwrap_or(1.0)
        };
        let secs = n * mult;
        if secs.is_finite() && secs >= 0.0 && secs < 24.0 * 3600.0 {
            return Some(Time::from_secs_f64(secs));
        }
    }
    None
}

fn has(toks: &[String], stems: &[&str]) -> bool {
    toks.iter().any(|t| stems.iter().any(|s| t.starts_with(s)))
}

pub fn parse(input: &str) -> Intent {
    let text = normalize(input);
    let toks = tokens(&text);
    if toks.is_empty() {
        return Intent::Unknown;
    }
    let delete = has(&toks, &["удал", "убер", "убра", "вырез", "выреж", "remove", "delete", "cut", "trim", "strip"]);
    let pauses = has(&toks, &["пауз", "тишин", "молчан", "silence", "pause", "gap", "dead"]);

    if pauses && (delete || has(&toks, &["без"])) {
        let min = find_duration(&toks).unwrap_or(DEFAULT_PAUSE);
        return Intent::RemovePauses { min };
    }
    if delete && has(&toks, &["последн", "last"]) {
        if let Some(d) = find_duration(&toks) {
            return Intent::DeleteLast { duration: d };
        }
    }
    if delete && has(&toks, &["перв", "first"]) {
        if let Some(d) = find_duration(&toks) {
            return Intent::DeleteFirst { duration: d };
        }
    }
    if has(&toks, &["разреж", "разрез", "режь", "split", "раздел"]) || text.trim() == "cut here" {
        return Intent::SplitAtPlayhead;
    }
    if has(&toks, &["маркер", "marker", "метк"]) {
        let skip = ["поставь", "поставить", "добавь", "добавить", "маркер", "метку", "add", "marker", "a", "здесь", "тут", "here"];
        let name: Vec<&str> = input
            .split_whitespace()
            .filter(|w| !skip.contains(&normalize(w).trim_matches(|c: char| !c.is_alphanumeric())))
            .collect();
        return Intent::AddMarker { name: name.join(" ") };
    }
    let first = toks[0].as_str();
    if toks.len() <= 3
        && (["верни", "вернуть", "отмени", "отменить", "назад", "undo"].contains(&first) || text.contains("верни как было"))
    {
        return Intent::Undo;
    }
    if toks.len() <= 3 && ["повтори", "повторить", "redo"].contains(&first) {
        return Intent::Redo;
    }
    Intent::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pauses(s: &str) -> Option<i64> {
        match parse(s) {
            Intent::RemovePauses { min } => Some(min.as_millis()),
            _ => None,
        }
    }

    #[test]
    fn remove_pauses_variants() {
        assert_eq!(pauses("Удали паузы длиннее двух секунд."), Some(2000));
        assert_eq!(pauses("удалить паузы длиннее 3 секунд"), Some(3000));
        assert_eq!(pauses("Убери тишину больше 1,5 с"), Some(1500));
        assert_eq!(pauses("вырежи паузы дольше полутора секунд"), Some(1500));
        assert_eq!(pauses("удали паузы длиннее секунды"), Some(1000));
        assert_eq!(pauses("удали паузы длиннее 500 мс"), Some(500));
        assert_eq!(pauses("Удали паузы"), Some(2000));
        assert_eq!(pauses("remove pauses longer than 2.5s"), Some(2500));
        assert_eq!(pauses("remove silences longer than two seconds"), Some(2000));
        assert_eq!(pauses("сделай без пауз длиннее пяти секунд"), Some(5000));
    }

    #[test]
    fn other_intents() {
        assert_eq!(parse("Разрежь здесь."), Intent::SplitAtPlayhead);
        assert_eq!(parse("split here"), Intent::SplitAtPlayhead);
        assert_eq!(parse("Удали последние пять секунд."), Intent::DeleteLast { duration: Time::from_secs(5) });
        assert_eq!(parse("Последние десять секунд удали."), Intent::DeleteLast { duration: Time::from_secs(10) });
        assert_eq!(parse("удали первые 2 секунды"), Intent::DeleteFirst { duration: Time::from_secs(2) });
        assert_eq!(parse("Верни."), Intent::Undo);
        assert_eq!(parse("отмени"), Intent::Undo);
        assert_eq!(parse("Поставь маркер Второй номер"), Intent::AddMarker { name: "Второй номер".into() });
    }

    #[test]
    fn creative_requests_go_to_llm() {
        assert_eq!(parse("Оставь только лучшие моменты этого выступления."), Intent::Unknown);
        assert_eq!(parse("Найди момент, где начинается второй номер."), Intent::Unknown);
        assert_eq!(parse("Сделай предварительную нарезку концерта."), Intent::Unknown);
    }
}
