//! SigV4 signing for the platform's IAM-authenticated API Gateway routes
//! (`execute-api`). The benchmark function signs with its own execution-role
//! credentials; the IAM surface then authorizes it via its seeded binding.

use anyhow::Result;

/// Signs requests for one region's `execute-api` service with fixed
/// credentials (the function's role at runtime; static test keys in tests).
#[derive(Clone)]
pub struct ExecuteApiSigner {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    region: String,
}

impl ExecuteApiSigner {
    pub fn new(
        access_key_id: impl Into<String>,
        secret_access_key: impl Into<String>,
        session_token: Option<String>,
        region: impl Into<String>,
    ) -> Self {
        Self {
            access_key_id: access_key_id.into(),
            secret_access_key: secret_access_key.into(),
            session_token,
            region: region.into(),
        }
    }
    /// Computes the SigV4 headers for one request (authorization, date,
    /// payload hash, and the session token when present). The caller adds
    /// them to its own HTTP client request; signing is per request because
    /// the timestamp is part of the signature.
    pub fn sign_headers(
        &self,
        method: &str,
        url: &str,
        body: &[u8],
    ) -> Result<Vec<(String, String)>> {
        use aws_sigv4::http_request::{
            sign, PayloadChecksumKind, SignableBody, SignableRequest, SigningParams,
            SigningSettings,
        };

        let credentials = aws_credential_types::Credentials::new(
            self.access_key_id.clone(),
            self.secret_access_key.clone(),
            self.session_token.clone(),
            None,
            "konductor-benchmarks",
        );
        let mut settings = SigningSettings::default();
        settings.payload_checksum_kind = PayloadChecksumKind::NoHeader;
        let identity = aws_smithy_runtime_api::client::identity::Identity::new(credentials, None);
        let v4_params = aws_sigv4::sign::v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name("execute-api")
            .time(std::time::SystemTime::now())
            .settings(settings)
            .build()?;
        let params = SigningParams::V4(v4_params);
        use sha2::Digest;
        // API Gateway expects the payload hash as a signed header; the
        // signer's NoHeader mode does not add it, so we supply it.
        let payload_hash = hex::encode(sha2::Sha256::digest(body));
        let extra_headers = [("x-amz-content-sha256", payload_hash.as_str())];
        let signable = SignableRequest::new(
            method,
            url,
            extra_headers.iter().copied(),
            SignableBody::Bytes(body),
        )?;
        let (instructions, _signature) = sign(signable, &params)?.into_parts();
        let mut headers: Vec<(String, String)> = instructions
            .headers()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        headers.push(("x-amz-content-sha256".to_string(), payload_hash));
        Ok(headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> ExecuteApiSigner {
        ExecuteApiSigner::new(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            Some("session-token-example".to_string()),
            "us-east-1",
        )
    }

    fn signed() -> Vec<(String, String)> {
        signer()
            .sign_headers(
                "POST",
                "https://abc123.execute-api.us-east-1.amazonaws.com/prod/publish",
                br#"{"domain":"benchmark"}"#,
            )
            .unwrap()
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn the_authorization_header_targets_execute_api_in_the_region() {
        let auth = header(&signed(), "authorization").unwrap().to_string();
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/"));
        assert!(auth.contains("/us-east-1/execute-api/aws4_request"));
        assert!(auth.contains("SignedHeaders="));
        assert!(auth.contains("Signature="));
    }

    #[test]
    fn the_payload_hash_and_date_headers_are_present() {
        let headers = signed();
        let hash = header(&headers, "x-amz-content-sha256").unwrap();
        assert_eq!(hash.len(), 64, "sha256 hex");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            header(&headers, "x-amz-date").unwrap().len(),
            16,
            "yyyyMMddTHHmmssZ"
        );
    }

    #[test]
    fn a_session_token_adds_the_security_token_header() {
        let with_token = signed();
        assert_eq!(
            header(&with_token, "x-amz-security-token"),
            Some("session-token-example")
        );
        let without = ExecuteApiSigner::new(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            None,
            "us-east-1",
        )
        .sign_headers(
            "POST",
            "https://abc123.execute-api.us-east-1.amazonaws.com/prod/publish",
            b"{}",
        )
        .unwrap();
        assert_eq!(header(&without, "x-amz-security-token"), None);
    }
}
