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
    ApiKey { api_key: String },
    UsernamePassword { username: String, password: String },
}
impl Credentials {
    pub(super) fn valid(&self, implementation: &str) -> bool {
        let valid = |s: &str| !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control);
        if serde_json::to_vec(self).map_or(true, |v| v.len() > 16355) {
            return false;
        }
        match self {
            Self::ApiKey { api_key } => valid(api_key),
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
}
