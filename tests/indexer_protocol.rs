use hrrdarr::{
    api::MediaDomain,
    providers::{
        ProviderSettings,
        indexer::{self, IndexerError, IndexerSearch, TvNumbering},
    },
};
use serde_json::json;
use std::collections::BTreeMap;

const CAPS: &str = r#"<caps><limits max="100" default="50"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,tvmazeid,rid,imdbid,season,ep"/><movie-search available="yes" supportedParams="q,imdbid,tmdbid,year"/></searching><categories><category id="2000"><subcat id="2040"/></category><category id="5000"><subcat id="5030"/><subcat id="5070"/></category><category id="100001"/></categories></caps>"#;
fn config(kind: &str, endpoint: &str) -> ProviderSettings {
    serde_json::from_value(json!({"implementation":kind,"endpoint":endpoint,"tv":{"categories":[5030],"anime_categories":[5070]},"movies":{"categories":[2040]}})).unwrap()
}
fn tv(numbering: TvNumbering) -> IndexerSearch {
    IndexerSearch::Tv {
        title: "A & B".into(),
        aliases: vec![],
        tmdb_id: None,
        search_mode: indexer::TvSearchMode::Default,
        tvdb_id: Some(12),
        tvmaze_id: Some(13),
        rage_id: None,
        imdb_id: Some("tt1234567".into()),
        numbering,
        offset: 0,
        query_index: 0,
        limit: 100,
    }
}
fn query(
    settings: &ProviderSettings,
    caps: &str,
    request: &IndexerSearch,
) -> BTreeMap<String, String> {
    indexer::plan_search(
        settings,
        &indexer::parse_capabilities(caps).unwrap(),
        request,
    )
    .unwrap()
    .parameters
    .into_iter()
    .collect()
}
#[test]
fn tv_movie_modes_capability_fallbacks_and_scopes() {
    for kind in ["torznab", "newznab"] {
        let settings = config(kind, "https://indexer.invalid/api");
        for (numbering, season, ep) in [
            (
                TvNumbering::Episode {
                    season: 0,
                    episode: 2,
                },
                "00",
                Some("2"),
            ),
            (TvNumbering::Season { season: 3 }, "3", None),
            (
                TvNumbering::Daily {
                    date: "2024-02-29".into(),
                },
                "2024",
                Some("02/29"),
            ),
        ] {
            let q = query(&settings, CAPS, &tv(numbering));
            assert_eq!(q["t"], "tvsearch");
            assert_eq!(q["tvdbid"], "12");
            assert_eq!(q["season"], season);
            assert_eq!(q.get("ep").map(String::as_str), ep);
            assert_eq!(q["cat"], "5030");
        }
        let q = query(
            &settings,
            CAPS,
            &tv(TvNumbering::Anime {
                absolute_episode: 123,
                season: None,
                episode: None,
            }),
        );
        // Advertised TV q+ID support carries the absolute selector without adding a season.
        assert_eq!(q["t"], "tvsearch");
        assert_eq!(q["q"], "123");
        assert_eq!(q["cat"], "5070");
        let q = query(
            &settings,
            CAPS,
            &tv(TvNumbering::DailySeason { year: 2024 }),
        );
        assert_eq!(q["season"], "2024");
        assert!(!q.contains_key("ep"));
        let q = query(
            &settings,
            CAPS,
            &tv(TvNumbering::Special {
                episode_title: "Holiday".into(),
            }),
        );
        assert_eq!(q["t"], "search");
        assert_eq!(q["q"], "A and B Holiday");
        for (supported, expected_key, expected_id) in [
            ("q,tvmazeid,rid,imdbid,season,ep", "tvmazeid", "13"),
            ("q,rid,imdbid,season,ep", "rid", "14"),
            ("q,imdbid,season,ep", "imdbid", "tt1234567"),
        ] {
            let mut request = tv(TvNumbering::Episode {
                season: 1,
                episode: 2,
            });
            if let IndexerSearch::Tv { rage_id, .. } = &mut request {
                *rage_id = Some(14);
            }
            let q = query(
                &settings,
                &CAPS.replace("q,tvdbid,tvmazeid,rid,imdbid,season,ep", supported),
                &request,
            );
            assert_eq!(q[expected_key], expected_id);
        }
        let no_ids = CAPS.replace("q,tvdbid,tvmazeid,rid,imdbid,season,ep", "q,season,ep");
        let q = query(
            &settings,
            &no_ids,
            &tv(TvNumbering::Episode {
                season: 1,
                episode: 2,
            }),
        );
        assert_eq!(q["q"], "A and B");
        assert!(!q.contains_key("tvdbid"));
        let generic = CAPS.replace("q,tvdbid,tvmazeid,rid,imdbid,season,ep", "q");
        let q = query(
            &settings,
            &generic,
            &tv(TvNumbering::Episode {
                season: 1,
                episode: 2,
            }),
        );
        assert_eq!(q["t"], "search");
        assert_eq!(q["q"], "A and B S01E02");
        let movie = IndexerSearch::Movie {
            title: "Remake".into(),
            aliases: vec![],
            year: Some(2020),
            imdb_id: Some("tt0012345".into()),
            tmdb_id: Some(42),
            offset: 100,
            query_index: 0,
            limit: 200,
        };
        let q = query(&settings, CAPS, &movie);
        assert_eq!(q["t"], "movie");
        assert_eq!(q["imdbid"], "0012345");
        assert_eq!(q["cat"], "2040");
        assert_eq!(q["offset"], "100");
        assert_eq!(q["limit"], "100");
        let q = query(
            &settings,
            &CAPS.replace("q,imdbid,tmdbid,year", "q,tmdbid,year"),
            &movie,
        );
        assert_eq!(q["tmdbid"], "42");
        let q = query(
            &settings,
            &CAPS.replace("q,imdbid,tmdbid,year", "q,year"),
            &movie,
        );
        // Movie fallback uses generic title+year irrespective of movie q/year capability.
        assert_eq!(q["q"], "Remake 2020");
        assert_eq!(q["t"], "search");
        let q = query(
            &settings,
            &CAPS.replace("q,imdbid,tmdbid,year", "q"),
            &movie,
        );
        assert_eq!(q["q"], "Remake 2020");
        let q = query(
            &settings,
            &CAPS.replace(
                "<movie-search available=\"yes\"",
                "<movie-search available=\"no\"",
            ),
            &movie,
        );
        assert_eq!(q["t"], "search");
        assert_eq!(q["q"], "Remake 2020");
        let q = query(
            &settings,
            CAPS,
            &IndexerSearch::Rss {
                media_type: MediaDomain::Tv,
                offset: 0,
                query_index: 0,
                limit: 1,
            },
        );
        assert_eq!(q["cat"], "5030,5070");
        assert!(!q.contains_key("q"));
        assert!(matches!(
            indexer::plan_search(
                &settings,
                &indexer::parse_capabilities(CAPS).unwrap(),
                &tv(TvNumbering::Daily {
                    date: "2023-02-29".into()
                })
            ),
            Err(IndexerError::InvalidRequest)
        ));
        let unsupported = CAPS.replace("id=\"5030\"", "id=\"5040\"");
        // TV configured categories remain valid requests even when capabilities omit them.
        assert!(
            indexer::plan_search(
                &settings,
                &indexer::parse_capabilities(&unsupported).unwrap(),
                &tv(TvNumbering::Season { season: 2 })
            )
            .is_ok()
        );
    }
}
fn feed(namespace: &str, offset: u32, total: u32) -> String {
    format!(
        r#"<rss xmlns:x="{namespace}"><channel><x:response offset="{offset}" total="{total}"/><item><title>A &amp; B 2020</title><guid>opaque-private-key</guid><pubDate>Tue, 22 Jun 2010 06:54:22 +0100</pubDate><enclosure url="https://indexer.invalid/get?apikey=PRIVATE" length="123"/><x:attr name="category" value="2040"/><x:attr name="category" value="2040"/><x:attr name="category" value="2000"/><x:attr name="size" value="4294967295"/><x:attr name="seeders" value="3"/><x:attr name="peers" value="5"/><x:attr name="language" value="English"/><x:attr name="language" value="Icelandic"/><x:attr name="minimumseedtime" value="3600"/></item></channel></rss>"#
    )
}
#[test]
fn xml_namespaces_metadata_paging_and_invalid_inputs() {
    for (namespace, torrent) in [
        ("http://torznab.com/schemas/2015/feed", true),
        ("http://www.newznab.com/DTD/2010/feeds/attributes/", false),
    ] {
        let body = feed(namespace, 50, 100);
        let page = indexer::parse_page(&body, 50, 1, torrent, MediaDomain::Movies).unwrap();
        assert_eq!(page.next_offset, Some(51));
        assert_eq!(page.total, Some(100));
        assert_eq!(page.items[0].metadata.size_bytes, Some(4294967295));
        assert_eq!(page.items[0].metadata.leechers, Some(2));
        assert_eq!(page.items[0].metadata.categories, vec![2040, 2000]);
        assert_eq!(
            page.items[0].metadata.languages,
            vec!["English", "Icelandic"]
        );
        assert_eq!(page.items[0].metadata.published_at, "2010-06-22T05:54:22Z");
        assert_eq!(page.items[0].attributes["minimumseedtime"], vec!["3600"]);
        assert!(page.items[0].download_url.contains("PRIVATE"));
        let wrong_mime = body.replace(
            "<enclosure url=",
            if torrent {
                "<enclosure type=\"application/x-nzb\" url="
            } else {
                "<enclosure type=\"application/x-bittorrent\" url="
            },
        );
        assert!(indexer::parse_page(&wrong_mime, 50, 1, torrent, MediaDomain::Movies).is_err());
        let public = serde_json::to_string(&page.items[0].metadata).unwrap();
        assert!(!public.contains("PRIVATE"));
        assert!(!public.contains("opaque-private"));
        for invalid in [
            body.replace("offset=\"50\"", "offset=\"0\""),
            body.replace("value=\"2040\"", "value=\"-1\""),
            body.replace(
                "https://indexer.invalid/get?apikey=PRIVATE",
                "file:///etc/passwd",
            ),
        ] {
            assert!(indexer::parse_page(&invalid, 50, 1, torrent, MediaDomain::Movies).is_err());
        }
    }
    for xml in [
        "<html>login</html>",
        "<!DOCTYPE caps [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><caps>&x;</caps>",
        "<rss><channel><item/></channel></rss>",
    ] {
        assert!(indexer::parse_page(xml, 0, 100, true, MediaDomain::Movies).is_err());
    }
    assert!(matches!(
        indexer::parse_capabilities("<error code='100' description='SECRET'/ >"),
        Err(IndexerError::InvalidResponse)
    ));
    assert!(matches!(
        indexer::parse_capabilities("<error code='100' description='SECRET'/>"),
        Err(IndexerError::Authentication)
    ));
    assert!(matches!(
        indexer::parse_capabilities("<error code='500' description='SECRET' retry='120'/>"),
        Err(IndexerError::RateLimited {
            retry_after_seconds: Some(120)
        })
    ));
    assert!(matches!(
        indexer::parse_capabilities("<error code='203'/>"),
        Err(IndexerError::Unsupported)
    ));
    assert!(indexer::parse_capabilities(&CAPS.replace("max=\"100\"", "max=\"0\"")).is_err());
    assert!(indexer::parse_capabilities(&" ".repeat(1024 * 1024 + 1)).is_err());
    let empty = r#"<rss xmlns:n="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><n:response offset="100" total="10"/></channel></rss>"#;
    assert!(
        indexer::parse_page(empty, 100, 100, false, MediaDomain::Movies)
            .unwrap()
            .next_offset
            .is_none()
    );
}

#[tokio::test]
async fn owned_http_requests_authentication_and_redaction() {
    use axum::{
        Router,
        extract::{Query, State},
        routing::get,
    };
    use hrrdarr::providers::http::{HttpClient, HttpError};
    use std::sync::{Arc, Mutex};
    type Calls = Arc<Mutex<Vec<BTreeMap<String, String>>>>;
    async fn handler(
        State(calls): State<Calls>,
        Query(query): Query<BTreeMap<String, String>>,
    ) -> String {
        calls.lock().unwrap().push(query.clone());
        if query.get("apikey").map(String::as_str) != Some("PRIVATE & +/KEY") {
            // Missing-key errors are authentication failures despite the generic parameter code.
            return "<error code='200' description='Missing parameter apikey'/>".into();
        }
        if query["t"] == "caps" {
            CAPS.into()
        } else {
            feed(
                "http://torznab.com/schemas/2015/feed",
                query["offset"].parse().unwrap(),
                1000,
            )
            .replace(
                "A &amp; B 2020",
                &format!(
                    "A PRIVATE &amp; +/KEY {}",
                    query
                        .get("passkey")
                        .map(|s| s.replace('&', "&amp;"))
                        .unwrap_or_default()
                ),
            )
        }
    }
    let calls: Calls = Arc::new(Mutex::new(vec![]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/api", get(handler))
                .with_state(calls.clone()),
        )
        .into_future(),
    );
    let client = HttpClient::new().unwrap();
    let tv_private = [hrrdarr::providers::IndexerParameter {
        name: "passkey".into(),
        value: "TV_PRIVATE &+".into(),
    }];
    let movie_private = [hrrdarr::providers::IndexerParameter {
        name: "passkey".into(),
        value: "MOVIE_PRIVATE &+".into(),
    }];
    for kind in ["torznab", "newznab"] {
        let settings = config(kind, &format!("http://{address}/api"));
        let id = uuid::Uuid::new_v4();
        let operation = client.operation(id).unwrap();
        let tested = indexer::test(
            &operation,
            &settings,
            &hrrdarr::providers::IndexerAccess {
                api_key: Some("PRIVATE & +/KEY"),
                tv_parameters: &tv_private,
                movie_parameters: &movie_private,
            },
        )
        .await
        .unwrap();
        assert_eq!(tested.domains, vec![MediaDomain::Tv, MediaDomain::Movies]);
        let page = indexer::search(
            &operation,
            &settings,
            &hrrdarr::providers::IndexerAccess {
                api_key: Some("PRIVATE & +/KEY"),
                tv_parameters: &tv_private,
                movie_parameters: &movie_private,
            },
            &tv(TvNumbering::Episode {
                season: 0,
                episode: 2,
            }),
        )
        .await
        .unwrap();
        let public = serde_json::to_string(&page).unwrap();
        assert!(!public.contains("PRIVATE"));
        assert!(!public.contains("apikey"));
        assert_eq!(
            page.items[0].title.as_deref(),
            Some("A [redacted] [redacted]")
        );
        assert!(matches!(client.operation(id), Err(HttpError::Busy)));
        drop(operation);
        let failed = client.operation(id).unwrap();
        assert!(matches!(
            indexer::test(
                &failed,
                &settings,
                &hrrdarr::providers::IndexerAccess {
                    api_key: None,
                    tv_parameters: &[],
                    movie_parameters: &[]
                }
            )
            .await,
            Err(IndexerError::Authentication)
        ));
    }
    let queries = calls.lock().unwrap();
    assert_eq!(queries.len(), 12);
    for block in queries.chunks(6) {
        assert_eq!(block[0]["t"], "caps");
        assert_eq!(block[0]["apikey"], "PRIVATE & +/KEY");
        assert!(!block[0].contains_key("passkey"));
        assert_eq!(block[1]["passkey"], "TV_PRIVATE &+");
        assert_eq!(block[2]["passkey"], "MOVIE_PRIVATE &+");
        assert_eq!(block[4]["passkey"], "TV_PRIVATE &+");
        assert_eq!(block[1]["cat"], "5030,5070");
        assert_eq!(block[2]["cat"], "2040");
        assert_eq!(block[4]["t"], "tvsearch");
        // Explicit zero season avoids providers treating plain 0 as an absent selector.
        assert_eq!(block[4]["season"], "00");
        assert_eq!(block[4]["ep"], "2");
        assert!(!block[5].contains_key("apikey"));
    }
    drop(queries);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn fallback_alias_cursors_and_errors_keep_query_scope() {
    use axum::{
        Router,
        extract::{Query, State},
        routing::get,
    };
    use hrrdarr::providers::{IndexerAccess, http::HttpClient};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    };
    type StateData = (Arc<AtomicU8>, Arc<Mutex<Vec<BTreeMap<String, String>>>>);
    fn empty(offset: u32) -> String {
        format!(
            "<rss xmlns:n='http://www.newznab.com/DTD/2010/feeds/attributes/'><channel><n:response offset='{offset}' total='0'/></channel></rss>"
        )
    }
    async fn handler(
        State((scenario, calls)): State<StateData>,
        Query(q): Query<BTreeMap<String, String>>,
    ) -> String {
        calls.lock().unwrap().push(q.clone());
        if q["t"] == "caps" {
            return CAPS.replace(
                "q,tvdbid,tvmazeid,rid,imdbid,season,ep",
                "q,tvdbid,tvmazeid,rid,imdbid,tmdbid,season,ep",
            );
        }
        let offset = q["offset"].parse().unwrap();
        let ids = q.contains_key("tvdbid") || q.contains_key("tmdbid") || q.contains_key("imdbid");
        if ids {
            match scenario.load(Ordering::Relaxed) {
                0 | 3 => return empty(offset),
                2 => return "<error code='100' description='PRIVATE'/>".into(),
                _ => {}
            }
        }
        feed(
            "http://torznab.com/schemas/2015/feed",
            offset,
            if ids { 2 } else { 1 },
        )
    }
    let scenario = Arc::new(AtomicU8::new(0));
    let calls = Arc::new(Mutex::new(vec![]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/api", get(handler))
                .with_state((scenario.clone(), calls.clone())),
        )
        .into_future(),
    );
    let client = HttpClient::new().unwrap();
    let access = IndexerAccess {
        api_key: None,
        tv_parameters: &[],
        movie_parameters: &[],
    };
    for kind in ["torznab", "newznab"] {
        for domain in ["tv", "movie"] {
            let settings = config(kind, &format!("http://{addr}/api"));
            let base = if domain == "tv" {
                json!({"kind":"tv","title":"Main","aliases":["Main","Alias","Alias"],"tvdb_id":12,"tmdb_id":34,"imdb_id":"tt1234567","numbering":{"kind":"episode","season":1,"episode":2},"limit":1})
            } else {
                json!({"kind":"movie","title":"Main","aliases":["Main","Alias","Alias"],"tmdb_id":34,"imdb_id":"tt1234567","year":2020,"limit":1})
            };
            scenario.store(0, Ordering::Relaxed);
            calls.lock().unwrap().clear();
            let request: IndexerSearch = serde_json::from_value(base.clone()).unwrap();
            let page = indexer::search(
                &client.operation(uuid::Uuid::new_v4()).unwrap(),
                &settings,
                &access,
                &request,
            )
            .await
            .unwrap();
            assert_eq!(page.query_index, 1);
            assert_eq!(page.query_count, 3);
            assert_eq!(page.next_query.as_ref().unwrap().query_index, 2);
            assert_eq!(page.next_query.as_ref().unwrap().offset, 0);
            {
                let seen = calls.lock().unwrap();
                assert_eq!(seen.len(), 3);
                assert_eq!(seen[1]["tmdbid"], "34");
                assert_eq!(
                    seen[1]["imdbid"],
                    if domain == "tv" {
                        "tt1234567"
                    } else {
                        "1234567"
                    }
                );
                assert_eq!(seen[2]["cat"], if domain == "tv" { "5030" } else { "2040" });
                assert_eq!(
                    seen[2]["q"],
                    if domain == "tv" { "Main" } else { "Main 2020" }
                );
            }
            let mut continued = base.clone();
            continued["query_index"] = 2.into();
            let page = indexer::search(
                &client.operation(uuid::Uuid::new_v4()).unwrap(),
                &settings,
                &access,
                &serde_json::from_value(continued).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(page.query_index, 2);
            assert!(page.next_query.is_none());
            scenario.store(1, Ordering::Relaxed);
            calls.lock().unwrap().clear();
            let page = indexer::search(
                &client.operation(uuid::Uuid::new_v4()).unwrap(),
                &settings,
                &access,
                &request,
            )
            .await
            .unwrap();
            assert_eq!(page.query_index, 0);
            assert_eq!(page.next_query.as_ref().unwrap().query_index, 0);
            assert_eq!(page.next_query.as_ref().unwrap().offset, 1);
            assert_eq!(calls.lock().unwrap().len(), 2);
            let mut continued = base.clone();
            continued["offset"] = 1.into();
            let page = indexer::search(
                &client.operation(uuid::Uuid::new_v4()).unwrap(),
                &settings,
                &access,
                &serde_json::from_value(continued).unwrap(),
            )
            .await
            .unwrap();
            assert!(page.next_query.is_none());
            if domain == "tv" {
                for (mode, numbering) in [
                    ("both", json!({"kind":"episode","season":1,"episode":2})),
                    ("ids", json!({"kind":"anime","absolute_episode":7})),
                ] {
                    let mut same_tier = base.clone();
                    same_tier["offset"] = 1.into();
                    same_tier["search_mode"] = mode.into();
                    same_tier["numbering"] = numbering;
                    let page = indexer::search(
                        &client.operation(uuid::Uuid::new_v4()).unwrap(),
                        &settings,
                        &access,
                        &serde_json::from_value(same_tier).unwrap(),
                    )
                    .await
                    .unwrap();
                    assert_eq!(
                        page.next_query.unwrap().query_index,
                        1,
                        "Both and anime continue title queries even after successful ID results"
                    );
                }
            }
            scenario.store(2, Ordering::Relaxed);
            calls.lock().unwrap().clear();
            assert!(matches!(
                indexer::search(
                    &client.operation(uuid::Uuid::new_v4()).unwrap(),
                    &settings,
                    &access,
                    &request
                )
                .await,
                Err(IndexerError::Authentication)
            ));
            assert_eq!(calls.lock().unwrap().len(), 2);
            scenario.store(3, Ordering::Relaxed);
            calls.lock().unwrap().clear();
            let mut nonzero = base.clone();
            nonzero["offset"] = 5.into();
            let page = indexer::search(
                &client.operation(uuid::Uuid::new_v4()).unwrap(),
                &settings,
                &access,
                &serde_json::from_value(nonzero).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(page.query_index, 0);
            assert!(page.items.is_empty());
            assert_eq!(calls.lock().unwrap().len(), 2);
            let mut invalid = base.clone();
            invalid["query_index"] = 3.into();
            assert!(matches!(
                indexer::plan_search(
                    &settings,
                    &indexer::parse_capabilities(CAPS).unwrap(),
                    &serde_json::from_value(invalid).unwrap()
                ),
                Err(IndexerError::InvalidRequest)
            ));
        }
    }
    server.abort();
    let _ = server.await;
}

#[test]
fn modes_legacy_capabilities_and_domain_aggregate_negotiation() {
    let settings = config("torznab", "https://indexer.invalid/api");
    let base = json!({"kind":"tv","title":"Primary","aliases":["Alias"],"tvdb_id":12,"tmdb_id":13,"imdb_id":"tt1234567","rage_id":14,"numbering":{"kind":"season","season":2}});
    let caps = indexer::parse_capabilities(CAPS).unwrap();
    for (mode, expected) in [
        ("default", "tvdbid"),
        ("ids", "tvdbid"),
        ("both", "tvdbid"),
        ("titles", "q"),
    ] {
        let mut request = base.clone();
        request["search_mode"] = mode.into();
        let plan =
            indexer::plan_search(&settings, &caps, &serde_json::from_value(request).unwrap())
                .unwrap();
        assert!(plan.parameters.iter().any(|(key, _)| key == expected));
    }
    let legacy = CAPS.replace(
        " supportedParams=\"q,tvdbid,tvmazeid,rid,imdbid,season,ep\"",
        "",
    );
    let legacycaps = indexer::parse_capabilities(&legacy).unwrap();
    assert!(!legacycaps.tv.aggregate_ids);
    assert!(legacycaps.movies.aggregate_ids);
    let q = query(
        &settings,
        &legacy,
        &serde_json::from_value(base.clone()).unwrap(),
    );
    assert_eq!(q["rid"], "14");
    assert!(!q.contains_key("tvdbid"));
    assert!(!q.contains_key("imdbid"));
    let titlecaps = CAPS.replace(
        "q,tvdbid,tvmazeid,rid,imdbid,season,ep",
        "title,q,season,ep",
    );
    let mut title = base.clone();
    title["search_mode"] = "titles".into();
    let q = query(
        &settings,
        &titlecaps,
        &serde_json::from_value(title).unwrap(),
    );
    assert_eq!(q["title"], "Primary");
    assert!(!q.contains_key("q"));
    let mut excessive = base;
    excessive["aliases"] = json!(vec!["alias"; 17]);
    assert!(matches!(
        indexer::plan_search(
            &settings,
            &caps,
            &serde_json::from_value(excessive).unwrap()
        ),
        Err(IndexerError::InvalidRequest)
    ));
}

#[test]
fn scoped_flags_anime_sequences_and_remove_year() {
    let mut settings = config("torznab", "https://indexer.invalid/api");
    if let ProviderSettings::Torznab {
        tv: Some(tv),
        movies: Some(movies),
        ..
    } = &mut settings
    {
        tv.anime_standard_format_search = true;
        movies.remove_year = true;
    }
    let base = json!({"kind":"tv","title":"Series","aliases":["Alias"],"tvdb_id":12,"numbering":{"kind":"anime","absolute_episode":7,"season":2,"episode":3},"search_mode":"ids"});
    let caps = indexer::parse_capabilities(CAPS).unwrap();
    let mut requests = vec![];
    for index in 0..6 {
        let mut request = base.clone();
        request["query_index"] = index.into();
        requests.push(
            indexer::plan_search(&settings, &caps, &serde_json::from_value(request).unwrap())
                .unwrap()
                .parameters
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
        );
    }
    assert_eq!(requests[0]["q"], "07");
    assert_eq!(requests[0]["tvdbid"], "12");
    assert_eq!(requests[1]["q"], "Series 07");
    assert_eq!(requests[2]["q"], "Alias 07");
    assert_eq!(requests[3]["season"], "2");
    assert_eq!(requests[3]["ep"], "3");
    assert_eq!(requests[3]["tvdbid"], "12");
    assert_eq!(requests[4]["q"], "Series");
    assert_eq!(requests[5]["q"], "Alias");
    assert!(requests.iter().all(|q| q["cat"] == "5070"));
    let restricted = CAPS
        .replace(
            "<search available=\"yes\" supportedParams=\"q\"/>",
            "<search available=\"no\" supportedParams=\"q\"/>",
        )
        .replace("q,tvdbid,tvmazeid,rid,imdbid,season,ep", "tvdbid,season,ep");
    let q = query(
        &settings,
        &restricted,
        &serde_json::from_value(base.clone()).unwrap(),
    );
    assert_eq!(q["tvdbid"], "12");
    assert_eq!(q["season"], "2");
    assert_eq!(q["ep"], "3");
    assert!(
        !q.contains_key("q"),
        "Unsupported absolute/text queries must not discard supported anime standard-ID queries"
    );
    let zero = query(&settings, CAPS, &tv(TvNumbering::Season { season: 0 }));
    assert_eq!(
        zero["season"], "00",
        "Zero-season compatibility applies to packs as well as episodes"
    );
    let base = json!({"kind":"tv","title":"Series","tvdb_id":12,"aliases":["Alias","SECOND ARC"],"numbering":{"kind":"anime_season","season":2,"season_aliases":["Second Arc","Alternate Arc","Second Arc"]}});
    for (index, expected) in [
        (1, "Second Arc"),
        (2, "Alternate Arc"),
        (3, "Series"),
        (4, "Alias"),
    ] {
        let mut request = base.clone();
        request["query_index"] = index.into();
        let q = query(&settings, CAPS, &serde_json::from_value(request).unwrap());
        assert_eq!(q["q"], expected);
        assert_eq!(q["cat"], "5070");
        if index >= 3 {
            assert_eq!(q["t"], "tvsearch");
            assert_eq!(q["season"], "2");
        } else {
            assert_eq!(q["t"], "search");
            assert!(!q.contains_key("season"));
        }
    }
    let id = query(
        &settings,
        CAPS,
        &serde_json::from_value(base.clone()).unwrap(),
    );
    assert_eq!(id["tvdbid"], "12");
    assert_eq!(id["season"], "2");
    assert_eq!(id["cat"], "5070");
    let mut beyond = base.clone();
    beyond["query_index"] = 5.into();
    assert!(
        indexer::plan_search(&settings, &caps, &serde_json::from_value(beyond).unwrap()).is_err(),
        "Case-insensitive season alias overlap must not add a structured title query"
    );
    if let ProviderSettings::Torznab { tv: Some(tv), .. } = &mut settings {
        tv.anime_standard_format_search = false;
    }
    let bare = query(
        &settings,
        CAPS,
        &serde_json::from_value(base.clone()).unwrap(),
    );
    assert_eq!(bare["q"], "Second Arc");
    assert_eq!(bare["t"], "search");
    assert!(!bare.contains_key("season"));
    let mut invalid = base;
    invalid["query_index"] = 2.into();
    assert!(matches!(
        indexer::plan_search(&settings, &caps, &serde_json::from_value(invalid).unwrap()),
        Err(IndexerError::InvalidRequest)
    ));
    let movie = json!({"kind":"movie","title":"Series","year":2020});
    let q = query(&settings, CAPS, &serde_json::from_value(movie).unwrap());
    assert_eq!(q["q"], "Series");
    assert_eq!(q["cat"], "2040");
}

#[test]
fn engine_normalization_is_scoped_bounded_and_preserves_title_parameters() {
    let settings = config("torznab", "https://indexer.invalid/api");
    let title = "The Café's & Co.";
    let tv = json!({"kind":"tv","title":title,"numbering":{"kind":"season","season":1}});
    let movie = json!({"kind":"movie","title":title,"year":2020});
    let q = query(
        &settings,
        CAPS,
        &serde_json::from_value(tv.clone()).unwrap(),
    );
    assert_eq!(q["q"], "Cafes and Co");
    let q = query(
        &settings,
        CAPS,
        &serde_json::from_value(movie.clone()).unwrap(),
    );
    assert_eq!(q["q"], "Cafes and Co 2020");
    let raw = CAPS.replace(
        "<tv-search available=\"yes\"",
        "<tv-search available=\"yes\" searchEngine=\"raw\"",
    );
    let q = query(
        &settings,
        &raw,
        &serde_json::from_value(tv.clone()).unwrap(),
    );
    assert_eq!(q["q"], title);
    let mut clean = tv.clone();
    clean["query_index"] = 1.into();
    let q = query(&settings, &raw, &serde_json::from_value(clean).unwrap());
    assert_eq!(q["q"], "Cafes and Co");
    let title_caps = raw.replace(
        "q,tvdbid,tvmazeid,rid,imdbid,season,ep",
        "title,q,season,ep",
    );
    let q = query(
        &settings,
        &title_caps,
        &serde_json::from_value(tv.clone()).unwrap(),
    );
    assert_eq!(q["title"], title);
    let mut invalid = tv.clone();
    invalid["query_index"] = 1.into();
    assert!(
        indexer::plan_search(
            &settings,
            &indexer::parse_capabilities(&title_caps).unwrap(),
            &serde_json::from_value(invalid).unwrap()
        )
        .is_err()
    );
    let raw_movie = CAPS
        .replace(
            "<search available=\"yes\"",
            "<search available=\"yes\" searchEngine=\"raw\"",
        )
        .replace(
            "<movie-search available=\"yes\"",
            "<movie-search available=\"yes\" searchEngine=\"sphinx\"",
        );
    let q = query(
        &settings,
        &raw_movie,
        &serde_json::from_value(movie).unwrap(),
    );
    assert_eq!(q["q"], format!("{title} 2020"));
    for (title, expected) in [
        ("A&B", "AandB"),
        ("The A_B", "A_B"),
        ("A+B", "A B"),
        ("The Cafe\u{301}", "Cafe"),
        ("The 東京", "東京"),
    ] {
        let mut request = tv.clone();
        request["title"] = title.into();
        let q = query(&settings, CAPS, &serde_json::from_value(request).unwrap());
        assert_eq!(q["q"], expected);
    }
    let mut empty = tv.clone();
    empty["title"] = "...!".into();
    assert!(matches!(
        indexer::plan_search(
            &settings,
            &indexer::parse_capabilities(CAPS).unwrap(),
            &serde_json::from_value(empty).unwrap()
        ),
        Err(IndexerError::Unsupported)
    ));
    let mut plus_only = tv.clone();
    plus_only["title"] = "+++".into();
    assert!(matches!(
        indexer::plan_search(
            &settings,
            &indexer::parse_capabilities(&raw).unwrap(),
            &serde_json::from_value(plus_only).unwrap()
        ),
        Err(IndexerError::Unsupported)
    ));
    let mut empty_special = tv.clone();
    empty_special["title"] = "+++".into();
    empty_special["numbering"] = json!({"kind":"special","episode_title":"Holiday"});
    assert!(matches!(
        indexer::plan_search(
            &settings,
            &indexer::parse_capabilities(&raw_movie).unwrap(),
            &serde_json::from_value(empty_special).unwrap()
        ),
        Err(IndexerError::Unsupported)
    ));
    let mut expanded = tv;
    expanded["title"] = "&".repeat(512).into();
    expanded["query_index"] = 1.into();
    let q = query(&settings, &raw, &serde_json::from_value(expanded).unwrap());
    assert_eq!(
        q["q"],
        "and".repeat(512),
        "Bounded generated title expansion must not reapply the original 512-byte input ceiling"
    );
}

#[test]
fn legacy_caps_and_optional_feed_facts_have_explicit_evidence() {
    let legacy = indexer::parse_capabilities("<caps/>").unwrap();
    assert_eq!(legacy.max_limit, 100);
    assert_eq!(legacy.default_limit, 100);
    assert!(legacy.tv.available);
    assert!(!legacy.tv.aggregate_ids);
    assert!(legacy.categories.is_empty());
    let no_limits = CAPS.replace("<limits max=\"100\" default=\"50\"/>", "");
    assert_eq!(
        indexer::parse_capabilities(&no_limits)
            .unwrap()
            .categories
            .len(),
        6
    );
    let invalid = CAPS.replace("max=\"100\"", "max=\"not-integer\"");
    let defaults = indexer::parse_capabilities(&invalid).unwrap();
    assert_eq!(defaults.default_limit, 100);
    assert!(defaults.categories.is_empty());
    assert!(!defaults.movies.aggregate_ids);
    let unavailable = CAPS.replace(
        "<tv-search available=\"yes\"",
        "<tv-search available=\"no\"",
    );
    assert!(
        !indexer::parse_capabilities(&unavailable)
            .unwrap()
            .tv
            .available
    );
    let mut settings = config("newznab", "https://indexer.invalid/api");
    if let ProviderSettings::Newznab {
        movies: Some(movies),
        ..
    } = &mut settings
    {
        movies.categories.push(2999);
    }
    let req: IndexerSearch =
        serde_json::from_value(json!({"kind":"rss","media_type":"movies"})).unwrap();
    let q = query(&settings, CAPS, &req);
    assert_eq!(q["cat"], "2040,2999");
    assert!(matches!(
        indexer::plan_search(&settings, &legacy, &req),
        Err(IndexerError::Unsupported)
    ));
    let good = r#"<item><pubDate>2024-01-02T03:04:05+01:00</pubDate><enclosure url="https://indexer.invalid/private"/><language>English,French</language><language>Icelandic</language><n:attr name="PREMATCH" value="1"/><n:attr name="nuked" value="1"/><n:attr name="subs" value="Icelandic"/><n:attr name="tvdbid" value="12"/><n:attr name="imdb" value="1234567"/><n:attr name="info" value="https://indexer.invalid/PRIVATE_INFO"/></item>"#;
    let bad = r#"<item><pubDate>2024-01-02</pubDate><link>file:///PRIVATE</link></item>"#;
    let body = format!(
        r#"<rss xmlns:n="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><n:response offset="4" total="10"/>{good}{bad}</channel></rss>"#
    );
    let page = indexer::parse_page(&body, 4, 2, false, MediaDomain::Tv).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.warnings[0].index, 5);
    assert_eq!(page.next_offset, Some(6));
    let item = &page.items[0];
    assert!(item.metadata.title.is_none());
    assert!(item.guid.is_none());
    assert!(item.metadata.size_bytes.is_none());
    assert!(item.metadata.categories.is_empty());
    assert_eq!(item.metadata.published_at, "2024-01-02T02:04:05Z");
    assert_eq!(
        item.metadata.languages,
        vec!["English", "French", "Icelandic"]
    );
    assert_eq!(item.facts.scene, Some(true));
    assert_eq!(item.facts.nuked, Some(true));
    assert_eq!(item.facts.has_subtitles, Some(true));
    assert!(
        matches!(&item.facts.identifiers,indexer::ReleaseIdentifiers::Tv{tvdb_id:Some(12),imdb_id:Some(id),..} if id=="tt1234567")
    );
    assert!(
        !serde_json::to_string(&item.metadata)
            .unwrap()
            .contains("PRIVATE")
    );
    let sparse_ids = body
        .replace(
            "name=\"tvdbid\" value=\"12\"",
            "name=\"tvdbid\" value=\"0\"",
        )
        .replace(
            "name=\"imdb\" value=\"1234567\"",
            "name=\"imdb\" value=\"12345\"",
        )
        .replace(
            "<n:attr name=\"info\" value=\"https://indexer.invalid/PRIVATE_INFO\"/>",
            "<comments>https://indexer.invalid/release#comments</comments>",
        );
    let sparse = indexer::parse_page(&sparse_ids, 4, 2, false, MediaDomain::Tv).unwrap();
    let facts = &sparse.items[0].facts;
    assert!(
        matches!(&facts.identifiers,indexer::ReleaseIdentifiers::Tv{tvdb_id:None,imdb_id:Some(id),..} if id=="tt0012345")
    );
    assert_eq!(
        facts.info_url.as_deref(),
        Some("https://indexer.invalid/release")
    );
    assert_eq!(
        facts.comments_url.as_deref(),
        Some("https://indexer.invalid/release#comments")
    );
    let missing_date = body.replace("<pubDate>2024-01-02</pubDate>", "");
    assert!(indexer::parse_page(&missing_date, 4, 2, false, MediaDomain::Tv).is_err());
    let priority = good.replace(
        "<pubDate>2024-01-02T03:04:05+01:00</pubDate>",
        "<n:attr name='usenetdate' value='2020-02-03'/>",
    );
    let priority = format!(
        r#"<rss xmlns:n="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel>{priority}</channel></rss>"#
    );
    assert_eq!(
        indexer::parse_page(&priority, 0, 10, false, MediaDomain::Movies)
            .unwrap()
            .items[0]
            .metadata
            .published_at,
        "2020-02-03T00:00:00Z"
    );
    assert!(indexer::parse_page(&priority, 0, 10, true, MediaDomain::Movies).is_err());
}

#[test]
fn torrent_enclosures_size_fallback_peers_and_private_facts() {
    let hash = "0123456789abcdef0123456789abcdef01234567";
    let item = format!(
        r#"<item><title>Release</title><pubDate>2024-01-02</pubDate><enclosure type="image/jpeg" url="https://indexer.invalid/image"/><enclosure type="application/x-bittorrent;x-scheme-handler/magnet" url="magnet:?xt=urn:btih:{hash}"/><enclosure type="application/x-bittorrent" url="https://indexer.invalid/PRIVATE_TORRENT" length="456"/><t:attr name="size" value="bad"/><t:attr name="seeders" value="2"/><t:attr name="leechers" value="3"/><t:attr name="infohash" value="{hash}"/><t:attr name="magneturl" value="magnet:?xt=urn:btih:{hash}"/><t:attr name="downloadvolumefactor" value="0"/><t:attr name="uploadvolumefactor" value="2"/><t:attr name="minimumratio" value="1.5"/><t:attr name="minimumseedtime" value="3600"/><t:attr name="tag" value="SCENE"/><t:attr name="tag" value="INTERNAL"/><language>English,French</language></item>"#
    );
    let body = format!(
        r#"<rss xmlns:t="http://torznab.com/schemas/2015/feed"><channel>{item}</channel></rss>"#
    );
    let page = indexer::parse_page(&body, 0, 10, true, MediaDomain::Movies).unwrap();
    let item = &page.items[0];
    assert_eq!(item.metadata.size_bytes, Some(456));
    assert_eq!(item.metadata.peers, Some(5));
    assert_eq!(item.metadata.languages, vec!["English,French"]);
    assert_eq!(item.download_url, "https://indexer.invalid/PRIVATE_TORRENT");
    let facts = item.facts.torrent.as_ref().unwrap();
    assert_eq!(facts.info_hash.as_deref(), Some(hash));
    assert_eq!(facts.minimum_seed_seconds, Some(3600));
    assert_eq!(facts.minimum_ratio, Some(1.5));
    assert_eq!(facts.download_volume_factor, Some(0.0));
    assert_eq!(facts.upload_volume_factor, Some(2.0));
    assert_eq!(facts.internal, Some(true));
    assert_eq!(item.facts.scene, Some(true));
    assert!(item.facts.has_subtitles.is_none());
    let peers = body.replace("</item>", "<t:attr name='peers' value='10'/></item>");
    assert_eq!(
        indexer::parse_page(&peers, 0, 10, true, MediaDomain::Movies)
            .unwrap()
            .items[0]
            .metadata
            .peers,
        Some(10)
    );
    for (title, expected) in [
        ("A &amp;amp; B", "A & B"),
        ("<![CDATA[A &amp; B]]>", "A & B"),
        ("<![CDATA[&#x43;af&eacute; &#38; Co]]>", "Café & Co"),
        ("<![CDATA[A &amp;amp; B]]>", "A &amp; B"),
    ] {
        let encoded = body.replace("<title>Release</title>", &format!("<title>{title}</title>"));
        let page = indexer::parse_page(&encoded, 0, 10, true, MediaDomain::Movies).unwrap();
        assert_eq!(
            page.items[0].metadata.title.as_deref(),
            Some(expected),
            "Decode HTML exactly once after XML decoding"
        );
    }
    let invalid = body.replace("value=\"1.5\"", "value=\"NaN\"");
    assert!(indexer::parse_page(&invalid, 0, 10, true, MediaDomain::Movies).is_err());
    assert!(matches!(
        indexer::parse_capabilities("<error code='999' description='Request limit reached'/>"),
        Err(IndexerError::RateLimited { .. })
    ));
}

#[tokio::test]
async fn category_discovery_is_caps_only_bounded_and_preserves_advertised_choices() {
    use axum::{
        Router,
        extract::{Query, State},
        routing::get,
    };
    use hrrdarr::providers::{IndexerAccess, IndexerParameter, http::HttpClient};
    use std::sync::{Arc, Mutex};
    type Fixture = Arc<Mutex<(String, Vec<BTreeMap<String, String>>)>>;
    async fn handler(
        State(state): State<Fixture>,
        Query(query): Query<BTreeMap<String, String>>,
    ) -> String {
        let mut state = state.lock().unwrap();
        state.1.push(query);
        state.0.clone()
    }
    let state: Fixture = Arc::new(Mutex::new((String::new(), vec![])));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api", listener.local_addr().unwrap());
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/api", get(handler))
                .with_state(state.clone()),
        )
        .into_future(),
    );
    let client = HttpClient::new().unwrap();
    let tv = [IndexerParameter {
        name: "passkey".into(),
        value: "TV_SECRET".into(),
    }];
    let movies = [IndexerParameter {
        name: "passkey".into(),
        value: "MOVIE_SECRET".into(),
    }];
    let access = IndexerAccess {
        api_key: Some("API_SECRET"),
        tv_parameters: &tv,
        movie_parameters: &movies,
    };
    // Both implementations share the caps protocol; configuration discriminators do not alter it.
    for kind in ["torznab", "newznab"] {
        let settings = config(kind, &endpoint);
        assert_eq!(
            serde_json::to_value(settings).unwrap()["implementation"],
            kind
        );
        for domain in [MediaDomain::Tv, MediaDomain::Movies] {
            state.lock().unwrap().0 = r#"<caps><limits default="broken"/><categories>
              <category id="5000" name="TV"><subcat id="5080" name="MOVIE_SECRET"/><subcat id="5010" name="TV_SECRET"/></category>
              <category id="2000" name="Movies"><subcat id="2045" name="API_SECRET"/></category>
              <category id="100001" name="Custom &amp; Unicode é"/>
              <category id="1000" name="Ignored"><subcat id="1001" name="Ignored child"/></category>
              <category id="3000" name="Ignored"/><category id="4000" name="Ignored"/><category id="6000" name="Ignored"/><category id="7000" name="Ignored"/>
            </categories></caps>"#.into();
            let operation = client.operation(uuid::Uuid::new_v4()).unwrap();
            let options = indexer::discover_categories(&operation, &endpoint, &access, domain)
                .await
                .unwrap();
            let ids = options.iter().map(|o| o.id).collect::<Vec<_>>();
            // Preferred/custom roots precede the opposite domain; children follow their own parent.
            let expected = if domain == MediaDomain::Tv {
                vec![5000, 5010, 5080, 100001, 2000, 2045]
            } else {
                vec![2000, 2045, 100001, 5000, 5010, 5080]
            };
            assert_eq!(ids, expected);
            for option in &options {
                if [5010, 5080, 2045].contains(&option.id) {
                    assert_eq!(option.label, "[redacted]");
                }
                if [5010, 5080].contains(&option.id) {
                    assert_eq!(option.parent_id, Some(5000));
                }
            }
            assert_eq!(
                options.iter().find(|o| o.id == 100001).unwrap().label,
                "Custom & Unicode é"
            );
            for empty in ["<caps/>", "<caps><categories/></caps>"] {
                state.lock().unwrap().0 = empty.into();
                assert!(
                    indexer::discover_categories(&operation, &endpoint, &access, domain)
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
        }
    }
    let oversized = format!(
        "<caps><categories><category id='5000' name='{}'/></categories></caps>",
        "a".repeat(513)
    );
    let overcount = format!(
        "<caps><categories>{}</categories></caps>",
        (1..=4097)
            .map(|id| format!("<category id='{id}' name='x'/>"))
            .collect::<String>()
    );
    for invalid in [
        "<caps>",
        "<rss/>",
        "<!DOCTYPE caps [<!ENTITY x 'x'>]><caps/>",
        "<caps><categories><category id='0' name='Zero'/></categories></caps>",
        "<caps><categories><category id='2147483648' name='Overflow'/></categories></caps>",
        "<caps><categories><category id='5000' name='TV'><subcat id='5000' name='Duplicate'/></category></categories></caps>",
        "<caps><categories><category id='5000'/></categories></caps>",
        "<caps><categories><category id='5000' name='&#10;'/></categories></caps>",
        &oversized,
        &overcount,
    ] {
        state.lock().unwrap().0 = invalid.into();
        let operation = client.operation(uuid::Uuid::new_v4()).unwrap();
        assert!(matches!(
            indexer::discover_categories(&operation, &endpoint, &access, MediaDomain::Tv).await,
            Err(IndexerError::InvalidResponse)
        ));
    }
    for query in &state.lock().unwrap().1 {
        assert_eq!(
            query,
            &BTreeMap::from([
                ("t".into(), "caps".into()),
                ("o".into(), "xml".into()),
                ("apikey".into(), "API_SECRET".into())
            ])
        );
    }
    let tv = indexer::standard_categories(MediaDomain::Tv);
    let movies = indexer::standard_categories(MediaDomain::Movies);
    assert_eq!(
        tv.iter()
            .map(|o| (o.id, o.label.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (5000, "TV"),
            (5010, "WEB-DL"),
            (5020, "Foreign"),
            (5030, "SD"),
            (5040, "HD"),
            (5045, "UHD"),
            (5050, "Other"),
            (5060, "Sport"),
            (5070, "Anime"),
            (5080, "Documentary")
        ]
    );
    assert_eq!(
        movies
            .iter()
            .map(|o| (o.id, o.label.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (2000, "Movies"),
            (2010, "Foreign"),
            (2020, "Other"),
            (2030, "SD"),
            (2040, "HD"),
            (2045, "UHD"),
            (2050, "BluRay"),
            (2060, "3D")
        ]
    );
    assert!(tv[1..].iter().all(|o| o.parent_id == Some(5000)));
    assert!(movies[1..].iter().all(|o| o.parent_id == Some(2000)));
    server.abort();
    let _ = server.await;
}

#[test]
fn selected_magnet_links_and_enclosures_have_typed_torrent_facts() {
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        for magnet in [
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".to_string(),
            format!("magnet:?xt=urn:btmh:1220{}", "a".repeat(64)),
        ] {
            for locator in [
                format!("<link>{magnet}</link>"),
                format!(r#"<enclosure type="application/x-bittorrent" url="{magnet}"/>"#),
            ] {
                let body = format!(
                    "<rss><channel><item><title>Release</title><pubDate>2024-01-02</pubDate>{locator}</item></channel></rss>"
                );
                let page = indexer::parse_page(&body, 0, 10, true, domain).unwrap();
                assert_eq!(page.items.len(), 1);
                assert_eq!(page.items[0].download_url, magnet);
                assert_eq!(
                    page.items[0]
                        .facts
                        .torrent
                        .as_ref()
                        .unwrap()
                        .magnet_url
                        .as_deref(),
                    Some(magnet.as_str()),
                    "a validated selected magnet locator must not be dispatched as an HTTP torrent URL"
                );
                assert!(
                    indexer::parse_page(&body, 0, 10, false, domain).is_err(),
                    "Newznab does not inherit torrent locator support"
                );
            }
        }
    }
}
