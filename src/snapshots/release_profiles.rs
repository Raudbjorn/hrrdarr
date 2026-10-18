//! Source restriction catalogs retain domain semantics and unresolved disabled references.
use super::*;
use crate::release_profiles::{
    self as native, Definition, IndexerReference, SourceApplication, TvPolicy,
};
use std::collections::BTreeSet;
pub(super) struct SourceProfile {
    pub id: i64,
    pub definition: Definition,
    pub indexer_ids: Vec<i64>,
}
pub(super) struct Plan {
    pub table: &'static str,
    pub profiles: Vec<SourceProfile>,
    pub prepared: crate::release_profile_terms::PreparedTerms,
}
fn issue(u: &mut Vec<Unsupported>, table: &str, reason: &str) {
    profiles::issue(u, table, reason)
}
fn array(row: &Record, key: &str) -> Result<Vec<String>> {
    let raw: serde_json::Value = serde_json::from_str(text(row, key)?)
        .map_err(|_| ImportError("malformed release profile terms"))?;
    raw.as_array()
        .ok_or(ImportError("release profile terms must be arrays"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or(ImportError("release profile terms must be strings"))
        })
        .collect()
}
fn ids(row: &Record, key: &str) -> Result<Vec<i64>> {
    let raw: serde_json::Value = serde_json::from_str(text(row, key)?)
        .map_err(|_| ImportError("malformed release profile references"))?;
    let mut seen = BTreeSet::new();
    let mut result = vec![];
    for value in raw
        .as_array()
        .ok_or(ImportError("release profile references must be arrays"))?
    {
        let id = value
            .as_i64()
            .filter(|id| (1..=9_007_199_254_740_991).contains(id))
            .ok_or(ImportError("invalid release profile reference"))?;
        if !seen.insert(id) {
            return Err(ImportError("duplicate release profile reference"));
        }
        result.push(id)
    }
    result.sort_unstable();
    Ok(result)
}
fn legacy_terms(row: &Record, key: &str) -> Result<Vec<String>> {
    match row.get(key) {
        Some(Value::Null) => Ok(vec![]),
        Some(Value::Text(s)) if s.trim().is_empty() => Ok(vec![]),
        Some(Value::Text(s)) => Ok(s
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()),
        _ => Err(ImportError("invalid legacy restriction terms")),
    }
}
/// Structural planning only; semantic matcher preparation must precede any activation writer.
pub(super) async fn read(
    source: &Source,
    app: Application,
    u: &mut Vec<Unsupported>,
) -> Result<Option<Plan>> {
    let tv = matches!(app, Application::Sonarr);
    let legacy = !tv && source.version == 206;
    let table = if legacy {
        "Restrictions"
    } else {
        "ReleaseProfiles"
    };
    let Some(raw) = source.tables.get(table) else {
        issue(u, table, "release profile settings unavailable");
        return Ok(None);
    };
    if raw.rows.len() > 1024 {
        return Err(ImportError("source release profile catalog exceeds limit"));
    }
    let allowed = if legacy {
        vec!["Id", "Required", "Ignored", "Preferred", "Tags"]
    } else if tv {
        vec![
            "Id",
            "Name",
            "Enabled",
            "Required",
            "Ignored",
            "IndexerIds",
            "Tags",
            "ExcludedTags",
            "AirDateRestriction",
            "AirDateGracePeriod",
            "AllowSeasonPackWithoutAllEpisodesAired",
        ]
    } else {
        vec![
            "Id",
            "Name",
            "Enabled",
            "Required",
            "Ignored",
            "IndexerId",
            "Tags",
        ]
    };
    if raw.columns.iter().any(|c| !allowed.contains(&c.as_str())) {
        issue(
            u,
            table,
            "unknown release profile fields; whole catalog inactive",
        );
        return Ok(None);
    }
    let media = if tv {
        crate::api::MediaDomain::Tv
    } else {
        crate::api::MediaDomain::Movies
    };
    let mut seen = BTreeSet::new();
    let mut profiles = vec![];
    let mut inactive_preferred = false;
    let mut noop = false;
    for row in &raw.rows {
        let id = positive(row, "Id")?;
        if id > 9_007_199_254_740_991 || !seen.insert(id) {
            return Err(ImportError("invalid release profile identity"));
        }
        let required = if legacy {
            legacy_terms(row, "Required")?
        } else {
            array(row, "Required")?
        };
        let ignored = if legacy {
            legacy_terms(row, "Ignored")?
        } else {
            array(row, "Ignored")?
        };
        if legacy {
            match row.get("Preferred") {
                Some(Value::Null) => {}
                Some(Value::Text(s)) => inactive_preferred |= !s.is_empty(),
                _ => return Err(ImportError("invalid legacy preferred data")),
            }
        }
        let tag_ids = ids(row, "Tags")?;
        if legacy && required.is_empty() && ignored.is_empty() {
            noop = true;
            continue;
        }
        let name = if legacy {
            None
        } else {
            match row.get("Name") {
                Some(Value::Null) => None,
                Some(Value::Text(s)) => Some(s.clone()),
                _ => return Err(ImportError("invalid release profile name")),
            }
        };
        let indexer_ids = if legacy {
            vec![]
        } else if tv {
            ids(row, "IndexerIds")?
        } else {
            let id = integer(row, "IndexerId")?;
            if !(0..=9_007_199_254_740_991).contains(&id) {
                return Err(ImportError("invalid release profile indexer"));
            }
            if id == 0 { vec![] } else { vec![id] }
        };
        let definition = Definition {
            name,
            enabled: if legacy {
                true
            } else {
                boolean(row, "Enabled")? == 1
            },
            required,
            ignored,
            tag_ids,
            indexers: vec![],
            tv: if tv {
                Some(TvPolicy {
                    excluded_tag_ids: ids(row, "ExcludedTags")?,
                    air_date_restriction: boolean(row, "AirDateRestriction")? == 1,
                    grace_days: i32::try_from(integer(row, "AirDateGracePeriod")?)
                        .map_err(|_| ImportError("release profile grace out of range"))?,
                    allow_unaired_pack: boolean(row, "AllowSeasonPackWithoutAllEpisodesAired")?
                        == 1,
                })
            } else {
                None
            },
        };
        if indexer_ids.len() > if tv { 200 } else { 1 }
            || native::validate_shape(media, &definition).is_err()
        {
            issue(
                u,
                table,
                "unrepresentable release profile; whole catalog inactive",
            );
            return Ok(None);
        }
        profiles.push(SourceProfile {
            id,
            definition,
            indexer_ids,
        });
    }
    profiles.sort_by_key(|p| p.id);
    let definitions = profiles
        .iter()
        .map(|p| p.definition.clone())
        .collect::<Vec<_>>();
    if native::validate_catalog(media, &definitions).is_err()
        || profiles
            .iter()
            .map(|p| {
                p.indexer_ids.len()
                    + p.definition.tag_ids.len()
                    + p.definition
                        .tv
                        .as_ref()
                        .map_or(0, |t| t.excluded_tag_ids.len())
            })
            .sum::<usize>()
            > 8192
    {
        issue(u, table, "release profile catalog exceeds work budget");
        return Ok(None);
    }
    u.retain(|p| p.table != table);
    if inactive_preferred {
        issue(
            u,
            table,
            "legacy Preferred retained as inactive data; no decision score",
        )
    };
    if noop {
        issue(u, table, "legacy no-effect restrictions retained inactive")
    };
    let prepared =
        match crate::release_profile_terms::prepare_terms(media, native::terms(&definitions)).await
        {
            Ok(p) => p,
            Err(error) if error.retryable() => return Err(ImportError(error.code())),
            Err(crate::release_profile_terms::TermError::InvalidSyntax) => {
                return Err(ImportError("release_term_invalid_syntax"));
            }
            Err(error) => {
                issue(
                    u,
                    table,
                    if matches!(
                        error,
                        crate::release_profile_terms::TermError::LimitExceeded
                    ) {
                        "release profile matcher work budget exceeded; whole catalog inactive"
                    } else {
                        "release profile matcher unsupported; whole catalog inactive"
                    },
                );
                return Ok(None);
            }
        };
    Ok(Some(Plan {
        table,
        profiles,
        prepared,
    }))
}
/// Resolve only exact snapshot identities. Missing enabled references leave the whole plan inactive.
async fn resolve(
    c: &Connection,
    plan: &Plan,
    report: &mut Report,
) -> Result<Option<Vec<(i64, Definition)>>> {
    let mut output = vec![];
    for source in &plan.profiles {
        let mut definition = source.definition.clone();
        for tag in definition.tag_ids.iter_mut().chain(
            definition
                .tv
                .iter_mut()
                .flat_map(|t| &mut t.excluded_tag_ids),
        ) {
            let row=c.query("SELECT destination_id FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table='tags' AND source_id=?",params![report.application.name(),report.fingerprint.clone(),*tag]).await?.next().await?;
            let Some(row) = row else {
                return Err(ImportError("release profile tag mapping unavailable"));
            };
            *tag = row.get(0)?;
        }
        for id in &source.indexer_ids {
            let retained=c.query("SELECT r.source_application,r.source_fingerprint,r.source_id FROM release_profile_indexers r JOIN snapshot_mappings m ON m.destination_table='release_profiles' AND m.destination_id=r.profile_id WHERE m.application=? AND m.fingerprint=? AND m.source_id=? AND r.source_application=? AND r.source_fingerprint=? AND r.source_id=?",params![report.application.name(),report.fingerprint.clone(),source.id,report.application.name(),report.fingerprint.clone(),*id]).await?.next().await?;
            if let Some(row) = retained {
                definition
                    .indexers
                    .push(IndexerReference::UnresolvedSource {
                        application: if matches!(report.application, Application::Sonarr) {
                            SourceApplication::Sonarr
                        } else {
                            SourceApplication::Radarr
                        },
                        fingerprint: row.get(1)?,
                        source_id: row.get(2)?,
                    });
                continue;
            }
            let row=c.query("SELECT provider_id FROM snapshot_provider_mappings WHERE application=? AND fingerprint=? AND source_table='Indexers' AND source_id=?",params![report.application.name(),report.fingerprint.clone(),*id]).await?.next().await?;
            let reference = if let Some(row) = row {
                let provider = row.get::<String>(0)?;
                let media = if matches!(report.application, Application::Sonarr) {
                    "tv"
                } else {
                    "movies"
                };
                if c.query("SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND s.media_type=? AND p.implementation IN ('torznab','newznab','torrentrss')",params![provider,media]).await?.next().await?.is_none(){report.conflicts+=1;return Ok(None)}
                IndexerReference::Provider {
                    id: row
                        .get::<String>(0)?
                        .parse()
                        .map_err(|_| ImportError("invalid release profile provider mapping"))?,
                }
            } else {
                if definition.enabled {
                    issue(
                        &mut report.unsupported,
                        plan.table,
                        "enabled release profile indexer unavailable; whole catalog inactive",
                    );
                    return Ok(None);
                }
                IndexerReference::UnresolvedSource {
                    application: if matches!(report.application, Application::Sonarr) {
                        SourceApplication::Sonarr
                    } else {
                        SourceApplication::Radarr
                    },
                    fingerprint: report.fingerprint.clone(),
                    source_id: *id,
                }
            };
            definition.indexers.push(reference);
        }
        definition.canonicalize();
        output.push((source.id, definition));
    }
    Ok(Some(output))
}

pub(super) async fn write(c: &Connection, plan: Option<&Plan>, report: &mut Report) -> Result<()> {
    let Some(plan) = plan else { return Ok(()) };
    let Some(wanted) = resolve(c, plan, report).await? else {
        return Ok(());
    };
    let media = if matches!(report.application, Application::Sonarr) {
        crate::api::MediaDomain::Tv
    } else {
        crate::api::MediaDomain::Movies
    };
    let app = report.application.name();
    crate::release_profile_terms::validate_prepared(
        &plan.prepared,
        media,
        &native::terms(&wanted.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>()),
    )
    .map_err(|e| ImportError(e.code()))?;
    let current = native::catalog(c, media)
        .await
        .map_err(|_| ImportError("release profile destination invalid"))?;
    let exact = current.profiles.len() == wanted.len()
        && current
            .profiles
            .iter()
            .zip(&wanted)
            .all(|(p, (_, v))| p.definition() == *v);
    let prior=c.query("SELECT 1 FROM snapshot_imports WHERE application=? AND release_profile_version=1 LIMIT 1",[app]).await?.next().await?.is_some();
    let activated=c.query("SELECT release_profile_version FROM snapshot_imports WHERE application=? AND fingerprint=?",params![app,report.fingerprint.clone()]).await?.next().await?.ok_or(ImportError("release profile provenance unavailable"))?.get::<i64>(0)?==1;
    let mut mappings = vec![];
    for (source, _) in &wanted {
        mappings.push(c.query("SELECT destination_id FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table='release_profiles' AND source_id=?",params![app,report.fingerprint.clone(),*source]).await?.next().await?.map(|r|r.get::<i64>(0)).transpose()?);
    }
    let touched = c
        .query(
            "SELECT locally_edited FROM release_profile_domains WHERE media_type=?",
            [crate::revision_policy::domain(media)],
        )
        .await?
        .next()
        .await?
        .ok_or(ImportError("release profile domain missing"))?
        .get::<i64>(0)?
        != 0;
    // Empty equality carries no mapped identity. Do not turn a protected native
    // empty catalog into a new source activation; an already activated replay is safe.
    if exact && wanted.is_empty() && !activated && (touched || prior) {
        report.conflicts += 1;
        return Ok(());
    }
    let mut destinations = vec![];
    if exact {
        for (p, mapping) in current.profiles.iter().zip(&mappings) {
            if mapping.is_some_and(|id| id != p.id()) || (activated && mapping.is_none()) {
                report.conflicts += 1;
                return Ok(());
            }
            destinations.push(p.id())
        }
        report.duplicates += wanted.len();
    } else {
        if touched || prior || !current.profiles.is_empty() || mappings.iter().any(Option::is_some)
        {
            report.conflicts += 1;
            return Ok(());
        }
        for (_, definition) in &wanted {
            destinations.push(
                native::store(c, media, None, definition.clone(), true)
                    .await
                    .map_err(|_| ImportError("release profile activation failed"))?,
            )
        }
        native::bump(c, media, current.revision, false)
            .await
            .map_err(|_| ImportError("release profile wake failed"))?;
        native::catalog(c, media)
            .await
            .map_err(|_| ImportError("release profile activated graph invalid"))?;
        report.mapped += wanted.len();
    }
    for ((source, _), destination) in wanted.iter().zip(destinations) {
        c.execute("INSERT INTO snapshot_mappings(application,fingerprint,destination_table,source_id,destination_id)VALUES(?,?,'release_profiles',?,?) ON CONFLICT DO NOTHING",params![app,report.fingerprint.clone(),*source,destination]).await?;
    }
    c.execute("UPDATE snapshot_imports SET release_profile_version=1 WHERE application=? AND fingerprint=?",params![app,report.fingerprint.clone()]).await?;
    Ok(())
}
