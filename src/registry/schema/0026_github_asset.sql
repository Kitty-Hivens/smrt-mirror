-- Which bytes a GitHub release asset turned out to be.
--
-- A pin names owner/name, a tag and an asset, which is enough to fetch the file
-- and not enough to say anything about it. The build needs a hash to put in the
-- manifest and the resolve needs one to place the mod, and neither may download
-- to find out: a resolve runs on every pack edit.
--
-- Keyed by the three fields rather than by sha1, because this is read in the
-- direction the pin runs. The reverse, bytes to identity, is what jar_read and
-- curseforge_file already answer.
--
-- An asset is immutable in practice but not by rule: a release can be deleted
-- and recreated under the same tag with different bytes. read_at is what lets a
-- later pass notice, and nothing here treats a row as final.
CREATE TABLE github_asset (
  repo     TEXT NOT NULL,
  tag      TEXT NOT NULL,
  asset    TEXT NOT NULL,
  sha1     TEXT NOT NULL,
  size     INTEGER NOT NULL,
  read_at  TEXT NOT NULL,
  PRIMARY KEY (repo, tag, asset)
);

CREATE INDEX github_asset_sha1 ON github_asset(sha1);
