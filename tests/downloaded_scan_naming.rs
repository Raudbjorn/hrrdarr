//! Requirements examples, expressed against the independent native token grammar.
//! These tests exercise naming only, not scan execution or filesystem reservations.
use hrrdarr::naming::render::*;

fn facts(numbers: &[i64]) -> EpisodeSetNamingFacts {
    EpisodeSetNamingFacts {
        series_title: "Halcyon Vale".into(),
        anime: false,
        members: numbers
            .iter()
            .map(|n| EpisodeMemberNamingFacts {
                episode_id: *n,
                season: 3,
                episode: *n,
                title: Some("The Journey".into()),
                air_date: Some(format!("2024-05-{:02}", n + 7)),
                absolute_episode_number: Some(n + 40),
            })
            .collect(),
        quality_title: "WEBDL-1080p".into(),
        revision: None,
        custom_formats: vec![],
    }
}
fn config() -> RenderConfig<'static> {
    RenderConfig {
        replace_illegal_characters: true,
        colon: ColonPolicy::Dash,
    }
}
fn render_set(numbers: &[i64], style: i64, template: &str) -> String {
    render_episode_set(
        &parse(TemplateField::StandardEpisode, template).unwrap(),
        &facts(numbers),
        Some(MultiEpisodeStyle::try_from(style).unwrap()),
        &config(),
    )
    .unwrap()
}
#[test]
fn all_six_modes_encode_complete_contiguous_and_gapped_sets() {
    let template = "{Series Title} - S{season:00}E{episode:00} - {Episode Title}";
    for (style, contiguous, gapped) in [
        (0, "S03E07-08-09", "S03E07-08+10"),
        (1, "S03E07 - S03E08 - S03E09", "S03E07 - S03E08 - S03E10"),
        (2, "S03E07E08E09", "S03E07E08E10"),
        (3, "S03E07-E08-E09", "S03E07-E08-E10"),
        (4, "S03E07-09", "S03E07-08+10"),
        (5, "S03E07-E09", "S03E07-E08+E10"),
    ] {
        assert_eq!(
            render_set(&[9, 7, 8, 7], style, template),
            format!("Halcyon Vale - {contiguous} - The Journey")
        );
        assert_eq!(
            render_set(&[10, 8, 7], style, template),
            format!("Halcyon Vale - {gapped} - The Journey")
        );
        // Distinct sets must not collide merely because first and last match.
        assert_ne!(
            render_set(&[7, 9], style, template),
            render_set(&[7, 8, 9], style, template)
        );
    }
}
#[test]
fn token_widths_and_numeric_block_separators_are_preserved() {
    assert_eq!(
        render_set(&[7, 8], 1, "{Series Title}.{season:0}x{episode:000}"),
        "Halcyon Vale.3x007.3x008"
    );
    assert_eq!(
        render_set(&[7, 8], 2, "S{season:000}e{episode:0000}"),
        "S003e0007e0008"
    );
    assert_eq!(render_set(&[7, 8], 5, "{episode:0}"), "7-8");
    assert!(parse(TemplateField::AnimeEpisode, "{absolute:00000}").is_err());
    assert!(parse(TemplateField::StandardMovie, "{absolute:000}").is_err());
}
#[test]
fn absolute_daily_and_titles_use_all_members_and_require_complete_facts() {
    let absolute = parse(
        TemplateField::AnimeEpisode,
        "{Series Title} - {absolute:000} - {Episode Title}",
    )
    .unwrap();
    for (style, expected) in [
        (0, "047-048-049"),
        (1, "047 - 048 - 049"),
        (2, "047-048-049"),
        (3, "047-048-049"),
        (4, "047-049"),
        (5, "047-049"),
    ] {
        assert_eq!(
            render_episode_set(
                &absolute,
                &facts(&[9, 7, 8]),
                Some(MultiEpisodeStyle::try_from(style).unwrap()),
                &config()
            )
            .unwrap(),
            format!("Halcyon Vale - {expected} - The Journey")
        );
    }
    let daily = parse(TemplateField::DailyEpisode, "{Air-Date} - {Episode Title}").unwrap();
    let mut set = facts(&[7, 8]);
    set.members[1].title = Some("The Return".into());
    assert_eq!(
        render_episode_set(&daily, &set, Some(MultiEpisodeStyle::Extend), &config()).unwrap(),
        "2024-05-14 + 2024-05-15 - The Journey + The Return"
    );
    set.members[1].air_date = set.members[0].air_date.clone();
    assert_eq!(
        render_episode_set(&daily, &set, Some(MultiEpisodeStyle::Extend), &config()),
        Err(RenderError::MissingMembershipToken)
    );
    set.members[1].air_date = Some("2024-02-30".into());
    assert_eq!(
        render_episode_set(&daily, &set, Some(MultiEpisodeStyle::Extend), &config()),
        Err(RenderError::MissingAirDate)
    );
    set.members[1].absolute_episode_number = None;
    assert_eq!(
        render_episode_set(&absolute, &set, Some(MultiEpisodeStyle::Extend), &config()),
        Err(RenderError::MissingAbsoluteNumber)
    );
}
#[test]
fn unknown_policy_conflicting_members_and_identity_truncation_fail_explicitly() {
    let template = parse(
        TemplateField::StandardEpisode,
        "{Series Title} S{season:00}E{episode:00}",
    )
    .unwrap();
    assert_eq!(
        render_episode_set(&template, &facts(&[7, 8]), None, &config()),
        Err(RenderError::NeedsPolicy)
    );
    let mut set = facts(&[7, 8]);
    set.members[1].season = 4;
    assert_eq!(
        render_episode_set(&template, &set, Some(MultiEpisodeStyle::Extend), &config()),
        Err(RenderError::InvalidEpisodeSet)
    );
    let mut set = facts(&[7, 7]);
    set.members[1].title = Some("Conflicting duplicate".into());
    assert_eq!(
        render_episode_set(&template, &set, Some(MultiEpisodeStyle::Extend), &config()),
        Err(RenderError::InvalidEpisodeSet)
    );
    let mut set = facts(&[7, 8]);
    set.series_title = "é".repeat(130);
    assert_eq!(
        render_episode_set(&template, &set, Some(MultiEpisodeStyle::Extend), &config()),
        Err(RenderError::ComponentTooLong)
    );
    let no_identity = parse(
        TemplateField::StandardEpisode,
        "{Series Title} {Episode Title}",
    )
    .unwrap();
    assert_eq!(
        render_episode_set(
            &no_identity,
            &facts(&[7, 8]),
            Some(MultiEpisodeStyle::Extend),
            &config()
        ),
        Err(RenderError::MissingMembershipToken)
    );
    let mut set = facts(&[7, 8]);
    set.series_title = "A/../B\\C".into();
    let rendered =
        render_episode_set(&template, &set, Some(MultiEpisodeStyle::Extend), &config()).unwrap();
    assert!(!rendered.contains(['/', '\\']));
    assert_eq!(
        render_episode_set(
            &template,
            &set,
            Some(MultiEpisodeStyle::Extend),
            &RenderConfig {
                replace_illegal_characters: false,
                ..config()
            }
        ),
        Err(RenderError::IllegalCharacter)
    );
    let mut set = facts(&[7]);
    set.members = vec![set.members[0].clone(); MAX_EPISODE_MEMBERS + 1];
    assert_eq!(
        render_episode_set(&template, &set, None, &config()),
        Err(RenderError::InvalidEpisodeSet)
    );
}
#[test]
fn singleton_output_does_not_depend_on_multi_style() {
    let single = EpisodeNamingFacts {
        series_title: "Halcyon Vale".into(),
        anime: false,
        season: 3,
        episode: 7,
        episode_title: Some("The Journey".into()),
        quality_title: "WEBDL-1080p".into(),
        revision: None,
        custom_formats: vec![],
        air_date: Some("2024-05-14".into()),
    };
    for raw in [
        "{Series Title} S{season:00}E{episode:00}",
        "{Episode Title}",
        "{Air-Date}",
        "{Quality Full}",
    ] {
        let template = parse(TemplateField::StandardEpisode, raw).unwrap();
        let expected = render(&template, RenderFacts::Episode(&single), &config()).unwrap();
        for style in [
            None,
            Some(MultiEpisodeStyle::Duplicate),
            Some(MultiEpisodeStyle::PrefixedRange),
        ] {
            assert_eq!(
                render_episode_set(
                    &template,
                    &EpisodeSetNamingFacts::from_single(&single, None),
                    style,
                    &config()
                )
                .unwrap(),
                expected
            );
        }
    }
}
