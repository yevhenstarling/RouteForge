//! The bot's settings, read from the environment.

use anyhow::{Context, ensure};
use reqwest::Url;

/// Where the API is when nothing says so: on this machine, where it listens when it
/// is started outside Docker.
const API_URL: &str = "http://127.0.0.1:8080";

/// The least the key may be. The API holds it to the same, and says so first.
const SHORTEST_KEY: usize = 32;

/// How the bot behaves. None of it is secret, so it can be logged: what must not be
/// is in [`Secrets`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Config {
    /// Where the API is, without a slash at its end.
    pub(crate) api_url: String,
    /// Where the Bot API is, if it is not where Telegram has it: a server of one's
    /// own, or what stands in for Telegram in a test.
    pub(crate) telegram_api_url: Option<Url>,
    /// A geocoding service that speaks as Nominatim does, to ask where an address is.
    /// Without one the bot asks nobody, and takes a place as a location or as
    /// coordinates (D72).
    pub(crate) geocoder_url: Option<Url>,
}

impl Config {
    /// Reads the settings from the environment of the process.
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// Reads the settings from `var`, which gives the value of a variable by name.
    ///
    /// A variable that is missing or empty has its default.
    pub(crate) fn from_vars(var: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let api_url = match address(&var, "BOT_API_URL")? {
            Some(url) => url.as_str().trim_end_matches('/').to_owned(),
            None => API_URL.to_owned(),
        };
        Ok(Self {
            api_url,
            telegram_api_url: address(&var, "TELEGRAM_API_URL")?,
            geocoder_url: address(&var, "GEOCODER_URL")?,
        })
    }
}

/// The address a setting names, of something that is spoken to over HTTP.
///
/// What was given is not repeated when it is refused. An address can carry a password,
/// and this ends in a log.
fn address(var: &impl Fn(&str) -> Option<String>, name: &str) -> anyhow::Result<Option<Url>> {
    let Some(value) = set(var, name) else {
        return Ok(None);
    };
    let url = Url::parse(value.trim())
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.has_host())
        .with_context(|| format!("{name} must be an address like http://127.0.0.1:8080"))?;
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "{name} must not carry a user or a password"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "{name} must be an address, with nothing after its path"
    );
    Ok(Some(url))
}

/// What the bot needs to work and must not show: the token that makes it the bot, and
/// the key it shows the API.
///
/// It has no `Debug`, so that neither can reach a log.
pub(crate) struct Secrets {
    /// What @BotFather gave. Whoever has it is the bot.
    pub(crate) telegram_token: String,
    /// What the bot shows the API to be given a token of a Telegram user (D60).
    pub(crate) service_key: String,
}

impl Secrets {
    pub(crate) fn from_env() -> anyhow::Result<Option<Self>> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// `None` when there is no Telegram token. The bot then has nobody to talk to,
    /// and idles (D8).
    ///
    /// # Errors
    ///
    /// With a token and no key, or one too short for the API to take: a bot that
    /// hears people and can do nothing for them is no bot.
    pub(crate) fn from_vars(var: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<Self>> {
        let Some(telegram_token) = set(&var, "TELEGRAM_BOT_TOKEN") else {
            return Ok(None);
        };
        let service_key = set(&var, "BOT_SERVICE_KEY").context(
            "TELEGRAM_BOT_TOKEN is set and BOT_SERVICE_KEY is not: \
             the bot needs the key the API was started with",
        )?;
        ensure!(
            service_key.len() >= SHORTEST_KEY,
            "BOT_SERVICE_KEY must be at least {SHORTEST_KEY} bytes long, got {}",
            service_key.len()
        );
        Ok(Some(Self {
            telegram_token: telegram_token.trim().to_owned(),
            service_key,
        }))
    }
}

/// The value of a variable, unless it is missing or empty. Docker Compose passes a
/// variable that is not set as an empty one.
fn set(var: &impl Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    var(name).filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "a key the bot shows, and long enough";

    fn vars<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(known, _)| *known == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    fn config(set: &[(&str, &str)]) -> anyhow::Result<Config> {
        Config::from_vars(vars(set))
    }

    fn secrets(set: &[(&str, &str)]) -> anyhow::Result<Option<Secrets>> {
        Secrets::from_vars(vars(set))
    }

    #[test]
    fn nothing_set_gives_the_defaults_and_a_bot_that_idles() {
        assert_eq!(
            config(&[]).unwrap(),
            Config {
                api_url: "http://127.0.0.1:8080".to_owned(),
                telegram_api_url: None,
                // Nobody to ask where an address is: the bot asks no third party
                // unless it is given one (D72).
                geocoder_url: None,
            }
        );
        assert!(secrets(&[]).unwrap().is_none());
        // The key alone is no reason to run: there is nobody to talk to.
        assert!(secrets(&[("BOT_SERVICE_KEY", KEY)]).unwrap().is_none());
    }

    #[test]
    fn every_setting_can_be_set() {
        let config = config(&[
            ("BOT_API_URL", " http://api:8080/ "),
            ("TELEGRAM_API_URL", "http://127.0.0.1:8081"),
            ("GEOCODER_URL", "https://nominatim.openstreetmap.org"),
        ])
        .unwrap();
        assert_eq!(config.api_url, "http://api:8080");
        assert_eq!(
            config.telegram_api_url.unwrap().as_str(),
            "http://127.0.0.1:8081/"
        );
        assert_eq!(
            config.geocoder_url.unwrap().as_str(),
            "https://nominatim.openstreetmap.org/"
        );
        // An API behind a path keeps the path.
        let behind = self::config(&[("BOT_API_URL", "https://example.com/routeforge/")]).unwrap();
        assert_eq!(behind.api_url, "https://example.com/routeforge");

        let secrets = secrets(&[
            ("TELEGRAM_BOT_TOKEN", " 123456:made-up-token\n"),
            ("BOT_SERVICE_KEY", KEY),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(secrets.telegram_token, "123456:made-up-token");
        assert_eq!(secrets.service_key, KEY);
    }

    /// Docker Compose passes a variable that is not set as an empty one.
    #[test]
    fn an_empty_variable_has_its_default() {
        let empty = [
            ("BOT_API_URL", ""),
            ("TELEGRAM_API_URL", "  "),
            ("GEOCODER_URL", ""),
            ("TELEGRAM_BOT_TOKEN", ""),
            ("BOT_SERVICE_KEY", KEY),
        ];
        assert_eq!(config(&empty).unwrap(), config(&[]).unwrap());
        assert!(secrets(&empty).unwrap().is_none());
    }

    #[test]
    fn an_address_that_is_none_is_refused_by_name_and_not_repeated() {
        for name in ["BOT_API_URL", "TELEGRAM_API_URL", "GEOCODER_URL"] {
            for value in [
                "api:8080",
                "the API",
                "ftp://api:8080",
                "http://",
                "http://bot:hunter2@api:8080",
                "http://:hunter2@api:8080",
                "http://hunter2@api:8080",
                "http://api:8080/?key=hunter2",
                "http://api:8080/#hunter2",
            ] {
                let error = config(&[(name, value)]).unwrap_err();
                assert!(error.to_string().contains(name), "{name}={value}: {error}");
                assert!(!error.to_string().contains("hunter2"), "{error}");
            }
        }
    }

    #[test]
    fn a_bot_with_a_token_needs_the_key_the_api_has() {
        let token = ("TELEGRAM_BOT_TOKEN", "123456:made-up-token");

        let Err(error) = secrets(&[token]) else {
            panic!("started with a token and no key");
        };
        assert!(error.to_string().contains("BOT_SERVICE_KEY"), "{error}");
        let Err(error) = secrets(&[token, ("BOT_SERVICE_KEY", " ")]) else {
            panic!("took a key of nothing");
        };
        assert!(error.to_string().contains("BOT_SERVICE_KEY"), "{error}");

        let short = &KEY[..31];
        let Err(error) = secrets(&[token, ("BOT_SERVICE_KEY", short)]) else {
            panic!("took a key of 31 bytes");
        };
        assert!(error.to_string().contains("at least 32"), "{error}");
        // What was refused is not repeated, and neither is the token.
        assert!(!error.to_string().contains(short), "{error}");
        assert!(!error.to_string().contains("made-up-token"), "{error}");
    }

    /// The lines of one service in `docker-compose.yml`, as they are on a clone that
    /// has no `.env`: every `${VARIABLE:-fallback}` replaced by its fallback.
    fn service_of_the_stack(name: &str) -> Vec<String> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docker-compose.yml");
        let compose = std::fs::read_to_string(path).unwrap();
        let opening = format!("  {name}:");
        compose
            .lines()
            .skip_while(|line| *line != opening)
            .skip(1)
            // The service ends where a line is indented as little as its own name.
            .take_while(|line| line.starts_with("    ") || line.trim().is_empty())
            .map(|line| fallbacks(line.trim()))
            .collect()
    }

    /// `text` with every `${VARIABLE:-fallback}` replaced by its fallback. A variable
    /// without one would be empty on a fresh clone, which is what must not be.
    fn fallbacks(text: &str) -> String {
        let mut resolved = String::new();
        let mut rest = text;
        while let Some((before, variable)) = rest.split_once("${") {
            let (variable, after) = variable.split_once('}').expect("a `${` is closed");
            let (_, fallback) = variable
                .split_once(":-")
                .unwrap_or_else(|| panic!("${{{variable}}} has nothing to fall back on"));
            resolved.push_str(before);
            resolved.push_str(fallback);
            rest = after;
        }
        resolved + rest
    }

    /// What a service is given for a variable, on a clone that has no `.env`.
    fn given(service: &[String], name: &str) -> Option<String> {
        service.iter().find_map(|line| {
            let (key, value) = line.split_once(": ")?;
            (key == name).then(|| value.to_owned())
        })
    }

    /// A fresh clone has no `.env` (D54). The bot then has no token and idles (D8),
    /// and all it takes to make it a bot is the token: the API is where compose puts
    /// it, and the key is the one the API of the stack falls back to.
    #[test]
    fn the_stack_starts_the_bot_on_what_the_compose_file_falls_back_to() {
        let bot = service_of_the_stack("bot");
        let on_a_clone = |name: &str| given(&bot, name);

        assert_eq!(
            Config::from_vars(on_a_clone).unwrap(),
            Config {
                api_url: "http://api:8080".to_owned(),
                telegram_api_url: None,
                // And it sends what people write to nobody: a geocoding service is
                // somebody's own choice (D72).
                geocoder_url: None,
            }
        );
        // It is passed on from `.env` when it is there, and has no fallback either.
        assert!(bot.contains(&"GEOCODER_URL:".to_owned()), "{bot:#?}");
        assert!(Secrets::from_vars(on_a_clone).unwrap().is_none());
        // The token is passed on from `.env` when it is there, and has no fallback.
        assert!(bot.contains(&"TELEGRAM_BOT_TOKEN:".to_owned()), "{bot:#?}");

        let with_a_token = |name: &str| match name {
            "TELEGRAM_BOT_TOKEN" => Some("123456:made-up-token".to_owned()),
            other => given(&bot, other),
        };
        let secrets = Secrets::from_vars(with_a_token).unwrap().unwrap();
        let api = service_of_the_stack("api");
        assert_eq!(
            Some(secrets.service_key),
            given(&api, "BOT_SERVICE_KEY"),
            "the bot and the API fall back to the same key"
        );
        // And the bot is started after the API can answer it.
        assert!(bot.contains(&"condition: service_healthy".to_owned()));
    }
}
