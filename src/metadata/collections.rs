//! Collection wire facts only. Image URLs are private metadata, never fetch authority.
use super::*;

pub const MAX_COLLECTION_MEMBERS: usize = 1000;
const MAX_IMAGE_JSON_BYTES: usize = 65_536;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum CollectionAssociation {
    #[default]
    Absent,
    Clear,
    Present(CollectionSummary),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct CollectionSummary {
    pub tmdb_id: i64,
    pub title: String,
}
#[derive(Clone, Debug)]
pub struct MovieWithCollection {
    pub movie: MovieDetails,
    pub collection: CollectionAssociation,
}
#[derive(Clone, Debug)]
pub struct CollectionDetails {
    pub tmdb_id: i64,
    pub title: String,
    pub overview: Option<String>,
    pub images: Option<Vec<CollectionImage>>,
    pub movies: Vec<CollectionMember>,
}
#[derive(Clone, Debug)]
pub struct CollectionMember {
    pub movie: MovieDetails,
    pub collection: CollectionAssociation,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub images: Option<Vec<CollectionImage>>,
    pub ratings: Option<BTreeMap<String, Option<CollectionRating>>>,
    pub legacy_ratings: Option<Vec<CollectionRating>>,
}
/// Retained source facts; intentionally not Serialize/TS or a URL admission API.
#[derive(Clone)]
pub struct CollectionImage {
    pub cover_type: String,
    source_url: String,
}
impl std::fmt::Debug for CollectionImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CollectionImage")
            .field("cover_type", &self.cover_type)
            .finish_non_exhaustive()
    }
}
impl CollectionImage {
    /// Source metadata only. Consumers must independently validate origin before fetching.
    pub(crate) fn source_url(&self) -> &str {
        &self.source_url
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionRating {
    pub count: i64,
    pub value: f64,
    pub origin: Option<String>,
    pub r#type: Option<String>,
}
#[derive(Default)]
pub(super) enum WireAssociation {
    #[default]
    Absent,
    Clear,
    Present(WireSummary),
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WireSummary {
    tmdb_id: i64,
    name: String,
}
impl<'de> Deserialize<'de> for WireAssociation {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Option::<WireSummary>::deserialize(d).map(|v| v.map_or(Self::Clear, Self::Present))
    }
}
impl WireAssociation {
    pub(super) fn normalize(self) -> Result<CollectionAssociation> {
        match self {
            Self::Absent => Ok(CollectionAssociation::Absent),
            Self::Clear => Ok(CollectionAssociation::Clear),
            Self::Present(v) => {
                identity(v.tmdb_id)?;
                text(&v.name, 512, false)?;
                Ok(CollectionAssociation::Present(CollectionSummary {
                    tmdb_id: v.tmdb_id,
                    title: v.name,
                }))
            }
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireCollection {
    tmdb_id: i64,
    name: String,
    overview: Option<String>,
    images: Option<Vec<WireImage>>,
    // No default: absent/null cannot erase an existing graph.
    parts: Vec<WireMember>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireMember {
    #[serde(flatten)]
    movie: Movie,
    original_title: Option<String>,
    overview: Option<String>,
    images: Option<Vec<WireImage>>,
    #[serde(default, deserialize_with = "rating_map")]
    movie_ratings: Option<BTreeMap<String, Option<CollectionRating>>>,
    ratings: Option<Vec<CollectionRating>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireImage {
    cover_type: String,
    url: String,
}
fn optional_text(value: Option<String>, limit: usize, multiline: bool) -> Result<Option<String>> {
    if let Some(s) = &value {
        if !s.is_empty() {
            text(s, limit, multiline)?;
        }
    }
    Ok(value)
}
fn images(value: Option<Vec<WireImage>>) -> Result<Option<Vec<CollectionImage>>> {
    value
        .map(|v| {
            if v.len() > 32 {
                return Err(MetadataError::InvalidResponse);
            }
            let images = v
                .into_iter()
                .map(|i| {
                    text(&i.cover_type, 64, false)?;
                    text(&i.url, 2048, false)?;
                    // Retain unknown kinds and untrusted URLs privately. No network follows them.
                    Ok(CollectionImage {
                        cover_type: i.cover_type,
                        source_url: i.url,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            // Bound the exact compact private storage shape, including JSON escaping.
            // Individual limits above also bound this temporary encoding allocation.
            #[derive(Serialize)]
            struct StoredImage<'a> {
                cover_type: &'a str,
                source_url: &'a str,
            }
            let pairs = images
                .iter()
                .map(|i| StoredImage {
                    cover_type: &i.cover_type,
                    source_url: &i.source_url,
                })
                .collect::<Vec<_>>();
            if serde_json::to_vec(&pairs)
                .map_err(|_| MetadataError::InvalidResponse)?
                .len()
                > MAX_IMAGE_JSON_BYTES
            {
                return Err(MetadataError::InvalidResponse);
            }
            Ok(images)
        })
        .transpose()
}
fn rating_map<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<BTreeMap<String, Option<CollectionRating>>>, D::Error> {
    struct Ratings(BTreeMap<String, Option<CollectionRating>>);
    impl<'de> Deserialize<'de> for Ratings {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Ratings;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("bounded unique rating sources")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> std::result::Result<Ratings, A::Error> {
                    let mut result = BTreeMap::new();
                    while let Some(key) = map.next_key::<String>()? {
                        if result.len() >= 16 || result.contains_key(&key) {
                            return Err(serde::de::Error::custom("invalid rating sources"));
                        }
                        result.insert(key, map.next_value()?);
                    }
                    Ok(Ratings(result))
                }
            }
            d.deserialize_map(Visitor)
        }
    }
    Option::<Ratings>::deserialize(d).map(|v| v.map(|v| v.0))
}
fn rating(v: &CollectionRating) -> Result<()> {
    if !(0..=9_007_199_254_740_991).contains(&v.count)
        || !v.value.is_finite()
        || !(0.0..=100.0).contains(&v.value)
    {
        return Err(MetadataError::InvalidResponse);
    }
    optional_text(v.origin.clone(), 128, false)?;
    optional_text(v.r#type.clone(), 128, false)?;
    Ok(())
}
impl WireCollection {
    fn normalize(self, requested: i64) -> Result<CollectionDetails> {
        identity(self.tmdb_id)?;
        if self.tmdb_id != requested || self.parts.len() > MAX_COLLECTION_MEMBERS {
            return Err(MetadataError::InvalidResponse);
        }
        text(&self.name, 512, false)?;
        let mut ids = BTreeSet::new();
        let mut members = Vec::with_capacity(self.parts.len());
        for part in self.parts {
            if !ids.insert(part.movie.tmdb_id) {
                return Err(MetadataError::InvalidResponse);
            }
            if let Some(ratings) = &part.movie_ratings {
                if ratings.len() > 16 {
                    return Err(MetadataError::InvalidResponse);
                }
                for (key, v) in ratings {
                    text(key, 64, false)?;
                    if let Some(v) = v {
                        rating(v)?;
                    }
                }
            }
            if let Some(ratings) = &part.ratings {
                if ratings.len() > 16 {
                    return Err(MetadataError::InvalidResponse);
                }
                for v in ratings {
                    rating(v)?;
                }
            }
            let mut movie = part.movie;
            let collection = std::mem::take(&mut movie.collection).normalize()?;
            if matches!(&collection, CollectionAssociation::Present(c) if c.tmdb_id != requested) {
                return Err(MetadataError::InvalidResponse);
            }
            members.push(CollectionMember {
                movie: movie.details()?,
                collection,
                original_title: optional_text(part.original_title, 1024, false)?,
                overview: optional_text(part.overview, 65536, true)?,
                images: images(part.images)?,
                ratings: part.movie_ratings,
                legacy_ratings: part.ratings,
            });
        }
        Ok(CollectionDetails {
            tmdb_id: self.tmdb_id,
            title: self.name,
            overview: optional_text(self.overview, 65536, true)?,
            images: images(self.images)?,
            movies: members,
        })
    }
}
impl MetadataClient {
    pub async fn collection(&self, id: i64) -> Result<CollectionDetails> {
        input_id(id)?;
        let wire: WireCollection = self
            .fetch(false, &format!("movie/collection/{id}"), &[])
            .await?;
        wire.normalize(id)
    }
    /// Presence-aware companion preserves existing MovieDetails API and struct literals.
    pub async fn movie_with_collection(&self, id: i64) -> Result<MovieWithCollection> {
        input_id(id)?;
        let mut wire: Movie = self.fetch(false, &format!("movie/{id}"), &[]).await?;
        if wire.tmdb_id != id {
            return Err(MetadataError::InvalidResponse);
        }
        let collection = std::mem::take(&mut wire.collection).normalize()?;
        Ok(MovieWithCollection {
            movie: wire.details()?,
            collection,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode(s: &str) -> Result<CollectionDetails> {
        serde_json::from_str::<WireCollection>(s)
            .map_err(|_| MetadataError::InvalidResponse)?
            .normalize(7)
    }
    #[test]
    fn complete_graph_identity_and_factual_bounds() {
        let value = json!({"tmdbId":7,"name":"Collection","overview":"Line\nTwo","parts":[{"tmdbId":8,"title":"Member","runtime":0,"year":2024,"originalTitle":"Original","overview":"Fact","genres":["Drama"],"images":[{"coverType":"poster","url":"https://private.invalid/SECRET"}],"movieRatings":{"tmdb":{"count":12,"value":7.5}},"digitalRelease":"2020-01-01T00:00:00Z"}]});
        let result = decode(&value.to_string()).unwrap();
        assert_eq!(result.movies[0].movie.runtime, Some(0));
        assert_eq!(result.movies[0].movie.status.as_deref(), Some("released"));
        assert_eq!(
            result.movies[0].ratings.as_ref().unwrap()["tmdb"]
                .as_ref()
                .unwrap()
                .value,
            7.5
        );
        assert_eq!(
            result.movies[0].images.as_ref().unwrap()[0].source_url(),
            "https://private.invalid/SECRET"
        );
        assert!(!format!("{result:?}").contains("SECRET"));
        for s in [
            r#"{"tmdbId":7,"name":"C"}"#,
            r#"{"tmdbId":7,"name":"C","parts":null}"#,
            r#"{"tmdbId":0,"name":"C","parts":[]}"#,
            r#"{"tmdbId":9,"name":"C","parts":[]}"#,
            r#"{"tmdbId":7,"tmdbId":7,"name":"C","parts":[]}"#,
        ] {
            assert!(decode(s).is_err(), "{s}");
        }
        assert!(
            decode(r#"{"tmdbId":7,"name":"C","parts":[]}"#)
                .unwrap()
                .movies
                .is_empty()
        );
        for patch in [
            json!({"runtime":-1}),
            json!({"overview":"\u{0}"}),
            json!({"collection":{"tmdbId":9,"name":"Other"}}),
            json!({"movieRatings":{"tmdb":{"count":-1,"value":1}}}),
        ] {
            let mut v = value.clone();
            v["parts"][0]
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(decode(&v.to_string()).is_err());
        }
        let mut duplicate = value.clone();
        duplicate["parts"]
            .as_array_mut()
            .unwrap()
            .push(value["parts"][0].clone());
        assert!(decode(&duplicate.to_string()).is_err());
        let mut too_many = value;
        too_many["parts"] = json!(
            (1..=1001)
                .map(|i| json!({"tmdbId":i,"title":"M"}))
                .collect::<Vec<_>>()
        );
        assert!(decode(&too_many.to_string()).is_err());
    }
    #[test]
    fn rating_duplicate_and_null_sources_are_distinct() {
        assert!(decode(r#"{"tmdbId":7,"name":"C","parts":[{"tmdbId":8,"title":"M","movieRatings":{"tmdb":null,"tmdb":null}}]}"#).is_err());
        let facts=decode(r#"{"tmdbId":7,"name":"C","parts":[{"tmdbId":8,"title":"M","movieRatings":{"tmdb":null}}]}"#).unwrap();
        assert!(
            facts.movies[0]
                .ratings
                .as_ref()
                .unwrap()
                .contains_key("tmdb")
        );
        assert!(facts.movies[0].ratings.as_ref().unwrap()["tmdb"].is_none());
        assert!(
            decode(r#"{"tmdbId":7,"name":"C","parts":[{"tmdbId":8,"tmdbId":9,"title":"M"}]}"#)
                .is_err()
        );
    }
    #[test]
    fn private_image_json_budget_counts_pair_keys_and_escaping() {
        let mut urls = vec!["x".repeat(2048); 32];
        let stored = |urls: &[String]| {
            serde_json::to_vec(
                &urls
                    .iter()
                    .map(|url| json!({"cover_type":"poster", "source_url":url}))
                    .collect::<Vec<_>>(),
            )
            .unwrap()
            .len()
        };
        let excess = stored(&urls) - MAX_IMAGE_JSON_BYTES;
        assert!(excess < urls[31].len());
        let last_length = urls[31].len() - excess;
        urls[31].truncate(last_length);
        assert_eq!(stored(&urls), MAX_IMAGE_JSON_BYTES);
        let wire = |urls: &[String]| {
            Some(
                urls.iter()
                    .map(|url| WireImage {
                        cover_type: "poster".into(),
                        url: url.clone(),
                    })
                    .collect(),
            )
        };
        assert!(images(wire(&urls)).is_ok());
        // Same raw UTF8 length; a quote adds an escaped byte to the stored JSON.
        urls[31].replace_range(..1, "\"");
        assert_eq!(stored(&urls), MAX_IMAGE_JSON_BYTES + 1);
        assert!(images(wire(&urls)).is_err());
    }
    #[test]
    fn sparse_summary_has_three_distinct_states() {
        for (extra, expected) in [
            (json!({}), CollectionAssociation::Absent),
            (json!({"collection":null}), CollectionAssociation::Clear),
            (
                json!({"collection":{"tmdbId":7,"name":"C"}}),
                CollectionAssociation::Present(CollectionSummary {
                    tmdb_id: 7,
                    title: "C".into(),
                }),
            ),
        ] {
            let mut v = json!({"tmdbId":8,"title":"M"});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let mut wire: Movie = serde_json::from_value(v).unwrap();
            assert_eq!(
                std::mem::take(&mut wire.collection).normalize().unwrap(),
                expected
            );
            assert!(wire.details().is_ok());
        }
        let wire: Movie = serde_json::from_str(
            r#"{"tmdbId":8,"title":"M","collection":{"tmdbId":0,"name":"C"}}"#,
        )
        .unwrap();
        assert!(wire.details().is_err());
    }
    #[tokio::test]
    async fn owned_protocol_path_status_body_and_summary() {
        use axum::{Router, extract::Path, routing::get};
        async fn collection(Path(id): Path<i64>) -> Response {
            match id {
                404 => StatusCode::NOT_FOUND.into_response(),
                302 => (
                    StatusCode::FOUND,
                    [("location", "http://127.0.0.1:1/forbidden")],
                )
                    .into_response(),
                500 => (StatusCode::INTERNAL_SERVER_ERROR, "PRIVATE_FAILURE").into_response(),
                1001 => "x"
                    .repeat(crate::providers::http::MAX_BODY_BYTES + 1)
                    .into_response(),
                9 => Json(json!({"tmdbId":10,"name":"Mismatch","parts":[]})).into_response(),
                _ => Json(
                    json!({"tmdbId":id,"name":"Live fixture","parts":[{"tmdbId":8,"title":"M"}]}),
                )
                .into_response(),
            }
        }
        let app = Router::new()
            .route("/movie/collection/{id}", get(collection))
            .route(
                "/movie/{id}",
                get(|Path(id): Path<i64>| async move {
                    Json(json!({"tmdbId":id,"title":"M","collection":null}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        // Abort on panic too: owned fixture cannot leave a service running after failure.
        struct Server(tokio::task::JoinHandle<()>);
        impl Drop for Server {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let server = Server(task);
        let client = MetadataClient::with_origins(&origin, &origin).unwrap();
        assert_eq!(
            client.collection(7).await.unwrap().movies[0].movie.tmdb_id,
            8
        );
        assert_eq!(
            client.movie_with_collection(8).await.unwrap().collection,
            CollectionAssociation::Clear
        );
        assert!(matches!(
            client.collection(404).await,
            Err(MetadataError::NotFound)
        ));
        for id in [302, 500, 9, 1001] {
            assert!(client.collection(id).await.is_err());
        }
        assert!(matches!(
            client.collection(0).await,
            Err(MetadataError::InvalidInput)
        ));
        server.0.abort();
        // The JoinHandle stays owned through cancellation observation.
        let mut server = server;
        let joined = tokio::time::timeout(std::time::Duration::from_secs(2), &mut server.0)
            .await
            .unwrap();
        assert!(joined.unwrap_err().is_cancelled());
    }
}
