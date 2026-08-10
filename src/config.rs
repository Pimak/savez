#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("missing required environment variable: {0}")]
    MissingVar(&'static str),
    #[error("invalid value for PORT: {0:?}")]
    InvalidPort(String),
    #[error("invalid value for AUTH_MODE: {0:?}")]
    InvalidAuthMode(String),
}

/// Phase 2 authentication switch (ROADMAP SC4). `Oracle` is the only functional mode in v1;
/// `Open`/`SteamOpenId` exist in configuration only — wiring their actual behavior is deferred
/// until the official service closes (see AppError::AuthModeNotImplemented).
///
/// Unlike `Config`, this enum carries no secret and MUST derive `Debug`: the startup log line
/// (plan 05-02) needs to print the active mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    Oracle,
    Open,
    SteamOpenId,
}

pub struct Config {
    pub database_url: String,
    pub jwt_key: String,
    pub official_api_url: String,
    pub port: u16,
    pub auth_mode: AuthMode,
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
        // Optional-with-default, like `port` above — never `ok_or(MissingVar)?` — the switch is
        // inert by default and most deployments will not set it. An unrecognized value is still
        // fail-fast (never silently falls back to Oracle): a misspelled AUTH_MODE in production
        // must be loud, not silently ignored.
        let auth_mode = match lookup("AUTH_MODE") {
            Some(value) => match value.as_str() {
                "oracle" => AuthMode::Oracle,
                "open" => AuthMode::Open,
                "steam-openid" => AuthMode::SteamOpenId,
                _ => return Err(ConfigError::InvalidAuthMode(value)),
            },
            None => AuthMode::Oracle,
        };

        Ok(Config {
            database_url,
            jwt_key,
            official_api_url,
            port,
            auth_mode,
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
            (
                "DATABASE_URL",
                "postgres://savez:savez@localhost:5432/savez",
            ),
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
        assert_eq!(config.auth_mode, AuthMode::Oracle);
        assert_eq!(
            config.bind_addr,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        );
    }

    #[test]
    fn missing_auth_mode_defaults_to_oracle() {
        let mut map = full_map();
        map.remove("AUTH_MODE");
        let config = Config::from_lookup(lookup_from(&map)).unwrap();
        assert_eq!(config.auth_mode, AuthMode::Oracle);
    }

    #[test]
    fn auth_mode_open_parses() {
        let mut map = full_map();
        map.insert("AUTH_MODE", "open");
        let config = Config::from_lookup(lookup_from(&map)).unwrap();
        assert_eq!(config.auth_mode, AuthMode::Open);
    }

    #[test]
    fn auth_mode_steam_openid_parses() {
        let mut map = full_map();
        map.insert("AUTH_MODE", "steam-openid");
        let config = Config::from_lookup(lookup_from(&map)).unwrap();
        assert_eq!(config.auth_mode, AuthMode::SteamOpenId);
    }

    #[test]
    fn invalid_auth_mode_errors() {
        let mut map = full_map();
        map.insert("AUTH_MODE", "steam");
        let err = expect_err(Config::from_lookup(lookup_from(&map)));
        assert!(matches!(err, ConfigError::InvalidAuthMode(v) if v == "steam"));
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

    #[test]
    fn missing_bind_addr_defaults_to_loopback() {
        // BIND_ADDR is deliberately absent from full_map() — the default must be the nominal
        // path this test exercises, not an opt-in.
        let map = full_map();
        let config = Config::from_lookup(lookup_from(&map)).unwrap();
        assert_eq!(
            config.bind_addr,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        );
    }

    #[test]
    fn bind_addr_unspecified_v4_parses() {
        let mut map = full_map();
        map.insert("BIND_ADDR", "0.0.0.0");
        let config = Config::from_lookup(lookup_from(&map)).unwrap();
        assert_eq!(
            config.bind_addr,
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        );
    }

    #[test]
    fn bind_addr_ipv6_parses() {
        let mut map = full_map();
        map.insert("BIND_ADDR", "::");
        let config = Config::from_lookup(lookup_from(&map)).unwrap();
        assert!(matches!(config.bind_addr, std::net::IpAddr::V6(_)));
    }

    #[test]
    fn invalid_bind_addr_errors() {
        let mut map = full_map();
        map.insert("BIND_ADDR", "pas-une-ip");
        let err = expect_err(Config::from_lookup(lookup_from(&map)));
        assert!(matches!(err, ConfigError::InvalidBindAddr(v) if v == "pas-une-ip"));
    }
}
