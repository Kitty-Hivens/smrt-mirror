use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub storage_dir: PathBuf,
    pub admin_token: Option<String>,
    /// Set the `Secure` flag on the panel session cookie. Default true (prod
    /// is fronted by nginx TLS); set `SMRT_COOKIE_SECURE=false` for local http
    /// dev so the cookie is still sent over plain `127.0.0.1`.
    pub cookie_secure: bool,
    /// Public base URL baked into built manifest URLs (cache + static). The
    /// authoring build uses it; defaults to the production mirror host. Also the
    /// origin the GitHub OAuth callback redirect_uri is built from.
    pub mirror_base: String,
    /// GitHub OAuth app credentials. When unset, the panel offers only the
    /// break-glass admin-token login.
    pub github_client_id: Option<String>,
    pub github_client_secret: Option<String>,
    /// GitHub numeric user ids granted the admin role on sign-in. Keyed by uid,
    /// not login: a login can be renamed or reassigned, a uid cannot.
    pub admin_github_uids: Vec<u64>,
    /// Machine-bearer token for the debug rung (compat-affecting authoring),
    /// above `admin_token`. Unset -> no break-glass debug bearer.
    pub debug_token: Option<String>,
    /// GitHub numeric user ids granted the debug role on sign-in -- the rung above
    /// admin (#39). A uid here outranks the admin allowlist.
    pub debug_github_uids: Vec<u64>,
    /// Key for the CurseForge metadata API, used server side only to ask whose
    /// file a cached jar is. Absent leaves the harvest with the Modrinth
    /// identity leg alone, which is what it had before.
    pub curseforge_api_key: Option<String>,
    /// Which language this deployment's audience reads, as a language tag.
    ///
    /// It settles one question: when a curator writes a card or a release note
    /// only per language and leaves the untagged copy empty, which translation
    /// fills it. That copy is what every client reading no language map gets,
    /// and for the launcher today that is every player, so filling it from
    /// English on a mirror whose players read Russian hands them text they
    /// cannot read.
    ///
    /// A property of the deployment rather than of the text: one mirror serves
    /// one audience. Defaults to `en`, which is what the rule did before this
    /// existed.
    pub default_language: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind_addr =
            std::env::var("SMRT_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:9000".to_string());
        let bind_addr = SocketAddr::from_str(&bind_addr)
            .map_err(|e| anyhow::anyhow!("invalid SMRT_BIND_ADDR '{bind_addr}': {e}"))?;

        let storage_dir = std::env::var("SMRT_STORAGE_DIR")
            .unwrap_or_else(|_| "/var/lib/smrt".to_string())
            .into();

        let admin_token = std::env::var("SMRT_ADMIN_TOKEN").ok();

        let cookie_secure = std::env::var("SMRT_COOKIE_SECURE")
            .map(|v| !matches!(v.as_str(), "false" | "0" | "no"))
            .unwrap_or(true);

        // Default to the local bind so a fresh self-hosted instance emits
        // working manifest URLs out of the box; a public deployment sets its
        // real origin here.
        let mirror_base = std::env::var("SMRT_MIRROR_BASE")
            .unwrap_or_else(|_| "http://127.0.0.1:9000".to_string());

        let nonempty = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
        let github_client_id = nonempty("SMRT_GITHUB_CLIENT_ID");
        let github_client_secret = nonempty("SMRT_GITHUB_CLIENT_SECRET");
        let parse_uids = |k: &str| -> Vec<u64> {
            std::env::var(k)
                .unwrap_or_default()
                .split(',')
                .filter_map(|s| s.trim().parse::<u64>().ok())
                .collect()
        };
        let admin_github_uids = parse_uids("SMRT_ADMIN_GITHUB_UIDS");
        let debug_github_uids = parse_uids("SMRT_DEBUG_GITHUB_UIDS");
        let debug_token = std::env::var("SMRT_DEBUG_TOKEN").ok();
        let curseforge_api_key = nonempty("SMRT_CURSEFORGE_API_KEY");

        // Trimmed and lower-cased on the way in, because it is matched against
        // map keys the mirror has already normalised the same way.
        let default_language = nonempty("SMRT_DEFAULT_LANGUAGE")
            .map(|v| v.trim().to_ascii_lowercase())
            .unwrap_or_else(|| "en".to_string());

        Ok(Self {
            bind_addr,
            storage_dir,
            admin_token,
            cookie_secure,
            mirror_base,
            github_client_id,
            github_client_secret,
            admin_github_uids,
            debug_token,
            debug_github_uids,
            curseforge_api_key,
            default_language,
        })
    }
}
