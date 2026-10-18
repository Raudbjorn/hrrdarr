//! Independent adapters for archived provider configuration; never invoke a provider.
use super::*;
use crate::{
    library::Change,
    providers::{
        CredentialKey, Credentials, DownloadContentLayout, DownloadInitialState, DownloadScope,
        IndexerParameter, MovieIndexerScope, ProviderInput, ProviderSettings, TvIndexerScope,
    },
};
use serde_json::{Map, Value as Json};

pub(super) struct ProviderRecord {
    table: &'static str,
    id: i64,
    input: ProviderInput,
    download_client_source_id: Option<i64>,
}
const INVALID: ImportError = ImportError("invalid source provider settings");
fn string<'a>(settings: &'a Map<String, Json>, key: &str) -> Result<Option<&'a str>> {
    match settings.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(s)) => Ok(Some(s)),
        _ => Err(INVALID),
    }
}
fn flag(settings: &Map<String, Json>, key: &str) -> Result<bool> {
    match settings.get(key) {
        None => Ok(false),
        Some(Json::Bool(v)) => Ok(*v),
        _ => Err(INVALID),
    }
}
// Older/partial snapshots carry no evidence that an absent operation was enabled.
// Preserve reconstruction, but keep each absent policy disabled and report the gap.
fn operation_flags(row: &Record, unsupported: &mut Vec<Unsupported>) -> Result<[bool; 3]> {
    let mut flags = [false; 3];
    for (index, key) in [
        "EnableRss",
        "EnableAutomaticSearch",
        "EnableInteractiveSearch",
    ]
    .into_iter()
    .enumerate()
    {
        flags[index] = match row.get(key) {
            Some(Value::Integer(0)) => false,
            Some(Value::Integer(1)) => true,
            None => {
                unsupported.push(Unsupported {
                    table: "Indexers".into(),
                    rows: 1,
                    columns: vec![format!("{key} (absent; imported disabled)")],
                });
                false
            }
            _ => return Err(INVALID),
        };
    }
    Ok(flags)
}
fn number(settings: &Map<String, Json>, key: &str, default: i64) -> Result<i64> {
    match settings.get(key) {
        None => Ok(default),
        Some(v) => v.as_i64().ok_or(INVALID),
    }
}
fn categories(settings: &Map<String, Json>, key: &str, required: bool) -> Result<Vec<u32>> {
    match settings.get(key) {
        None if !required => Ok(Vec::new()),
        Some(Json::Array(v)) if v.len() <= 64 => v
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or(INVALID)
            })
            .collect(),
        _ => Err(INVALID),
    }
}
fn nonempty(value: Option<&str>) -> Option<String> {
    value.filter(|s| !s.is_empty()).map(str::to_owned)
}
fn endpoint(base: &str, path: &str) -> Result<String> {
    // Concatenate path components only. URL joins could replace the intended origin or base path.
    if !path.is_empty() && (!path.starts_with('/') || path.starts_with("//")) {
        return Err(INVALID);
    }
    if path.contains(['?', '#', '\\']) || path.split('/').any(|p| p == ".." || p == ".") {
        return Err(INVALID);
    }
    Ok(format!("{}{}", base.trim_end_matches('/'), path))
}
// form_urlencoded is deliberately forgiving; reject malformed bytes before using it.
fn validate_encoding(raw: &str) -> Result<()> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3).ok_or(INVALID)?;
            let nibble = |b: u8| match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                b'A'..=b'F' => Some(b - b'A' + 10),
                _ => None,
            };
            decoded.push(nibble(hex[0]).ok_or(INVALID)? * 16 + nibble(hex[1]).ok_or(INVALID)?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    std::str::from_utf8(&decoded).map_err(|_| INVALID)?;
    Ok(())
}
pub(super) fn read(
    source: &Source,
    app: Application,
    unsupported: &mut Vec<Unsupported>,
) -> Result<Vec<ProviderRecord>> {
    if ["Indexers", "DownloadClients"]
        .iter()
        .filter_map(|name| source.tables.get(*name))
        .map(|t| t.rows.len())
        .sum::<usize>()
        > 256
    {
        return Err(ImportError("snapshot exceeds provider limit"));
    }
    let mut output = Vec::new();
    let tv = matches!(app, Application::Sonarr);
    for table_name in ["Indexers", "DownloadClients"] {
        let Some(table) = source.tables.get(table_name) else {
            continue;
        };
        unsupported.retain(|u| u.table != table_name);
        let mut ids = std::collections::BTreeSet::new();
        for row in &table.rows {
            let id = positive(row, "Id")?;
            if !ids.insert(id) {
                return Err(ImportError("duplicate source provider identity"));
            }
            let implementation = text(row, "Implementation")?;
            if !matches!(
                (table_name, implementation),
                ("Indexers", "Torznab" | "Newznab") | ("DownloadClients", "QBittorrent")
            ) {
                unsupported.push(Unsupported {
                    table: table_name.into(),
                    rows: 1,
                    columns: table.columns.clone(),
                });
                continue;
            }
            if output.len() >= 256 {
                return Err(ImportError("snapshot exceeds supported provider limit"));
            }
            let contract = match implementation {
                "Torznab" => "TorznabSettings",
                "Newznab" => "NewznabSettings",
                _ => "QBittorrentSettings",
            };
            if row
                .get("ConfigContract")
                .is_some_and(|v| v != &Value::Text(contract.into()))
            {
                return Err(INVALID);
            }
            let settings: Json =
                serde_json::from_str(text(row, "Settings")?).map_err(|_| INVALID)?;
            let settings = settings.as_object().ok_or(INVALID)?;
            let download_client_source_id = if table_name == "Indexers" {
                match row.get("DownloadClientId") {
                    // Source sentinel zero means no preference; it is absence, not an identity.
                    Some(Value::Integer(0)) => None,
                    Some(Value::Integer(id)) if *id > 0 => Some(*id),
                    None => {
                        unsupported.push(Unsupported {
                            table: table_name.into(),
                            rows: 1,
                            columns: vec![
                                "DownloadClientId (absent; source association unknown)".into(),
                            ],
                        });
                        None
                    }
                    _ => return Err(INVALID),
                }
            } else {
                None
            };
            let mut used = vec!["apiKey"];
            let (config, credentials) = if table_name == "Indexers" {
                let [
                    enable_rss,
                    enable_automatic_search,
                    enable_interactive_search,
                ] = operation_flags(row, unsupported)?;
                used.extend(["baseUrl", "apiPath", "categories", "additionalParameters"]);
                let base = string(settings, "baseUrl")?.ok_or(INVALID)?;
                let url = endpoint(
                    base,
                    if settings.contains_key("apiPath") {
                        string(settings, "apiPath")?.ok_or(INVALID)?
                    } else {
                        "/api"
                    },
                )?;
                let cats = if settings.contains_key("categories") {
                    categories(settings, "categories", true)?
                } else if tv {
                    vec![5030, 5040]
                } else {
                    vec![2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060]
                };
                let (tv_scope, movies_scope) = if tv {
                    used.extend(["animeCategories", "animeStandardFormatSearch"]);
                    (
                        Some(TvIndexerScope {
                            download_client_id: None,
                            enable_rss,
                            enable_automatic_search,
                            enable_interactive_search,
                            categories: cats,
                            anime_categories: categories(settings, "animeCategories", false)?,
                            anime_standard_format_search: flag(
                                settings,
                                "animeStandardFormatSearch",
                            )?,
                        }),
                        None,
                    )
                } else {
                    used.push("removeYear");
                    (
                        None,
                        Some(MovieIndexerScope {
                            download_client_id: None,
                            enable_rss,
                            enable_automatic_search,
                            enable_interactive_search,
                            categories: cats,
                            remove_year: flag(settings, "removeYear")?,
                        }),
                    )
                };
                let config = if implementation == "Torznab" {
                    ProviderSettings::Torznab {
                        endpoint: url,
                        tv: tv_scope,
                        movies: movies_scope,
                    }
                } else {
                    ProviderSettings::Newznab {
                        endpoint: url,
                        tv: tv_scope,
                        movies: movies_scope,
                    }
                };
                let raw = string(settings, "additionalParameters")?.unwrap_or("");
                if raw.len() > 8192 || (!raw.is_empty() && !raw.starts_with('&')) {
                    return Err(INVALID);
                }
                validate_encoding(raw)?;
                let parameters: Vec<IndexerParameter> =
                    url::form_urlencoded::parse(raw.trim_start_matches('&').as_bytes())
                        .map(|(name, value)| IndexerParameter {
                            name: name.into_owned(),
                            value: value.into_owned(),
                        })
                        .collect();
                let api_key = nonempty(string(settings, "apiKey")?);
                let secret = if parameters.is_empty() {
                    api_key.map(|api_key| Credentials::ApiKey { api_key })
                } else {
                    {
                        let (tv_parameters, movie_parameters) = if tv {
                            (parameters, Vec::new())
                        } else {
                            (Vec::new(), parameters)
                        };
                        Some(Credentials::Indexer {
                            api_key,
                            tv_parameters,
                            movie_parameters,
                        })
                    }
                };
                (config, secret)
            } else {
                used.extend([
                    "host",
                    "port",
                    "useSsl",
                    "urlBase",
                    "username",
                    "password",
                    "initialState",
                    "contentLayout",
                    "sequentialOrder",
                    "firstAndLast",
                ]);
                let host = if settings.contains_key("host") {
                    string(settings, "host")?.ok_or(INVALID)?
                } else {
                    "localhost"
                };
                if host.is_empty() || host.contains(['/', '?', '#', '@', '\\']) {
                    return Err(INVALID);
                }
                let port = u16::try_from(number(settings, "port", 8080)?)
                    .ok()
                    .filter(|p| *p > 0)
                    .ok_or(INVALID)?;
                let host = if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
                    format!("[{ip}]")
                } else if host.starts_with('[') && host.ends_with(']') {
                    format!(
                        "[{}]",
                        host[1..host.len() - 1]
                            .parse::<std::net::Ipv6Addr>()
                            .map_err(|_| INVALID)?
                    )
                } else if host.contains(':') {
                    return Err(INVALID);
                } else {
                    host.into()
                };
                let base = format!(
                    "{}://{host}:{port}",
                    if flag(settings, "useSsl")? {
                        "https"
                    } else {
                        "http"
                    }
                );
                let url = endpoint(&base, string(settings, "urlBase")?.unwrap_or(""))?;
                let (category, imported, recent, older) = if tv {
                    (
                        "tvCategory",
                        "tvImportedCategory",
                        "recentTvPriority",
                        "olderTvPriority",
                    )
                } else {
                    (
                        "movieCategory",
                        "movieImportedCategory",
                        "recentMoviePriority",
                        "olderMoviePriority",
                    )
                };
                used.extend([category, imported, recent, older]);
                if tv {
                    used.push("addSeriesTags")
                }
                let scope = DownloadScope {
                    category: if settings.contains_key(category) {
                        string(settings, category)?.ok_or(INVALID)?
                    } else if tv {
                        "tv-sonarr"
                    } else {
                        "radarr"
                    }
                    .into(),
                    imported_category: nonempty(string(settings, imported)?),
                    recent_priority: i8::try_from(number(settings, recent, 0)?)
                        .map_err(|_| INVALID)?,
                    older_priority: i8::try_from(number(settings, older, 0)?)
                        .map_err(|_| INVALID)?,
                    initial_state: match number(settings, "initialState", 0)? {
                        0 => DownloadInitialState::Started,
                        1 => DownloadInitialState::Forced,
                        2 => DownloadInitialState::Stopped,
                        _ => return Err(INVALID),
                    },
                    content_layout: match number(settings, "contentLayout", 0)? {
                        0 => DownloadContentLayout::Default,
                        1 => DownloadContentLayout::Original,
                        2 => DownloadContentLayout::Subfolder,
                        _ => return Err(INVALID),
                    },
                    sequential_order: flag(settings, "sequentialOrder")?,
                    first_last_first: flag(settings, "firstAndLast")?,
                    add_tags: tv && flag(settings, "addSeriesTags")?,
                };
                let config = ProviderSettings::Qbittorrent {
                    endpoint: url,
                    tv: tv.then(|| scope.clone()),
                    movies: (!tv).then_some(scope),
                };
                let api_key = nonempty(string(settings, "apiKey")?);
                let username = nonempty(string(settings, "username")?);
                let password = nonempty(string(settings, "password")?);
                let secret = match (api_key, username, password) {
                    (Some(api_key), None, None) => Some(Credentials::ApiKey { api_key }),
                    (None, Some(username), Some(password)) => {
                        Some(Credentials::UsernamePassword { username, password })
                    }
                    (None, None, None) => None,
                    // Do not silently choose one of multiple supplied auth methods.
                    _ => return Err(INVALID),
                };
                (config, secret)
            };
            let mut columns: Vec<String> = table
                .columns
                .iter()
                .filter(|c| {
                    !(table_name == "Indexers"
                        && matches!(
                            c.as_str(),
                            "EnableRss"
                                | "EnableAutomaticSearch"
                                | "EnableInteractiveSearch"
                                | "DownloadClientId"
                        ))
                        && !matches!(
                            c.as_str(),
                            "Id" | "Name"
                                | "Implementation"
                                | "ConfigContract"
                                | "Settings"
                                | "Priority"
                        )
                })
                .cloned()
                .collect();
            if settings.keys().any(|k| !used.contains(&k.as_str())) {
                columns.push("Settings (unsupported fields)".into());
            }
            if !columns.is_empty() {
                unsupported.push(Unsupported {
                    table: table_name.into(),
                    rows: 1,
                    columns,
                })
            }
            output.push(ProviderRecord {
                table: table_name,
                id,
                download_client_source_id,
                input: ProviderInput {
                    name: text(row, "Name")?.into(),
                    enabled: false,
                    priority: u8::try_from(integer(row, "Priority")?).map_err(|_| INVALID)?,
                    settings: config,
                    credentials: credentials.map_or(Change::Null, Change::Value),
                },
            });
        }
    }
    Ok(output)
}
pub(super) async fn write(
    conn: &Connection,
    records: &[ProviderRecord],
    key: Option<&CredentialKey>,
    report: &mut Report,
) -> Result<()> {
    let mut changes = crate::providers::HealthConfigurationChanges::default();
    let mut reconciled_clients = std::collections::BTreeSet::new();
    // The source iterator places indexers first; resolve client mappings before executable indexers.
    for record in records
        .iter()
        .filter(|r| r.table == "DownloadClients")
        .chain(records.iter().filter(|r| r.table == "Indexers"))
    {
        let download_client_id = if let Some(source_id) = record.download_client_source_id {
            let mapped = if reconciled_clients.contains(&source_id) {
                conn.query("SELECT m.provider_id FROM snapshot_provider_mappings m JOIN providers p ON p.id=m.provider_id JOIN provider_scopes s ON s.provider_id=p.id WHERE m.application=? AND m.fingerprint=? AND m.source_table='DownloadClients' AND m.source_id=? AND p.implementation='qbittorrent' AND s.media_type=?", params![report.application.name(),report.fingerprint.clone(),source_id,if matches!(report.application, Application::Sonarr) { "tv" } else { "movies" }]).await?.next().await?
            } else {
                None
            };
            let Some(mapped) = mapped else {
                // Never turn an explicit but unresolved source preference into an unbound executable indexer.
                report.unsupported.push(Unsupported {
                    table: "Indexers".into(),
                    rows: 1,
                    columns: vec!["DownloadClientId (unmapped; indexer not reconstructed)".into()],
                });
                continue;
            };
            let id = mapped.get::<String>(0)?;
            // The mapped row must be fully released before another statement uses the connection.
            drop(mapped);
            Some(uuid::Uuid::parse_str(&id).map_err(|_| INVALID)?)
        } else {
            None
        };
        let mapped=conn.query("SELECT provider_id,provider_revision FROM snapshot_provider_mappings WHERE application=? AND fingerprint=? AND source_table=? AND source_id=?",params![report.application.name(),report.fingerprint.clone(),record.table,record.id]).await?.next().await?.map(|r|->Result<(String,i64)>{Ok((r.get(0)?,r.get(1)?))}).transpose()?;
        let outcome = crate::providers::import_configuration(
            conn,
            key,
            &record.input,
            download_client_id,
            &mut changes,
            mapped
                .as_ref()
                .map(|(id, revision)| (id.as_str(), *revision)),
        )
        .await
        .map_err(ImportError)?;
        if let Some((id, revision, created)) = outcome {
            if created {
                report.mapped += 1
            } else {
                report.duplicates += 1
            }
            conn.execute("INSERT INTO snapshot_provider_mappings(application,fingerprint,source_table,source_id,provider_id,provider_revision) VALUES(?,?,?,?,?,?) ON CONFLICT DO NOTHING",params![report.application.name(),report.fingerprint.clone(),record.table,record.id,id,revision]).await?;
            if record.table == "DownloadClients" {
                reconciled_clients.insert(record.id);
            }
        } else {
            report.conflicts += 1
        }
    }
    changes.finish(conn).await.map_err(ImportError)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(table: &str, implementation: &str, settings: &str) -> Source {
        let row = Record::from([
            ("Id".into(), Value::Integer(1)),
            ("Name".into(), Value::Text("Defaulted".into())),
            ("Implementation".into(), Value::Text(implementation.into())),
            ("Priority".into(), Value::Integer(1)),
            ("Settings".into(), Value::Text(settings.into())),
        ]);
        let columns = row.keys().cloned().collect();
        Source {
            version: 233,
            tables: BTreeMap::from([(
                table.into(),
                Table {
                    columns,
                    rows: vec![row],
                },
            )]),
        }
    }
    // Reasoning: the reference applications' TorrentRss rows keep the feed URL, which usually embeds a
    // passkey in its query, inside source settings. Mapping would have to split secret query parameters
    // into sealed credentials, which is not small; the row is therefore reported as unsupported, never
    // imported and never silently dropped.
    #[test]
    fn torrent_rss_source_rows_are_reported_unsupported_for_both_applications() {
        for app in [Application::Sonarr, Application::Radarr] {
            let snapshot = source(
                "Indexers",
                "TorrentRss",
                r#"{"baseUrl":"https://tracker.example/rss?passkey=SOURCE_SECRET","cookie":"uid=1"}"#,
            );
            let mut unsupported = vec![];
            let records = read(&snapshot, app, &mut unsupported).unwrap();
            assert!(records.is_empty());
            assert_eq!(unsupported.len(), 1);
            assert_eq!(
                (unsupported[0].table.as_str(), unsupported[0].rows),
                ("Indexers", 1)
            );
            assert!(
                !format!("{:?}", unsupported[0].columns).contains("SOURCE_SECRET"),
                "reports name columns, never values"
            );
        }
    }
    #[test]
    fn source_operation_flags_are_top_level_strict_and_missing_is_disabled() {
        for app in [Application::Sonarr, Application::Radarr] {
            let mut snapshot = source(
                "Indexers",
                "Torznab",
                r#"{"baseUrl":"https://example.test","enableRss":true}"#,
            );
            let mut unsupported = vec![];
            let records = read(&snapshot, app, &mut unsupported).unwrap();
            let domain = if matches!(app, Application::Sonarr) {
                "tv"
            } else {
                "movies"
            };
            let settings = serde_json::to_value(&records[0].input.settings).unwrap();
            for key in [
                "enable_rss",
                "enable_automatic_search",
                "enable_interactive_search",
            ] {
                assert_eq!(settings[domain][key], false);
            }
            assert!(!records[0].input.enabled);
            assert_eq!(
                unsupported
                    .iter()
                    .filter(|u| u.columns.iter().any(|c| c.contains("absent")))
                    .count(),
                // Reasoning: three absent operation flags plus the absent DownloadClientId association column
                // (0049) are each reported once per source indexer row; the fixture omits all four columns.
                4
            );
            let table = snapshot.tables.get_mut("Indexers").unwrap();
            for (key, value) in [
                ("EnableRss", 0),
                ("EnableAutomaticSearch", 1),
                ("EnableInteractiveSearch", 0),
            ] {
                table.columns.push(key.into());
                table.rows[0].insert(key.into(), Value::Integer(value));
            }
            let records = read(&snapshot, app, &mut vec![]).unwrap();
            let settings = serde_json::to_value(&records[0].input.settings).unwrap();
            assert_eq!(settings[domain]["enable_rss"], false);
            assert_eq!(settings[domain]["enable_automatic_search"], true);
            assert_eq!(settings[domain]["enable_interactive_search"], false);
            for invalid in [Value::Integer(2), Value::Text("1".into()), Value::Null] {
                snapshot.tables.get_mut("Indexers").unwrap().rows[0]
                    .insert("EnableRss".into(), invalid);
                assert!(read(&snapshot, app, &mut vec![]).is_err());
            }
        }
    }
    #[test]
    fn provider_defaults_and_external_encoding_are_explicit() {
        for (app, domain, category) in [
            (Application::Sonarr, "tv", "tv-sonarr"),
            (Application::Radarr, "movies", "radarr"),
        ] {
            for implementation in ["Torznab", "Newznab"] {
                let records = read(
                    &source(
                        "Indexers",
                        implementation,
                        r#"{"baseUrl":"https://example.test"}"#,
                    ),
                    app,
                    &mut vec![],
                )
                .unwrap();
                let config = serde_json::to_value(&records[0].input.settings).unwrap();
                assert_eq!(config["endpoint"], "https://example.test/api");
                assert_eq!(
                    config[domain]["categories"],
                    if domain == "tv" {
                        serde_json::json!([5030, 5040])
                    } else {
                        serde_json::json!([2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060])
                    }
                );
                assert!(matches!(records[0].input.credentials, Change::Null));
            }
            let records = read(
                &source("DownloadClients", "QBittorrent", "{}"),
                app,
                &mut vec![],
            )
            .unwrap();
            let config = serde_json::to_value(&records[0].input.settings).unwrap();
            assert_eq!(config["endpoint"], "http://localhost:8080");
            assert_eq!(config[domain]["category"], category);
            let records = read(
                &source(
                    "DownloadClients",
                    "QBittorrent",
                    r#"{"host":"::1","port":8081}"#,
                ),
                app,
                &mut vec![],
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(&records[0].input.settings).unwrap()["endpoint"],
                "http://[::1]:8081"
            );
        }
        for settings in [
            r#"{"baseUrl":"https://example.test","categories":null}"#,
            r#"{"baseUrl":"https://example.test","apiPath":null}"#,
            r#"{"baseUrl":"https://example.test","additionalParameters":"&key=%FF"}"#,
            r#"{"baseUrl":"https://example.test","additionalParameters":"&key=%z1"}"#,
        ] {
            assert!(
                read(
                    &source("Indexers", "Torznab", settings),
                    Application::Sonarr,
                    &mut vec![]
                )
                .is_err()
            );
        }
        for settings in [
            r#"{"host":null}"#,
            r#"{"host":"example.test:8080"}"#,
            r#"{"apiKey":"secret","username":"u","password":"p"}"#,
        ] {
            assert!(
                read(
                    &source("DownloadClients", "QBittorrent", settings),
                    Application::Sonarr,
                    &mut vec![]
                )
                .is_err()
            );
        }
        assert!(validate_encoding("&key=hello+world%26%E2%98%83").is_ok());
        let mut oversized = source("Indexers", "Unknown", "{}");
        let table = oversized.tables.get_mut("Indexers").unwrap();
        let row = table.rows[0].clone();
        table.rows = vec![row; 257];
        assert!(read(&oversized, Application::Sonarr, &mut vec![]).is_err());
    }
}
