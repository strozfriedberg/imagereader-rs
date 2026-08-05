use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_config::default_provider::credentials::DefaultCredentialsChain;
use aws_config::provider_config::ProviderConfig;
use aws_credential_types::Credentials as AwsCredentials;
use aws_credential_types::provider::ProvideCredentials;
use aws_credential_types::provider::error::CredentialsError;
use aws_smithy_async::rt::sleep::TokioSleep;
use awscreds::Rfc3339OffsetDateTime;
use s3::creds::Credentials;
use time::OffsetDateTime;
use tokio::runtime::Runtime;
use tokio::sync::{Mutex as AsyncMutex, RwLock};

const AWS_AUTH_ENV_VARS: &[&str] = &[
    "AWS_PROFILE",
    "AWS_ACCESS_KEY_ID",
    "AWS_ACCESS_KEY",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "AWS_ROLE_ARN",
];

const EXPIRY_SKEW: Duration = Duration::from_secs(5 * 60);

static CHAIN_CACHE: Mutex<Option<HashMap<String, std::sync::Arc<DefaultCredentialsChain>>>> =
    Mutex::new(None);

pub struct S3Auth {
    pub shared_creds: std::sync::Arc<RwLock<Credentials>>,
    /// Region from `AWS_REGION` / profile config (via aws-config), if resolved at open.
    pub region: Option<String>,
    chain: Option<std::sync::Arc<DefaultCredentialsChain>>,
    refresh_lock: AsyncMutex<()>,
}

fn aws_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            Some(std::ffi::OsString::from(format!(
                "{}{}",
                drive.to_string_lossy(),
                path.to_string_lossy()
            )))
        })
        .map(PathBuf::from)
}

fn default_credentials_path() -> Option<PathBuf> {
    aws_home().map(|h| h.join(".aws/credentials"))
}

fn default_config_path() -> Option<PathBuf> {
    aws_home().map(|h| h.join(".aws/config"))
}

fn path_configured(path: Option<PathBuf>) -> bool {
    path.is_some_and(|p| p.exists())
}

fn aws_auth_configured() -> bool {
    AWS_AUTH_ENV_VARS
        .iter()
        .any(|key| std::env::var(key).is_ok())
        || std::env::var("AWS_SHARED_CREDENTIALS_FILE").is_ok()
        || std::env::var("AWS_CONFIG_FILE").is_ok()
        || path_configured(default_credentials_path())
        || path_configured(default_config_path())
}

fn auth_expected() -> bool {
    aws_auth_configured()
}

fn map_provider_error(err: CredentialsError) -> std::io::Error {
    std::io::Error::other(format!("S3 credentials could not be resolved: {err}"))
}

fn to_s3_credentials(aws: AwsCredentials) -> Credentials {
    let token = aws.session_token().map(str::to_string);
    Credentials {
        access_key: Some(aws.access_key_id().to_string()),
        secret_key: Some(aws.secret_access_key().to_string()),
        session_token: token.clone(),
        security_token: token,
        expiration: aws
            .expiry()
            .map(|t| Rfc3339OffsetDateTime::from(OffsetDateTime::from(t))),
    }
}

fn is_expired(creds: &Credentials) -> bool {
    match creds.expiration {
        Some(ref exp) => {
            let now = OffsetDateTime::now_utc();
            let skewed = now + time::Duration::seconds(EXPIRY_SKEW.as_secs() as i64);
            exp.0 <= skewed
        }
        None => false,
    }
}

fn profile_region_from_config(runtime: &Runtime) -> Option<String> {
    let sdk_config = runtime.block_on(aws_config::defaults(BehaviorVersion::latest()).load());
    sdk_config
        .region()
        .map(|region| region.as_ref().to_string())
}

fn chain_for_profile(
    runtime: &Runtime,
    profile_key: &str,
) -> Result<std::sync::Arc<DefaultCredentialsChain>, std::io::Error> {
    let mut cache = CHAIN_CACHE
        .lock()
        .map_err(|_| std::io::Error::other("S3 credential chain cache lock poisoned"))?;
    if cache.is_none() {
        *cache = Some(HashMap::new());
    }
    let map = cache.as_mut().expect("initialized above");
    if let Some(chain) = map.get(profile_key) {
        return Ok(chain.clone());
    }

    let conf = ProviderConfig::empty().with_sleep_impl(TokioSleep::new());
    let mut builder = DefaultCredentialsChain::builder().configure(conf);
    if profile_key != "default" {
        builder = builder.profile_name(profile_key);
    }
    if let Ok(region) = std::env::var("AWS_REGION").or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
    {
        builder = builder.region(aws_config::Region::new(region));
    }
    let chain = std::sync::Arc::new(runtime.block_on(builder.build()));
    map.insert(profile_key.to_string(), chain.clone());
    Ok(chain)
}

pub fn resolve_s3_auth(runtime: &Runtime) -> Result<S3Auth, std::io::Error> {
    let shared_creds = std::sync::Arc::new(RwLock::new(
        Credentials::anonymous().map_err(std::io::Error::other)?,
    ));
    let refresh_lock = AsyncMutex::new(());
    let profile_key = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".into());
    let chain = chain_for_profile(runtime, &profile_key)?;
    let region = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .ok()
        .or_else(|| profile_region_from_config(runtime));

    match runtime.block_on(chain.provide_credentials()) {
        Ok(aws_creds) => {
            let creds = to_s3_credentials(aws_creds);
            runtime.block_on(async {
                *shared_creds.write().await = creds;
            });
            Ok(S3Auth {
                shared_creds,
                region,
                chain: Some(chain),
                refresh_lock,
            })
        }
        Err(err) if auth_expected() => Err(map_provider_error(err)),
        Err(_) => {
            let anon = Credentials::anonymous().map_err(std::io::Error::other)?;
            runtime.block_on(async {
                *shared_creds.write().await = anon;
            });
            Ok(S3Auth {
                shared_creds,
                region,
                chain: None,
                refresh_lock,
            })
        }
    }
}

/// Effective S3 region from `AWS_REGION` / `AWS_DEFAULT_REGION`, then the profile
/// region resolved into [`S3Auth`]. Returns `None` when no region is configured,
/// so callers can discover the bucket's real region rather than assuming one.
pub fn s3_region_name(auth: Option<&S3Auth>) -> Option<String> {
    std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .ok()
        .or_else(|| auth.and_then(|a| a.region.clone()))
}

/// Whether a bucket holding `current` needs to be handed `fresh`.
///
/// Only the three fields that get signed are compared. Expiry deliberately is
/// not: a bucket carrying credentials that are valid but nearer their expiry
/// than the snapshot signs exactly the same, and treating that as a difference
/// would rewrite the bucket on every single call for no gain.
pub fn credentials_rotated(current: &Credentials, fresh: &Credentials) -> bool {
    current.access_key != fresh.access_key
        || current.secret_key != fresh.secret_key
        || current.session_token != fresh.session_token
}

pub fn snapshot_credentials_sync(
    runtime: &Runtime,
    auth: &S3Auth,
) -> Result<Credentials, std::io::Error> {
    ensure_fresh_sync(runtime, auth)?;
    Ok(runtime.block_on(async { auth.shared_creds.read().await.clone() }))
}

#[allow(dead_code)]
pub async fn snapshot_credentials_async(auth: &S3Auth) -> Result<Credentials, std::io::Error> {
    ensure_fresh_async(auth).await?;
    Ok(auth.shared_creds.read().await.clone())
}

pub fn ensure_fresh_sync(runtime: &Runtime, auth: &S3Auth) -> Result<(), std::io::Error> {
    runtime.block_on(ensure_fresh_async(auth))
}

pub async fn ensure_fresh_async(auth: &S3Auth) -> Result<(), std::io::Error> {
    let _guard = auth.refresh_lock.lock().await;
    let chain = match auth.chain.as_ref() {
        Some(chain) => chain,
        None => return Ok(()),
    };

    {
        let creds = auth.shared_creds.read().await;
        if !is_expired(&creds) {
            return Ok(());
        }
    }

    let aws_creds = chain
        .provide_credentials()
        .await
        .map_err(map_provider_error)?;
    let creds = to_s3_credentials(aws_creds);
    *auth.shared_creds.write().await = creds;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::SystemTime;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new() -> Self {
            Self {
                _lock: ENV_LOCK.lock().expect("env lock"),
            }
        }
    }

    fn with_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        let _guard = EnvGuard::new();
        static DID_SET: AtomicBool = AtomicBool::new(false);
        for (key, value) in vars {
            match value {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
            DID_SET.store(true, Ordering::SeqCst);
        }
        f();
        if DID_SET.load(Ordering::SeqCst) {
            for (key, _) in vars {
                unsafe { std::env::remove_var(key) };
            }
        }
    }

    #[test]
    fn to_s3_credentials_maps_fields_and_expiry() {
        let expiry = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let aws = AwsCredentials::builder()
            .access_key_id("AKID")
            .secret_access_key("SECRET")
            .session_token("TOKEN")
            .expiry(expiry)
            .provider_name("test")
            .build();

        let creds = to_s3_credentials(aws);
        assert_eq!(creds.access_key.as_deref(), Some("AKID"));
        assert_eq!(creds.secret_key.as_deref(), Some("SECRET"));
        assert_eq!(creds.session_token.as_deref(), Some("TOKEN"));
        assert_eq!(creds.security_token.as_deref(), Some("TOKEN"));
        assert!(creds.expiration.is_some());
    }

    fn creds(access: &str, secret: &str, token: Option<&str>) -> Credentials {
        Credentials {
            access_key: Some(access.into()),
            secret_key: Some(secret.into()),
            session_token: token.map(str::to_string),
            security_token: token.map(str::to_string),
            expiration: None,
        }
    }

    /// Holding one bucket across many probes is only safe if rotation is
    /// noticed, so each signed field has to count.
    #[test]
    fn credentials_rotated_spots_each_signed_field() {
        let base = creds("AKID", "SECRET", Some("TOKEN"));

        assert!(!credentials_rotated(&base, &base.clone()));
        assert!(credentials_rotated(
            &base,
            &creds("AKID2", "SECRET", Some("TOKEN"))
        ));
        assert!(credentials_rotated(
            &base,
            &creds("AKID", "SECRET2", Some("TOKEN"))
        ));
        assert!(credentials_rotated(
            &base,
            &creds("AKID", "SECRET", Some("TOKEN2"))
        ));
        assert!(credentials_rotated(&base, &creds("AKID", "SECRET", None)));
    }

    /// The other half: identical signing material must NOT count as rotation,
    /// or the bucket gets rewritten on every probe and the caching is undone.
    /// Expiry moving on its own is the case that matters -- it does not change
    /// what gets signed.
    #[test]
    fn credentials_rotated_ignores_expiry_alone() {
        let mut a = creds("AKID", "SECRET", Some("TOKEN"));
        let mut b = a.clone();
        a.expiration = Some(Rfc3339OffsetDateTime::from(
            OffsetDateTime::now_utc() + time::Duration::hours(1),
        ));
        b.expiration = Some(Rfc3339OffsetDateTime::from(
            OffsetDateTime::now_utc() + time::Duration::hours(9),
        ));
        assert!(!credentials_rotated(&a, &b));
    }

    #[test]
    fn is_expired_respects_skew_buffer() {
        let soon = OffsetDateTime::now_utc() + time::Duration::minutes(4);
        let creds = Credentials {
            access_key: None,
            secret_key: None,
            security_token: None,
            session_token: None,
            expiration: Some(Rfc3339OffsetDateTime::from(soon)),
        };
        assert!(is_expired(&creds));

        let later = OffsetDateTime::now_utc() + time::Duration::hours(2);
        let creds = Credentials {
            access_key: None,
            secret_key: None,
            security_token: None,
            session_token: None,
            expiration: Some(Rfc3339OffsetDateTime::from(later)),
        };
        assert!(!is_expired(&creds));
    }

    #[test]
    fn auth_expected_true_for_env_and_file_vars() {
        with_env(&[("AWS_PROFILE", Some("dev"))], || {
            assert!(auth_expected());
        });
        with_env(&[("AWS_CONFIG_FILE", Some("/tmp/aws-config"))], || {
            assert!(auth_expected());
        });
    }

    #[test]
    fn auth_expected_false_without_aws_config() {
        with_env(
            &[
                ("AWS_PROFILE", None),
                ("AWS_ACCESS_KEY_ID", None),
                ("AWS_CONFIG_FILE", None),
                ("AWS_SHARED_CREDENTIALS_FILE", None),
                ("HOME", Some("/nonexistent-empty-home-for-test")),
            ],
            || {
                assert!(!auth_expected());
            },
        );
    }

    #[test]
    fn s3_region_name_prefers_env_over_profile() {
        with_env(
            &[
                ("AWS_REGION", Some("eu-west-1")),
                ("AWS_DEFAULT_REGION", None),
            ],
            || {
                let auth = S3Auth {
                    shared_creds: std::sync::Arc::new(RwLock::new(
                        Credentials::anonymous().unwrap(),
                    )),
                    region: Some("us-west-2".into()),
                    chain: None,
                    refresh_lock: AsyncMutex::new(()),
                };
                assert_eq!(s3_region_name(Some(&auth)), Some("eu-west-1".to_string()));
            },
        );
    }

    #[test]
    fn s3_region_name_uses_profile_when_env_unset() {
        with_env(
            &[("AWS_REGION", None), ("AWS_DEFAULT_REGION", None)],
            || {
                let auth = S3Auth {
                    shared_creds: std::sync::Arc::new(RwLock::new(
                        Credentials::anonymous().unwrap(),
                    )),
                    region: Some("us-west-2".into()),
                    chain: None,
                    refresh_lock: AsyncMutex::new(()),
                };
                assert_eq!(s3_region_name(Some(&auth)), Some("us-west-2".to_string()));
            },
        );
    }
}
