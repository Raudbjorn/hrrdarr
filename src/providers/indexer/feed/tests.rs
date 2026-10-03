use super::*;

const BASE: &str = "https://tracker.example/feeds/rss.xml";
const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

fn wrap(items: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed" xmlns:torrent="http://xmlns.ezrss.it/0.1/" xmlns:nyaa="https://nyaa.si/xmlns/nyaa" xmlns:dc="http://purl.org/dc/elements/1.1/"><channel><title>t</title>{items}</channel></rss>"#
    )
}
fn parse(items: &str) -> Result<ParsedFeed> {
    parse_feed(
        &wrap(items),
        &url::Url::parse(BASE).unwrap(),
        MediaDomain::Tv,
    )
}
fn one(items: &str) -> Release {
    let mut feed = parse(items).unwrap();
    assert!(feed.warnings.is_empty(), "{} warnings", feed.warnings.len());
    assert_eq!(feed.items.len(), 1);
    feed.items.remove(0)
}
fn invalid(result: Result<ParsedFeed>) -> bool {
    matches!(result, Err(IndexerError::InvalidResponse))
}

#[test]
fn enclosure_item_maps_every_core_field() {
    let release = one(
        r#"<item><title>Show.S01E01.1080p.WEB-DL</title><guid>g-1</guid>
        <pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate>
        <link>https://tracker.example/details/1</link>
        <enclosure url="https://tracker.example/dl/1.torrent" length="1073741824" type="application/x-bittorrent"/>
        <comments>https://tracker.example/details/1#comments</comments></item>"#,
    );
    assert_eq!(
        release.metadata.title.as_deref(),
        Some("Show.S01E01.1080p.WEB-DL")
    );
    assert_eq!(release.guid.as_deref(), Some("g-1"));
    assert_eq!(release.download_url, "https://tracker.example/dl/1.torrent");
    assert_eq!(release.metadata.size_bytes, Some(1_073_741_824));
    assert_eq!(release.metadata.published_at, "2024-01-01T12:00:00Z");
    assert_eq!(
        release.metadata.categories,
        [5000],
        "a TV-scope item without categories gets the TV root category"
    );
    assert_eq!(
        release.facts.info_url.as_deref(),
        Some("https://tracker.example/details/1")
    );
    assert!(release.facts.torrent.is_some());
}

#[test]
fn magnet_links_and_hash_derivation() {
    let in_link = one(&format!(
        "<item><title>A</title><pubDate>Mon, 01 Jan 2024 12:00:00 GMT</pubDate><link>magnet:?xt=urn:btih:{HASH}&amp;dn=A</link></item>"
    ));
    assert!(in_link.download_url.starts_with("magnet:?xt=urn:btih:"));
    let torrent = in_link.facts.torrent.unwrap();
    assert_eq!(torrent.info_hash.as_deref(), Some(HASH));
    assert!(torrent.magnet_url.is_some());
    // An ezrss magnet next to an http enclosure: the enclosure downloads, the magnet is kept.
    let both = one(&format!(
        r#"<item><title>A</title><pubDate>2024-01-01T12:00:00Z</pubDate>
        <enclosure url="/dl/a.torrent" length="0" type="application/x-bittorrent"/>
        <torrent:magnetURI>magnet:?xt=urn:btih:{HASH}</torrent:magnetURI></item>"#
    ));
    assert_eq!(both.download_url, "https://tracker.example/dl/a.torrent");
    assert_eq!(
        both.metadata.size_bytes, None,
        "a zero enclosure length means unreported"
    );
    let torrent = both.facts.torrent.unwrap();
    assert!(torrent.magnet_url.unwrap().contains(HASH));
    assert_eq!(torrent.info_hash.as_deref(), Some(HASH));
    // Uppercase explicit hashes normalise; a malformed one rejects only that item.
    let upper = one(&format!(
        "<item><title>A</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/a</link><torrent:infoHash>{}</torrent:infoHash></item>",
        HASH.to_uppercase()
    ));
    assert_eq!(
        upper.facts.torrent.unwrap().info_hash.as_deref(),
        Some(HASH)
    );
    let feed = parse(&format!(
        "<item><title>bad</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/a</link><torrent:infoHash>nothex</torrent:infoHash></item>
         <item><title>ok</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>magnet:?xt=urn:btih:{HASH}</link></item>"
    ))
    .unwrap();
    assert_eq!((feed.items.len(), feed.warnings.len()), (1, 1));
    assert_eq!(feed.warnings[0].index, 0);
}

#[test]
fn relative_urls_resolve_against_the_feed_url() {
    for (given, expected) in [
        ("/dl/1.torrent", "https://tracker.example/dl/1.torrent"),
        ("dl/1.torrent", "https://tracker.example/feeds/dl/1.torrent"),
        ("../dl/1.torrent", "https://tracker.example/dl/1.torrent"),
        ("//cdn.example/1.torrent", "https://cdn.example/1.torrent"),
    ] {
        let release = one(&format!(
            r#"<item><title>A</title><pubDate>2024-01-01T12:00:00Z</pubDate><enclosure url="{given}" length="5" type="application/x-bittorrent"/></item>"#
        ));
        assert_eq!(release.download_url, expected);
    }
    let from_link = one(
        "<item><title>A</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>/d/9</link></item>",
    );
    assert_eq!(from_link.download_url, "https://tracker.example/d/9");
}

#[test]
fn unsafe_locators_reject_the_item_only() {
    for bad in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "ftp://tracker.example/a.torrent",
        "https://user:pw@tracker.example/a.torrent",
        "magnet:?dn=no-hash",
        "data:text/plain,hi",
    ] {
        let feed = parse(&format!(
            r#"<item><title>bad</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>{bad}</link></item>
            <item><title>ok</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/ok</link></item>"#
        ))
        .unwrap();
        assert_eq!((feed.items.len(), feed.warnings.len()), (1, 1), "{bad}");
    }
}

#[test]
fn torznab_ezrss_and_nyaa_attributes() {
    let torznab = one(&format!(
        r#"<item><title>T</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/t</link>
        <torznab:attr name="seeders" value="12"/><torznab:attr name="peers" value="20"/>
        <torznab:attr name="infohash" value="{HASH}"/><torznab:attr name="size" value="2048"/>
        <torznab:attr name="imdb" value="tt0123456"/></item>"#
    ));
    assert_eq!(torznab.metadata.seeders, Some(12));
    assert_eq!(torznab.metadata.peers, Some(20));
    assert_eq!(torznab.metadata.leechers, Some(8));
    assert_eq!(torznab.metadata.size_bytes, Some(2048));
    assert_eq!(
        torznab.facts.torrent.unwrap().info_hash.as_deref(),
        Some(HASH)
    );
    let ezrss = one(&format!(
        r#"<item><title>E</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/e</link>
        <torrent:contentLength>3072</torrent:contentLength><torrent:seeds>7</torrent:seeds>
        <torrent:peers>9</torrent:peers><torrent:infoHash>{HASH}</torrent:infoHash></item>"#
    ));
    assert_eq!(
        (
            ezrss.metadata.size_bytes,
            ezrss.metadata.seeders,
            ezrss.metadata.peers
        ),
        (Some(3072), Some(7), Some(9))
    );
    let nyaa = one(&format!(
        r#"<item><title>N</title><guid isPermaLink="true">https://nyaa.example/view/1</guid><pubDate>Mon, 01 Jan 2024 12:00:00 -0000</pubDate>
        <link>https://nyaa.example/download/1.torrent</link><nyaa:seeders>5</nyaa:seeders><nyaa:leechers>2</nyaa:leechers>
        <nyaa:infoHash>{HASH}</nyaa:infoHash><nyaa:size>1.5 GiB</nyaa:size></item>"#
    ));
    assert_eq!(
        (
            nyaa.metadata.seeders,
            nyaa.metadata.leechers,
            nyaa.metadata.peers
        ),
        (Some(5), Some(2), Some(7))
    );
    assert_eq!(nyaa.metadata.size_bytes, Some(1_610_612_736));
    // Malformed optional numbers reject the item; they are never silently zeroed.
    for bad in [
        r#"<torznab:attr name="seeders" value="many"/>"#,
        r#"<torznab:attr name="seeders" value="-1"/>"#,
        r#"<torznab:attr name="size" value="huge"/>"#,
        "<torrent:seeds>x</torrent:seeds>",
        "<torrent:contentLength>-5</torrent:contentLength>",
        "<nyaa:size>12 parsecs</nyaa:size>",
    ] {
        let feed = parse(&format!(
            "<item><title>bad</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/b</link>{bad}</item>
             <item><title>ok</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/o</link></item>"
        ))
        .unwrap();
        assert_eq!((feed.items.len(), feed.warnings.len()), (1, 1), "{bad}");
    }
}

#[test]
fn byte_sizes() {
    for (input, expected) in [
        ("0", 0),
        ("1", 1),
        (" 2048 ", 2048),
        ("1 KiB", 1024),
        ("1.5 MB", 1_572_864),
        ("2GiB", 2_147_483_648),
        ("1 TiB", 1_099_511_627_776),
    ] {
        assert_eq!(byte_size(input).unwrap(), expected, "{input}");
    }
    for bad in [
        "",
        "abc",
        "-1",
        "1.2.3",
        ".5",
        "1 PB",
        "NaN",
        "9999999999999 TiB",
        "99999999999999999999",
    ] {
        assert!(byte_size(bad).is_err(), "{bad}");
    }
}

#[test]
fn dates_accept_absolute_forms_and_reject_relative_ones() {
    for (input, expected) in [
        ("Mon, 01 Jan 2024 12:00:00 +0000", "2024-01-01T12:00:00Z"),
        ("Mon, 01 Jan 2024 12:00:00 GMT", "2024-01-01T12:00:00Z"),
        ("Mon, 01 Jan 2024 14:00:00 +0200", "2024-01-01T12:00:00Z"),
        ("Mon, 01 Jan 2024 12:00:00 EST", "2024-01-01T17:00:00Z"),
        ("2024-01-01T12:00:00Z", "2024-01-01T12:00:00Z"),
        ("2024-01-01T14:00:00+02:00", "2024-01-01T12:00:00Z"),
        ("2024-01-01 12:00:00", "2024-01-01T12:00:00Z"),
        ("Mon, 01 Jan 2024 12:00:00", "2024-01-01T12:00:00Z"),
        ("2024-01-01", "2024-01-01T00:00:00Z"),
    ] {
        assert_eq!(feed_date(input).unwrap(), expected, "{input}");
    }
    for relative in [
        "2 hours ago",
        "yesterday",
        "just now",
        "",
        "tomorrow",
        "1700000000",
        "31/12/2024",
    ] {
        assert!(feed_date(relative).is_err(), "{relative}");
    }
    let release = one(
        r#"<item><title>D</title><dc:date>2024-03-04T05:06:07Z</dc:date><link>https://t.example/d</link></item>"#,
    );
    assert_eq!(release.metadata.published_at, "2024-03-04T05:06:07Z");
    let feed = parse(
        r#"<item><title>rel</title><pubDate>3 minutes ago</pubDate><link>https://t.example/r</link></item>
           <item><title>nodate</title><link>https://t.example/n</link></item>
           <item><title>ok</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/o</link></item>"#,
    )
    .unwrap();
    assert_eq!((feed.items.len(), feed.warnings.len()), (1, 2));
    assert_eq!(
        feed.warnings.iter().map(|w| w.index).collect::<Vec<_>>(),
        [0, 1]
    );
}

#[test]
fn malformed_item_versus_malformed_feed() {
    // Well-formed feed: bad items are dropped individually.
    let feed = parse(
        r#"<item><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/no-title</link></item>
           <item><title>   </title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/blank</link></item>
           <item><title>no locator</title><pubDate>2024-01-01T12:00:00Z</pubDate></item>
           <item><title>fine</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/fine</link></item>"#,
    )
    .unwrap();
    assert_eq!((feed.items.len(), feed.warnings.len()), (1, 3));
    assert_eq!(feed.items[0].metadata.title.as_deref(), Some("fine"));
    // A well-formed feed with no items at all is valid evidence of access.
    let empty = parse("").unwrap();
    assert!(empty.items.is_empty() && empty.warnings.is_empty());
    // Items exist but none is usable: the feed itself is not a usable torrent feed.
    assert!(invalid(parse(
        "<item><title>x</title></item><item><link>https://t.example/z</link></item>"
    )));
    // Not well-formed or not RSS: whole-feed failure, never a partial result.
    for bad in [
        "",
        "not xml",
        "<html><body>login</body></html>",
        "<rss><channel><item><title>x</title>",
        "<rss></rss>",
        r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry/></feed>"#,
        "<rss><channel/><channel/></rss>",
        "<rss><channel><item><title>a</item></channel></rss>",
    ] {
        assert!(
            invalid(parse_feed(
                bad,
                &url::Url::parse(BASE).unwrap(),
                MediaDomain::Movies
            )),
            "{bad:?}"
        );
    }
}

#[test]
fn duplicate_guids_keep_the_first_item() {
    let feed = parse(
        r#"<item><title>first</title><guid>same</guid><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/1</link></item>
           <item><title>second</title><guid>same</guid><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/2</link></item>
           <item><title>other</title><guid>other</guid><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/3</link></item>
           <item><title>noguid-a</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/4</link></item>
           <item><title>noguid-b</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/5</link></item>"#,
    )
    .unwrap();
    let titles: Vec<_> = feed
        .items
        .iter()
        .map(|r| r.metadata.title.clone().unwrap())
        .collect();
    assert_eq!(titles, ["first", "other", "noguid-a", "noguid-b"]);
    assert_eq!(
        feed.warnings.iter().map(|w| w.index).collect::<Vec<_>>(),
        [1]
    );
}

#[test]
fn hostile_documents_are_rejected_without_expansion() {
    let base = url::Url::parse(BASE).unwrap();
    // Internal and external entity declarations and any DTD at all.
    for doc in [
        r#"<?xml version="1.0"?><!DOCTYPE rss [<!ENTITY a "aaaa">]><rss><channel><item><title>&a;</title></item></channel></rss>"#,
        r#"<?xml version="1.0"?><!DOCTYPE rss SYSTEM "http://127.0.0.1:1/x.dtd"><rss><channel/></rss>"#,
        r#"<?xml version="1.0"?><!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;"><!ENTITY lol3 "&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;">]><rss><channel><item><title>&lol3;</title></item></channel></rss>"#,
        r#"<?xml version="1.0"?><!DOCTYPE rss [<!ENTITY x SYSTEM "file:///etc/passwd">]><rss><channel><item><title>&x;</title></item></channel></rss>"#,
    ] {
        assert!(invalid(parse_feed(doc, &base, MediaDomain::Tv)), "{doc}");
    }
}
#[test]
fn undeclared_entities_oversize_and_item_cap() {
    // Undeclared entities are not expanded either.
    assert!(invalid(parse(
        "<item><title>&bogus;</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/a</link></item>"
    )));
    // Oversized body (over the 1 MiB XML cap) and too many items.
    let padding = "x".repeat(1024 * 1024);
    assert!(invalid(parse(&format!(
        "<item><title>{padding}</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/a</link></item>"
    ))));
    let one_item = "<item><title>t</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/a</link></item>";
    assert_eq!(
        parse(&one_item.repeat(FEED_MAX_ITEMS)).unwrap().items.len(),
        FEED_MAX_ITEMS // identical items without a guid are all kept: de-duplication is by guid only
    );
    assert!(invalid(parse(&one_item.repeat(FEED_MAX_ITEMS + 1))));
}
#[test]
fn deep_nesting_is_bounded() {
    // A deeply nested document is bounded by the node cap instead of exhausting memory.
    let nested = format!("{}{}", "<a>".repeat(30_000), "</a>".repeat(30_000));
    assert!(invalid(parse(&nested)));
}
#[test]
fn cdata_and_standard_entities_decode() {
    // Standard XML entities and CDATA decode normally.
    let release = one(
        r#"<item><title><![CDATA[Tom &amp; Jerry]]></title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/a?x=1&amp;y=2</link></item>"#,
    );
    assert_eq!(release.metadata.title.as_deref(), Some("Tom & Jerry"));
    assert_eq!(release.download_url, "https://t.example/a?x=1&y=2");
}

#[test]
fn nzb_enclosures_are_never_selected_for_a_torrent_feed() {
    let nzb_only = parse(
        r#"<item><title>n</title><pubDate>2024-01-01T12:00:00Z</pubDate><enclosure url="https://t.example/a.nzb" length="5" type="application/x-nzb"/></item>
           <item><title>ok</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/ok</link></item>"#,
    )
    .unwrap();
    assert_eq!((nzb_only.items.len(), nzb_only.warnings.len()), (1, 1));
    let mixed = one(
        r#"<item><title>m</title><pubDate>2024-01-01T12:00:00Z</pubDate>
        <enclosure url="https://t.example/a.nzb" length="5" type="application/x-nzb"/>
        <enclosure url="https://t.example/untyped" length="6"/>
        <enclosure url="https://t.example/b.torrent" length="7" type="application/x-bittorrent; charset=binary"/></item>"#,
    );
    assert_eq!(
        mixed.download_url, "https://t.example/b.torrent",
        "a torrent-typed enclosure (parameters ignored) wins over an untyped one"
    );
}

#[test]
fn titles_are_bounded_and_identifier_fields_stay_scoped_to_the_domain() {
    let long = "x".repeat(2049);
    assert!(invalid(parse(&format!(
        "<item><title>{long}</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/a</link></item>"
    ))));
    let movie = parse_feed(
        &wrap(
            r#"<item><title>M</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/m</link><torznab:attr name="tmdbid" value="42"/><torznab:attr name="tvdbid" value="7"/></item>"#,
        ),
        &url::Url::parse(BASE).unwrap(),
        MediaDomain::Movies,
    )
    .unwrap();
    assert!(matches!(
        movie.items[0].facts.identifiers,
        ReleaseIdentifiers::Movie {
            tmdb_id: Some(42),
            imdb_id: None
        }
    ));
}

#[test]
fn nesting_prescan_counts_real_elements_only() {
    use super::super::{XML_DEPTH, depth_within};
    let nested = |n: usize| format!("{}{}", "<a>".repeat(n), "</a>".repeat(n));
    assert!(depth_within(&nested(XML_DEPTH), XML_DEPTH));
    assert!(!depth_within(&nested(XML_DEPTH + 1), XML_DEPTH));
    // Siblings, self-closing tags, attributes containing markup characters, comments, CDATA and
    // processing instructions do not add depth.
    let flat = format!(
        r#"<?xml version="1.0"?><r>{}<!-- <deep><deep><deep> -->{}<![CDATA[<x><x><x>]]><e a="<b>" c='>' d="/>"/></r>"#,
        "<s/>".repeat(100),
        "<t></t>".repeat(100)
    );
    assert!(depth_within(&flat, 3));
    // Hiding opens in quoted attributes or comments cannot lower the count of real nesting.
    let sneaky = format!(
        r#"<a t="<b>">{}{}</a>"#,
        "<c>".repeat(40),
        "</c>".repeat(40)
    );
    assert!(!depth_within(&sneaky, XML_DEPTH));
    assert!(
        depth_within("<a><b>", 8),
        "truncated input is the parser's to reject"
    );
    assert!(depth_within("<a><!-- never closed", 8));
    assert!(depth_within("<a x=\"never closed", 8));
}

#[test]
fn categories_come_from_the_scope_unless_the_item_states_numeric_ones() {
    let base = url::Url::parse(BASE).unwrap();
    let plain = wrap(
        "<item><title>M</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/m</link><category>Movies / HD</category></item>",
    );
    for (domain, expected) in [(MediaDomain::Tv, 5000), (MediaDomain::Movies, 2000)] {
        let feed = parse_feed(&plain, &base, domain).unwrap();
        assert_eq!(feed.items[0].metadata.categories, [expected]);
    }
    let explicit = one(
        r#"<item><title>X</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/x</link><torznab:attr name="category" value="2040"/><torznab:attr name="category" value="5030"/></item>"#,
    );
    assert_eq!(explicit.metadata.categories, [2040, 5030]);
    let feed = parse(
        r#"<item><title>bad</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/b</link><torznab:attr name="category" value="movies"/></item>
           <item><title>ok</title><pubDate>2024-01-01T12:00:00Z</pubDate><link>https://t.example/o</link></item>"#,
    )
    .unwrap();
    assert_eq!((feed.items.len(), feed.warnings.len()), (1, 1));
}
