//! Whole ordered delay graphs activate only without overwriting native/source intent.
use super::*;
use crate::{
    api::MediaDomain,
    delay_profiles::{self as native, FullSettings, Input, Protocol, Settings},
};
use std::collections::BTreeSet;
pub(super) struct Plan {
    profiles: Vec<(i64, Input)>,
    availability: i32,
}
fn unsupported(u: &mut Vec<Unsupported>, reason: &str) -> Option<Plan> {
    profiles::issue(u, "DelayProfiles", reason);
    None
}
pub(super) fn read(
    source: &Source,
    app: Application,
    u: &mut Vec<Unsupported>,
) -> Result<Option<Plan>> {
    let Some(table) = source.tables.get("DelayProfiles") else {
        return Ok(unsupported(u, "delay profiles unavailable"));
    };
    if table.rows.len() > 1024 {
        return Err(ImportError("source delay catalog exceeds limit"));
    }
    let allowed = [
        "Id",
        "EnableUsenet",
        "EnableTorrent",
        "PreferredProtocol",
        "UsenetDelay",
        "TorrentDelay",
        "Order",
        "BypassIfHighestQuality",
        "BypassIfAboveCustomFormatScore",
        "MinimumCustomFormatScore",
        "Tags",
    ];
    if table.columns.iter().any(|c| !allowed.contains(&c.as_str())) {
        return Ok(unsupported(u, "unknown delay profile fields"));
    }
    let modern = matches!(app, Application::Sonarr) || source.version >= 228;
    if !modern
        && table.columns.iter().any(|c| {
            ["BypassIfAboveCustomFormatScore", "MinimumCustomFormatScore"].contains(&c.as_str())
        })
    {
        return Ok(unsupported(
            u,
            "unexpected delay score fields for source version",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut all_tags = BTreeSet::new();
    let mut order = BTreeSet::new();
    let mut parsed = vec![];
    for row in &table.rows {
        let id = positive(row, "Id")?;
        if id > 9_007_199_254_740_991 || !ids.insert(id) {
            return Err(ImportError("invalid source delay identity"));
        }
        let global = id == 1;
        let pos = integer(row, "Order")?;
        if !global && (pos <= 0 || !order.insert(pos)) {
            return Err(ImportError("invalid source delay order"));
        }
        let raw: serde_json::Value = serde_json::from_str(text(row, "Tags")?)
            .map_err(|_| ImportError("malformed source delay tags"))?;
        let tags = raw
            .as_array()
            .ok_or(ImportError("source delay tags must be array"))?;
        let mut tag_ids = vec![];
        let mut unique = BTreeSet::new();
        for tag in tags {
            let id = tag
                .as_i64()
                .filter(|i| *i > 0 && *i <= 9_007_199_254_740_991)
                .ok_or(ImportError("invalid source delay tag"))?;
            if !unique.insert(id) || !all_tags.insert(id) {
                return Err(ImportError("duplicate source delay tag"));
            }
            tag_ids.push(id);
        }
        let preferred = match integer(row, "PreferredProtocol")? {
            1 => Protocol::Usenet,
            2 => Protocol::Torrent,
            _ => return Ok(unsupported(u, "unsupported preferred protocol")),
        };
        let score_bypass = if modern {
            boolean(row, "BypassIfAboveCustomFormatScore")? == 1
        } else {
            false
        };
        let score = if modern {
            match row.get("MinimumCustomFormatScore") {
                Some(Value::Null) if !score_bypass => 0,
                Some(Value::Integer(v)) => match i32::try_from(*v) {
                    Ok(v) => v,
                    Err(_) => return Ok(unsupported(u, "unsupported delay score threshold")),
                },
                _ => return Ok(unsupported(u, "unknown enabled delay score threshold")),
            }
        } else {
            0
        };
        let torrent = integer(row, "TorrentDelay")?;
        let usenet = integer(row, "UsenetDelay")?;
        if !(0..=10080).contains(&torrent) || !(0..=10080).contains(&usenet) {
            return Ok(unsupported(u, "source delay exceeds native bound"));
        }
        let input = Input {
            settings: FullSettings {
                torrent_delay_minutes: torrent as u32,
                usenet_delay_minutes: usenet as u32,
                enable_torrent: boolean(row, "EnableTorrent")? == 1,
                enable_usenet: boolean(row, "EnableUsenet")? == 1,
                preferred_protocol: preferred,
                bypass_if_highest_quality: boolean(row, "BypassIfHighestQuality")? == 1,
                bypass_if_above_custom_format_score: score_bypass,
                minimum_custom_format_score: score,
            },
            tag_ids,
        };
        if native::validate(&input, global).is_err() {
            return Ok(unsupported(u, "unrepresentable whole delay profile"));
        }
        parsed.push((global, pos, id, input));
    }
    if !ids.contains(&1) {
        return Ok(unsupported(u, "source global delay profile missing"));
    }
    // Global source row must really be the fallback, not an earlier untagged matcher.
    let global_order = parsed.iter().find(|p| p.0).unwrap().1;
    if parsed.iter().any(|p| !p.0 && p.1 >= global_order) {
        return Ok(unsupported(u, "source global delay order unsupported"));
    }
    let availability = if matches!(app, Application::Sonarr) {
        0
    } else {
        let Some(config) = source.tables.get("Config") else {
            return Ok(unsupported(u, "source availability settings unavailable"));
        };
        if !["Key", "Value"]
            .iter()
            .all(|key| config.columns.iter().any(|c| c == key))
        {
            return Err(ImportError("invalid source Config shape"));
        }
        if config
            .columns
            .iter()
            .any(|c| !["Id", "Key", "Value"].contains(&c.as_str()))
        {
            return Ok(unsupported(u, "unsupported availability settings fields"));
        }
        let mut found = None;
        for row in &config.rows {
            if text(row, "Key")?.eq_ignore_ascii_case("availabilitydelay") {
                if found.is_some() {
                    return Err(ImportError("duplicate source availability delay"));
                }
                found = Some(
                    text(row, "Value")?
                        .parse::<i32>()
                        .map_err(|_| ImportError("invalid source availability delay"))?,
                );
            }
        }
        let days = found.unwrap_or(0);
        if !(-365..=365).contains(&days) {
            return Ok(unsupported(u, "source availability exceeds native bound"));
        }
        days
    };
    parsed.sort_by_key(|(global, pos, ..)| (!*global, *pos)); // Write fallback first, then tagged order.
    u.retain(|issue| issue.table != "DelayProfiles");
    Ok(Some(Plan {
        profiles: parsed.into_iter().map(|(_, _, id, p)| (id, p)).collect(),
        availability,
    }))
}
pub(super) async fn write(c: &Connection, plan: Option<&Plan>, report: &mut Report) -> Result<()> {
    let Some(plan) = plan else { return Ok(()) };
    let app = report.application.name();
    let media = if matches!(report.application, Application::Sonarr) {
        MediaDomain::Tv
    } else {
        MediaDomain::Movies
    };
    let mut wanted = vec![];
    for (source, input) in &plan.profiles {
        let mut input = input.clone();
        for tag in &mut input.tag_ids {
            let mapped=c.query("SELECT destination_id FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table='tags' AND source_id=?",params![app,report.fingerprint.clone(),*tag]).await?.next().await?;
            let Some(mapped) = mapped else {
                report.conflicts += 1;
                return Ok(());
            };
            *tag = mapped.get(0)?;
        }
        input.tag_ids.sort_unstable();
        wanted.push((*source, input));
    }
    let current = native::catalog(c, media)
        .await
        .map_err(|_| ImportError("delay destination invalid"))?;
    let global = current
        .profiles
        .iter()
        .find(|p| p.is_global)
        .ok_or(ImportError("delay global missing"))?;
    let mut actual = vec![global];
    actual.extend(current.profiles.iter().filter(|p| !p.is_global));
    let exact = current.configured
        && current.availability_delay_days == Some(plan.availability)
        && actual.len() == wanted.len()
        && actual.iter().zip(&wanted).all(|(p, (_, input))| {
            p.tag_ids == input.tag_ids && p.settings == Settings::Profile(input.settings.clone())
        });
    let prior=c.query("SELECT 1 FROM snapshot_imports WHERE application=? AND delay_profile_version=1 LIMIT 1",[app]).await?.next().await?.is_some();
    let this_activated=c.query("SELECT delay_profile_version FROM snapshot_imports WHERE application=? AND fingerprint=?",params![app,report.fingerprint.clone()]).await?.next().await?.ok_or(ImportError("snapshot delay provenance missing"))?.get::<i64>(0)?==1;
    let mut mapped = vec![];
    for (source, _) in &wanted {
        mapped.push(c.query("SELECT destination_id FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table='delay_profiles' AND source_id=?",params![app,report.fingerprint.clone(),*source]).await?.next().await?.map(|r|r.get::<i64>(0)).transpose()?);
    }
    let mut ids = vec![];
    if exact {
        for (p, mapped) in actual.iter().zip(&mapped) {
            if mapped.is_some_and(|id| id != p.id) || (this_activated && mapped.is_none()) {
                report.conflicts += 1;
                return Ok(());
            }
            ids.push(p.id);
        }
        report.duplicates += wanted.len();
    } else {
        let touched = c
            .query(
                "SELECT locally_edited FROM delay_profile_domains WHERE media_type=?",
                [crate::revision_policy::domain(media)],
            )
            .await?
            .next()
            .await?
            .ok_or(ImportError("delay domain missing"))?
            .get::<i64>(0)?
            != 0;
        if touched
            || prior
            || current.configured
            || current.profiles.len() != 1
            || mapped.iter().any(Option::is_some)
        {
            report.conflicts += 1;
            return Ok(());
        }
        for (source, input) in &wanted {
            let id = native::store(
                c,
                media,
                (*source == 1).then_some(global.id),
                input,
                Some(plan.availability),
            )
            .await
            .map_err(|_| ImportError("delay source activation failed"))?;
            ids.push(id);
        }
        native::bump(c, media, current.revision, false, true)
            .await
            .map_err(|_| ImportError("delay source wake failed"))?;
        native::catalog(c, media)
            .await
            .map_err(|_| ImportError("delay destination graph invalid"))?;
        report.mapped += wanted.len();
    }
    for ((source, _), id) in wanted.iter().zip(ids) {
        c.execute("INSERT INTO snapshot_mappings(application,fingerprint,destination_table,source_id,destination_id)VALUES(?,?,'delay_profiles',?,?) ON CONFLICT DO NOTHING",params![app,report.fingerprint.clone(),*source,id]).await?;
    }
    c.execute(
        "UPDATE snapshot_imports SET delay_profile_version=1 WHERE application=? AND fingerprint=?",
        params![app, report.fingerprint.clone()],
    )
    .await?;
    Ok(())
}
