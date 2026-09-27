//! Shared domain language identities; source-specific wire validation belongs to each producer.
use crate::api::MediaDomain;

// Pinned catalog facts: TV and movies diverge after Czech. Never cast across domains.
const LANGUAGES: &[(&str, Option<i32>, Option<i32>)] = &[
    ("English", Some(1), Some(1)),
    ("French", Some(2), Some(2)),
    ("Spanish", Some(3), Some(3)),
    ("German", Some(4), Some(4)),
    ("Italian", Some(5), Some(5)),
    ("Danish", Some(6), Some(6)),
    ("Dutch", Some(7), Some(7)),
    ("Japanese", Some(8), Some(8)),
    ("Icelandic", Some(9), Some(9)),
    ("Chinese", Some(10), Some(10)),
    ("Russian", Some(11), Some(11)),
    ("Polish", Some(12), Some(12)),
    ("Vietnamese", Some(13), Some(13)),
    ("Swedish", Some(14), Some(14)),
    ("Norwegian", Some(15), Some(15)),
    ("Finnish", Some(16), Some(16)),
    ("Turkish", Some(17), Some(17)),
    ("Portuguese", Some(18), Some(18)),
    ("Flemish", Some(19), Some(19)),
    ("Greek", Some(20), Some(20)),
    ("Korean", Some(21), Some(21)),
    ("Hungarian", Some(22), Some(22)),
    ("Hebrew", Some(23), Some(23)),
    ("Lithuanian", Some(24), Some(24)),
    ("Czech", Some(25), Some(25)),
    ("Arabic", Some(26), Some(31)),
    ("Hindi", Some(27), Some(26)),
    ("Bulgarian", Some(28), Some(29)),
    ("Malayalam", Some(29), Some(48)),
    ("Ukrainian", Some(30), Some(32)),
    ("Slovak", Some(31), Some(35)),
    ("Thai", Some(32), Some(28)),
    ("Portuguese (Brazil)", Some(33), Some(30)),
    ("Spanish (Latino)", Some(34), Some(37)),
    ("Romanian", Some(35), Some(27)),
    ("Latvian", Some(36), Some(36)),
    ("Persian", Some(37), Some(33)),
    ("Catalan", Some(38), Some(38)),
    ("Croatian", Some(39), Some(39)),
    ("Serbian", Some(40), Some(40)),
    ("Bosnian", Some(41), Some(41)),
    ("Estonian", Some(42), Some(42)),
    ("Tamil", Some(43), Some(43)),
    ("Indonesian", Some(44), Some(44)),
    ("Macedonian", Some(45), Some(46)),
    ("Slovenian", Some(46), Some(47)),
    ("Azerbaijani", Some(47), None),
    ("Uzbek", Some(48), None),
    ("Malay", Some(49), None),
    ("Urdu", Some(50), Some(54)),
    ("Romansh", Some(51), Some(55)),
    ("Georgian", Some(52), Some(57)),
    ("Bengali", None, Some(34)),
    ("Telugu", None, Some(45)),
    ("Kannada", None, Some(49)),
    ("Albanian", None, Some(50)),
    ("Afrikaans", None, Some(51)),
    ("Marathi", None, Some(52)),
    ("Tagalog", None, Some(53)),
    ("Mongolian", None, Some(56)),
    ("Unknown", Some(0), Some(0)),
    ("Original", Some(-2), Some(-2)),
    ("Any", None, Some(-1)),
];

pub fn language_id(media: MediaDomain, name: &str) -> Option<i32> {
    let name = match name {
        value
            if value.eq_ignore_ascii_case("PortugueseBR")
                || value.eq_ignore_ascii_case("PortugueseBrazil") =>
        {
            "Portuguese (Brazil)"
        }
        value if value.eq_ignore_ascii_case("SpanishLatino") => "Spanish (Latino)",
        other => other,
    };
    LANGUAGES
        .iter()
        .find(|(label, ..)| label.eq_ignore_ascii_case(name))
        .and_then(|(_, tv, movies)| match media {
            MediaDomain::Tv => *tv,
            MediaDomain::Movies => *movies,
        })
}

pub fn iso_language_id(media: MediaDomain, code: &str) -> Option<i32> {
    if code.len() > 32 {
        return None;
    }
    let lower = code.to_ascii_lowercase();
    let (code, region) = lower
        .split_once('-')
        .map_or((lower.as_str(), None), |(c, r)| (c, Some(r)));
    if region.is_some_and(|r| r.len() != 2 || !r.bytes().all(|c| c.is_ascii_alphabetic())) {
        return None;
    }
    if code == "pt" && region == Some("br") {
        return language_id(media, "Portuguese (Brazil)");
    }
    if code == "es" && region == Some("mx") {
        return language_id(media, "Spanish (Latino)");
    }
    if let Some(region) = region {
        if matches!(code, "fr" | "de" | "pt" | "zh" | "th")
            && region
                != match code {
                    "zh" => "cn",
                    other => other,
                }
        {
            return None;
        }
    }
    let name = match code {
        "en" | "eng" => Some("English"),
        "fr" | "fra" | "fre" => Some("French"),
        "es" | "spa" => Some("Spanish"),
        "de" | "deu" | "ger" | "gsw" => Some("German"),
        "it" | "ita" => Some("Italian"),
        "da" | "dan" => Some("Danish"),
        "nl" | "nld" | "dut" => Some("Dutch"),
        "ja" | "jpn" => Some("Japanese"),
        "is" | "isl" | "ice" => Some("Icelandic"),
        "zh" | "zho" | "chi" | "yue" | "zhtw" => Some("Chinese"),
        "ru" | "rus" => Some("Russian"),
        "pl" | "pol" => Some("Polish"),
        "vi" | "vie" => Some("Vietnamese"),
        "sv" | "swe" => Some("Swedish"),
        "no" | "nor" | "nb" | "nob" => Some("Norwegian"),
        "fi" | "fin" => Some("Finnish"),
        "tr" | "tur" => Some("Turkish"),
        "pt" | "por" => Some("Portuguese"),
        "el" | "ell" | "gre" => Some("Greek"),
        "ko" | "kor" => Some("Korean"),
        "hu" | "hun" => Some("Hungarian"),
        "he" | "heb" => Some("Hebrew"),
        "lt" | "lit" => Some("Lithuanian"),
        "cs" | "ces" | "cze" => Some("Czech"),
        "ar" | "ara" => Some("Arabic"),
        "hi" | "hin" => Some("Hindi"),
        "bg" | "bul" => Some("Bulgarian"),
        "ml" | "mal" => Some("Malayalam"),
        "uk" | "ukr" => Some("Ukrainian"),
        "sk" | "slk" | "slo" => Some("Slovak"),
        "th" | "tha" => Some("Thai"),
        "ro" | "ron" | "rum" => Some("Romanian"),
        "lv" | "lav" => Some("Latvian"),
        "fa" | "fas" | "per" => Some("Persian"),
        "ca" | "cat" => Some("Catalan"),
        "hr" | "hrv" => Some("Croatian"),
        "sr" | "srp" => Some("Serbian"),
        "bs" | "bos" => Some("Bosnian"),
        "et" | "est" => Some("Estonian"),
        "ta" | "tam" => Some("Tamil"),
        "id" | "ind" => Some("Indonesian"),
        "mk" | "mkd" | "mac" => Some("Macedonian"),
        "sl" | "slv" => Some("Slovenian"),
        "az" | "aze" => Some("Azerbaijani"),
        "uz" | "uzb" => Some("Uzbek"),
        "ms" | "msa" | "may" => Some("Malay"),
        "ur" | "urd" => Some("Urdu"),
        "rm" | "roh" => Some("Romansh"),
        "ka" | "kat" | "geo" => Some("Georgian"),
        "bn" | "ben" => Some("Bengali"),
        "te" | "tel" => Some("Telugu"),
        "kn" | "kan" => Some("Kannada"),
        "sq" | "sqi" | "alb" => Some("Albanian"),
        "af" | "afr" => Some("Afrikaans"),
        "mr" | "mar" => Some("Marathi"),
        "tl" | "tgl" => Some("Tagalog"),
        "mn" | "mon" => Some("Mongolian"),
        "nn" | "nno" => {
            if matches!(media, MediaDomain::Movies) {
                Some("Norwegian")
            } else {
                None
            }
        }
        _ => None,
    };
    name.and_then(|name| language_id(media, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_and_iso_codes_preserve_domain_identity() {
        for (name, tv, movies) in LANGUAGES {
            assert_eq!(language_id(MediaDomain::Tv, name), *tv);
            assert_eq!(
                language_id(MediaDomain::Movies, &name.to_ascii_lowercase()),
                *movies
            );
        }
        for (code, tv, movies) in [
            ("ar", Some(26), Some(31)),
            ("ARA", Some(26), Some(31)),
            ("hi", Some(27), Some(26)),
            ("pt-BR", Some(33), Some(30)),
            ("es-MX", Some(34), Some(37)),
            ("ISL", Some(9), Some(9)),
            ("ger", Some(4), Some(4)),
            ("az", Some(47), None),
            ("bn", None, Some(34)),
            ("nn", None, Some(15)),
            ("zz", None, None),
            ("English", None, None),
        ] {
            assert_eq!(iso_language_id(MediaDomain::Tv, code), tv, "{code}");
            assert_eq!(iso_language_id(MediaDomain::Movies, code), movies, "{code}");
        }
    }
}
