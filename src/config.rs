#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("missing required environment variable: {0}")]
    MissingVar(&'static str),
    #[error("invalid value for PORT: {0:?}")]
    InvalidPort(String),
}

pub struct Config {
    pub database_url: String,
    pub jwt_key: String,
    pub official_api_url: String,
    pub port: u16,
}

impl Config {
    /// Core logic: reads all four env vars via an injectable lookup function, so unit tests
    /// can pass a `HashMap` instead of mutating the real process environment (avoids the
    /// parallel-`cargo test`-thread race on `std::env::set_var`).
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        let database_url = lookup("DATABASE_URL").ok_or(ConfigError::MissingVar("DATABASE_URL"))?;
        let jwt_key = lookup("JWT_KEY").ok_or(ConfigError::MissingVar("JWT_KEY"))?;
        let official_api_url =
            lookup("OFFICIAL_API_URL").ok_or(ConfigError::MissingVar("OFFICIAL_API_URL"))?;
        let port = match lookup("PORT") {
            Some(value) => value
                .parse::<u16>()
                .map_err(|_| ConfigError::InvalidPort(value))?,
            None => 15001,
        };

        Ok(Config {
            database_url,
            jwt_key,
            official_api_url,
            port,
        })
    }

    /// Thin wrapper over `from_lookup` reading from the real process environment.
    pub fn from_env() -> Result<Config, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_from<'a>(map: &'a HashMap<&'a str, &'a str>) -> impl Fn(&str) -> Option<String> + 'a {
        move |key: &str| map.get(key).map(|v| v.to_string())
    }

    fn full_map() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("DATABASE_URL", "postgres://savez:savez@localhost:5432/savez"),
            ("JWT_KEY", "test-key"),
            ("OFFICIAL_API_URL", "https://api.shapez.io"),
            ("PORT", "15001"),
        ])
    }

    /// `Config` never derives `Debug` (secrets must not be printable), so `Result::unwrap_err`
    /// is unavailable here — extract the error manually instead.
    fn expect_err(result: Result<Config, ConfigError>) -> ConfigError {
        match result {
            Err(err) => err,
            Ok(_) => panic!("expected Err, got Ok"),
        }
    }

    #[test]
    fn all_vars_present_returns_ok_with_matching_values() {
        let map = full_map();
        let config = Config::from_lookup(lookup_from(&map)).unwrap();
        assert_eq!(
            config.database_url,
            "postgres://savez:savez@localhost:5432/savez"
        );
        assert_eq!(config.jwt_key, "test-key");
        assert_eq!(config.official_api_url, "https://api.shapez.io");
        assert_eq!(config.port, 15001);
    }

    #[test]
    fn missing_database_url_errors() {
        let mut map = full_map();
        map.remove("DATABASE_URL");
        let err = expect_err(Config::from_lookup(lookup_from(&map)));
        assert!(matches!(err, ConfigError::MissingVar("DATABASE_URL")));
    }

    #[test]
    fn missing_jwt_key_errors() {
        let mut map = full_map();
        map.remove("JWT_KEY");
        let err = expect_err(Config::from_lookup(lookup_from(&map)));
        assert!(matches!(err, ConfigError::MissingVar("JWT_KEY")));
    }

    #[test]
    fn missing_official_api_url_errors() {
        let mut map = full_map();
        map.remove("OFFICIAL_API_URL");
        let err = expect_err(Config::from_lookup(lookup_from(&map)));
        assert!(matches!(err, ConfigError::MissingVar("OFFICIAL_API_URL")));
    }

    #[test]
    fn missing_port_defaults_to_15001() {
        let mut map = full_map();
        map.remove("PORT");
        let config = Config::from_lookup(lookup_from(&map)).unwrap();
        assert_eq!(config.port, 15001);
    }

    #[test]
    fn non_numeric_port_errors() {
        let mut map = full_map();
        map.insert("PORT", "not-a-number");
        let err = expect_err(Config::from_lookup(lookup_from(&map)));
        assert!(matches!(err, ConfigError::InvalidPort(v) if v == "not-a-number"));
    }
}
