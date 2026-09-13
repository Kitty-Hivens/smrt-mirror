-- What CurseForge says a cached jar is.
--
-- The harvest asks Modrinth by sha1 and believes the answer over anything the
-- jar says about itself. For a mod Modrinth does not carry there is no such
-- outside voice, so the registry falls back to the name and version written
-- inside the jar by whoever built it -- and a relabelled release rewrites
-- exactly those strings. This is the second voice, keyed by content the way the
-- first one is.
--
-- Keyed by sha1 like jar_read, and written for every jar that was asked about,
-- including the ones nothing matched: "asked, published nowhere" is the answer
-- this table exists to be able to give, and it cannot be told from "never
-- asked" unless the row is there.
--
-- asked_at is what keeps a negative from becoming permanent. A jar's bytes do
-- not change, so a match never needs asking again, but CurseForge builds its
-- fingerprint index lazily (the response says so in isCacheBuilt) and a file
-- can also simply be published later. So a row with a project is final and a
-- row without one is re-asked once it is old enough.
--
-- download_url is null in two different ways and the difference matters: no
-- row, or a row with no project, means nothing was matched, while a matched row
-- with a null url means the project's author turned off third-party
-- distribution. That last case is the one where the mirror may name a file but
-- must not serve it.
CREATE TABLE curseforge_file (
  sha1         TEXT PRIMARY KEY,
  fingerprint  INTEGER NOT NULL,
  project_id   INTEGER,
  file_id      INTEGER,
  display_name TEXT,
  file_name    TEXT,
  download_url TEXT,
  asked_at     TEXT NOT NULL
);

-- The lookup the harvest makes every run: which cached jars still owe an
-- answer. Partial, because the rows worth revisiting are only the unmatched
-- ones and they are the minority.
CREATE INDEX curseforge_file_unmatched
  ON curseforge_file (asked_at)
  WHERE project_id IS NULL;
