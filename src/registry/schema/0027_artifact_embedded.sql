-- The mods a jar carries inside itself rather than being. A Forge or NeoForge
-- jar lists its jar-in-jar under META-INF/jarjar/metadata.json and a Fabric
-- one under `jars` in fabric.mod.json, and the loader loads what is listed as
-- mods in their own right: Ars Nouveau 4.2.4 ships GeckoLib 4.2.1 that way, and
-- a dependency on `geckolib` is met by it with no GeckoLib jar in the pack.
--
-- Read from each nested jar's own metadata, so a row names the nested mod by
-- its modid, the version it declares and the loader its metadata is for. Kept
-- per artifact because a Modrinth pin's bytes are fetched once to be read and
-- then dropped: the next harvest has nothing to read them from again.
--
-- A row per (artifact, nested modid, loader): a jar shipping one mod for three
-- loaders nests three builds of it. A single row with an empty modid is the
-- negative (read, carries nothing), so a jar that nests nothing is not fetched
-- again to find that out.
CREATE TABLE artifact_embedded (
  sha1     TEXT NOT NULL,
  modid    TEXT NOT NULL DEFAULT '',
  loader   TEXT NOT NULL DEFAULT '',
  version  TEXT,
  read_at  TEXT NOT NULL,
  PRIMARY KEY (sha1, modid, loader)
);

-- The slug of a Modrinth project a dependency names and no mod in the
-- registry owns. A Modrinth mod states its dependencies by project, and a
-- library the pack only carries embedded is known by its modid alone, so the
-- slug is what joins the two: the same slug-as-modid rule the harvest already
-- links a self-hosted provider by. Refreshed by every harvest that sees the
-- dependency, unlike modrinth_project_name, which is a display cache.
CREATE TABLE modrinth_dep_slug (
  project_id TEXT PRIMARY KEY,
  slug       TEXT NOT NULL,
  read_at    TEXT NOT NULL
);
