//! Default client selection uses scoped owner tags and volatile per-domain cursors. Only clients
//! sharing the indexer's release protocol are eligible; an explicit mismatch is rejected visibly.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientSelection {
    Default {},
    Explicit {
        provider_id: Uuid,
        provider_revision: i64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientRef {
    pub client_id: String,
    pub client_revision: i64,
}
struct Eligible {
    client: ClientRef,
    priority: i64,
    tagged: bool,
    matching: bool,
    blocked: bool,
}
pub(super) fn tags_input<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Vec<i64>>, D::Error> {
    Vec::<i64>::deserialize(d).map(Some)
}
pub(super) async fn read_tags(c: &Connection, id: &str, media: &str) -> Result<Vec<i64>> {
    let mut rows = c.query("SELECT tag_id FROM download_client_tags WHERE provider_id=? AND media_type=? ORDER BY tag_id LIMIT 201",params![id,media]).await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        ids.push(row.get(0)?);
    }
    if ids.len() > crate::tags::MAX_ASSIGNMENTS {
        return Err(corrupt());
    }
    Ok(ids)
}
pub(super) async fn validate_tags(c: &Connection, media: &str, ids: &[i64]) -> Result<()> {
    crate::tags::validate(&crate::tags::Assignment {
        mode: crate::tags::Mode::Replace,
        ids: ids.to_vec(),
    })
    .map_err(|_| bad())?;
    for id in ids {
        if c.query(
            "SELECT 1 FROM tags WHERE id=? AND media_type=?",
            params![*id, media],
        )
        .await?
        .next()
        .await?
        .is_none()
        {
            return Err(bad());
        }
    }
    Ok(())
}
fn unavailable() -> Error {
    Error::Plain(
        StatusCode::CONFLICT,
        "download_client_unavailable",
        "No eligible download client is available",
    )
}
fn protocol_mismatch() -> Error {
    Error::Plain(
        StatusCode::CONFLICT,
        "protocol_mismatch",
        "The selected download client does not accept this release protocol",
    )
}
fn domain(target: &crate::db::MediaTarget) -> (&'static str, usize, i64) {
    match target {
        crate::db::MediaTarget::Episode(id) => ("tv", 0, *id),
        crate::db::MediaTarget::Movie(id) => ("movies", 1, *id),
    }
}
fn filter(mut clients: Vec<Eligible>, binding: Option<&str>, strict: bool) -> Vec<Eligible> {
    let matching = clients.iter().any(|c| c.matching);
    clients.retain(|c| if matching { c.matching } else { !c.tagged });
    if let Some(binding) = binding {
        clients.retain(|c| c.client.client_id == binding);
    }
    if strict || clients.iter().any(|c| !c.blocked) {
        clients.retain(|c| !c.blocked);
    }
    clients.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.client.client_id.cmp(&b.client.client_id))
    });
    clients
}
fn order(mut clients: Vec<Eligible>, last: Option<&str>, movies: bool) -> Vec<ClientRef> {
    if movies && let Some(first) = clients.first() {
        let priority = first.priority;
        clients.retain(|c| c.priority == priority);
        let next = last
            .and_then(|last| {
                clients
                    .iter()
                    .position(|c| c.client.client_id.as_str() > last)
            })
            .unwrap_or(0);
        return vec![clients.remove(next).client];
    }
    let mut start = 0;
    while start < clients.len() {
        let count = clients[start..]
            .iter()
            .take_while(|c| c.priority == clients[start].priority)
            .count();
        let group = &mut clients[start..start + count];
        if let Some(position) = group
            .iter()
            .position(|c| Some(c.client.client_id.as_str()) == last)
        {
            group.rotate_left((position + 1) % count);
        }
        start += count;
    }
    clients.into_iter().map(|c| c.client).collect()
}
async fn eligible(
    c: &Connection,
    indexer: Uuid,
    target: &crate::db::MediaTarget,
    intent: ClientSelection,
    strict: bool,
    now: i64,
) -> Result<Vec<Eligible>> {
    if c.is_autocommit() {
        return Err(corrupt());
    }
    let (media, _, target_id) = domain(target);
    let source=c.query("SELECT r.protocol,s.download_client_id FROM providers p JOIN provider_implementations r ON r.implementation=p.implementation JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND p.enabled=1 AND r.kind='indexer' AND s.media_type=?",params![indexer.to_string(),media]).await?.next().await?.ok_or_else(unavailable)?;
    let protocol: String = source.get(0)?;
    if !matches!(protocol.as_str(), "torrent" | "usenet") {
        return Err(Error::Plain(
            StatusCode::CONFLICT,
            "unsupported_target",
            "No client implements this release protocol",
        ));
    }
    let binding: Option<String> = source.get(1)?;
    // A stored binding predating protocol validation cannot silently select nothing.
    if let Some(bound) = &binding
        && let Some(row) = c.query("SELECT r.protocol FROM providers p JOIN provider_implementations r ON r.implementation=p.implementation WHERE p.id=? AND r.kind='download_client'",[bound.clone()]).await?.next().await?
            && row.get::<String>(0)? != protocol {
                return Err(protocol_mismatch());
            }
    let owner = match target {
        crate::db::MediaTarget::Episode(_) => c
            .query("SELECT series_id FROM episodes WHERE id=?", [target_id])
            .await?
            .next()
            .await?
            .ok_or_else(unavailable)?
            .get::<i64>(0)?,
        crate::db::MediaTarget::Movie(_) => c
            .query("SELECT id FROM movies WHERE id=?", [target_id])
            .await?
            .next()
            .await?
            .ok_or_else(unavailable)?
            .get::<i64>(0)?,
    };
    if let ClientSelection::Explicit {
        provider_id,
        provider_revision,
    } = intent
    {
        let row = c.query("SELECT p.id,p.revision,p.priority,r.protocol FROM providers p JOIN provider_implementations r ON r.implementation=p.implementation JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND p.revision=? AND p.enabled=1 AND r.kind='download_client' AND s.media_type=?",params![provider_id.to_string(),provider_revision,media]).await?.next().await?.ok_or_else(unavailable)?;
        if row.get::<String>(3)? != protocol {
            return Err(protocol_mismatch());
        }
        return Ok(vec![Eligible {
            client: ClientRef {
                client_id: row.get(0)?,
                client_revision: row.get(1)?,
            },
            priority: row.get(2)?,
            tagged: false,
            matching: false,
            blocked: false,
        }]);
    }
    let (table, column) = if media == "tv" {
        ("series_tags", "series_id")
    } else {
        ("movie_tags", "movie_id")
    };
    let mut rows=c.query(&format!("SELECT p.id,p.revision,p.priority,EXISTS(SELECT 1 FROM download_client_tags t WHERE t.provider_id=p.id AND t.media_type=s.media_type),EXISTS(SELECT 1 FROM download_client_tags t JOIN {table} o ON o.tag_id=t.tag_id WHERE t.provider_id=p.id AND t.media_type=s.media_type AND o.{column}=?),COALESCE(b.disabled_until>?,0) FROM providers p JOIN provider_scopes s ON s.provider_id=p.id LEFT JOIN download_client_status b ON b.provider_id=p.id AND b.media_type=s.media_type AND b.config_revision=p.revision WHERE p.enabled=1 AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client' AND protocol=?) AND s.media_type=? ORDER BY p.id LIMIT 129"),params![owner,now,protocol.clone(),media]).await?;
    let mut clients = Vec::new();
    while let Some(row) = rows.next().await? {
        clients.push(Eligible {
            client: ClientRef {
                client_id: row.get(0)?,
                client_revision: row.get(1)?,
            },
            priority: row.get(2)?,
            tagged: row.get::<i64>(3)? == 1,
            matching: row.get::<i64>(4)? == 1,
            blocked: row.get::<i64>(5)? == 1,
        });
    }
    if clients.len() > 128 {
        return Err(Error::Plain(
            StatusCode::CONFLICT,
            "refresh_limit",
            "Client selection exceeds its bound",
        ));
    }
    clients = filter(clients, binding.as_deref(), strict);
    if clients.is_empty() {
        return Err(unavailable());
    }
    Ok(clients)
}
impl RefreshClient {
    pub(crate) async fn selection_plan(
        &self,
        c: &Connection,
        indexer: Uuid,
        target: &crate::db::MediaTarget,
        intent: ClientSelection,
        strict: bool,
        now: i64,
    ) -> std::result::Result<Vec<ClientRef>, AutomationError> {
        let clients = eligible(c, indexer, target, intent, strict, now)
            .await
            .map_err(refresh_error)?;
        let (_, index, _) = domain(target);
        let cursor = self
            .0
            .selection_cursor
            .lock()
            .map_err(|_| RefreshError::new("provider_unavailable", false))?;
        Ok(order(clients, cursor[index].as_deref(), index == 1))
    }
    pub(crate) async fn revalidate_selection(
        &self,
        c: &Connection,
        indexer: Uuid,
        target: &crate::db::MediaTarget,
        intent: ClientSelection,
        strict: bool,
        now: i64,
        selected: &ClientRef,
    ) -> std::result::Result<(), AutomationError> {
        if eligible(c, indexer, target, intent, strict, now)
            .await
            .map_err(refresh_error)?
            .iter()
            .any(|c| &c.client == selected)
        {
            Ok(())
        } else {
            Err(RefreshError::new("download_client_unavailable", false))
        }
    }
    pub(crate) fn selection_committed(&self, domain: MediaDomain, client_id: &str) {
        if matches!(domain, MediaDomain::Movies)
            && let Ok(mut cursor) = self.0.selection_cursor.lock()
        {
            cursor[1] = Some(client_id.into());
        }
    }
    pub(crate) fn selection_succeeded(&self, domain: MediaDomain, client_id: &str) {
        if matches!(domain, MediaDomain::Tv)
            && let Ok(mut cursor) = self.0.selection_cursor.lock()
        {
            cursor[0] = Some(client_id.into());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn client(id: &str, priority: i64, tagged: bool, matching: bool, blocked: bool) -> Eligible {
        Eligible {
            client: ClientRef {
                client_id: id.into(),
                client_revision: 1,
            },
            priority,
            tagged,
            matching,
            blocked,
        }
    }
    #[test]
    fn tags_displace_untagged_then_binding_and_backoff_apply() {
        assert_eq!(
            filter(
                vec![
                    client("a", 1, false, false, false),
                    client("b", 2, true, true, false)
                ],
                None,
                false
            )[0]
            .client
            .client_id,
            "b"
        );
        assert!(
            filter(
                vec![
                    client("a", 1, false, false, false),
                    client("b", 2, true, true, false)
                ],
                Some("a"),
                false
            )
            .is_empty()
        );
        assert_eq!(
            filter(
                vec![
                    client("a", 1, false, false, true),
                    client("b", 2, false, false, false)
                ],
                None,
                false
            )[0]
            .client
            .client_id,
            "b"
        );
        assert_eq!(
            filter(vec![client("a", 1, false, false, true)], None, false).len(),
            1
        );
        assert!(filter(vec![client("a", 1, false, false, true)], None, true).is_empty());
        assert!(filter(vec![client("a", 1, true, false, false)], None, false).is_empty());
    }
    #[test]
    fn domain_rotation_and_priority_groups_are_distinct() {
        let clients = || {
            vec![
                client("a", 1, false, false, false),
                client("b", 1, false, false, false),
                client("c", 2, false, false, false),
            ]
        };
        assert_eq!(
            order(clients(), Some("a"), false)
                .iter()
                .map(|c| c.client_id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a", "c"]
        );
        assert_eq!(
            order(clients(), Some("a"), true)
                .iter()
                .map(|c| c.client_id.as_str())
                .collect::<Vec<_>>(),
            vec!["b"]
        );
        assert_eq!(order(clients(), Some("b"), true)[0].client_id, "a");
        assert_eq!(order(clients(), Some("aa"), true)[0].client_id, "b");
    }
    #[tokio::test]
    async fn transactional_selection_resolves_each_domain_and_explicit_override() {
        let root = std::env::temp_dir().join(format!("hrrdarr-selection-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let db = Arc::new(Database::open_local(root.join("db")).await.unwrap());
        let (_, client) = router_with_refresh(db.clone(), None);
        let c = db.connect().await.unwrap();
        c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'TV','/tv/1'); INSERT INTO seasons(series_id,number)VALUES(1,1); INSERT INTO episodes(id,series_id,season,number,title)VALUES(1,1,1,1,'Episode'); INSERT INTO movie_metadata(id,title)VALUES(1,'Movie'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies/1'); INSERT INTO tags(id,media_type,label)VALUES(1,'tv','tv-tag'),(2,'movies','movie-tag'); INSERT INTO series_tags(series_id,tag_id)VALUES(1,1); INSERT INTO movie_tags(movie_id,tag_id)VALUES(1,2);").await.unwrap();
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        let indexer = Uuid::new_v4();
        let untagged = Uuid::new_v4();
        let tagged = Uuid::new_v4();
        for (id, implementation) in [
            (indexer, "torznab"),
            (untagged, "qbittorrent"),
            (tagged, "qbittorrent"),
        ] {
            tx.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,?,?,1,1,1,1,'http://127.0.0.1:1')",params![id.to_string(),implementation,id.to_string()]).await.unwrap();
            let settings: ProviderSettings = if implementation == "torznab" {
                serde_json::from_value(serde_json::json!({"implementation":"torznab","endpoint":"http://127.0.0.1:1","tv":{"categories":[5000],"anime_categories":[]},"movies":{"categories":[2000]}})).unwrap()
            } else {
                serde_json::from_value(serde_json::json!({"implementation":"qbittorrent","endpoint":"http://127.0.0.1:1","tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0,"tags":if id==tagged {vec![1]}else{vec![]}},"movies":{"category":"movies","imported_category":null,"recent_priority":0,"older_priority":0,"tags":if id==tagged {vec![2]}else{vec![]}}})).unwrap()
            };
            write_scopes(&tx, &id.to_string(), &settings).await.unwrap();
        }
        for target in [
            crate::db::MediaTarget::Episode(1),
            crate::db::MediaTarget::Movie(1),
        ] {
            let plan = client
                .selection_plan(
                    &tx,
                    indexer,
                    &target,
                    ClientSelection::Default {},
                    false,
                    1000,
                )
                .await
                .unwrap();
            assert_eq!(
                plan,
                vec![ClientRef {
                    client_id: tagged.to_string(),
                    client_revision: 1
                }]
            );
            let explicit = ClientSelection::Explicit {
                provider_id: untagged,
                provider_revision: 1,
            };
            assert_eq!(
                client
                    .selection_plan(&tx, indexer, &target, explicit, true, 1000)
                    .await
                    .unwrap()[0]
                    .client_id,
                untagged.to_string()
            );
            let media = domain(&target).0;
            tx.execute("UPDATE provider_scopes SET download_client_id=? WHERE provider_id=? AND media_type=?",params![untagged.to_string(),indexer.to_string(),media]).await.unwrap();
            assert_eq!(
                client
                    .selection_plan(
                        &tx,
                        indexer,
                        &target,
                        ClientSelection::Default {},
                        false,
                        1000
                    )
                    .await
                    .unwrap_err()
                    .code,
                "download_client_unavailable"
            );
            assert!(
                client
                    .revalidate_selection(
                        &tx,
                        indexer,
                        &target,
                        explicit,
                        true,
                        1000,
                        &ClientRef {
                            client_id: untagged.to_string(),
                            client_revision: 1
                        }
                    )
                    .await
                    .is_ok()
            );
            assert!(
                client
                    .selection_plan(
                        &tx,
                        indexer,
                        &target,
                        ClientSelection::Explicit {
                            provider_id: untagged,
                            provider_revision: 2
                        },
                        false,
                        1000
                    )
                    .await
                    .is_err()
            );
        }
        // Use the validated scoped writer so the overflow fixture retains all required client options.
        let settings = read(&tx, &untagged.to_string()).await.unwrap().0.settings;
        // Explicit authority is independent of the default plan's 128-client bound.
        for _ in 0..127 {
            let id = Uuid::new_v4().to_string();
            tx.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'qbittorrent',?,1,1,1,1,'http://127.0.0.1:1')",params![id.clone(),id.clone()]).await.unwrap();
            write_scopes(&tx, &id, &settings).await.unwrap();
        }
        for target in [
            crate::db::MediaTarget::Episode(1),
            crate::db::MediaTarget::Movie(1),
        ] {
            assert_eq!(
                client
                    .selection_plan(
                        &tx,
                        indexer,
                        &target,
                        ClientSelection::Default {},
                        false,
                        1000
                    )
                    .await
                    .unwrap_err()
                    .code,
                "refresh_limit"
            );
            let explicit = ClientSelection::Explicit {
                provider_id: untagged,
                provider_revision: 1,
            };
            let selected = ClientRef {
                client_id: untagged.to_string(),
                client_revision: 1,
            };
            assert_eq!(
                client
                    .selection_plan(&tx, indexer, &target, explicit, true, 1000)
                    .await
                    .unwrap(),
                vec![selected.clone()]
            );
            client
                .revalidate_selection(&tx, indexer, &target, explicit, true, 1000, &selected)
                .await
                .unwrap();
        }
        tx.rollback().await.unwrap();
        drop(c);
        drop(client);
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn cursor_reads_rollback_commit_success_and_new_runtime_are_distinct() {
        async fn choices(client: &RefreshClient, c: &Connection, indexer: Uuid) -> [String; 2] {
            let mut selected = Vec::new();
            for target in [
                crate::db::MediaTarget::Episode(1),
                crate::db::MediaTarget::Movie(1),
            ] {
                let plan = client
                    .selection_plan(
                        c,
                        indexer,
                        &target,
                        ClientSelection::Default {},
                        false,
                        1000,
                    )
                    .await
                    .unwrap();
                client
                    .revalidate_selection(
                        c,
                        indexer,
                        &target,
                        ClientSelection::Default {},
                        false,
                        1000,
                        &plan[0],
                    )
                    .await
                    .unwrap();
                selected.push(plan[0].client_id.clone());
            }
            selected.try_into().unwrap()
        }
        let root = std::env::temp_dir().join(format!("hrrdarr-cursor-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let db = Arc::new(Database::open_local(root.join("db")).await.unwrap());
        let (_, client) = router_with_refresh(db.clone(), None);
        let c = db.connect().await.unwrap();
        c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'TV','/tv/1'); INSERT INTO seasons(series_id,number)VALUES(1,1); INSERT INTO episodes(id,series_id,season,number,title)VALUES(1,1,1,1,'Episode'); INSERT INTO movie_metadata(id,title)VALUES(1,'Movie'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies/1');").await.unwrap();
        let indexer = Uuid::new_v4();
        let mut ids = [Uuid::new_v4().to_string(), Uuid::new_v4().to_string()];
        ids.sort();
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        for (id, implementation) in [
            (indexer.to_string(), "torznab"),
            (ids[0].clone(), "qbittorrent"),
            (ids[1].clone(), "qbittorrent"),
        ] {
            tx.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,?,?,1,1,1,1,'http://127.0.0.1:1')",params![id.clone(),implementation,id.clone()]).await.unwrap();
            let settings: ProviderSettings = if implementation == "torznab" {
                serde_json::from_value(serde_json::json!({"implementation":"torznab","endpoint":"http://127.0.0.1:1","tv":{"categories":[5000],"anime_categories":[]},"movies":{"categories":[2000]}})).unwrap()
            } else {
                serde_json::from_value(serde_json::json!({"implementation":"qbittorrent","endpoint":"http://127.0.0.1:1","tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":{"category":"movies","imported_category":null,"recent_priority":0,"older_priority":0}})).unwrap()
            };
            write_scopes(&tx, &id, &settings).await.unwrap();
        }
        tx.commit().await.unwrap();
        let tx = c.transaction().await.unwrap();
        assert_eq!(
            choices(&client, &tx, indexer).await,
            [ids[0].clone(), ids[0].clone()]
        );
        assert_eq!(
            choices(&client, &tx, indexer).await,
            [ids[0].clone(), ids[0].clone()]
        );
        tx.rollback().await.unwrap();
        let tx = c.transaction().await.unwrap();
        assert_eq!(
            choices(&client, &tx, indexer).await,
            [ids[0].clone(), ids[0].clone()]
        );
        tx.commit().await.unwrap();
        // The serial worker publishes a movie choice only after its transaction commits.
        client.selection_committed(MediaDomain::Movies, &ids[0]);
        client.selection_committed(MediaDomain::Tv, &ids[0]);
        client.selection_succeeded(MediaDomain::Movies, &ids[1]);
        let tx = c.transaction().await.unwrap();
        assert_eq!(
            choices(&client, &tx, indexer).await,
            [ids[0].clone(), ids[1].clone()]
        );
        tx.commit().await.unwrap();
        client.selection_succeeded(MediaDomain::Tv, &ids[0]);
        let tx = c.transaction().await.unwrap();
        assert_eq!(
            choices(&client, &tx, indexer).await,
            [ids[1].clone(), ids[1].clone()]
        );
        let explicit = ClientSelection::Explicit {
            provider_id: Uuid::parse_str(&ids[0]).unwrap(),
            provider_revision: 1,
        };
        client
            .selection_plan(
                &tx,
                indexer,
                &crate::db::MediaTarget::Movie(1),
                explicit,
                false,
                1000,
            )
            .await
            .unwrap();
        assert_eq!(
            choices(&client, &tx, indexer).await,
            [ids[1].clone(), ids[1].clone()]
        );
        let (_, fresh) = router_with_refresh(db.clone(), None);
        assert_eq!(
            choices(&fresh, &tx, indexer).await,
            [ids[0].clone(), ids[0].clone()]
        );
        tx.rollback().await.unwrap();
        drop(c);
        drop(client);
        drop(fresh);
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn selection_is_closed_and_tags_distinguish_omission() {
        assert_eq!(
            serde_json::from_value::<ClientSelection>(serde_json::json!({"kind":"default"}))
                .unwrap(),
            ClientSelection::Default {}
        );
        assert_eq!(
            serde_json::to_value(ClientSelection::Default {}).unwrap(),
            serde_json::json!({"kind":"default"})
        );
        assert!(serde_json::from_value::<ClientSelection>(serde_json::json!({"kind":"explicit","provider_id":Uuid::nil(),"provider_revision":1,"extra":true})).is_err());
        assert!(
            serde_json::from_value::<ClientSelection>(
                serde_json::json!({"kind":"default","provider_revision":1})
            )
            .is_err()
        );
        let scope = serde_json::json!({"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0});
        assert!(
            serde_json::from_value::<DownloadScope>(scope.clone())
                .unwrap()
                .tags
                .is_none()
        );
        let mut empty = scope.clone();
        empty["tags"] = serde_json::json!([]);
        assert_eq!(
            serde_json::from_value::<DownloadScope>(empty).unwrap().tags,
            Some(vec![])
        );
        let mut null = scope;
        null["tags"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<DownloadScope>(null).is_err());
    }
}
