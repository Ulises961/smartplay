//! Text normalization: the comparison keys everything else matches on.
//!
//! Pure functions. They derive keys from the strings a server holds; they never change those strings.

use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

/// Words that join two names. `Florence + The Machine`, `Florence and the Machine` and
/// `Patricio Rey y sus Redonditos` all fold to the same joiner.
const CONJUNCTIONS: [&str; 4] = ["and", "y", "et", "und"];

/// Words that carry no identity when two names are compared.
const STOPWORDS: [&str; 15] = [
    "the", "a", "an", "los", "las", "el", "la", "les", "le", "de", "del", "of", "sus", "su", "&",
];

/// Lowercase, without accents or punctuation, with every conjunction written `&`.
///
/// `Último Bondi, a Finisterre!` becomes `ultimo bondi a finisterre`.
pub fn fold(text: &str) -> String {
    let unaccented: String = text.nfd().filter(|c| !is_combining_mark(*c)).collect();
    let spaced: String = unaccented
        .to_lowercase()
        .chars()
        .map(|c| match c {
            '&' | '+' => " & ".to_string(),
            c if c.is_alphanumeric() => c.to_string(),
            _ => " ".to_string(),
        })
        .collect();
    let words: Vec<&str> = spaced.split_whitespace().collect();
    let many = words.len() > 1;
    words
        .iter()
        .map(|w| {
            if many && CONJUNCTIONS.contains(w) {
                "&"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The words of a folded name that identify it: no articles, joiners or small prepositions.
/// A name made only of such words (`The The`) keeps them.
/// The title of a work, folded: without a track number the file's tag carries in front (`04 Siguiendo la Luna`) and without
/// the part after ` - ` or in brackets, so `Tangled - Demo`, `Maps - Live In Manchester` and `Magic Dance (Remix)` are
/// `tangled`, `maps` and `magic dance`. What tells the versions of one song from each other, and what lets a title of the
/// library be found in a list another service keeps.
pub fn work_title(title: &str) -> String {
    let digits = title.bytes().take_while(u8::is_ascii_digit).count();
    let title = match title[digits..].strip_prefix(' ') {
        Some(rest) if (1..=3).contains(&digits) && !rest.trim().is_empty() => rest.trim_start(),
        _ => title,
    };
    let end = [" - ", " (", " ["]
        .iter()
        .filter_map(|cut| title.find(cut))
        .min()
        .unwrap_or(title.len());
    fold(&title[..end])
}

pub fn content_tokens(folded: &str) -> Vec<String> {
    let all: Vec<&str> = folded.split(' ').filter(|w| !w.is_empty()).collect();
    let content: Vec<String> = all
        .iter()
        .filter(|w| !STOPWORDS.contains(w))
        .map(|w| w.to_string())
        .collect();
    if content.is_empty() {
        all.iter()
            .filter(|w| **w != "&")
            .map(|w| w.to_string())
            .collect()
    } else {
        content
    }
}

/// An artist credit split into the main artist and the guests.
#[derive(Debug, PartialEq, Eq)]
pub struct Credit {
    pub main: String,
    pub guests: Vec<String>,
}

/// Splits `Adele feat. Drake`, `Adele (ft. Drake & Jay-Z)` and `Adele featuring Drake`.
///
/// Only `feat.`, `feat`, `ft.`, `ft` and `featuring` count. `&`, `and`, `with` and `vs` are not split: they
/// are as often part of a name (`Simon & Garfunkel`) as a credit.
pub fn split_credit(artist: &str) -> Credit {
    const MARKERS: [&str; 5] = ["featuring", "feat.", "feat", "ft.", "ft"];
    let bytes = artist.as_bytes();
    for (i, _) in artist.char_indices() {
        let opens = i > 0 && matches!(bytes[i - 1], b' ' | b'(' | b'[');
        if !opens {
            continue;
        }
        for marker in MARKERS {
            let end = i + marker.len();
            let matches_marker = bytes.len() > end
                && artist.is_char_boundary(end)
                && bytes[i..end].eq_ignore_ascii_case(marker.as_bytes())
                && bytes[end] == b' ';
            if matches_marker {
                let main = artist[..i].trim_end_matches(['(', '[', ' ']).to_string();
                let rest = artist[end..].trim().trim_end_matches([')', ']']);
                let guests = rest
                    .replace(" & ", ",")
                    .replace(" and ", ",")
                    .split(',')
                    .map(|g| g.trim().to_string())
                    .filter(|g| !g.is_empty())
                    .collect();
                return Credit { main, guests };
            }
        }
    }
    Credit {
        main: artist.trim().to_string(),
        guests: vec![],
    }
}

/// Removes one trailing remaster note: `Song (Remastered 2011)`, `Song - 2011 Remaster`.
/// Other decorations (`Live`, `Remix`, `Radio Edit`) are different recordings and stay.
pub fn strip_remaster(title: &str) -> String {
    let start = [title.rfind('('), title.rfind('['), title.rfind(" - ")]
        .into_iter()
        .flatten()
        .max();
    match start {
        Some(i) if title[i..].to_lowercase().contains("remaster") => {
            title[..i].trim_end().to_string()
        }
        _ => title.to_string(),
    }
}

/// Edit distance between two words, in characters.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(above + 1)
                .min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_ignores_case_accents_and_punctuation() {
        assert_eq!(
            fold("Último Bondi, a Finisterre!"),
            "ultimo bondi a finisterre"
        );
        assert_eq!(fold("  Beyoncé  "), "beyonce");
    }

    #[test]
    fn fold_writes_every_conjunction_the_same_way() {
        let wanted = "florence & the machine";
        assert_eq!(fold("Florence + The Machine"), wanted);
        assert_eq!(fold("Florence and the Machine"), wanted);
        assert_eq!(fold("Florence & The Machine"), wanted);
        assert_eq!(
            fold("Patricio Rey y sus Redonditos"),
            fold("Patricio Rey & Sus Redonditos")
        );
    }

    #[test]
    fn fold_keeps_a_lone_conjunction_word() {
        assert_eq!(fold("Y"), "y");
    }

    #[test]
    fn content_tokens_drop_articles_and_joiners() {
        assert_eq!(
            content_tokens(&fold("Los Redonditos de Ricota")),
            ["redonditos", "ricota"]
        );
        assert_eq!(content_tokens(&fold("The The")), ["the", "the"]);
    }

    #[test]
    fn credits_are_split_in_every_common_spelling() {
        let want = Credit {
            main: "Adele".into(),
            guests: vec!["Drake".into()],
        };
        assert_eq!(split_credit("Adele feat. Drake"), want);
        assert_eq!(split_credit("Adele (feat. Drake)"), want);
        assert_eq!(split_credit("Adele ft Drake"), want);
        assert_eq!(split_credit("Adele Featuring Drake"), want);
        assert_eq!(split_credit("Adele [ft. Drake]"), want);
    }

    #[test]
    fn credits_with_several_guests() {
        let credit = split_credit("Adele (feat. Drake & Jay-Z, Nas)");
        assert_eq!(credit.guests, ["Drake", "Jay-Z", "Nas"]);
    }

    #[test]
    fn names_that_contain_a_joiner_are_not_split() {
        for name in [
            "Simon & Garfunkel",
            "Florence + The Machine",
            "Feather",
            "Soft Cell",
            "Craft",
        ] {
            assert_eq!(
                split_credit(name),
                Credit {
                    main: name.into(),
                    guests: vec![]
                },
                "{name}"
            );
        }
    }

    #[test]
    fn only_remaster_notes_are_stripped() {
        assert_eq!(strip_remaster("Song (Remastered 2011)"), "Song");
        assert_eq!(strip_remaster("Song - 2011 Remaster"), "Song");
        assert_eq!(
            strip_remaster("Song (Live) (Remastered 2011)"),
            "Song (Live)"
        );
        assert_eq!(strip_remaster("Song (Live)"), "Song (Live)");
        assert_eq!(strip_remaster("Song (Radio Edit)"), "Song (Radio Edit)");
        assert_eq!(strip_remaster("Remaster"), "Remaster");
    }

    #[test]
    fn edit_distance_counts_characters() {
        assert_eq!(edit_distance("redondos", "redonditos"), 2);
        assert_eq!(edit_distance("kravitz", "kraviz"), 1);
        assert_eq!(edit_distance("same", "same"), 0);
        assert_eq!(edit_distance("", "abc"), 3);
    }
}
