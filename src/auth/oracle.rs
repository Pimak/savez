use reqwest::Client;

use crate::error::AppError;

// Vérifiée directement dans tobspr-games/shapez.io ET tobspr-games/shapez-community-edition,
// src/js/platform/api.js : les deux clients envoient cette valeur d'x-api-key, identique et
// figée. Ce n'est PAS un secret par utilisateur — c'est une clé d'application fixe.
//
// Inversion de rôle (Pitfall 1, 05-RESEARCH.md) : en ENTRÉE (requêtes reçues par NOTRE serveur),
// x-api-key doit être ignoré (CON-api-contract). En SORTIE (ce module, quand NOTRE serveur
// appelle api.shapez.io en tant que client), la même valeur doit être envoyée telle quelle,
// sinon le vrai api.shapez.io peut refuser la requête comme provenant d'une application inconnue.
const CLIENT_API_KEY: &str = "d5c54aaa491f200709afff082c153ef2";

/// Unique appel de vérification de possession du DLC (CON-official-api-usage : un seul appel
/// oracle par inscription). `client` et `official_api_url` sont TOUJOURS des paramètres, jamais
/// lus depuis un état global — convention `Config::from_lookup`, rend cette fonction testable
/// sans `AppState`.
///
/// D-05 verrouillé, non négociable : seul un HTTP 200 franc dont le corps est un tableau JSON
/// vaut preuve de possession. Un refus explicite, un timeout, une erreur réseau ou un corps 200
/// inattendu échouent tous de façon indiscernable — seule la trace de log diffère (le client ne
/// doit jamais pouvoir distinguer un token invalide d'un oracle indisponible).
pub async fn verify_official_ownership(
    client: &Client,
    official_api_url: &str,
    official_token: &str,
) -> Result<(), AppError> {
    let url = format!(
        "{}/v1/puzzles/list/mine",
        official_api_url.trim_end_matches('/')
    );

    let result = client
        .get(&url)
        .header("x-token", official_token)
        .header("x-api-key", CLIENT_API_KEY)
        .send()
        .await;

    let response = match result {
        Ok(response) => response,
        Err(err) => {
            tracing::warn!(error = %err, "oracle unreachable");
            return Err(AppError::OracleVerificationFailed);
        }
    };

    if response.status() != reqwest::StatusCode::OK {
        tracing::warn!(status = %response.status(), "oracle explicitly refused token");
        return Err(AppError::OracleVerificationFailed);
    }

    // A1 (05-RESEARCH.md): defense in depth. A literal 200 alone is not sufficient proof — the
    // body must also parse as the documented `PuzzleMetadata[]` success shape (any JSON array).
    // This is the signal that would prove A1 wrong if the real oracle ever used an all-200
    // convention for its own errors too (distinct trace vs. the two branches above).
    match response.json::<serde_json::Value>().await {
        Ok(body) if body.is_array() => {
            tracing::info!("oracle verification succeeded");
            Ok(())
        }
        Ok(_) => {
            tracing::warn!("oracle returned 200 but body is not a JSON array");
            Err(AppError::OracleVerificationFailed)
        }
        Err(err) => {
            tracing::warn!(error = %err, "oracle returned 200 but body could not be parsed");
            Err(AppError::OracleVerificationFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // `cargo test --lib` runs in a separate OS process from every `tests/*.rs` integration
    // binary (which installs its own copy via tests/common/mod.rs, 05-02) — this test module
    // never executes main.rs's install call either, so a fresh install is required here too.
    // These tests never perform a real TLS handshake (wiremock and 127.0.0.1:1 are both plain
    // HTTP), but reqwest's rustls-no-provider backend requires a provider installed before a
    // `Client` can even be *built*, not just before its first handshake.
    static CRYPTO_PROVIDER_INIT: Once = Once::new();

    fn ensure_crypto_provider_installed() {
        CRYPTO_PROVIDER_INIT.call_once(|| {
            rustls::crypto::ring::default_provider()
                .install_default()
                .expect("install rustls ring crypto provider");
        });
    }

    #[tokio::test]
    async fn accepts_200_with_json_array_body() {
        ensure_crypto_provider_installed();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("x-token", "test-token"))
            .and(header("x-api-key", CLIENT_API_KEY))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let result = verify_official_ownership(&client, &server.uri(), "test-token").await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn rejects_401() {
        ensure_crypto_provider_installed();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = Client::new();
        let result = verify_official_ownership(&client, &server.uri(), "test-token").await;

        assert!(matches!(result, Err(AppError::OracleVerificationFailed)));
    }

    #[tokio::test]
    async fn rejects_500() {
        ensure_crypto_provider_installed();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = Client::new();
        let result = verify_official_ownership(&client, &server.uri(), "test-token").await;

        assert!(matches!(result, Err(AppError::OracleVerificationFailed)));
    }

    #[tokio::test]
    async fn rejects_200_with_error_object_body() {
        ensure_crypto_provider_installed();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"error": "bad-token"})),
            )
            .mount(&server)
            .await;

        let client = Client::new();
        let result = verify_official_ownership(&client, &server.uri(), "test-token").await;

        assert!(matches!(result, Err(AppError::OracleVerificationFailed)));
    }

    #[tokio::test]
    async fn rejects_unreachable_host() {
        ensure_crypto_provider_installed();
        let client = Client::new();
        let result = verify_official_ownership(&client, "http://127.0.0.1:1", "test-token").await;

        assert!(matches!(result, Err(AppError::OracleVerificationFailed)));
    }
}
