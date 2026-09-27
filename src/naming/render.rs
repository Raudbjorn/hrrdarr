//! Clean-room naming-template renderer and parser.
//!
//! Pure functions only: no database access, no filesystem access, no clock. Callers
//! supply trusted facts (sourced from the DB) and get back either a validated
//! [`Template`] (parse time, no facts needed) or a sanitized rendered path (render
//! time, facts required). The token vocabulary, grammar and sanitization rules here
//! are an independent design; they intentionally do not reproduce any upstream
//! filename-builder algorithm.
use crate::api::MediaDomain;
use std::fmt;

/// Templates longer than this are rejected before parsing begins.
pub const MAX_TEMPLATE_BYTES: usize = 1024;
/// Each rendered path component (a single filename, or one folder segment) is
/// capped at this many bytes. The cap applies to the rendered value itself; a
/// caller that appends a file extension afterwards must budget for it separately.
pub const MAX_COMPONENT_BYTES: usize = 255;
const MAX_PAD_DIGITS: usize = 4;

/// Which naming_settings column a template came from. Determines whether a
/// literal `/` is a hard error (a single-file name) or a folder separator
/// (a multi-segment folder path), and which tokens are admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateField {
    StandardEpisode,
    DailyEpisode,
    AnimeEpisode,
    SeriesFolder,
    SeasonFolder,
    SpecialsFolder,
    StandardMovie,
    MovieFolder,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Filename,
    Folder,
}
impl TemplateField {
    fn kind(self) -> FieldKind {
        use TemplateField::*;
        match self {
            StandardEpisode | DailyEpisode | AnimeEpisode | StandardMovie => FieldKind::Filename,
            SeriesFolder | SeasonFolder | SpecialsFolder | MovieFolder => FieldKind::Folder,
        }
    }
    fn domain(self) -> MediaDomain {
        use TemplateField::*;
        match self {
            StandardEpisode | DailyEpisode | AnimeEpisode | SeriesFolder | SeasonFolder
            | SpecialsFolder => MediaDomain::Tv,
            StandardMovie | MovieFolder => MediaDomain::Movies,
        }
    }
    fn allows(self, token: &Token) -> bool {
        use TemplateField::*;
        match self {
            StandardEpisode | DailyEpisode | AnimeEpisode => matches!(
                token,
                Token::SeriesTitle
                    | Token::Season(_)
                    | Token::Episode(_)
                    | Token::EpisodeTitle
                    | Token::AirDate
                    | Token::QualityTitle
                    | Token::CustomFormats { .. }
            ),
            SeriesFolder => matches!(token, Token::SeriesTitle),
            SeasonFolder | SpecialsFolder => matches!(token, Token::SeriesTitle | Token::Season(_)),
            StandardMovie => matches!(
                token,
                Token::MovieTitle
                    | Token::ReleaseYear
                    | Token::EditionTags
                    | Token::QualityTitle
                    | Token::CustomFormats { .. }
            ),
            MovieFolder => matches!(
                token,
                Token::MovieTitle | Token::ReleaseYear | Token::EditionTags
            ),
        }
    }
    /// The `naming_settings` column name, used to build specific error messages.
    pub fn name(self) -> &'static str {
        use TemplateField::*;
        match self {
            StandardEpisode => "standard_episode_format",
            DailyEpisode => "daily_episode_format",
            AnimeEpisode => "anime_episode_format",
            SeriesFolder => "series_folder_format",
            SeasonFolder => "season_folder_format",
            SpecialsFolder => "specials_folder_format",
            StandardMovie => "standard_movie_format",
            MovieFolder => "movie_folder_format",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    SeriesTitle,
    Season(u8),
    Episode(u8),
    EpisodeTitle,
    AirDate,
    QualityTitle,
    MovieTitle,
    ReleaseYear,
    EditionTags,
    CustomFormats {
        singular: bool,
        filter: String,
        separator: String,
        case: LetterCase,
        prefix: String,
        suffix: String,
    },
}
impl Token {
    /// `None` means the token is shared by both domains.
    fn domain(&self) -> Option<MediaDomain> {
        match self {
            Token::SeriesTitle
            | Token::Season(_)
            | Token::Episode(_)
            | Token::EpisodeTitle
            | Token::AirDate => Some(MediaDomain::Tv),
            Token::MovieTitle | Token::ReleaseYear | Token::EditionTags => {
                Some(MediaDomain::Movies)
            }
            Token::QualityTitle | Token::CustomFormats { .. } => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LetterCase {
    Preserve,
    Lower,
    Upper,
}

fn parse_pad(digits: &str) -> Option<u8> {
    if digits.is_empty() || digits.len() > MAX_PAD_DIGITS || !digits.bytes().all(|b| b == b'0') {
        return None;
    }
    Some(digits.len() as u8)
}
fn parse_token(text: &str) -> Result<Token, TemplateError> {
    if text.chars().any(char::is_control) {
        return Err(TemplateError::IllegalLiteralCharacter);
    }
    // CF selectors are exact names; normalize only the token identifier.
    let (identifier, filter) = text.split_once(':').unwrap_or((text, ""));
    let start = identifier.trim_start_matches([' ', '.', '-', '_', '[', '(']);
    let end = start.trim_end_matches([' ', '.', '-', '_', ']', ')']);
    let normalized: String = end
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    if matches!(normalized.as_str(), "customformat" | "customformats")
        && end
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_'))
    {
        let suffix_text = if text.contains(':') { filter } else { start };
        let suffix_start = suffix_text
            .trim_end_matches([' ', '.', '-', '_', ']', ')'])
            .len();
        let selector = if text.contains(':') {
            &filter[..suffix_start]
        } else {
            ""
        };
        let separator = end
            .chars()
            .skip_while(|c| c.is_ascii_alphanumeric())
            .take_while(|c| matches!(c, ' ' | '.' | '-' | '_'))
            .collect::<String>();
        return Ok(Token::CustomFormats {
            singular: normalized == "customformat",
            filter: selector.to_owned(),
            separator,
            case: if end
                .chars()
                .filter(|c| c.is_alphabetic())
                .all(char::is_lowercase)
            {
                LetterCase::Lower
            } else if end
                .chars()
                .filter(|c| c.is_alphabetic())
                .all(char::is_uppercase)
            {
                LetterCase::Upper
            } else {
                LetterCase::Preserve
            },
            prefix: identifier[..identifier.len() - start.len()].to_owned(),
            suffix: suffix_text[suffix_start..].to_owned(),
        });
    }
    match text {
        "Series Title" => Ok(Token::SeriesTitle),
        "Episode Title" => Ok(Token::EpisodeTitle),
        "Air-Date" => Ok(Token::AirDate),
        "Quality Title" => Ok(Token::QualityTitle),
        "Movie Title" => Ok(Token::MovieTitle),
        "Release Year" => Ok(Token::ReleaseYear),
        "Edition Tags" => Ok(Token::EditionTags),
        _ => {
            if let Some(digits) = text.strip_prefix("season:") {
                return parse_pad(digits)
                    .map(Token::Season)
                    .ok_or_else(|| TemplateError::InvalidPadding(format!("{{{text}}}")));
            }
            if let Some(digits) = text.strip_prefix("episode:") {
                return parse_pad(digits)
                    .map(Token::Episode)
                    .ok_or_else(|| TemplateError::InvalidPadding(format!("{{{text}}}")));
            }
            Err(TemplateError::UnknownToken(format!("{{{text}}}")))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    Empty,
    TooLong,
    UnbalancedBraces,
    UnknownToken(String),
    WrongDomain(String),
    NotAllowedInField(String),
    InvalidPadding(String),
    PathSeparatorInFilename,
    EmptyPathComponent,
    IllegalLiteralCharacter,
}
impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "template must not be empty; use null to unset"),
            Self::TooLong => write!(f, "template exceeds {MAX_TEMPLATE_BYTES} bytes"),
            Self::UnbalancedBraces => write!(f, "template has unbalanced {{ }} braces"),
            Self::UnknownToken(t) => write!(f, "unknown token {t}"),
            Self::WrongDomain(t) => write!(f, "token {t} belongs to another media domain"),
            Self::NotAllowedInField(t) => write!(f, "token {t} is not allowed in this field"),
            Self::InvalidPadding(t) => write!(f, "invalid zero-padding in {t}; use 1 to 4 zeros"),
            Self::PathSeparatorInFilename => write!(f, "filename templates must not contain '/'"),
            Self::EmptyPathComponent => write!(f, "template produces an empty path component"),
            Self::IllegalLiteralCharacter => {
                write!(f, "template literal text contains a control character")
            }
        }
    }
}

#[derive(Debug, Clone)]
enum Segment {
    Literal(String),
    Token(Token),
}

/// A parsed, validated template. Structurally sound; still needs facts to render.
#[derive(Debug, Clone)]
pub struct Template {
    domain: MediaDomain,
    components: Vec<Vec<Segment>>,
}

impl Template {
    pub(super) fn uses_custom_formats(&self) -> bool {
        self.components
            .iter()
            .flatten()
            .any(|s| matches!(s, Segment::Token(Token::CustomFormats { .. })))
    }
}

pub fn parse(field: TemplateField, raw: &str) -> Result<Template, TemplateError> {
    if raw.is_empty() {
        return Err(TemplateError::Empty);
    }
    if raw.len() > MAX_TEMPLATE_BYTES {
        return Err(TemplateError::TooLong);
    }
    let mut components: Vec<Vec<Segment>> = vec![Vec::new()];
    let mut literal = String::new();
    let mut in_token = false;
    let mut token_text = String::new();
    for c in raw.chars() {
        match c {
            '{' if !in_token => {
                if !literal.is_empty() {
                    components
                        .last_mut()
                        .expect("components always has one entry")
                        .push(Segment::Literal(std::mem::take(&mut literal)));
                }
                in_token = true;
                token_text.clear();
            }
            '{' => return Err(TemplateError::UnbalancedBraces),
            '}' if in_token => {
                let token = parse_token(&token_text)?;
                if token.domain().is_some_and(|d| d != field.domain()) {
                    return Err(TemplateError::WrongDomain(format!("{{{token_text}}}")));
                }
                if !field.allows(&token) {
                    return Err(TemplateError::NotAllowedInField(format!(
                        "{{{token_text}}}"
                    )));
                }
                components
                    .last_mut()
                    .expect("components always has one entry")
                    .push(Segment::Token(token));
                in_token = false;
            }
            '}' => return Err(TemplateError::UnbalancedBraces),
            '/' if !in_token => {
                if field.kind() == FieldKind::Filename {
                    return Err(TemplateError::PathSeparatorInFilename);
                }
                if !literal.is_empty() {
                    components
                        .last_mut()
                        .expect("components always has one entry")
                        .push(Segment::Literal(std::mem::take(&mut literal)));
                }
                components.push(Vec::new());
            }
            _ if in_token => token_text.push(c),
            _ => {
                // Operator-authored literal text is trusted less than fact-derived
                // substitutions get sanitized to; reject control characters up
                // front rather than letting them reach a rendered path unchanged.
                if c.is_control() {
                    return Err(TemplateError::IllegalLiteralCharacter);
                }
                literal.push(c);
            }
        }
    }
    if in_token {
        return Err(TemplateError::UnbalancedBraces);
    }
    if !literal.is_empty() {
        components
            .last_mut()
            .expect("components always has one entry")
            .push(Segment::Literal(literal));
    }
    for component in &components {
        if component.is_empty() {
            return Err(TemplateError::EmptyPathComponent);
        }
        if let [Segment::Literal(text)] = component.as_slice() {
            if trim_edges(text).is_empty() {
                return Err(TemplateError::EmptyPathComponent);
            }
        }
    }
    Ok(Template {
        domain: field.domain(),
        components,
    })
}

/// Trusted single-episode naming facts. The renderer performs no lookups; the
/// caller is responsible for sourcing these from the database.
#[derive(Debug, Clone)]
pub struct EpisodeNamingFacts {
    pub series_title: String,
    pub season: i64,
    pub episode: i64,
    pub episode_title: Option<String>,
    pub quality_title: String,
    /// Already matched, rename-enabled names; selection is independent of profile scores.
    pub custom_formats: Vec<String>,
    pub air_date: Option<String>,
}
/// Trusted single-movie naming facts.
#[derive(Debug, Clone)]
pub struct MovieNamingFacts {
    pub movie_title: String,
    pub release_year: Option<i64>,
    pub edition: Option<String>,
    pub quality_title: String,
    /// Already matched, rename-enabled names; selection is independent of profile scores.
    pub custom_formats: Vec<String>,
}
#[derive(Debug, Clone, Copy)]
pub enum RenderFacts<'a> {
    Episode(&'a EpisodeNamingFacts),
    Movie(&'a MovieNamingFacts),
}
impl RenderFacts<'_> {
    fn domain(&self) -> MediaDomain {
        match self {
            Self::Episode(_) => MediaDomain::Tv,
            Self::Movie(_) => MediaDomain::Movies,
        }
    }
}

/// The six colon_replacement variants, plus the operator-supplied text for `custom`.
#[derive(Debug, Clone, Copy)]
pub enum ColonPolicy<'a> {
    Delete,
    Dash,
    SpaceDash,
    SpaceDashSpace,
    Smart,
    Custom(&'a str),
}
#[derive(Debug, Clone, Copy)]
pub struct RenderConfig<'a> {
    pub replace_illegal_characters: bool,
    pub colon: ColonPolicy<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderError {
    /// The `Template`'s domain does not match the supplied `RenderFacts` variant.
    DomainMismatch,
    /// A `/`, `\`, NUL or control character was substituted and
    /// `replace_illegal_characters` is false, so the render fails rather than
    /// silently letting the character split or corrupt a path component.
    IllegalCharacter,
    /// After sanitization a path component was empty, or only dots/spaces.
    EmptyComponent,
}
impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DomainMismatch => write!(f, "facts do not match the template's media domain"),
            Self::IllegalCharacter => write!(
                f,
                "a substituted value contained an illegal character and replace_illegal_characters is disabled"
            ),
            Self::EmptyComponent => write!(f, "rendering produced an empty path component"),
        }
    }
}

fn token_value(token: &Token, facts: RenderFacts<'_>) -> String {
    if let Token::CustomFormats {
        singular,
        filter,
        separator,
        case,
        ..
    } = token
    {
        let names = match facts {
            RenderFacts::Episode(f) => &f.custom_formats,
            RenderFacts::Movie(f) => &f.custom_formats,
        };
        let mut names = names
            .iter()
            .filter(|name| {
                if *singular {
                    !filter.trim().is_empty() && *name == filter
                } else if filter.trim().is_empty() {
                    true
                } else if let Some(excluded) = filter.strip_prefix('-') {
                    !excluded.split(',').any(|n| n == name.as_str())
                } else {
                    filter.split(',').any(|n| n == name.as_str())
                }
            })
            .collect::<Vec<_>>();
        names.sort();
        if *singular {
            names.truncate(1);
        }
        let value = names
            .into_iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        let value = value.trim();
        if value.is_empty() {
            return String::new();
        }
        let value = match case {
            LetterCase::Lower => value.to_lowercase(),
            LetterCase::Upper => value.to_uppercase(),
            LetterCase::Preserve => value.to_owned(),
        };
        let value = if separator.trim().is_empty() {
            value
        } else {
            value.replace(' ', separator)
        };
        return value;
    }
    match (token, facts) {
        (Token::SeriesTitle, RenderFacts::Episode(f)) => f.series_title.clone(),
        (Token::Season(pad), RenderFacts::Episode(f)) => {
            format!("{:0width$}", f.season, width = *pad as usize)
        }
        (Token::Episode(pad), RenderFacts::Episode(f)) => {
            format!("{:0width$}", f.episode, width = *pad as usize)
        }
        (Token::EpisodeTitle, RenderFacts::Episode(f)) => {
            f.episode_title.clone().unwrap_or_default()
        }
        (Token::AirDate, RenderFacts::Episode(f)) => f.air_date.clone().unwrap_or_default(),
        (Token::QualityTitle, RenderFacts::Episode(f)) => f.quality_title.clone(),
        (Token::QualityTitle, RenderFacts::Movie(f)) => f.quality_title.clone(),
        (Token::MovieTitle, RenderFacts::Movie(f)) => f.movie_title.clone(),
        (Token::ReleaseYear, RenderFacts::Movie(f)) => {
            f.release_year.map(|y| y.to_string()).unwrap_or_default()
        }
        (Token::EditionTags, RenderFacts::Movie(f)) => f.edition.clone().unwrap_or_default(),
        // Unreachable: `parse` already rejected any token whose domain doesn't
        // match the field's domain, and `render` checks the template's domain
        // against `facts` before calling this function.
        _ => unreachable!("token/facts domain mismatch should have been rejected earlier"),
    }
}
fn replace_colons(raw: &str, policy: ColonPolicy<'_>) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != ':' {
            out.push(c);
            continue;
        }
        match policy {
            ColonPolicy::Delete => {}
            ColonPolicy::Dash => out.push('-'),
            ColonPolicy::SpaceDash => out.push_str(" -"),
            ColonPolicy::SpaceDashSpace => out.push_str(" - "),
            // A colon already followed by whitespace needs no separator of its own;
            // otherwise insert a dash so words don't run together.
            ColonPolicy::Smart => {
                if chars.peek() != Some(&' ') {
                    out.push('-');
                }
            }
            ColonPolicy::Custom(text) => out.push_str(text),
        }
    }
    out
}
fn is_illegal(c: char) -> bool {
    c.is_control() || c == '/' || c == '\\' || matches!(c, '<' | '>' | '"' | '|' | '?' | '*')
}
fn replace_illegal(raw: &str, replace: bool) -> Result<String, RenderError> {
    if !raw.chars().any(is_illegal) {
        return Ok(raw.to_owned());
    }
    if !replace {
        return Err(RenderError::IllegalCharacter);
    }
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c == '/' || c == '\\' {
            // A hard path separator in a fact value must never create an extra
            // component; substitute a visual separator that stays inside one.
            out.push('-');
        } else if !is_illegal(c) {
            out.push(c);
        }
    }
    Ok(out)
}
fn trim_edges(s: &str) -> &str {
    s.trim_matches(|c: char| c == '.' || c == ' ')
}
pub(super) fn cap_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
fn finish_component(raw: &str) -> Result<String, RenderError> {
    let trimmed = trim_edges(raw);
    let capped = cap_bytes(trimmed, MAX_COMPONENT_BYTES);
    let result = trim_edges(capped);
    if result.is_empty() {
        return Err(RenderError::EmptyComponent);
    }
    Ok(result.to_owned())
}

/// Renders one filename or folder path. Folder-format output joins components with
/// `/`; filename-format output is always a single component. No I/O; the caller
/// owns turning this into an actual filesystem path (root + extension).
pub fn render(
    template: &Template,
    facts: RenderFacts<'_>,
    config: &RenderConfig<'_>,
) -> Result<String, RenderError> {
    if template.domain != facts.domain() {
        return Err(RenderError::DomainMismatch);
    }
    let mut parts = Vec::with_capacity(template.components.len());
    for component in &template.components {
        let mut buf = String::new();
        for segment in component {
            match segment {
                Segment::Literal(text) => buf.push_str(text),
                Segment::Token(token) => {
                    let raw_value = token_value(token, facts);
                    let colon_replaced = replace_colons(&raw_value, config.colon);
                    let sanitized =
                        replace_illegal(&colon_replaced, config.replace_illegal_characters)?;
                    if let Token::CustomFormats { prefix, suffix, .. } = token {
                        // Conditional punctuation applies after sanitization: a name made
                        // entirely of removed characters must not leave empty brackets.
                        if !sanitized.trim().is_empty() {
                            buf.push_str(prefix);
                            buf.push_str(sanitized.trim());
                            buf.push_str(suffix);
                        }
                    } else {
                        buf.push_str(&sanitized);
                    }
                }
            }
        }
        if component
            .iter()
            .any(|s| matches!(s, Segment::Token(Token::CustomFormats { .. })))
        {
            // Naming separators collapse only on the newly supported CF path.
            let mut previous = None;
            buf.retain(|c| {
                let keep = previous != Some(c) || !matches!(c, ' ' | '.' | '-' | '_');
                previous = Some(c);
                keep
            });
            buf = buf.trim_matches([' ', '.', '-', '_']).to_owned();
        }
        parts.push(finish_component(&buf)?);
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn episode_facts() -> EpisodeNamingFacts {
        EpisodeNamingFacts {
            series_title: "Halcyon Vale".into(),
            season: 3,
            episode: 7,
            episode_title: Some("The Long Dark".into()),
            quality_title: "WEBDL-1080p".into(),
            custom_formats: vec![],
            air_date: Some("2024-05-14".into()),
        }
    }
    fn movie_facts() -> MovieNamingFacts {
        MovieNamingFacts {
            movie_title: "The Wandering Harbor".into(),
            release_year: Some(2023),
            edition: Some("Director's Cut".into()),
            quality_title: "Bluray-1080p".into(),
            custom_formats: vec![],
        }
    }
    fn cfg(replace: bool, colon: ColonPolicy<'_>) -> RenderConfig<'_> {
        RenderConfig {
            replace_illegal_characters: replace,
            colon,
        }
    }

    #[test]
    fn custom_format_filters_normalization_and_bounded_paths() {
        let mut episode = episode_facts();
        let mut movie = movie_facts();
        let names = vec!["Zulu".into(), "A Format".into(), "ignored".into()];
        episode.custom_formats = names.clone();
        movie.custom_formats = names;
        for (field, facts) in [
            (
                TemplateField::StandardEpisode,
                RenderFacts::Episode(&episode),
            ),
            (TemplateField::StandardMovie, RenderFacts::Movie(&movie)),
        ] {
            for (input, expected) in [
                ("{Custom Formats}", "A Format Zulu ignored"),
                ("{Custom Formats:Zulu,A Format}", "A Format Zulu"),
                ("{Custom Formats:-ignored}", "A Format Zulu"),
                ("{Custom Format:A Format}", "A Format"),
                ("title {Custom Format:missing}", "title"),
                ("title {[Custom Formats:missing]}", "title"),
                ("title {-Custom Format}", "title"),
                ("{CUSTOM.FORMATS:-ignored}", "A.FORMAT.ZULU"),
                ("{custom_formats:Zulu}", "zulu"),
                ("{[Custom Formats:A Format]}", "[A Format]"),
                ("title  {Custom Formats:missing}.", "title"),
                ("title {Custom Format:a format}", "title"),
            ] {
                let parsed = parse(field, input).unwrap();
                assert_eq!(
                    render(&parsed, facts, &cfg(true, ColonPolicy::Dash)).unwrap(),
                    expected,
                    "{input}"
                );
            }
        }
        assert!(parse(TemplateField::MovieFolder, "{Custom Formats}").is_err());
        movie.custom_formats = vec!["Face/Off:secret".into()];
        let parsed = parse(TemplateField::StandardMovie, "{Custom Formats}").unwrap();
        assert_eq!(
            render(
                &parsed,
                RenderFacts::Movie(&movie),
                &cfg(true, ColonPolicy::Dash)
            )
            .unwrap(),
            "Face-Off-secret"
        );
        assert_eq!(
            render(
                &parsed,
                RenderFacts::Movie(&movie),
                &cfg(false, ColonPolicy::Dash)
            ),
            Err(RenderError::IllegalCharacter)
        );
        movie.custom_formats = vec!["???".into()];
        let wrapped = parse(TemplateField::StandardMovie, "title {[Custom Formats]}").unwrap();
        assert_eq!(
            render(
                &wrapped,
                RenderFacts::Movie(&movie),
                &cfg(true, ColonPolicy::Dash)
            )
            .unwrap(),
            "title"
        );
        movie.custom_formats = vec!["é".repeat(200)];
        assert_eq!(
            render(
                &parsed,
                RenderFacts::Movie(&movie),
                &cfg(true, ColonPolicy::Dash)
            )
            .unwrap()
            .len(),
            254
        );
    }

    #[test]
    fn renders_standard_episode_and_movie_tokens() {
        let facts = episode_facts();
        let t = parse(
            TemplateField::StandardEpisode,
            "{Series Title} - S{season:00}E{episode:00} - {Episode Title} [{Quality Title}]",
        )
        .unwrap();
        let out = render(
            &t,
            RenderFacts::Episode(&facts),
            &cfg(true, ColonPolicy::Dash),
        )
        .unwrap();
        assert_eq!(out, "Halcyon Vale - S03E07 - The Long Dark [WEBDL-1080p]");
        let facts = movie_facts();
        let t = parse(
            TemplateField::StandardMovie,
            "{Movie Title} ({Release Year}) {Edition Tags} [{Quality Title}]",
        )
        .unwrap();
        let out = render(
            &t,
            RenderFacts::Movie(&facts),
            &cfg(true, ColonPolicy::Dash),
        )
        .unwrap();
        assert_eq!(
            out,
            "The Wandering Harbor (2023) Director's Cut [Bluray-1080p]"
        );
    }

    #[test]
    fn folder_format_produces_multiple_components() {
        let facts = episode_facts();
        let t = parse(
            TemplateField::SeasonFolder,
            "{Series Title}/Season {season:00}",
        )
        .unwrap();
        let out = render(
            &t,
            RenderFacts::Episode(&facts),
            &cfg(true, ColonPolicy::Dash),
        )
        .unwrap();
        assert_eq!(out, "Halcyon Vale/Season 03");
    }

    #[test]
    fn unknown_token_is_rejected_and_named() {
        let err = parse(TemplateField::StandardEpisode, "{Not A Real Token}").unwrap_err();
        assert_eq!(
            err,
            TemplateError::UnknownToken("{Not A Real Token}".into())
        );
    }

    #[test]
    fn cross_domain_token_is_rejected() {
        let err = parse(TemplateField::StandardEpisode, "{Movie Title}").unwrap_err();
        assert_eq!(err, TemplateError::WrongDomain("{Movie Title}".into()));
        let err = parse(TemplateField::StandardMovie, "{Series Title}").unwrap_err();
        assert_eq!(err, TemplateError::WrongDomain("{Series Title}".into()));
    }

    #[test]
    fn field_scoping_blocks_out_of_place_tokens() {
        // A series folder is per-series; season/episode/quality tokens don't belong there.
        let err = parse(TemplateField::SeriesFolder, "{Series Title} S{season:00}").unwrap_err();
        assert_eq!(err, TemplateError::NotAllowedInField("{season:00}".into()));
        // A season folder is per-season; episode-level tokens don't belong there.
        let err = parse(TemplateField::SeasonFolder, "{Episode Title}").unwrap_err();
        assert_eq!(
            err,
            TemplateError::NotAllowedInField("{Episode Title}".into())
        );
        // A movie folder is per-movie; quality is a file-level fact.
        let err = parse(TemplateField::MovieFolder, "{Quality Title}").unwrap_err();
        assert_eq!(
            err,
            TemplateError::NotAllowedInField("{Quality Title}".into())
        );
    }

    #[test]
    fn padding_bounds_are_enforced() {
        assert!(parse(TemplateField::StandardEpisode, "{season:0}").is_ok());
        assert!(parse(TemplateField::StandardEpisode, "{season:0000}").is_ok());
        assert_eq!(
            parse(TemplateField::StandardEpisode, "{season:00000}").unwrap_err(),
            TemplateError::InvalidPadding("{season:00000}".into())
        );
        assert_eq!(
            parse(TemplateField::StandardEpisode, "{season:}").unwrap_err(),
            TemplateError::InvalidPadding("{season:}".into())
        );
        assert_eq!(
            parse(TemplateField::StandardEpisode, "{season:01}").unwrap_err(),
            TemplateError::InvalidPadding("{season:01}".into())
        );
    }

    #[test]
    fn structural_rejections() {
        assert_eq!(
            parse(TemplateField::StandardEpisode, "").unwrap_err(),
            TemplateError::Empty
        );
        assert_eq!(
            parse(
                TemplateField::StandardEpisode,
                &"a".repeat(MAX_TEMPLATE_BYTES + 1)
            )
            .unwrap_err(),
            TemplateError::TooLong
        );
        assert_eq!(
            parse(TemplateField::StandardEpisode, "{Series Title").unwrap_err(),
            TemplateError::UnbalancedBraces
        );
        assert_eq!(
            parse(TemplateField::StandardEpisode, "Series Title}").unwrap_err(),
            TemplateError::UnbalancedBraces
        );
        assert_eq!(
            parse(TemplateField::StandardEpisode, "{{Series Title}}").unwrap_err(),
            TemplateError::UnbalancedBraces
        );
        assert_eq!(
            parse(TemplateField::StandardEpisode, "{Series Title}/x").unwrap_err(),
            TemplateError::PathSeparatorInFilename
        );
        // Leading, trailing, and doubled folder separators all yield an empty component.
        for template in ["/{Series Title}", "{Series Title}/", "{Series Title}//x"] {
            assert_eq!(
                parse(TemplateField::SeriesFolder, template).unwrap_err(),
                TemplateError::EmptyPathComponent,
                "{template}"
            );
        }
        assert_eq!(
            parse(TemplateField::SeriesFolder, "..").unwrap_err(),
            TemplateError::EmptyPathComponent
        );
    }

    #[test]
    fn colon_replacement_policies() {
        let title = "Alien: Resurrection";
        let cases: &[(ColonPolicy<'_>, &str)] = &[
            (ColonPolicy::Delete, "Alien Resurrection"),
            (ColonPolicy::Dash, "Alien- Resurrection"),
            (ColonPolicy::SpaceDash, "Alien - Resurrection"),
            (ColonPolicy::SpaceDashSpace, "Alien -  Resurrection"),
            (ColonPolicy::Smart, "Alien Resurrection"),
            (ColonPolicy::Custom(" ~ "), "Alien ~  Resurrection"),
        ];
        for (policy, expected) in cases {
            assert_eq!(replace_colons(title, *policy), *expected, "{policy:?}");
        }
        // Smart inserts a dash only when no whitespace already separates the parts.
        assert_eq!(replace_colons("A:B", ColonPolicy::Smart), "A-B");
    }

    #[test]
    fn hostile_episode_title_face_off_stays_in_one_component() {
        let mut facts = episode_facts();
        facts.episode_title = Some("Face/Off".into());
        let t = parse(TemplateField::StandardEpisode, "{Episode Title}").unwrap();
        let out = render(
            &t,
            RenderFacts::Episode(&facts),
            &cfg(true, ColonPolicy::Dash),
        )
        .unwrap();
        assert_eq!(out, "Face-Off");
        assert!(!out.contains('/'));
        let err = render(
            &t,
            RenderFacts::Episode(&facts),
            &cfg(false, ColonPolicy::Dash),
        )
        .unwrap_err();
        assert_eq!(err, RenderError::IllegalCharacter);
    }

    #[test]
    fn hostile_title_of_dots_alone_is_rejected() {
        let mut facts = episode_facts();
        facts.episode_title = Some("..".into());
        let t = parse(TemplateField::StandardEpisode, "{Episode Title}").unwrap();
        let err = render(
            &t,
            RenderFacts::Episode(&facts),
            &cfg(true, ColonPolicy::Dash),
        )
        .unwrap_err();
        assert_eq!(err, RenderError::EmptyComponent);
    }

    #[test]
    fn hostile_control_characters_and_nul() {
        let mut facts = episode_facts();
        facts.episode_title = Some("Bad\u{0}Ti\ntle".into());
        let t = parse(TemplateField::StandardEpisode, "{Episode Title}").unwrap();
        let out = render(
            &t,
            RenderFacts::Episode(&facts),
            &cfg(true, ColonPolicy::Dash),
        )
        .unwrap();
        assert_eq!(out, "BadTitle");
        let err = render(
            &t,
            RenderFacts::Episode(&facts),
            &cfg(false, ColonPolicy::Dash),
        )
        .unwrap_err();
        assert_eq!(err, RenderError::IllegalCharacter);
    }

    #[test]
    fn hostile_very_long_title_is_truncated_to_byte_cap() {
        let mut facts = episode_facts();
        facts.episode_title = Some("x".repeat(500));
        let t = parse(TemplateField::StandardEpisode, "{Episode Title}").unwrap();
        let out = render(
            &t,
            RenderFacts::Episode(&facts),
            &cfg(true, ColonPolicy::Dash),
        )
        .unwrap();
        assert_eq!(out.len(), MAX_COMPONENT_BYTES);
    }

    #[test]
    fn hostile_very_long_multibyte_title_truncates_at_a_char_boundary() {
        // Each "é" is 2 bytes; 200 of them is 400 bytes, well past the 255 cap.
        // The cap must land on a char boundary, so the byte count comes out one
        // short of 255 (254) rather than panicking or splitting a code point.
        let mut facts = episode_facts();
        facts.episode_title = Some("é".repeat(200));
        let t = parse(TemplateField::StandardEpisode, "{Episode Title}").unwrap();
        let out = render(
            &t,
            RenderFacts::Episode(&facts),
            &cfg(true, ColonPolicy::Dash),
        )
        .unwrap();
        assert_eq!(out.len(), 254);
        assert_eq!(out.chars().count(), 127);
        assert!(out.chars().all(|c| c == 'é'));
    }

    #[test]
    fn literal_control_characters_in_the_template_itself_are_rejected() {
        assert_eq!(
            parse(TemplateField::StandardEpisode, "Bad\u{0}Name").unwrap_err(),
            TemplateError::IllegalLiteralCharacter
        );
        assert_eq!(
            parse(TemplateField::StandardEpisode, "Bad\nName").unwrap_err(),
            TemplateError::IllegalLiteralCharacter
        );
    }

    #[test]
    fn domain_mismatch_between_template_and_facts_is_rejected() {
        let t = parse(TemplateField::StandardEpisode, "{Series Title}").unwrap();
        let facts = movie_facts();
        let err = render(
            &t,
            RenderFacts::Movie(&facts),
            &cfg(true, ColonPolicy::Dash),
        )
        .unwrap_err();
        assert_eq!(err, RenderError::DomainMismatch);
    }

    #[test]
    fn shared_quality_token_works_in_both_domains() {
        let episode = episode_facts();
        let t = parse(TemplateField::StandardEpisode, "{Quality Title}").unwrap();
        assert_eq!(
            render(
                &t,
                RenderFacts::Episode(&episode),
                &cfg(true, ColonPolicy::Dash)
            )
            .unwrap(),
            "WEBDL-1080p"
        );
        let movie = movie_facts();
        let t = parse(TemplateField::StandardMovie, "{Quality Title}").unwrap();
        assert_eq!(
            render(
                &t,
                RenderFacts::Movie(&movie),
                &cfg(true, ColonPolicy::Dash)
            )
            .unwrap(),
            "Bluray-1080p"
        );
    }

    #[test]
    fn absent_optional_facts_substitute_empty_string() {
        let mut facts = episode_facts();
        facts.episode_title = None;
        let t = parse(TemplateField::StandardEpisode, "[{Episode Title}]").unwrap();
        let out = render(
            &t,
            RenderFacts::Episode(&facts),
            &cfg(true, ColonPolicy::Dash),
        )
        .unwrap();
        assert_eq!(out, "[]");
    }
}
