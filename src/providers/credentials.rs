use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};

/// Intentionally neither Debug nor serializable: only the secret facility supplies this key.
pub struct CredentialKey(aead::LessSafeKey);
impl CredentialKey {
    pub fn from_hex(value: &str) -> Result<Self, &'static str> {
        if value.len() != 64 {
            return Err("Provider key must contain 64 hexadecimal characters");
        }
        let mut bytes = [0u8; 32];
        for (i, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let digit = |c: u8| (c as char).to_digit(16).map(|n| n as u8);
            bytes[i] = digit(pair[0])
                .zip(digit(pair[1]))
                .map(|(a, b)| a * 16 + b)
                .ok_or("Provider key must contain 64 hexadecimal characters")?;
        }
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, &bytes)
            .map_err(|_| "Invalid provider key")?;
        bytes.fill(0);
        Ok(Self(aead::LessSafeKey::new(key)))
    }
    /// Separate AEAD purpose from provider credentials; payloads never have a plaintext fallback.
    pub(super) fn seal_release(
        &self,
        context: &str,
        plain: &[u8],
    ) -> Result<Vec<u8>, &'static str> {
        if plain.len() > 65536 || context.len() > 512 {
            return Err("invalid_release");
        }
        let mut nonce = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "key_unavailable")?;
        let mut body = plain.to_vec();
        self.0
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(format!("hrrdarr/rss-private-release/v1/{context}")),
                &mut body,
            )
            .map_err(|_| "key_unavailable")?;
        let mut envelope = vec![1];
        envelope.extend_from_slice(&nonce);
        envelope.extend(body);
        Ok(envelope)
    }
    pub(super) fn open_release(
        &self,
        context: &str,
        envelope: &[u8],
    ) -> Result<Vec<u8>, &'static str> {
        if !(29..=65565).contains(&envelope.len()) || envelope[0] != 1 || context.len() > 512 {
            return Err("invalid_release");
        }
        let nonce: [u8; 12] = envelope[1..13].try_into().map_err(|_| "invalid_release")?;
        let mut body = envelope[13..].to_vec();
        let length = self
            .0
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(format!("hrrdarr/rss-private-release/v1/{context}")),
                &mut body,
            )
            .map_err(|_| "key_unavailable")?
            .len();
        body.truncate(length);
        Ok(body)
    }
    pub(super) fn seal(
        &self,
        id: &str,
        implementation: &str,
        value: &Credentials,
    ) -> Result<Vec<u8>, &'static str> {
        let mut nonce = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "Credential encryption failed")?;
        let mut body = serde_json::to_vec(value).map_err(|_| "Credential encryption failed")?;
        self.0
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(context(id, implementation)),
                &mut body,
            )
            .map_err(|_| "Credential encryption failed")?;
        let mut envelope = vec![1];
        envelope.extend_from_slice(&nonce);
        envelope.extend(body);
        Ok(envelope)
    }
    pub(super) fn open(
        &self,
        id: &str,
        implementation: &str,
        value: &[u8],
    ) -> Result<Credentials, &'static str> {
        if !(29..=16384).contains(&value.len()) || value[0] != 1 {
            return Err("Provider credentials cannot be unlocked");
        }
        let nonce: [u8; 12] = value[1..13]
            .try_into()
            .map_err(|_| "Provider credentials cannot be unlocked")?;
        let mut body = value[13..].to_vec();
        let plain = self
            .0
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(context(id, implementation)),
                &mut body,
            )
            .map_err(|_| "Provider credentials cannot be unlocked")?;
        let result =
            serde_json::from_slice(plain).map_err(|_| "Provider credentials cannot be unlocked");
        body.fill(0);
        result
    }
}
fn context(id: &str, implementation: &str) -> String {
    format!("hrrdarr/provider-credentials/v1/{implementation}/{id}")
}

/// Write-only wire input. Serialize is used exclusively inside the AEAD envelope.
#[derive(Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(rename = "ProviderCredentials")]
pub enum Credentials {
    ApiKey {
        api_key: String,
    },
    UsernamePassword {
        username: String,
        password: String,
    },
    Indexer {
        #[serde(default)]
        #[ts(optional=nullable)]
        api_key: Option<String>,
        #[serde(default)]
        #[ts(as = "Option<Vec<IndexerParameter>>", optional)]
        tv_parameters: Vec<IndexerParameter>,
        #[serde(default)]
        #[ts(as = "Option<Vec<IndexerParameter>>", optional)]
        movie_parameters: Vec<IndexerParameter>,
    },
}
/// Write-only additional query parameters; names and values never enter public configuration.
#[derive(Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct IndexerParameter {
    pub name: String,
    pub value: String,
}

#[derive(Default)]
pub struct IndexerAccess<'a> {
    pub api_key: Option<&'a str>,
    pub tv_parameters: &'a [IndexerParameter],
    pub movie_parameters: &'a [IndexerParameter],
}
impl<'a> IndexerAccess<'a> {
    pub(super) fn validate(&self) -> bool {
        self.api_key.is_none_or(|key| {
            !key.is_empty() && key.len() <= 4096 && !key.chars().any(char::is_control)
        }) && valid_parameters(self.tv_parameters)
            && valid_parameters(self.movie_parameters)
    }

    pub(super) fn from_credentials(credentials: &'a Option<Credentials>) -> Self {
        match credentials {
            Some(Credentials::ApiKey { api_key }) => Self {
                api_key: Some(api_key),
                ..Self::default()
            },
            Some(Credentials::Indexer {
                api_key,
                tv_parameters,
                movie_parameters,
            }) => Self {
                api_key: api_key.as_deref(),
                tv_parameters,
                movie_parameters,
            },
            _ => Self::default(),
        }
    }
}
fn valid_parameters(parameters: &[IndexerParameter]) -> bool {
    const RESERVED: &[&str] = &[
        "t",
        "apikey",
        "api_key",
        "api-key",
        "cat",
        "categories",
        "q",
        "title",
        "tvdbid",
        "tvmazeid",
        "rid",
        "rageid",
        "imdbid",
        "imdbtitle",
        "imdbyear",
        "tmdbid",
        "traktid",
        "doubanid",
        "year",
        "season",
        "ep",
        "episode",
        "offset",
        "limit",
        "extended",
        "o",
        "attrs",
        "id",
        "r",
    ];
    let mut names = std::collections::BTreeSet::new();
    parameters.len() <= 16
        && parameters.iter().all(|p| {
            let name = p.name.to_ascii_lowercase();
            !name.is_empty()
                && name.len() <= 64
                && name.as_bytes()[0].is_ascii_alphabetic()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
                && !RESERVED.contains(&name.as_str())
                && names.insert(name)
                && p.value.len() <= 1024
                && !p.value.chars().any(char::is_control)
        })
}
impl Credentials {
    pub(super) fn valid(&self, implementation: &str) -> bool {
        let valid = |s: &str| !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control);
        if serde_json::to_vec(self).map_or(true, |v| v.len() > 16355) {
            return false;
        }
        match self {
            Self::ApiKey { api_key } => valid(api_key),
            Self::Indexer {
                api_key,
                tv_parameters,
                movie_parameters,
            } => {
                matches!(implementation, "torznab" | "newznab")
                    && api_key.as_ref().is_none_or(|key| valid(key))
                    && (api_key.is_some()
                        || !tv_parameters.is_empty()
                        || !movie_parameters.is_empty())
                    && valid_parameters(tv_parameters)
                    && valid_parameters(movie_parameters)
            }
            Self::UsernamePassword { username, password } => {
                implementation == "qbittorrent" && valid(username) && valid(password)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_indexer_parameters_validate_and_roundtrip_without_changing_old_credentials() {
        let key = CredentialKey::from_hex(&"ab".repeat(32)).unwrap();
        let input = r#"{"kind":"indexer","api_key":null,"tv_parameters":[{"name":"passkey","value":"PRIVATE_QUERY_SENTINEL"}]}"#;
        let credentials: Credentials = serde_json::from_str(input).unwrap();
        assert!(credentials.valid("torznab"));
        assert!(!credentials.valid("qbittorrent"));
        let bytes = key.seal("provider", "torznab", &credentials).unwrap();
        assert!(
            !bytes
                .windows(b"PRIVATE_QUERY_SENTINEL".len())
                .any(|w| w == b"PRIVATE_QUERY_SENTINEL")
        );
        let opened = Some(key.open("provider", "torznab", &bytes).unwrap());
        let access = IndexerAccess::from_credentials(&opened);
        assert!(access.validate());
        assert_eq!(access.tv_parameters[0].value, "PRIVATE_QUERY_SENTINEL");
        assert!(access.movie_parameters.is_empty());
        let old: Credentials =
            serde_json::from_str(r#"{"kind":"api_key","api_key":"old-key"}"#).unwrap();
        let old_bytes = key.seal("provider", "torznab", &old).unwrap();
        let old = Some(key.open("provider", "torznab", &old_bytes).unwrap());
        assert_eq!(
            IndexerAccess::from_credentials(&old).api_key,
            Some("old-key")
        );
        for reserved in [
            "T",
            "apiKey",
            "API_KEY",
            "q",
            "cat",
            "offset",
            "limit",
            "imdbtitle",
            "imdbyear",
            "tvdbid",
        ] {
            assert!(!valid_parameters(&[IndexerParameter {
                name: reserved.into(),
                value: "private".into()
            }]));
        }
        assert!(!valid_parameters(&[
            IndexerParameter {
                name: "passkey".into(),
                value: "first".into()
            },
            IndexerParameter {
                name: "PASSKEY".into(),
                value: "second".into()
            }
        ]));
        assert!(!valid_parameters(&[IndexerParameter {
            name: "filter".into(),
            value: "x".repeat(1025)
        }]));
        assert!(valid_parameters(&[IndexerParameter {
            name: "filter".into(),
            value: String::new()
        }]));
        let too_many = (0..17)
            .map(|n| IndexerParameter {
                name: format!("filter{n}"),
                value: String::new(),
            })
            .collect::<Vec<_>>();
        assert!(!valid_parameters(&too_many));
        let large = Credentials::Indexer {
            api_key: Some("x".repeat(4096)),
            tv_parameters: (0..16)
                .map(|n| IndexerParameter {
                    name: format!("filter{n}"),
                    value: "x".repeat(1024),
                })
                .collect(),
            movie_parameters: vec![],
        };
        assert!(
            !large.valid("newznab"),
            "Complete serialized payload must still fit the existing 16 KiB authenticated envelope"
        );
    }
    #[test]
    fn credentials_are_authenticated_randomized_and_identity_bound() {
        let key = CredentialKey::from_hex(&"ab".repeat(32)).unwrap();
        let wrong = CredentialKey::from_hex(&"cd".repeat(32)).unwrap();
        let secret = Credentials::ApiKey {
            api_key: "sentinel-provider-secret".into(),
        };
        let first = key.seal("id-one", "torznab", &secret).unwrap();
        let second = key.seal("id-one", "torznab", &secret).unwrap();
        assert_ne!(
            first, second,
            "Fresh nonces prevent repeated plaintext producing identical envelopes"
        );
        assert!(!first.windows(24).any(|w| w == b"sentinel-provider-secret"));
        assert!(matches!(
            key.open("id-one", "torznab", &first),
            Ok(Credentials::ApiKey { .. })
        ));
        assert!(wrong.open("id-one", "torznab", &first).is_err());
        assert!(key.open("id-two", "torznab", &first).is_err());
        assert!(key.open("id-one", "newznab", &first).is_err());
        let mut tampered = first.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(key.open("id-one", "torznab", &tampered).is_err());
        let mut unknown = first;
        unknown[0] = 2;
        assert!(key.open("id-one", "torznab", &unknown).is_err());
        assert!(CredentialKey::from_hex(&"z".repeat(64)).is_err());
        assert!(CredentialKey::from_hex("short").is_err());
    }
    #[test]
    fn release_envelopes_are_bounded_randomized_and_purpose_separated() {
        let key = CredentialKey::from_hex(&"ab".repeat(32)).unwrap();
        let wrong = CredentialKey::from_hex(&"cd".repeat(32)).unwrap();
        let context = "candidate/indexer/1/client/1/tv";
        let private = b"https://owned.invalid/torrent?passkey=private-release-key";
        let first = key.seal_release(context, private).unwrap();
        assert_ne!(
            first,
            key.seal_release(context, private).unwrap(),
            "fresh nonces protect repeated pending payloads"
        );
        assert_eq!(key.open_release(context, &first).unwrap(), private);
        assert!(!first.windows(private.len()).any(|w| w == private));
        assert!(
            key.open_release("candidate/indexer/1/client/1/movies", &first)
                .is_err()
        );
        assert!(wrong.open_release(context, &first).is_err());
        let mut tampered = first.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(key.open_release(context, &tampered).is_err());
        assert!(key.open("id", "torznab", &first).is_err());
        let credentials = key
            .seal(
                "id",
                "torznab",
                &Credentials::ApiKey {
                    api_key: "secret".into(),
                },
            )
            .unwrap();
        assert!(
            key.open_release(context, &credentials).is_err(),
            "credential envelopes cannot be repurposed as release payloads"
        );
        assert!(key.seal_release(context, &vec![0; 65537]).is_err());
    }
}
