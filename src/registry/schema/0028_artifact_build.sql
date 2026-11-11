-- What each build a jar carries says about its dependencies, per loader.
--
-- A multi-loader jar is one artifact with one build per loader (More
-- Enchantment Info 0.4.2 nests a Fabric, a Forge and a NeoForge build), and the
-- build a pack runs is the one for its own loader. Modrinth lists one set of
-- dependencies for the whole version, so the build's own manifest and code are
-- what tell which of them that build actually needs.
--
-- `facts` is JSON: the modids the build declares required and optional, the
-- language loader its manifest names when a separate mod provides it, the
-- package prefixes it owns, and the package prefixes its code references
-- unconditionally. Kept per artifact for the reason artifact_embedded is: a
-- Modrinth pin's bytes are fetched once to be read and then dropped.
--
-- A single row with an empty loader is the negative (read, no build found),
-- so a jar is not fetched again to find that out.
CREATE TABLE artifact_build (
  sha1     TEXT NOT NULL,
  loader   TEXT NOT NULL DEFAULT '',
  facts    TEXT NOT NULL DEFAULT '{}',
  read_at  TEXT NOT NULL,
  PRIMARY KEY (sha1, loader)
);
