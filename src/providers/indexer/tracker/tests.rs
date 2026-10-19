//! Parser and request-shape tests for the tracker indexers. The fixtures follow each tracker's
//! documented response format as remembered while implementing; no real tracker was contacted.
use super::*;
use crate::providers::TrackerScope;

fn scope(categories: Vec<u32>) -> TrackerScope {
    TrackerScope {
        enable_rss: true,
        enable_automatic_search: Some(true),
        enable_interactive_search: Some(true),
        minimum_seeders: None,
        categories,
    }
}
fn rss(domain: MediaDomain) -> IndexerSearch {
    IndexerSearch::Rss {
        media_type: domain,
        offset: 0,
        query_index: 0,
        limit: 10,
    }
}
fn episode() -> IndexerSearch {
    serde_json::from_value(
        json!({"kind":"tv","title":"The Show: Reborn!","tvdb_id":4242,
        "numbering":{"kind":"episode","season":2,"episode":5}}),
    )
    .unwrap()
}
fn anime() -> IndexerSearch {
    serde_json::from_value(
        json!({"kind":"tv","title":"Some Anime","numbering":{"kind":"anime","absolute_episode":7}}),
    )
    .unwrap()
}
fn movie() -> IndexerSearch {
    serde_json::from_value(
        json!({"kind":"movie","title":"Harbor Lights","year":2020,"imdb_id":"tt0123456"}),
    )
    .unwrap()
}
fn titled_movie() -> IndexerSearch {
    serde_json::from_value(json!({"kind":"movie","title":"Harbor Lights","year":2020})).unwrap()
}

#[test]
fn query_text_is_cleaned_and_numbered() {
    assert_eq!(
        tv_text(
            "The Show: Reborn!",
            &TvNumbering::Episode {
                season: 2,
                episode: 5
            }
        )
        .unwrap(),
        "Show Reborn S02E05"
    );
    assert_eq!(
        tv_text(
            "Some Anime",
            &TvNumbering::Anime {
                absolute_episode: 7,
                season: None,
                episode: None
            }
        )
        .unwrap(),
        "Some Anime 07"
    );
    assert_eq!(
        movie_text("Harbor Lights", Some(2020)).unwrap(),
        "Harbor Lights 2020"
    );
    assert!(query_text("   ").is_err());
    assert!(query_text(&"x".repeat(QUERY_BYTES + 1)).is_err());
}

#[test]
fn btn_criteria_prefer_ids_and_encode_numbering() {
    assert_eq!(btn_criteria(&rss(MediaDomain::Tv)).unwrap(), json!({}));
    assert_eq!(
        btn_criteria(&episode()).unwrap(),
        json!({"tvdb":4242,"name":"S02E05","category":"Episode"})
    );
    let season: IndexerSearch = serde_json::from_value(
        json!({"kind":"tv","title":"Show","numbering":{"kind":"season","season":3}}),
    )
    .unwrap();
    assert_eq!(
        btn_criteria(&season).unwrap(),
        json!({"series":"Show","name":"Season 3","category":"Season"})
    );
    let daily: IndexerSearch = serde_json::from_value(json!({"kind":"tv","title":"Late Night","rage_id":9,"numbering":{"kind":"daily","date":"2024-05-01"}})).unwrap();
    assert_eq!(
        btn_criteria(&daily).unwrap(),
        json!({"tvrage":9,"name":"2024.05.01","category":"Episode"})
    );
    // No absolute numbering exists: the series alone is searched.
    assert_eq!(
        btn_criteria(&anime()).unwrap(),
        json!({"series":"Some Anime"})
    );
    assert!(matches!(
        btn_criteria(&movie()),
        Err(IndexerError::Unsupported)
    ));
}

#[test]
fn btn_response_maps_flags_and_drops_bad_rows() {
    let body = json!({"id":"hrrdarr","result":{"results":"3","torrents":{
        "1001":{"GroupName":"S02E05","GroupID":"77","TorrentID":"1001","SeriesID":"5","Series":"The Show","Category":"Episode",
            "Snatched":"12","Seeders":"31","Leechers":"2","Source":"WEB-DL","Container":"MKV","Codec":"H.264","Resolution":"1080p",
            "Origin":"Scene","ReleaseName":"The.Show.S02E05.1080p.WEB-DL.H.264-GRP","Size":"1610612736","Time":"1704067200",
            "TvdbID":"4242","TvrageID":"0","ImdbID":"1234567","InfoHash":"0123456789ABCDEF0123456789ABCDEF01234567",
            "DownloadURL":"https://btn.example/torrents.php?action=download&id=1001&authkey=PRIVATE_AUTH&torrent_pass=PRIVATE_PASS"},
        "1002":{"TorrentID":"1002","Seeders":0,"Leechers":0,"Resolution":"SD","Origin":"Internal","ReleaseName":"The.Show.S02E06.SD","Size":734003200,"Time":1704153600,
            "DownloadURL":"https://btn.example/torrents.php?action=download&id=1002"},
        "1003":{"TorrentID":"1003","ReleaseName":"No.Time.Or.Link"}}}});
    let (items, warnings) = parse_btn(body.to_string().as_bytes(), MediaDomain::Tv).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(warnings.len(), 1);
    let first = items
        .iter()
        .find(|r| r.guid.as_deref() == Some("broadcasthenet:1001"))
        .unwrap();
    assert_eq!(
        first.metadata.title.as_deref(),
        Some("The.Show.S02E05.1080p.WEB-DL.H.264-GRP")
    );
    assert_eq!(first.metadata.size_bytes, Some(1_610_612_736));
    assert_eq!(first.metadata.published_at, "2024-01-01T00:00:00Z");
    assert_eq!(first.metadata.seeders, Some(31));
    assert_eq!(first.metadata.peers, Some(33));
    assert_eq!(first.metadata.categories, vec![5040]);
    assert_eq!(first.facts.scene, Some(true));
    let torrent = first.facts.torrent.as_ref().unwrap();
    assert_eq!(
        torrent.info_hash.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(torrent.internal, Some(false));
    assert!(
        matches!(&first.facts.identifiers, ReleaseIdentifiers::Tv { tvdb_id: Some(4242), imdb_id: Some(i), .. } if i == "tt1234567")
    );
    let second = items
        .iter()
        .find(|r| r.guid.as_deref() == Some("broadcasthenet:1002"))
        .unwrap();
    assert_eq!(second.metadata.categories, vec![5030]);
    assert_eq!(second.facts.torrent.as_ref().unwrap().internal, Some(true));
}

#[test]
fn btn_empty_results_and_error_codes() {
    // PHP serializes an empty map as [].
    let empty = json!({"result":{"results":"0","torrents":[]}}).to_string();
    let (items, warnings) = parse_btn(empty.as_bytes(), MediaDomain::Tv).unwrap();
    assert!(items.is_empty() && warnings.is_empty());
    let auth = json!({"error":{"code":-32001,"message":"Invalid API Key"}}).to_string();
    assert!(matches!(
        parse_btn(auth.as_bytes(), MediaDomain::Tv),
        Err(IndexerError::Authentication)
    ));
    let limit = json!({"error":{"code":-32002,"message":"Call Limit Exceeded"}}).to_string();
    assert!(matches!(
        parse_btn(limit.as_bytes(), MediaDomain::Tv),
        Err(IndexerError::RateLimited {
            retry_after_seconds: Some(BTN_CALL_LIMIT_SECONDS)
        })
    ));
    let other = json!({"error":{"code":-1,"message":"?"}}).to_string();
    assert!(matches!(
        parse_btn(other.as_bytes(), MediaDomain::Tv),
        Err(IndexerError::InvalidResponse)
    ));
    assert!(matches!(
        parse_btn(b"<html>", MediaDomain::Tv),
        Err(IndexerError::InvalidResponse)
    ));
    // Every row invalid: the document is unusable, not "empty".
    let bad = json!({"result":{"torrents":{"1":{"TorrentID":"1"}}}}).to_string();
    assert!(matches!(
        parse_btn(bad.as_bytes(), MediaDomain::Tv),
        Err(IndexerError::InvalidResponse)
    ));
}

#[test]
fn filelist_request_shapes() {
    let tv = scope(vec![21, 23, 27]);
    let t = Target {
        kind: TrackerKind::Filelist,
        endpoint: "https://filelist.example/api.php",
        scope: &tv,
        domain: MediaDomain::Tv,
    };
    let latest = filelist_params(&t, &rss(MediaDomain::Tv)).unwrap();
    assert_eq!(
        latest,
        vec![
            ("action".to_string(), "latest-torrents".to_string()),
            ("category".to_string(), "21,23,27".to_string())
        ]
    );
    let search: BTreeMap<_, _> = filelist_params(&t, &episode())
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(search["action"], "search-torrents");
    assert_eq!(search["type"], "name");
    assert_eq!(search["query"], "Show Reborn");
    assert_eq!(
        (search["season"].as_str(), search["episode"].as_str()),
        ("2", "5")
    );
    let movies = scope(vec![]);
    let t = Target {
        kind: TrackerKind::Filelist,
        endpoint: "https://filelist.example/api.php",
        scope: &movies,
        domain: MediaDomain::Movies,
    };
    let by_id: BTreeMap<_, _> = filelist_params(&t, &movie()).unwrap().into_iter().collect();
    assert_eq!(
        (by_id["type"].as_str(), by_id["query"].as_str()),
        ("imdb", "tt0123456")
    );
    assert!(!by_id.contains_key("category"));
    let by_name: BTreeMap<_, _> = filelist_params(&t, &titled_movie())
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(
        (by_name["type"].as_str(), by_name["query"].as_str()),
        ("name", "Harbor Lights 2020")
    );
}

#[test]
fn filelist_response_converts_romanian_local_time() {
    let body = json!([
        {"id":501,"name":"Harbor.2020.1080p.BluRay.x264","imdb":"tt0123456","freeleech":1,"internal":0,"moderated":1,
         "upload_date":"2024-07-01 12:00:00","download_link":"https://filelist.example/download.php?id=501&passkey=PRIVATE_PASSKEY",
         "size":4294967296u64,"leechers":3,"seeders":40,"times_completed":9,"comments":0,"files":3,"category":"Filme HD","small_description":""},
        {"id":502,"name":"Harbor.Winter.2020.720p","imdb":null,"freeleech":0,"internal":1,"upload_date":"2024-01-01 12:00:00",
         "download_link":"https://filelist.example/download.php?id=502&passkey=PRIVATE_PASSKEY","size":1073741824,"leechers":0,"seeders":1,"category":"Filme HD"},
        {"id":503,"name":"Broken","upload_date":"not a date","download_link":"https://filelist.example/x"}
    ]).to_string();
    let (items, warnings) = parse_filelist(
        body.as_bytes(),
        MediaDomain::Movies,
        "https://filelist.example",
    )
    .unwrap();
    assert_eq!((items.len(), warnings.len()), (2, 1));
    assert_eq!(items[0].metadata.published_at, "2024-07-01T09:00:00Z");
    assert_eq!(items[1].metadata.published_at, "2024-01-01T10:00:00Z");
    assert_eq!(
        items[0]
            .facts
            .torrent
            .as_ref()
            .unwrap()
            .download_volume_factor,
        Some(0.0)
    );
    assert_eq!(
        items[1].facts.torrent.as_ref().unwrap().internal,
        Some(true)
    );
    assert_eq!(
        items[0].facts.info_url.as_deref(),
        Some("https://filelist.example/details.php?id=501")
    );
    assert_eq!(items[0].metadata.categories, vec![MOVIE_ROOT]);
    assert!(
        matches!(&items[0].facts.identifiers, ReleaseIdentifiers::Movie { imdb_id: Some(i), .. } if i == "tt0123456")
    );
    let denied = json!({"error":"Invalid credentials"}).to_string();
    assert!(matches!(
        parse_filelist(denied.as_bytes(), MediaDomain::Movies, "https://x"),
        Err(IndexerError::Authentication)
    ));
}

#[test]
fn bucharest_time_follows_the_european_summer_time_rules() {
    // 2024: summer time starts 2024-03-31 01:00 UTC and ends 2024-10-27 01:00 UTC.
    assert_eq!(
        bucharest_date("2024-03-30 12:00:00").unwrap(),
        "2024-03-30T10:00:00Z"
    );
    assert_eq!(
        bucharest_date("2024-03-31 12:00:00").unwrap(),
        "2024-03-31T09:00:00Z"
    );
    assert_eq!(
        bucharest_date("2024-10-26 12:00:00").unwrap(),
        "2024-10-26T09:00:00Z"
    );
    assert_eq!(
        bucharest_date("2024-10-27 12:00:00").unwrap(),
        "2024-10-27T10:00:00Z"
    );
    assert!(bucharest_date("yesterday").is_err());
}

#[test]
fn hdbits_request_body_uses_ids_then_text() {
    let tv = scope(vec![2]);
    let t = Target {
        kind: TrackerKind::Hdbits,
        endpoint: "https://hdb.example/api/torrents",
        scope: &tv,
        domain: MediaDomain::Tv,
    };
    let access = IndexerAccess {
        username: Some("alice"),
        password: Some("PRIVATE_PASSKEY"),
        ..IndexerAccess::default()
    };
    let latest = hdbits_body(&t, &access, &rss(MediaDomain::Tv), 40).unwrap();
    assert_eq!(
        latest,
        json!({"username":"alice","passkey":"PRIVATE_PASSKEY","limit":40,"category":[2]})
    );
    let search = hdbits_body(&t, &access, &episode(), 100).unwrap();
    assert_eq!(search["tvdb"], json!({"id":4242,"season":2,"episode":5}));
    assert!(search.get("search").is_none());
    let by_text = hdbits_body(&t, &access, &anime(), 100).unwrap();
    assert_eq!(by_text["search"], "Some Anime 07");
    let movies = scope(vec![1]);
    let t = Target {
        kind: TrackerKind::Hdbits,
        endpoint: "https://hdb.example/api/torrents",
        scope: &movies,
        domain: MediaDomain::Movies,
    };
    assert_eq!(
        hdbits_body(&t, &access, &movie(), 100).unwrap()["imdb"],
        json!({"id":123456})
    );
    assert_eq!(
        hdbits_body(&t, &access, &titled_movie(), 100).unwrap()["search"],
        "Harbor Lights 2020"
    );
    assert!(matches!(
        hdbits_body(&t, &IndexerAccess::default(), &movie(), 100),
        Err(IndexerError::Authentication)
    ));
}

#[test]
fn hdbits_response_status_and_rows() {
    let body = json!({"status":0,"data":[
        {"id":9001,"hash":"abcdefabcdefabcdefabcdefabcdefabcdefabcd","leechers":1,"seeders":25,"name":"Harbor.2020.1080p.BluRay.REMUX","times_completed":3,
         "size":30000000000u64,"utadded":1704067200,"added":"2024-01-01T00:00:00+0000","comments":0,"numfiles":1,"filename":"x.torrent",
         "type_category":1,"type_codec":1,"type_medium":5,"type_origin":1,"imdb":{"id":123456,"englishtitle":"Harbor","year":2020},"tvdb":{"id":0,"season":0,"episode":0},"freeleech":"yes","torrent_status":"all"},
        {"id":9002,"hash":"short","name":"Bad.Hash","utadded":1704067200}
    ]}).to_string();
    let (items, warnings) = parse_hdbits(
        body.as_bytes(),
        MediaDomain::Movies,
        "https://hdb.example",
        "PRIVATE_PASSKEY",
    )
    .unwrap();
    assert_eq!((items.len(), warnings.len()), (1, 1));
    let item = &items[0];
    assert_eq!(
        item.download_url,
        "https://hdb.example/download.php?id=9001&passkey=PRIVATE_PASSKEY"
    );
    assert_eq!(item.facts.torrent.as_ref().unwrap().internal, Some(true));
    assert_eq!(item.facts.scene, Some(false));
    assert_eq!(
        item.facts.torrent.as_ref().unwrap().download_volume_factor,
        Some(0.0)
    );
    assert!(
        matches!(&item.facts.identifiers, ReleaseIdentifiers::Movie { imdb_id: Some(i), .. } if i == "tt0123456")
    );
    for status in [4, 5] {
        let denied = json!({"status":status,"message":"nope"}).to_string();
        assert!(matches!(
            parse_hdbits(denied.as_bytes(), MediaDomain::Tv, "https://x", "k"),
            Err(IndexerError::Authentication)
        ));
    }
    let failure = json!({"status":1}).to_string();
    assert!(matches!(
        parse_hdbits(failure.as_bytes(), MediaDomain::Tv, "https://x", "k"),
        Err(IndexerError::InvalidResponse)
    ));
}

#[test]
fn ptp_request_and_response() {
    let params: BTreeMap<_, _> = ptp_params(&movie()).unwrap().into_iter().collect();
    assert_eq!(params["searchstr"], "tt0123456");
    assert_eq!(params["json"], "noredirect");
    assert!(matches!(
        ptp_params(&episode()),
        Err(IndexerError::Unsupported)
    ));
    let body = json!({"TotalResults":"2","Page":"1","AuthKey":"PRIVATE_AUTHKEY","PassKey":"PRIVATE_PASSKEY","Movies":[
        {"GroupId":"31337","Title":"Harbor Lights","Year":"2020","ImdbId":"0123456","Torrents":[
            {"Id":"555","ReleaseName":"Harbor.Lights.2020.1080p.BluRay.x264-GRP","Quality":"High Definition","Source":"Blu-ray","Codec":"x264","Resolution":"1080p",
             "Scene":true,"Size":"8589934592","UploadTime":"2024-01-01 12:00:00","Seeders":"20","Leechers":"1","Snatched":"4","FreeleechType":"Freeleech","GoldenPopcorn":false,"Checked":true},
            {"Id":"556","Source":"DVD","Codec":"XviD","Resolution":"SD","Size":"734003200","UploadTime":"2023-06-01 08:30:00","Seeders":"2","Leechers":"0"}]},
        {"GroupId":"1","Title":"Skipped","Year":"2001","Torrents":[{"Id":"9","UploadTime":"broken"}]}]}).to_string();
    let (items, warnings) =
        parse_ptp(body.as_bytes(), MediaDomain::Movies, "https://ptp.example").unwrap();
    assert_eq!((items.len(), warnings.len()), (2, 1));
    assert_eq!(
        items[0].metadata.title.as_deref(),
        Some("Harbor.Lights.2020.1080p.BluRay.x264-GRP")
    );
    assert_eq!(
        items[0].download_url,
        "https://ptp.example/torrents.php?action=download&id=555&authkey=PRIVATE_AUTHKEY&torrent_pass=PRIVATE_PASSKEY"
    );
    assert_eq!(items[0].facts.scene, Some(true));
    assert_eq!(
        items[0]
            .facts
            .torrent
            .as_ref()
            .unwrap()
            .download_volume_factor,
        Some(0.0)
    );
    // Without a release name the title is rebuilt from the group.
    assert_eq!(
        items[1].metadata.title.as_deref(),
        Some("Harbor Lights 2020 SD DVD XviD")
    );
    assert_eq!(items[1].metadata.published_at, "2023-06-01T08:30:00Z");
    let none = json!({"TotalResults":"0","Movies":[]}).to_string();
    assert!(
        parse_ptp(none.as_bytes(), MediaDomain::Movies, "https://x")
            .unwrap()
            .0
            .is_empty()
    );
    // A document with torrents but no keys cannot build downloads: every row fails.
    let keyless = json!({"Movies":[{"GroupId":"1","Title":"T","Torrents":[{"Id":"1","UploadTime":"2024-01-01 00:00:00","ReleaseName":"x"}]}]}).to_string();
    assert!(parse_ptp(keyless.as_bytes(), MediaDomain::Movies, "https://x").is_err());
}

#[test]
fn torrentpotato_response_uses_megabytes() {
    let body = json!({"results":[
        {"release_name":"Harbor.2020.720p.BluRay","torrent_id":"abc-1","details_url":"https://potato.example/details/1","download_url":"https://potato.example/dl/1?passkey=PRIVATE_PASSKEY",
         "freeleech":false,"type":"movie","size":1400,"leechers":2,"seeders":9,"publish_date":"2024-01-01T10:30:00Z","imdb_id":"tt0123456"},
        {"release_name":"No.Link","torrent_id":"2","size":1,"publish_date":"2024-01-01T10:30:00Z"}],"total_results":2}).to_string();
    let (items, warnings) = parse_potato(body.as_bytes(), MediaDomain::Movies).unwrap();
    assert_eq!((items.len(), warnings.len()), (1, 1));
    assert_eq!(items[0].metadata.size_bytes, Some(1400 * 1024 * 1024));
    assert_eq!(items[0].metadata.published_at, "2024-01-01T10:30:00Z");
    assert_eq!(
        items[0].facts.info_url.as_deref(),
        Some("https://potato.example/details/1")
    );
    assert!(
        parse_potato(
            json!({"results":[]}).to_string().as_bytes(),
            MediaDomain::Movies
        )
        .unwrap()
        .0
        .is_empty()
    );
    assert!(matches!(
        parse_potato(
            json!({"error":"bad passkey"}).to_string().as_bytes(),
            MediaDomain::Movies
        ),
        Err(IndexerError::Authentication)
    ));
}

#[test]
fn fanzub_feed_yields_usenet_releases() {
    let base = url::Url::parse("https://fanzub.example/rss").unwrap();
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?><rss version="2.0"><channel><title>Fanzub</title>
      <item><title>[Group] Some Anime - 07 [720p]</title><link>https://fanzub.example/browse/771</link><guid isPermaLink="true">https://fanzub.example/browse/771</guid>
        <pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><enclosure url="https://fanzub.example/nzb/771.nzb?x=1" length="367001600" type="application/x-nzb"/></item>
      <item><title>No Enclosure</title><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate></item>
      <item><title>Torrent Only</title><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><enclosure url="https://fanzub.example/t.torrent" type="application/x-bittorrent" length="5"/></item>
    </channel></rss>"#;
    let (items, warnings) = parse_nzb_feed(xml, &base, MediaDomain::Tv).unwrap();
    assert_eq!((items.len(), warnings.len()), (1, 2));
    let item = &items[0];
    assert_eq!(item.metadata.size_bytes, Some(367_001_600));
    assert_eq!(item.metadata.categories, vec![TV_ANIME]);
    assert!(
        item.facts.torrent.is_none(),
        "an NZB release is not a torrent"
    );
    assert_eq!(item.download_url, "https://fanzub.example/nzb/771.nzb?x=1");
    assert_eq!(
        item.facts.info_url.as_deref(),
        Some("https://fanzub.example/browse/771")
    );
}

#[test]
fn nyaa_feed_reads_the_nyaa_namespace() {
    let base = url::Url::parse("https://nyaa.example/").unwrap();
    let xml = r#"<?xml version="1.0" encoding="utf-8"?><rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom" xmlns:nyaa="https://nyaa.si/xmlns/nyaa"><channel><title>Nyaa - Home - Torrent File RSS</title>
      <item><title>[Group] Some Anime - 07 (1080p) [ABCD1234].mkv</title><link>https://nyaa.example/download/1234.torrent</link><guid isPermaLink="true">https://nyaa.example/view/1234</guid>
        <pubDate>Mon, 01 Jan 2024 12:00:00 -0000</pubDate><nyaa:seeders>120</nyaa:seeders><nyaa:leechers>4</nyaa:leechers><nyaa:downloads>900</nyaa:downloads>
        <nyaa:infoHash>ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD</nyaa:infoHash><nyaa:categoryId>1_2</nyaa:categoryId><nyaa:category>Anime - English-translated</nyaa:category>
        <nyaa:size>1.4 GiB</nyaa:size><nyaa:comments>2</nyaa:comments><nyaa:trusted>Yes</nyaa:trusted><nyaa:remake>No</nyaa:remake><description>#1234 | desc</description></item></channel></rss>"#;
    let parsed = feed::parse_feed_with(
        xml,
        &base,
        MediaDomain::Tv,
        FeedOptions {
            description_size: false,
            default_category: Some(TV_ANIME),
        },
    )
    .unwrap();
    let item = &parsed.items[0];
    assert_eq!(item.metadata.seeders, Some(120));
    assert_eq!(item.metadata.leechers, Some(4));
    assert_eq!(item.metadata.peers, Some(124));
    assert_eq!(
        item.metadata.size_bytes,
        Some((1.4 * 1024.0 * 1024.0 * 1024.0) as u64)
    );
    assert_eq!(item.metadata.categories, vec![TV_ANIME]);
    assert_eq!(
        item.facts.torrent.as_ref().unwrap().info_hash.as_deref(),
        Some("abcdefabcdefabcdefabcdefabcdefabcdefabcd")
    );
    assert_eq!(
        item.download_url,
        "https://nyaa.example/download/1234.torrent"
    );
}

#[test]
fn private_tracker_feeds_read_sizes_from_descriptions() {
    assert_eq!(
        feed::description_size("1.5 GB; TV/Web-DL"),
        Some((1.5 * 1024.0 * 1024.0 * 1024.0) as u64)
    );
    assert_eq!(
        feed::description_size("Category: TV :: Episodes HD - Size: 700 MB"),
        Some(700 * 1024 * 1024)
    );
    assert_eq!(feed::description_size("no size here; TV"), None);
    assert_eq!(feed::description_size("Size: 0 B"), None);
    let base = url::Url::parse("https://iptorrents.example/t.rss").unwrap();
    let xml = r#"<rss version="2.0"><channel><title>IPT</title>
      <item><title>Show.S01E02.720p.HDTV.x264-GRP</title><link>https://iptorrents.example/download.php/55/Show.S01E02.torrent?torrent_pass=PRIVATE_PASSKEY</link>
        <description>800.5 MB; TV/x264</description><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><guid>https://iptorrents.example/details.php?id=55</guid></item></channel></rss>"#;
    let parsed = feed::parse_feed_with(
        xml,
        &base,
        MediaDomain::Tv,
        FeedOptions {
            description_size: true,
            default_category: None,
        },
    )
    .unwrap();
    assert_eq!(
        parsed.items[0].metadata.size_bytes,
        Some((800.5 * 1024.0 * 1024.0) as u64)
    );
    assert_eq!(parsed.items[0].metadata.categories, vec![5000]);
}

#[test]
fn row_assembly_rejects_unsafe_locators_and_bad_hashes() {
    let row = |url: &str, hash: Option<&str>, seeders: u32| Row {
        kind: TrackerKind::Filelist,
        id: "1".into(),
        title: "Title".into(),
        size: Some(1),
        published_at: "2024-01-01T00:00:00Z".into(),
        seeders: Some(seeders),
        leechers: Some(1),
        download_url: url.into(),
        info_url: None,
        info_hash: hash.map(str::to_owned),
        imdb: None,
        tvdb: None,
        free: None,
        internal: None,
        scene: None,
        category: MOVIE_ROOT,
    };
    // Seeders plus leechers overflow is an invalid row, not a wrapped number.
    assert!(
        build(
            row("https://t.example/x", None, u32::MAX),
            MediaDomain::Movies
        )
        .is_err()
    );
    let ok = |url: &str, hash: Option<&str>| build(row(url, hash, 1), MediaDomain::Movies);
    assert!(ok("https://t.example/x", None).is_ok());
    assert!(ok("ftp://t.example/x", None).is_err());
    assert!(ok("javascript:alert(1)", None).is_err());
    assert!(ok("https://user:pw@t.example/x", None).is_err());
    assert!(ok("https://t.example/x", Some("zz")).is_err());
}
