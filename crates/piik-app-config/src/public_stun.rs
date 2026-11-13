use piik_protocol::{MAX_ICE_SERVER_URLS, MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS};

use crate::{Error, Result};

const DEFAULT_PUBLIC_STUN_URLS: [&str; 3] = [
    "stun:stun.cloudflare.com:3478",
    "stun:stun.miwifi.com:3478",
    "stun:stun.chat.bilibili.com:3478",
];

/// The shared App public-link discovery policy, as `(stun, prediction)`.
/// A non-empty `PIIK_STUN_URLS` override replaces every destination; fewer
/// than three URLs disable the survey pair.
pub fn public_stun_urls(override_value: &str) -> Result<(Vec<String>, Vec<String>)> {
    let mut stun_urls = piik_config::parse_stun_urls(override_value, "PIIK_STUN_URLS")
        .map_err(|error| Error::invalid(error.to_string()))?;
    if stun_urls.is_empty() {
        stun_urls = DEFAULT_PUBLIC_STUN_URLS.map(str::to_owned).to_vec();
    }
    let mut prediction = Vec::new();
    if stun_urls.len() >= 3 {
        if stun_urls.len() > MAX_ICE_SERVER_URLS - MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS {
            return Err(Error::invalid("PIIK_STUN_URLS must contain at most 6 URLs"));
        }
        prediction = stun_urls[1..3].to_vec();
    }
    Ok((stun_urls, prediction))
}

#[cfg(test)]
mod tests {
    use super::public_stun_urls;

    #[test]
    fn applies_the_public_policy() {
        let (stun, prediction) = public_stun_urls("").unwrap();
        assert_eq!(stun.len(), 3);
        assert_eq!(prediction, stun[1..3]);

        let (stun, prediction) = public_stun_urls("stun:a.test:3478,stun:b.test:3478").unwrap();
        assert_eq!(stun, ["stun:a.test:3478", "stun:b.test:3478"]);
        assert!(prediction.is_empty());

        let seven: Vec<String> = (0..7).map(|i| format!("stun:s{i}.test:3478")).collect();
        let error = public_stun_urls(&seven.join(",")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "PIIK_STUN_URLS must contain at most 6 URLs"
        );
        assert_eq!(
            public_stun_urls("turn:t.test").unwrap_err().to_string(),
            "PIIK_STUN_URLS contains an invalid STUN URL"
        );
    }
}
