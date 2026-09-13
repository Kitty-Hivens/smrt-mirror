use crate::accounts::Accounts;
use crate::authoring::curseforge::CurseForge;
use crate::authoring::{HarvestScheduler, Modrinth};
use crate::authoring::{PackDocs, PackStream};
use crate::config::Config;
use crate::events::MirrorEvents;
use crate::jobs::JobRegistry;
use crate::registry::Registry;
use crate::storage::Storage;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<Storage>,
    pub config: Arc<Config>,
    pub jobs: Arc<JobRegistry>,
    /// One shared Modrinth client (pooled connections) for the admin proxy
    /// handlers, instead of a fresh TLS handshake per request.
    pub modrinth: Arc<Modrinth>,
    /// The second identity leg, present only when a key is configured. Server
    /// side by construction: no client is ever told that CurseForge is involved.
    pub curseforge: Option<Arc<CurseForge>>,
    /// Mod-identity registry (embedded SQLite under the storage root).
    pub registry: Arc<Registry>,
    /// Coalescing background harvester. Construction only wires the deps; call
    /// `harvest.clone().spawn()` once after the runtime is up to start it.
    pub harvest: Arc<HarvestScheduler>,
    /// Persistent accounts + sessions (GitHub identities, server-side sessions).
    pub accounts: Arc<Accounts>,
    /// Who is in which pack, and what has happened to it. In-process like the
    /// job registry: presence that outlived the process would be a list of
    /// ghosts.
    pub packs: Arc<PackStream>,
    /// The live merge point for packs being edited. In-process like the room
    /// above, and rebuilt from `config.json` whenever nobody holds one, so it is
    /// a cache of an edit in flight rather than a second source of truth.
    pub docs: Arc<PackDocs>,
    /// What changed on the mirror, as it changes: the channel a panel view
    /// listens on instead of asking again on a timer.
    pub events: Arc<MirrorEvents>,
}

impl AppState {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        // the registry db lives next to cache/ + packs/; the parent must exist
        // before SQLite can create the file.
        std::fs::create_dir_all(&config.storage_dir).ok();
        let storage = Arc::new(Storage::new(config.storage_dir.clone()));
        let registry = Arc::new(Registry::open(config.storage_dir.join("registry.db"))?);
        let accounts = Arc::new(Accounts::open(config.storage_dir.join("accounts.db"))?);
        let modrinth = Arc::new(Modrinth::new()?);
        let curseforge = match config.curseforge_api_key.clone() {
            Some(key) => match CurseForge::new(key) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    tracing::warn!(error = %e, "curseforge client not built; identity leg disabled");
                    None
                }
            },
            None => None,
        };
        let events = Arc::new(MirrorEvents::default());
        let harvest = HarvestScheduler::new(
            storage.clone(),
            modrinth.clone(),
            curseforge.clone(),
            registry.clone(),
            events.clone(),
        );
        Ok(Self {
            storage,
            modrinth,
            curseforge,
            registry,
            harvest,
            config: Arc::new(config),
            jobs: Arc::new(JobRegistry::default()),
            packs: Arc::new(PackStream::default()),
            docs: Arc::new(PackDocs::default()),
            events,
            accounts,
        })
    }
}
