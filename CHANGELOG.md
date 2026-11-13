# Changelog

Notable changes to the smrt mirror. The service deploys continuously from
`main`; entries land under Unreleased as they ship and collapse into a
version section when a release is tagged.

## Unreleased

### Fixed

- The operator panel fell apart on a phone. The packs and servers tables
  scrolled sideways and hid their actions, the users row pushed suspend off
  the edge, a my packs row laid its visibility over the pack name, the pack
  editor lost Preview and Close past the right edge, and at tablet width the
  top strip showed one of twelve sections. Tables now fold into cards, rows
  move their actions under the name, the editor header wraps, and a window
  too narrow for a side rail gets the strip only when every section fits in
  it, the drawer otherwise. The overview and my packs lists show pack banners.
- The catalog never showed a pack's banner, though six official packs carry
  one. It was an accordion of rows: a hovered row's version pill vanished into
  the row's own background, the version column drifted with the width of the
  strings in it, and on a phone the tagline came down to a word beside two
  fixed tags. The catalog is now a showcase of cards led by each pack's banner,
  and a card opens the pack's own page at `/browse/<id>`, with the banner shown
  whole, the loader, Java and mod count, the description, the gallery, the
  discussion and the mods. A pack can be linked to the way a mod can.
- A jar a pack ships and the registry has not indexed opened a mod page that
  said only "Mod not found", with an error notice on top for an expected
  answer. The page now shows the name, source, size, file, hash and pack the
  link came from, and keeps them across a reload. The packs a mod page lists
  are links to their catalog pages and go by their card names.
- A multi-loader mod read its dependencies off Modrinth's list for the whole
  version. More Enchantment Info 0.4.2 is one Modrinth version for Fabric, Forge
  and NeoForge, so a NeoForge pack was told it lacked Fabric Language Kotlin and
  Fabric API and the build refused to publish, while KotlinLangForge, which the
  NeoForge build cannot load without, was named only in its `modLoader`. A mod
  is now read through the build the pack runs: Modrinth's list stays the base, a
  dependency the build declares optional and whose packages its code never
  touches is optional, a language loader the build names is a hard dependency
  on its provider, and an unmet dependency with builds only for other loaders is
  listed apart rather than blocking. The harvest records each build's facts once
  per artifact (migration 0028).
- A library a declared jar already carries was pulled in beside it, and stayed.
  MezzConfig's standalone build has no mod of its own, only the mod nested in it,
  so the mirror never learned it was `mezz_config`, and JEI's own copy could not
  answer JEI's dependency. The wrapper is now known as the mod it carries, and a
  pulled jar whose dependency a declared jar answers as well is pruned.

- A pack's discussion was invisible to the people it is for. The reads were
  public and nothing in the browser used them: the catalog showed a pack's
  description and its mods and said nothing about what was being asked of it,
  and a thread could only be opened from inside an editor, which is a surface a
  reader has no business being in. A pack in the catalog now carries its
  discussion, folded away until wanted, and a thread has an address that works
  with no session behind it -- so "see the report" is a link anybody can follow,
  and signing in is what saying something needs, not what reading it needs.
- The panel guessed what the viewer was allowed to do here, from the pack id and
  the caller's role. That answers correctly for the pack's owner and for an
  admin, and wrongly for everybody who reached a pack by grant -- the one case
  grants exist for -- so a granted keeper saw no merge button, no moderation and
  no access list, while the mirror would have accepted all three from them. The
  panel now asks the gate that enforces it.
- Every hairline in the history, commit, access and discussion views was drawn
  in a colour that does not exist. `--line` was never a token in the stylesheet,
  so each of those borders resolved to nothing: comment blocks ran together, the
  rule under a report's text was absent, and rows in a list had no separators.
- A button that reads as a line of text was still wearing a card: the panel's
  shared control style gives every button a shadow and a hover fill, and the six
  views that hand-rolled a "link" reset the background and the border and forgot
  those two, so "show settled too" sat on a faint tile that lit up under the
  cursor. The reset now lives once, next to the style it undoes.
- A pack's access list named whoever decided a row by number. `uid 0` is the
  mirror's own break-glass hand rather than a person, and a bare uid beside
  "granted by" tells a reader nothing; both now resolve to a name where there is
  one.
- Opening proposals had no ceiling. The rate window covered reports and comments
  and left the third way of writing to a pack open, so the one write that costs
  a reviewer the most attention was the one nothing limited.
- Listing the packs somebody may reach asked the access store once per pack on
  the mirror. It now asks once.
- "47 changes since the last commit" and the list under it were two different
  answers, computed twice from different rules. The count came from a JSON walk
  that compared arrays by position, so adding one mod to an alphabetically
  ordered pack reported every row below it as changed -- 79, for a single
  arrival -- while the list beside it correctly showed one. The walk also
  compared the whole config, including the fields only the server writes and the
  rows the dependency fill appends, which is how a save that changed nothing a
  person did could still report 22. There is now one diff, on the mirror, and
  everything that says what changed reads it: the commit box, the count, the
  build's refusal, and what a commit recorded. It matches rows by identity
  rather than position -- a mod is its Modrinth project, else its curator slug,
  else its filename -- and compares the projection `edit_rev` hashes, minus what
  only the mirror writes inside a row, so a revision that has not moved answers
  "nothing to commit" without reading the checkpoint at all.
- A jar renamed without changing is a rename, not a departure and an unrelated
  arrival. Both rows point at the same artifact, and that is the evidence.
- A re-pin and a switched install default on one mod are two rows. The old list
  showed the first and dropped the second, so a pack could ship a mod nobody
  meant to turn off with a message that only mentioned the version bump.
- An edit to a mod's project url no longer passes unnoticed: the panel's list of
  authored fields named `homepage`, which is not a field the manifest has -- the
  one it does have is `url`, and it was never compared.
- Reverting to a published build records a checkpoint of its own. It wrote the
  config and stopped, so the pack came back looking changed by nobody, with the
  next build refusing to publish and nothing in the history saying why.
- Reading a pack's history no longer reads every manifest it ever published.
  Both the log and a commit page answered "which versions came out of this
  checkpoint" by parsing the header of every retained manifest -- a hundred and
  thirty files on the reference deployment's flagship pack, once per page. The
  answer is now kept and dropped whenever a manifest is written, so it can only
  be behind by a build nobody has published yet.
- The live-edit markers address the row someone touched rather than the position
  it held. Paths were indexes, so a mod arriving in the middle of the list
  counted as a change to every row below it, and the summary above the editor
  said "someone - a mod" once per displaced row. It now groups by what it says,
  so two mods edited in one window read as one line naming both people. Rows
  that name nothing (a freshly added asset, a list of tags) keep their
  positions, and two rows sharing a name are still two rows.
- Restoring a commit asks first, and says what it will do: how many arrivals,
  departures and changes it writes over the working state. It was a single
  click, next to "build this", with no statement of consequences anywhere.
- A pack whose pinned loader build is older than one of its mods demands no
  longer publishes clean. Mods declare the loader window they run in, and the
  pin next to them is a hand-typed line that nothing moves; when JEI raised its
  floor to `neoforge [21.1.238,)` under a pack pinned to `21.1.234`, the loader
  stopped before the main menu and named JEI, which is neither the file to
  change nor a hint that a config line is. The window was being read out of
  every jar and thrown away -- the loader is present by construction, so its
  dependency block was filtered out with the platform ids, and the version check
  that existed only ever compared mods against each other. It is now kept, and a
  build whose pin falls outside one is blocked with the jar, the window and the
  pin all named. The editor's resolve view shows the same finding while the pin
  is being chosen.
- The window lives inside the jar, and a Modrinth-pinned jar's bytes are not on
  the mirror -- 92 of the 97 mods in the reference deployment's flagship pack.
  Rather than download half a gigabyte per pack to read a few kilobytes, each
  artifact's manifest is fetched with two HTTP range requests against the zip's
  own directory, and what it declared is remembered: a jar is immutable, so the
  next check of the same pin is a database query.
- A Modrinth project's icon is served by the mirror instead of by Modrinth's
  CDN. The panel asked the mirror for the upstream url and then put that url in
  an `<img>`, so every viewer's browser announced itself to a third party once
  per icon on a page -- the exact thing the GitHub avatar proxy next door exists
  to prevent. The icon is now fetched once, kept, and served from here, which
  also ends two other things: the endpoint behind it was session-gated, so a
  signed-out visitor got a 401 per Modrinth mod on the public catalog and saw
  letter avatars instead of icons (342 of the 354 unauthorized answers in a day
  of the reference deployment were this), and drawing a page cost one request per
  mod row where it now costs a cached image. Jars are still not rehosted -- an
  icon is a picture, not the mod. Svg icons are not served back: an svg from
  elsewhere, served from our own origin, is a document that can carry script.
- A mod icon is extracted from its jar once, not on every request. Serving one
  read the whole jar into memory and walked its zip to hand back a few
  kilobytes of image -- and it is the busiest endpoint the mirror has: a day of
  the reference deployment answered 2194 icon requests, which at that cache's
  average jar size meant reading and unzipping some 4.7 GB to deliver 6.7 MB. A
  jar is named by its own hash, so its icon cannot change; it is now kept beside
  the cache and read from there. Jars that carry no icon -- most of them -- are
  remembered as such, which is the half that mattered more, and a takedown drops
  the remembered icon along with the bytes it came from.
- The Modrinth icon lookup is remembered for a few hours instead of asked on
  every request. The panel asks once per mod row it draws, so rendering the
  registry sent a burst of outbound calls to a third party for an answer that
  changes about never -- 1393 of them in the same day, against a rate limit the
  client already has to back off from.
- The mark is in the shell, where a blank rounded square stood in for it. The
  header and the sign-in card both reserved a slot for a logo and filled it with
  a flat block of the foreground colour; they draw the actual mark now. A home
  screen icon and a theme colour come with it.

  What deliberately keeps its placeholder: a mod or a pack with no icon of its
  own still gets a letter tile. The site mark there would not be a placeholder
  but a claim -- it would read as though the mirror authored the thing.
- The site icon is served in every shape something asks for. Naming an svg in
  the shell covers browsers that read the declaration, and left everything that
  asks for `/favicon.ico` by convention -- bookmark bars, link previews, older
  browsers -- being handed the app shell as an image, which shows nothing. A png
  and an ico are served under their own names now, cacheable for a day rather
  than re-fetched with every page load.
- Reading one jar's facts no longer costs a walk of the whole cache. `cached` on
  a mod page, on a version list, on a build's mods, and a jar's size when it is
  given an identity, were each answered by listing every shard directory in the
  cache and every file in it -- a cost that grew with the mirror while the
  question stayed the size of one hash. A jar's path is its own hash, so each is
  now one lookup, or one per artifact actually asked about. The reads that are
  about the cache as a whole -- the inventory, the needs-identity bucket, the
  usage report, the harvest -- still list it, which is what listing is for.
- Opening the registry or the graph after a deploy no longer fails on a missing
  file. The panel's chunks are named after a hash of their contents and ship
  inside the binary, so a deploy replaces the whole set at once -- while a
  browser was free to cache the shell that names them by its own guesswork, and
  a cached shell asks for chunks that no longer exist. The parts loaded on
  demand (the registry browser, the graph) were exactly the ones that failed,
  and they failed as a MIME-type complaint about a 404 with a body, which named
  neither cause. The shell is now revalidated on every load, hashed assets are
  declared immutable, a missing asset answers a bare 404, and a page that was
  already open when the deploy landed reloads itself once rather than showing
  the error.
- A pack's own files are named for the pack, not for one launcher. Icons,
  banners and dropped assets were minted under `_nexira/`, the reference
  deployment's client name, so every pack on every self-hosted mirror carried a
  directory named after somebody else's launcher -- in the storage layout rather
  than in a string somebody reads. New files go under `_pack/`. Forward-only and
  no migration: a stored path is just a path, so `_nexira/...` keeps resolving
  for every pack that already has one, and the sweep that keeps an icon
  resolving to exactly one file sees both names.
- The handshake claim says up front when it cannot be built. It is copied from
  what a server advertises in its status ping, and a NeoForge or Fabric server
  advertises nothing -- modern loaders negotiate after connecting, over
  registered channels rather than a declared list. The mirror already refused
  correctly; the panel now says so before the button rather than after, and
  explains why the claim does not port to a loader whose channels are live pipes
  instead of assertions.
- Republishing a version that already exists is refused unless it is asked for.
  Auto-numbering never collides -- it takes the next counter past the highest
  published -- so this only fires when a version is named by hand, which is
  exactly when it is worth stopping. Rewriting a published manifest changes what
  anyone already holding that version downloaded under that name, makes the
  commit it records as its origin no longer what shipped, and retroactively
  changes every diff that touches it. `--overwrite-version` on the CLI and
  `overwrite_version` on the build endpoint do it deliberately, and the build
  says so in its log.
- The pack importer is named for what it reads. Bootstrap and validate called
  their input an "SC archive" everywhere -- module docs, log lines, CLI flags,
  a wire field, and the empty-state a new self-hoster meets first -- while the
  code behind it accepts any zip with a top-level `mods/`: a launcher export, a
  plain instance directory. It is an instance archive now.
  `ValidateReport.sc_mod_count` becomes `archive_mod_count`, and `smrt-pack
  validate --against-sc-archive` becomes `--against-archive`.
- Incompatibility is reported at the strength it was declared. The registry
  carried `conflicts` and `breaks` as separate kinds, and every parser stored
  the hard declaration as `conflicts` and the advisory one as `breaks` -- while
  in Fabric's own vocabulary `breaks` is the hard one, so everything that
  reported a conflict read the variant name, believed it, and printed the
  alarming word on the milder fact. Forge and Fabric use the same two words for
  opposite intensities, so a name could never carry it: incompatibility is now
  one kind with a severity the declaration states, and the panel, the
  pre-publish check and the graph all say which one they mean.
- Live editing no longer poisons the panel with a value JSON cannot carry.
  Every whole number in a config went into the shared document as a yrs
  BigInt, which yjs decodes in the browser as a JavaScript `BigInt` --
  so an editor merged an update and then threw
  `BigInt value can't be serialized in JSON` on the next save, with the pack's
  own Java version enough to trigger it. Numbers now cross as doubles, which
  is what the same config already is when the panel reads it over REST, and
  come back as whole numbers so a `u32` field still deserializes.

### Added

- Dependencies can be recomputed without editing a pack. A save fills them only
  when the declared mods change, so what the mirror learns afterwards, a library
  a jar turns out to carry or a build that settles a dependency, never reached a
  pack nobody touched. `POST /v1/authoring/packs/{id}/fill` runs the fill on the
  stored config and saves it like an edit, with the same `If-Match`, and the
  editor has a button for it beside the dependency check.
- Notifications can be read outside the panel, without the mirror reaching
  anybody. `GET /v1/feed.atom?key=<key>` is the account's own list as Atom: one
  address per account, minted when it is first asked for and retired by rotating
  it. Nothing leaves this machine to deliver it -- no address of anybody's is
  stored, no third party is asked, and an account that never wants a feed never
  has a key to leak. A feed reader has no session and cannot be given one, so
  the address is the credential; the panel says so where it hands it over.
- The list of past checkpoints opens as a dock over whatever is being worked on,
  from any tab of the editor, instead of living inside the Build tab. It is a
  tool rather than a place: consulted while doing something else, so the editor
  underneath never reflows and the pack does not have to be left to read what
  happened to it. Declaring a checkpoint stays beside the build button, because
  a build is made from a commit and the same sentence serves both acts.

- The mirror's operators can stop an account, which a pack's own block cannot
  reach. A block answers who writes in one discussion; it says nothing about
  somebody whose pack was itself the offence. A suspension bars that account
  from putting anything on the mirror -- authoring or forking a pack, uploading
  a jar, opening a thread, saying anything on one -- and touches reading
  nowhere, because it is not a way to unpublish what somebody already made. It
  is enforced in the access gate itself rather than at each of the forty writes
  behind it, it carries the reason the operator wrote, and the account is told
  in a standing bar instead of by controls that look usable and then refuse.
  Operators cannot be suspended: take the rung away first.

- Being answered reaches the person it is for. A discussion where a reply
  reaches nobody is one people stop reading: a report sat open because its
  author never learned it had been answered, and a pack's keepers learned about
  a report by happening to open the tab. A comment now tells everybody already
  in the discussion and the people who keep the pack, opening a thread tells the
  keepers, and a decision tells everybody who was in it -- nobody is ever told
  about their own act. The account's own page carries the list and the rail
  carries the count. What is stored is the event, not a copy of the thread: the
  title and the standing are read live, so an edited title leaves no stale line
  behind, and taking somebody's access to a pack away forgets what they were
  told about it. The list pages like every other listing here; the unread count
  stays the count of the whole list, because a badge and a page are different
  numbers.
- A long discussion is read a page at a time, and so is a pack's list of them.
  Both answered whole however much had accumulated. They now take `?limit=` and
  name the next page in a `Link` header -- keyset, like every other listing here,
  so a comment arriving while somebody reads cannot shift the page under them --
  and without `limit` they answer whole, as before. The panel asks for a page and
  offers to show more; saying something appends it rather than re-reading the
  discussion, which on a long thread used to mean losing every page but the
  first.

- A pack's keepers can stop somebody writing on it. Anyone signed in may report
  something on a published pack -- which is what makes a report worth having --
  so the only thing standing between a pack and a flood was a rate window and
  hiding messages one at a time, which answers what was already said and nothing
  about the next one. A block refuses that person's reports, proposals and
  comments on that pack, carries the keepers' own note about why, and touches
  reading not at all: what they said before stays where it is, because a block
  is not a way to unsay something. It sits at the moderation level rather than
  the owner's, beside taking a comment down, and the gate refuses to block
  anybody who keeps the pack.
- What a proposal offers can be read by anyone who can read the proposal. The
  diff was the one part of a discussion that still needed a session, so a
  stranger could see that an offer had been declined and not what was declined.
  Offering a commit to a pack publishes that commit's content to the pack's
  readers -- the offer is the act of publication.
- The discussion surface is gated where it is written, not only where it is
  read: the levels are now pinned by tests that drive the real router with a
  real session, so a handler that forgets to gate, or gates at the wrong level,
  fails the build rather than somebody's pack. Writing has a ceiling -- twenty
  comments or five threads in ten minutes, counted from the rows themselves, so
  a restart hands nobody a fresh allowance. The public routes are in the
  OpenAPI document, where the wire is supposed to be described. And a fork can
  be offered back from the panel rather than only over HTTP.
- The panel can hold a discussion. A pack gains a Discussion tab -- reports and
  proposals in one list, because to a reader they are one thing: somebody asking
  the pack's keepers for something, and a proposal hidden in its own tab is a
  request nobody stumbles over. A thread has an address of its own, so "see the
  report" is a link. A proposal's page carries what taking it would do to the
  pack as it stands, drawn by the same component the commit box uses, and the
  keeper's buttons -- take it, decline, close, reopen, take a comment down -- sit
  under the discussion they belong to.
- Packs have discussions, in the open. Two kinds of thread: an issue (a report)
  and a proposal (a fork offered back, which is the same thread with a commit
  attached). Reading is as public as the pack -- a published pack's reports,
  proposals, comments and outcomes are readable without signing in, because a
  decision nobody can see is indistinguishable from one nobody made -- and a
  draft's discussion stays as private as the draft. Anyone signed in may report
  something or join a discussion they can read; closing, declining, merging and
  moderating need `edit` on the pack, and whoever opened a thread may withdraw
  it. A moderated comment is hidden rather than deleted: the mirror stops
  serving its body and keeps the gap visible, because a hole in a discussion
  reads worse than a marked one.
- A fork can be offered back to the pack it came from. Forking was already
  possible and entirely one-way: `fork_of` was recorded and read by nothing, so
  somebody who improved a fork of your pack had no way to offer the improvement
  and you had no way to take it. A proposal is now a request around a commit --
  the offered state is an immutable snapshot in the fork, so what a reviewer
  reads cannot move while they read it -- and the review shows what taking it
  would do to the pack as it stands now, not against the fork's parent. Taking
  it writes the offered content in as an ordinary commit, readable afterwards by
  the same history as everything else; ownership, tier, visibility and identity
  stay the target's, because a proposal that could move them would be a rename
  away from a takeover. Saying no keeps the row, and settling is a one-time
  write, so two reviewers pressing at once cannot both decide it.
- Access to a pack can be given to one person, on one pack (ADR 0006). Until
  now the only way to let somebody help was to make them an admin of the whole
  mirror -- every pack, the registry, moderation, takedowns and the user list --
  because who may author a pack was a function of an identity with exactly two
  answers: the owner of a community namespace, or an admin. Three levels are
  now grantable per pack: `view` (a draft, its history, its reports), `edit`
  (config, commits, builds) and `own` (also access, visibility, deletion). The
  two old answers still need no lookup and are deliberately not rows in the
  list: the namespace owner and the admins reach a pack without being granted
  anything, and the list says so rather than inventing entries. One gate decides
  all of it, ahead of every authored read and write, so the levels are compared
  in one place rather than re-derived in each of the thirty-nine handlers that
  used to ask.
- A pack's history is readable past its first page. The log answered a hundred
  commits by default and five hundred at most, with nothing to continue from, so a pack that had been curated
  for long enough simply had no way to reach its older checkpoints. It now pages
  by the chain itself -- every commit names its parent, so the cursor is a
  commit id and a page boundary cannot drift when someone commits while another
  person is reading.
- A commit has a page and an address (`/packs/<id>/commit/<sha>`): its message,
  who declared it, everyone whose work it took in, the full id, and what it
  recorded -- read against the commit before it, or against the working state,
  which is the question a restore asks (the endpoint takes any other checkpoint
  as well). Building from it and putting it back are both there.
- A build made from a checkpoint records which one, and the history says which
  versions came out of each commit. The field was written and read by nothing,
  so "which state is 0.1.31" had no answer; it is now in the public versions
  listing as `built_from`. A CLI build has none to name -- it builds the working
  config -- and neither does a dry run.
- A commit message has a subject and a body, and the box offers a first line
  drawn from the changes themselves. A message written from memory tends to omit
  the change nobody remembers making.

- The commit box lists what is about to be recorded. It showed a count and a
  message field, so the sentence beside it was written from memory -- and the
  count is of changed JSON paths, which is not a list of things a person did: a
  save whose only effect was the dependency fill writing two `display.requires`
  lists reported 22 changes. The box now names them: mods that arrived, left, or
  moved a pin (with both versions, not the two Modrinth ids), assets, install
  defaults, the loader, and the pack's own fields. What the mirror fills in
  server-side is deliberately not a row, and when that is all that moved the box
  says so rather than showing an empty list under a number.

- Reads cost what they are worth. Four things the whole `/v1` surface does now,
  written up in the API guide:
  - **Compression.** Manifests and listings are repetitive JSON and went out as
    plain text -- tens of kilobytes per read. It lives in the mirror rather than
    in a reverse proxy, because the proxy is one deployment's choice and a
    self-hoster behind Caddy or behind nothing is still the product's user. Jars
    and zips are served as they lie (a zip re-compresses to its own size), and
    so are live event streams, where buffering a line to encode it is the one
    thing a tail must not do.
  - **Conditional GET.** Every JSON read carries an `ETag`; send it back as
    `If-None-Match` and an unchanged answer costs `304` with no body. The tag is
    the hash of the answer, so it is stable across restarts and across two
    mirrors serving the same data, and weak, because the same data gzipped and
    plain is the same data.
  - **Paging.** `/v1/registry/mods`, `/v1/cache/inventory` and `/v1/audit` take
    `?limit=` and name the next page in a `Link` header. Keyset rather than
    offset: rows arriving mid-walk land outside the page being read instead of
    shifting it. Asked for rather than imposed -- without `limit` each listing
    answers exactly as before, and the body shape does not change either way.
    Each now costs what its page costs: the registry listing folded facets over
    every artifact the mirror holds before answering, and the cache inventory
    walked the whole cache to discard everything before the cursor. The audit
    trail is also readable past its most recent 200 entries for the first time,
    which is where the entry anyone goes looking for usually is.
  - **Resumable downloads.** Files answer `Range`, so a transfer that died at
    90% asks for the rest instead of starting over. A cache jar cannot change
    under a resume at all, being content-addressed; for a static file that can,
    pairing the range with `If-Unmodified-Since` answers `412` rather than
    splicing two versions together.
- A build listing says what it targets and what it weighs: `minecraft_version`,
  `loader` and `size_bytes` (every mod and asset the build lists, added up) on
  each entry of `/v1/packs/{id}/manifest/versions`. Telling a player that an
  update moves them to 1.20.1, off Forge, or costs 400 MB previously meant
  fetching every manifest in the list to read three fields out of each.
- `GET /v1/events` (SSE, any signed-in caller): what changed on the mirror, as
  it changes -- the mod index moved, a pack published, the moderation queue
  moved. A view listens instead of asking again on a timer, which is both dearer
  (a round trip whether or not anything happened) and slower (a harvest that
  finishes just after a poll stays invisible until the next one). The event is a
  nudge rather than the data: refetch the one view that cares, and that read is
  the conditional GET above. Filtered by role -- the moderation queue is the
  operator's -- and live-only, like the per-pack rooms and the job registry.
- A publish stops when a jar's required mixin patches a class the pack no longer
  carries. Twice in one day a published pack died during init on exactly this,
  and neither case was visible in any declaration -- both mods declare an open
  lower bound on their host, which every version satisfies, and nothing in the
  metadata says "I reach into this class". The mirror now reads what each jar's
  required mixin configs must resolve, records what each artifact provides, and
  refuses the build when the pack's own copy of the host has lost it. The finding
  names the jar that asks, the class that is gone, and whose copy lacks it -- the
  three things the crash report cannot, since the loader blames whichever mod
  first reached the missing class.
- Release notes on a build can be written per language. A build carries
  `changelog_i18n` beside `changelog` -- the same notes keyed by language tag --
  and a launcher renders whichever matches its user, falling back to the
  untagged text every client already reads. A language left blank is absent
  rather than published as an empty note. The panel offers a tab per language it
  speaks; the wire accepts any tag.
- Pack history: a commit is a snapshot of the config, an author, a message
  and a parent, and a build is made from one rather than from whatever is on
  screen. With edits merging live, building the working state could ship a
  half-typed word and the pre-publish check judged something that moved while
  it was judging it. A publish takes the head of the history and refuses when
  work sits uncommitted, saying how much; in the panel that is one press --
  the build declares the checkpoint from the message already on screen. An
  older commit can be built again by name. The manifest records the commit it came from, so what shipped in a
  given version is answerable from the manifest alone.
  History is linear and append-only: restoring writes an older state forward
  as a new commit rather than rewinding, so no build ends up naming a commit
  its own history no longer admits. Saves note their author, so a commit names
  everyone whose work it takes in instead of reconstructing attribution later.
- Modrinth-shaped version model for packs: plain `base.counter` version
  numbers, a stored release/beta/alpha channel chosen at build time
  (default beta), and a versions listing speaking Modrinth field names
  (`version_number`, `version_type`, `date_published`, fingerprint, counts).
- Structured build diff for update dialogs:
  `GET /v1/packs/{id}/diff?from=&to=` -- loader/minecraft/java bumps, mods
  added/removed/updated/toggled with registry-enriched version labels.
- Hash-first artifact lookup `GET /v1/files/{sha1}`; mod pages resolve by
  slug and expose the project environment flags.
- Job snapshots: build job ids survive service restarts; a job killed by a
  restart reads failed with an explicit interrupted line.
- Full OpenAPI coverage of the public surface at `/docs`, and a real
  documentation set under `docs/` (architecture, concepts, API guide,
  operations, development).
- Side/required/presence model: per-jar classification (Modrinth env flags
  first, bytecode second) drives derived required-ness with a hard
  invariant -- a client-side mod is never force-installed. Presence classes
  ride the manifest display block.
- Dependency auto-fill on config save: missing hard dependencies pull in
  from Modrinth or the mirror cache; resolved requires graphs feed the
  launcher's dependency tree.
- Modern jar metadata extraction: displayName, version (including
  `${file.jarVersion}` resolution), logoFile and target MC from
  mods.toml / neoforge.mods.toml / fabric.mod.json; NeoForge jars register
  under the `neoforge` loader; jar-embedded icons serve for modern mods.

### Changed

- The version the mirror reports means something now. `0.1.0` had not moved
  since the first commit and never would -- it recorded only that somebody had
  typed a number once -- so the panel footer, `/v1/health` and `smrt-pack
  --version` all agreed on a fact about nothing. The version is taken from git
  at build time instead: the year of the commit and its height, `2026.389`. It
  rises with every commit, so an operator can see at a glance how far behind
  `main` a running binary is, and the height names exactly one commit. A build
  with no history behind it -- a source archive, a shallow clone -- says
  `unknown` rather than a small number that would pass for a real version.

- The repository is `smrt-mirror`. The crate metadata and the systemd unit's
  documentation link follow it; GitHub redirects the old address, so a clone
  already on disk keeps working.

- A block says why. The reason a pack's keepers write when they stop somebody
  writing was a note to themselves, and the person it named got a bare refusal:
  the panel could only show a generic failure, which is a door that shuts with
  no word through it. It is served to them now -- in the refusal itself, and
  before that in what the panel asks about its own standing, so the reply box is
  replaced by a line saying writing here is suspended and what for, instead of
  offering a box that cannot work. It is kept to one line and 300 characters --
  it rides in every refusal that account gets and sits in a row of a list, and
  the long version of an argument belongs on the thread.

- Reading a discussion has one home. The list, a thread and a proposal's diff
  each had two implementations -- one public, one behind the session gate --
  answering the same question with two slightly different rules: the
  authenticated copy refused an unlisted pack's discussion that the public one
  served. The authenticated copies are gone, and the panel reads what a stranger
  reads.
- A view reflows against its own column, not against the window. Every
  responsive rule in the panel asked the viewport, which is the same question
  only while a view is the whole screen -- so a narrow window and a narrow
  column disagreed about identical content, and a view could never be one pane
  beside another. The content area is a named container now and the view-level
  rules ask it; the shell's own layout (the rail becoming a strip, then a
  drawer) stays window-keyed, because that is a fact about the screen. Measured:
  at a 560px column inside a 1400px window the editor's basics grid collapses to
  one column and the mod row becomes a card, which a viewport rule cannot see.
  The report dock follows the same box -- a container is the containing block
  for the fixed panels inside it, so the dock now belongs to its view instead of
  floating over the rail, and its drag bounds measure the column.

### Changed

- The mirror stands alone as a self-hostable product: deployment-specific
  values (operator uid, public base URL) moved to the environment; the
  SmartyCraft/Nexira setup is the reference deployment, not the definition.

### Added

- Opening travels. A mod's builds unroll under the mod and roll back up, so the
  rows below are pushed rather than covered and the list stays one surface; the
  pack editor rises the last few pixels into place instead of being present.
  Both take their duration from the stylesheet's own tokens, read at the moment
  the transition starts -- so the one `prefers-reduced-motion` rule that zeroes
  those tokens disarms these too, and the product keeps a single motion switch
  rather than growing a second one in JavaScript. The easing is read from the
  same place: `--ease-out` is parsed into a curve, so a sliding panel and a
  counting digit trace the same deceleration.
  Measured rather than assumed, since the previous attempt at this concluded the
  opposite from a bad probe: `element.animate` never writes inline styles, so
  reading `element.style` shows a working transition as a dead one. Sampling the
  running animations and the computed style shows the builds' height climbing
  0 -> 41 -> 71 -> 84 -> 98px across a 150ms declared duration, and the editor's
  opacity climbing over 240ms; under `prefers-reduced-motion` both report zero
  animations and arrive already finished on the first frame. The preview's two
  collapsed lists -- libraries and config files -- open the same way.

### Fixed

- A version window is enforced, not noted -- and the comparison behind it now
  reads the versions mods actually carry. A pack shipped Sodium `0.6.13` while a
  mod in it demanded `[0.8.12,)`; the game died before the main menu and blamed
  an unrelated mod, because the loader had already given up and the next thing
  to touch a missing class was the one that got named.
  The mirror had the finding and said nothing, twice over. Both versions carry
  the game version as semver build metadata -- `0.6.13+mc1.21.1` -- which the
  specification says to ignore when comparing and which the comparator refused
  to read, so the check landed in a silent "could not be checked" counter. And
  even read, it was recorded rather than enforced, on the reasoning that such
  windows are written optimistically. They are not: a loader reads the range out
  of the jar's own manifest and refuses to start.
  Build metadata is dropped as the specification requires, and a hard dependency
  present outside its declared window now stops a publish. A Modrinth file label
  is still not a version: reading `mc1.21.1-0.6.13-neoforge` would be the guess
  this comparison exists to refuse.

### Added

- The version fields stop being free text. Java is a list; the Minecraft version
  and the loader build are offered from lists the mirror holds, and still accept
  anything typed. A typo used to travel into the manifest and announce itself as
  a launcher that would not start, a long way from where it was typed.
  What is offered is never a cage: a build published an hour ago, or a private
  one, has to remain typeable, so an unknown value is said rather than refused --
  and when the list itself is out of date the note says that instead of blaming
  the value. Forge is fetched whole rather than from its promotions file, because
  the pack this mirror runs is pinned to a build that is neither latest nor
  recommended; the promotions markers are applied on top, since *recommended* is
  a fact a typed field cannot carry.
  Java asks the loader before the Minecraft version: lwjgl3ify and Cleanroom
  exist to run an old Minecraft on a new Java, and this mirror's own 1.7.10 pack
  runs Java 21 through the first of them.
  The lists are held rather than proxied, so opening the editor does not depend
  on four external services being up, and an outage narrows the offer to what was
  last known rather than emptying it.

- A pack's handshake claim is written from what its server says, not typed. A
  1.12.2 server refuses a client whose mod list is not the one it expects, and
  the file that answers that went stale in silence -- the server bumped its list,
  the file kept claiming the old one, and the failure arrived as a rejected
  handshake explaining nothing. The mirror asks the server, writes the claim, and
  can say afterwards what moved between the two.
  The claim is not checked against reality, deliberately: a server here demands a
  library version its author never published, which is why a patched jar was
  being shipped to satisfy it. Copying the answer verbatim is what lets the
  genuine build ship instead.

- Two people can edit one pack at the same time. Presence (#113) and the
  revision check (#52) let them see each other and stopped them overwriting each
  other, but the unit of conflict was still the whole pack: one adding mods and
  one writing the card copy collided anyway, and the loser reapplied by hand.
  The config is now a document the mirror merges, and the unit of a change is
  the shape of the thing changed. A paragraph merges a character at a time,
  because two people writing in one sentence have no correct winner. A mod row
  merges as a row -- rows added by either side both land, and a neighbour's
  filename appearing one letter at a time would be noise rather than
  collaboration. A scalar has nothing to merge inside it, so the last write wins
  and everyone converges on the same answer.
  `config.json` is unchanged and still written on the same path a save always
  used, so the build, the publish check and the CLI go on reading a plain file.
  The merged document is written once the typing stops rather than per
  keystroke, and the dependency fill -- which reaches Modrinth -- runs only when
  the declared set actually moved. A whole-config save or a revert settles any
  live document, since it would otherwise put back the version it still
  remembered.
  Server-owned fields (owner, tier, visibility, fork_of) are not in the document
  at all, so a client cannot propose a change to one even by accident.
  In the panel, edits go out as they are typed and a colleague's arrive the same
  way, through the room already open for presence. A save event stops meaning
  "re-read the pack" and starts meaning "the mirror wrote what you already
  have", so nothing replaces the screen mid-sentence. The conditional save
  remains for what it is actually for -- creating a pack, where there is no
  document to join yet.

### Removed

- Mod roles. A role grouped interchangeable mods into one selectable slot in a
  launcher -- "Minimap: VoxelMap" with Xaero behind a dropdown -- and the only
  way to set one was a hand-written TOML table plus a CLI pass, on a mirror
  whose point is that authoring happens in the panel. That is more curation
  than the grouping was worth, and it is a vocabulary that has to be kept in
  step with the launcher's own strings to render at all. What actually protects
  an install is `display.incompatible_with` and the registry's conflict edges:
  enabling one of a conflicting pair still switches the other off, and the
  pre-publish check still reports live conflicts. `display.role` is gone from
  the wire, `RoleTable` / `apply_role_table` / `smrt-pack apply-role-table` and
  the example table with them. A manifest or config still carrying the key
  parses -- an unknown field is ignored -- and a rebuild stops emitting it, so
  nothing needs migrating. `display.category` remains for plain labelling.

### Added

- A build is checked before it publishes. Until now a real build wrote the
  manifest, moved the `latest` pointer and rewrote the summary card with nothing
  having asked whether the result held together -- and the launcher reads that
  pointer the moment it moves, so the first thing to check a broken pack was a
  player's crash log, while the mirror already knew. The pack is resolved
  against the registry graph first, and two findings stop the publish because
  they mean it cannot start: a declared hard dependency nothing satisfies, and
  an artifact built for a loader the pack does not run with nothing present to
  bridge it. A dependency the mirror inferred from bytecode is recorded instead
  of enforced -- that reading cannot tell a dependency from an optional
  integration, and refusing a publish on a guess is how a gate stops being
  believed. Everything else is recorded rather than enforced -- an active
  conflict may be deliberate, a version outside a declared window usually runs,
  an unidentified jar means the check was partial -- because a gate that blocks
  on all of it is one operators route around. What was found rides on the built
  manifest, which is the artifact of a build; a job log lives in memory and its
  snapshot outlives nothing. A curator who knows better than the graph can
  publish anyway, and it is never quiet when they do: the job log says it, the
  audit trail records who asked, and the manifest carries what it went out over.
  The preview runs the same check and reports the same verdict without being
  stopped by it, since it publishes nothing. `smrt-pack build` gates the same
  way (`--force` to override) -- it writes into the same tree, so it cannot be
  the way around.

- Switching the theme or the language no longer happens in one frame. Both
  change more at once than anything else in the panel -- every token, or every
  word on screen -- and both were instant, which reads as a flicker with nothing
  for the eye to hold onto. They are crossfaded now through a view transition:
  one snapshot before, one after, at no per-element cost, where a CSS transition
  on colour and background would have animated the whole page on every repaint.
  The two do not read the same, because they are not the same act -- a substrate
  swap keeps every shape in place and only moves colour, so it is quick, while a
  rewrite of the text replaces the content and is drawn in over about twice as
  long. Both are absent under `prefers-reduced-motion`: the helper that starts
  them asks the same question the duration tokens answer, so the product keeps
  one motion switch rather than growing a second. Neither pretends to be
  loading -- nothing is; a placeholder saying otherwise would buy a moment of
  calm with the credibility of every real loading indicator.

### Added

- A pack says who is in it and what is happening to it, live
  (`GET /v1/authoring/packs/{id}/events`, server-sent). The revision check
  refuses a save that would overwrite someone else's, which stops the loss and
  says nothing until the collision -- you learned another person was there by
  colliding with them. The editor now subscribes while it is open: it lists who
  else has the pack, and a save by any of them arrives immediately. With nothing
  of your own in flight the editor takes their version rather than showing a
  stale screen; with unsaved edits it says who moved and leaves the decision
  where it already lives, in the conflict resolution. Subscribing is the
  presence, so a closed tab or a dropped connection is a departure without
  anything having to say goodbye, and one person in two tabs is one person.

### Added

- The mirror can ask a server what it runs
  (`GET /v1/servers/{id}/advertised`). The handshake spoof a pack ships has to
  claim exactly the mod list the server expects, and until now that list was
  pasted in by hand and went stale in silence after a server bump. The server
  states the list itself -- on 1.12.2 Forge the FML handshake's mod list also
  rides in the status ping, and newer Forge carries an equivalent under
  `forgeData` -- so asking is a status query, no account and no login, and it can
  be repeated whenever the answer matters. A server that will not advertise is
  reported as exactly that rather than as a server with no mods: a spoof built
  from silence would be a guess wearing the shape of an answer. `ServerEntry`
  gained the address this needs, which the mirror did not hold at all -- it
  modelled servers without recording where any of them was.

### Added

- One picker for adding a mod, on that search. "From the mirror" and "From
  Modrinth" were a decision about provenance taken before the decision about
  which mod, and it cannot be made correctly without already knowing whether the
  mirror carries the thing you have not found. One entry point now: a row says
  whether the mirror holds the bytes, and picking a build offers the cache
  source when it does and the Modrinth pin when it does not. What does not fit
  the pack's loader is ranked down and labelled rather than hidden -- a mod
  riding a bridge the pack ships reads differently from one that would need the
  bridge added. Copying a whole build's mod set and picking a raw jar by hash
  are different questions and keep their own picker.

### Added

- One search over both places a mod can come from
  (`GET /v1/search/mods?q=&mc=&loader=&pack=`). Adding a mod started with a
  question nobody can answer yet -- from the mirror, or from Modrinth? -- since
  choosing the door correctly means already knowing whether the mirror carries
  the thing you have not found. Both are searched and merged: a project the
  mirror has harvested is one row carrying what it knows exactly, filled in with
  Modrinth's description and icon, rather than two rows of differing confidence.
  Each hit says whether the mirror holds the bytes.
  The pack's loader ranks rather than filters, because the registry models four
  different answers and flattening them would either hide working mods or
  promise ones that cannot load: native (including a loader the pack's inherits
  from, and loader-agnostic jars), carried by a bridge the pack already ships,
  carried by a bridge it would have to add, and foreign. Foreign is last rather
  than absent -- it was searched for, and "this will not load here" is an answer.
  A Modrinth outage narrows the results to what the mirror knows instead of
  failing the search.

### Added

- A settings surface, and a light theme to put in it. The panel had nowhere for
  a preference to live -- the locale switch sat in the top bar because there was
  no other place -- and the theme could not exist at all: the tokens were
  dark-first with white and black tints written literally at their use sites, so
  on a light background the seams, the dot field, the table zebra and the scrims
  inverted. Those are values now, and the light half is measured rather than
  guessed: every text tier clears its floor against the field it sits on (the
  binding case on paper, not the card), and the four status hues are re-solved
  because the dark greens and ambers land near 1.6:1 there. Elevation swaps with
  the substrate -- a shadow on paper, a lighter surface on black -- which is what
  the token file already said and could not do. The paper is deliberately not
  office-white: the first pass took the surfaces to 0.97 luminance and glared,
  since on an emissive screen the brightest thing in the room should not be the
  empty half of a form. Follow-the-system is the default
  and keeps following as the desktop changes; the choice is applied before first
  paint, so a light session never flashes black. The launcher preview keeps its
  own dark, since it renders another product.
- A pack still builds while Modrinth is down. Every Modrinth-sourced mod
  resolved its hash and size live from Modrinth at build time, so a pack with
  one such mod could not be rebuilt until upstream came back -- on exactly the
  class of mod the mirror deliberately keeps no bytes for. Harvest had already
  recorded those numbers, and the build falls back to them: the network is asked
  first and stays authoritative, so a re-uploaded version is never built from
  stale numbers, but a version the registry knows no longer takes the build down
  with it. A pin the mirror has never harvested still fails, naming both the
  upstream failure and the missing local record. The build log says when it
  answered from the registry -- a build that reached upstream and one that did
  not are different events.
- Adding a mod says what comes with it. The dependencies used to arrive later,
  on save, so whoever added a mod found out afterwards -- by opening the preview
  or resolving by hand -- and a pack quietly gained jars nobody chose to look
  at. The plan was always computed; it is now asked for at the moment of the add
  (`POST /v1/authoring/packs/{id}/dependency-preview`, read-only: it runs the
  real fill on a copy so the answer cannot drift from what the save does, and
  writes nothing) and reported as one line naming what is coming. Silent when a
  mod brings nothing, which is most of them. The rows themselves now appear as
  soon as the save returns, too: the mirror always answered with the config it
  stored, dependencies included, and the editor was discarding that answer.

### Added

- The panel's state lives in the URL. Sections are paths, a mod page is a
  shareable link, and the server serves the app shell for any path it does
  not claim -- so back and forward (and the mouse buttons wired to them) work,
  a reload keeps the mod page you had open, and a link to what you are
  looking at can be sent to someone. Navigation was a variable and a
  localStorage key before, which the browser knew nothing about.
- A motion system, where the panel had fifteen hand-picked CSS durations and
  nothing else: three duration tokens and two easings, short and linear-out to
  match a flat interface, with one `prefers-reduced-motion` rule that disarms
  the whole product by zeroing them. Requests in flight show as a hairline
  wire under the top bar -- one place for "work is happening" instead of a
  spinner per view; long lists reveal in sequence rather than as a block; the
  dock arrives as a panel being placed; controls take a press; the overview
  counts its numbers up once.

### Fixed

- A failed build reports every broken source at once, and says why each broke.
  Resolution stopped at the first source it could not resolve, so a config with
  three dead pins cost one build per pin to discover: you fixed one, rebuilt,
  and met the next. Every source is tried now and the failure lists all of them,
  with how much of the pack it got through. Each entry carries its whole cause
  chain rather than only the outermost line -- the outer line names the mod, and
  the cause underneath is the half that says whether upstream was unreachable,
  the version ships no jar, or the pin is gone. The job log's other steps carry
  their chains too, for the same reason.

### Fixed

- The registry view opens more than one mod at a time, shows an icon on every
  build, and stops leaving a band of empty space when a mod is expanded.
  Comparing two mods' builds is the reason to open them at all, and a single
  open slot made that a matter of remembering what the other one said. A build
  the mirror has not cached had no icon candidate at all -- its own jar is not
  here to read one from -- so it fell to a letter; it now borrows the mod's,
  which is what the row above it already shows. The operator actions (rename,
  merge, the id) sat in a right-aligned band above the releases, so the left of
  that band read as an unexplained indent; they are on the mod's own row now,
  where the mod they act on is.

### Fixed

- A bad value is reported at the field, while it is being typed. Around ten
  error signals existed across every view, so a wrong value surfaced as a notice
  after saving: you learned it was wrong after trying to use it. Every rule the
  panel now checks mirrors one the server actually enforces and names it in the
  source -- a stricter client rule would reject values the mirror accepts, a
  looser one would promise a save that fails. A labelled field says the sentence
  under the control and points the control at it, so it reaches a screen reader
  and not only the eye; a row too dense for a caption marks the control and puts
  the sentence in its title. The server editor's submit is refused for exactly
  what the mirror would refuse, so a disabled button always has a field
  explaining itself.

### Fixed

- The panel and every curated pack asset are served again. The axum 0.8 route
  migration wrote the two wildcard routes with escaped braces (`{{*path}}`),
  which matches a literal path rather than any real request: every panel asset
  was answered with the app shell, so the panel loaded as a blank page, and
  every `/v1/packs/{id}/static/...` file -- pack icons, banners, and any
  `smrt_static` source a manifest points at -- answered 404. Both routes are
  pinned by tests that assert the request reaches its handler.
- Navigation is links. Every place in the panel had an address -- sections are
  paths, a mod page and now a pack editor are their own URLs -- but nothing
  wore them: eighteen navigations were click handlers on buttons and rows
  against two `<a href>` in the whole product. So the middle click did nothing,
  ctrl-click did nothing, "copy link" copied the page you happened to load
  first, and a screen reader announced "button" for a destination. The rail,
  pack rows, mod names and the graph's open-page control are links now, with
  the handler intercepting a plain click for client-side routing and leaving a
  modified one to the browser.
- Form fields say what they are. The visible caption a `Field` draws was a
  plain span with nothing tying it to the control, so a screen reader on the
  input heard silence -- and where an `aria-label` existed it repeated the
  placeholder, naming the example ("main", "https://...") instead of the field
  and overriding the caption that was right there. The caption is now named and
  the control points at it, in the one component every labelled field goes
  through; the example-echoing labels are gone, and controls too dense for a
  caption (mod rows, the asset table, the drop zones) carry a real field name
  instead. Measured on a running panel: the pack editor and the server editor
  have no unnamed control and none named by its own example.
- The pack editor can be left with the back button. It opened from local state
  while the URL stayed on the section, so nothing about opening it entered
  history: back walked out of the editor's own view or out of the panel
  entirely, and a reload lost the pack you had open. The open editor is a
  location now (`/packs/<id>`, `/mypacks/<id>`), so back and the trackpad
  gesture close it and a link reopens it. The unsaved-changes guard moved with
  it: it lives on the route rather than on the Close button, so every way out
  asks -- declining walks back to the editor rather than stacking a history
  step.
- Two accounts editing one pack no longer lose each other's work. The config
  read answers with an `ETag` of its authored content and a save may carry it
  as `If-Match`; a save whose base has moved on is refused with 409 instead of
  overwriting whoever saved first, and the pack's whole load-merge-write runs
  under a per-pack lock so two saves cannot interleave either. The editor keeps
  the refused edits on screen, stops autosaving, and asks which version wins --
  take the stored one, or save over it, the latter rebasing onto the current
  revision so it stays a conditional write. Server-side changes a client cannot
  cause -- publishing a pack, a pulled dependency appearing -- are outside the
  revision, so they never reject an edit in flight, and a request that sends no
  `If-Match` (the CLI, a script) writes unconditionally as before.
- The curator slug is offered where it does something. It is load-bearing for
  a self-hosted mod, whose filename changes under it and which has no project
  id -- so a Modrinth row now states what actually keys it instead of showing
  an empty field that changes nothing, and the source column fits the word
  `modrinth` rather than spending the same width on an ellipsis. The three
  identities a mod has (file, registry, across-builds) are written down in
  `docs/concepts.md`.
- The activity counter no longer turns a one-shot fetch into a request loop.
  Counting in-flight requests through reactive state meant any request started
  inside an effect made that effect depend on its own side effect: the shell's
  single health fetch became an unbounded loop that starved every other
  request on the page, so views rendered empty. Only the derived flag is
  reactive now.
- A failure notice shows the server's sentence, not its envelope: four lines
  of JSON where the actual problem was one line inside it.
- The report dock opens below the view's header instead of on top of the
  controls that opened it.
- Panel-wide design pass. The type scale had eight sizes inside a four-pixel
  band, bottoming out at 9px on the public catalog; it is six steps now, and
  nothing in the product is smaller than an 11px mono label. Every control
  has a 28px minimum target, where the compact variants sat at 24-26px with
  several of them packed side by side in a row. Thirty-seven inputs whose
  only name was a placeholder -- which disappears exactly when you type --
  carry an accessible name.
- Failures no longer move the page anywhere, not just in the pack editor.
  Fourteen views inserted an error banner at the top of their content; they
  all report through the notice stack now. A dialog keeps its own inline
  error: it is already an overlay, and the failure belongs to the thing in
  front of you.
- Palette defects the tokens themselves documented: faint text measured
  3.86:1 on the panel surface it actually sits on (the note claimed 4:1,
  which held only against pure black) and is now 4.51:1; the four soft state
  tints shared one 0.14 alpha and landed at 1.13-1.21:1 -- invisible as fills,
  which is why state read off borders alone -- and now carry per-hue alphas
  solved to equal perceived weight; the retired-for-contrast `--accent-dim`
  and the single-use second red are gone.
- The editor stops moving under the cursor. Reports and failures were
  inserted at the top of the form, so asking for a resolve or hitting an
  error pushed everything down by however tall the answer was. Reports now
  open in a draggable dock that overlays the page and remembers where it was
  parked; failures are notices in a fixed corner stack, with the rejected
  save carrying its reason and a retry. Nothing in the flow reflows.
- The top-bar refresh works on every view. It bumped a shared signal only one
  view listened to, so on the registry, graph, my-packs and public catalog it
  was a button that did nothing -- while the graph kept a second refresh of
  its own beside it. Every view now listens, the duplicate is gone, and the
  button is offered to any signed-in user rather than operators alone.
- The pack editor no longer loses edits quietly. A rejected autosave was a
  grey word in the header with the reason only in a tooltip, and it never
  retried, so a failed save plus a closed tab meant the work was gone. It is
  now a banner with the server's reason, a retry, and a confirmation before
  leaving. Emptying the Java field no longer sends a null into a required
  field, and switching a mod's source type keeps the reference it had, so a
  stray click on the dropdown is recoverable.
- Uniqueness holds on every add path, not just the pickers: dropped jars go
  through the same identity check, and assets are unique by `dest` in the
  editor and on save -- two rows writing one path installed whichever the
  launcher fetched last.
- A loader that ships a Forge mod's capability natively is understood as
  answering that dependency. Cleanroom loads mixins itself, so MixinBooter
  (the Forge backport) is redundant on a Cleanroom pack -- but removing it
  used to leave Entity Culling and Relictium with an unsatisfied mixinbooter
  dependency the resolver flagged as missing. A `loader_provides` seed, keyed
  to the exact loader, now records what the loader covers; the dependency
  resolves clean and auto-fill does not pull the mod back. One row per
  capability, no code change to add another.
- A connector's `loader:<name>` capability is now shipped as data and emitted
  by the harvest, so a Fabric mod carried by Sinytra Connector reads as
  carried instead of "will not load". The resolver understood bridges
  already; nothing ever produced the fact, so on any fresh mirror every
  bridged mod was a false alarm. Add a niche connector with one row in
  `loader_bridge` -- no code change.
- Dependency auto-fill no longer waits for a build: a Modrinth pin the
  harvest has not read yet contributes its dependencies straight from the
  version it declares, so a mod just added to a config -- or re-pinned to a
  newer build -- pulls its libraries immediately instead of after the pack
  has been built and harvested once. A dependency that names an exact
  version is pulled at that version.
- A Modrinth version upstream published without a jar is no longer
  selectable: the picker greys it out, auto-fill skips it, and the build
  error says what happened.
- One row per mod: configs declaring the same artifact twice, or two rows
  writing the same `mods/<filename>`, are refused on save, and the pickers
  no longer offer what the pack already ships. Artifact identity ignores the
  pinned version, so a second version of a mod already in the pack counts as
  a duplicate rather than a new entry.
- Derived state no longer depends on upstream weather: pulled dependencies
  are sticky across saves and outages, one unresolvable target does not
  abort the fill pass, a degraded Modrinth leg does not wipe harvested
  relations, and builds wait for an in-flight harvest before classifying.
- Modrinth client resilience: hard per-request deadlines and an unfiltered
  fallback for the filtered version listing.
