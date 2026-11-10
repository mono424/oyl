-- ##################################################################
-- STUDYCALL SCHEMA
--
-- Source of truth for the database. `spky generate` turns it into the typed
-- client schema (apps/web/src/schema.gen.ts). Tables the client never syncs
-- (search chunks with their embeddings, extracted images, the parts of each
-- uploaded PDF) live in apps/api/src/server-schema.surql, and their bytes are
-- files in the `materials` bucket (schema/buckets/materials.surql).
-- ##################################################################

-- ##################################################################
-- ACCESS
--
-- `IF NOT EXISTS`, not OVERWRITE: `spky deploy` applies this whole schema on
-- every deploy, and redefining a record access without `WITH JWT` mints a new
-- signing key, which signs every user out. To change an access on purpose,
-- redefine it with OVERWRITE in a migration (that signs everyone out once).
-- ##################################################################

-- Email + password accounts for the web app.
DEFINE ACCESS IF NOT EXISTS account ON DATABASE TYPE RECORD
  SIGNUP {
    RETURN CREATE ONLY user SET
      email    = string::lowercase($email),
      name     = $name,
      password = crypto::argon2::generate($password);
  }
  SIGNIN ( SELECT * FROM user WHERE email = string::lowercase($email) AND crypto::argon2::compare(password, $password) )
  -- The API Worker checks this token on every request (it asks SurrealDB for
  -- `$auth.id`), and the client has no refresh flow, so the token lives as
  -- long as the session.
  DURATION FOR TOKEN 30d, FOR SESSION 30d
;

-- AI clients (the MCP server) sign in with a personal API key the user created
-- in Settings. The session they get IS the key's owner, so every permission
-- below applies to them unchanged. Only the key's SHA-256 is stored.
DEFINE ACCESS IF NOT EXISTS agent ON DATABASE TYPE RECORD
  SIGNIN (
    SELECT * FROM user WHERE id = (
      SELECT VALUE owner FROM api_key WHERE key_hash = crypto::sha256($key) AND revoked_at = NONE LIMIT 1
    )[0]
  )
  DURATION FOR TOKEN 1h, FOR SESSION 1h
;

-- ##################################################################
-- USER
-- ##################################################################

DEFINE TABLE OVERWRITE user SCHEMAFULL
  PERMISSIONS
    FOR select, update WHERE id = $auth.id
    FOR create, delete WHERE false;

DEFINE FIELD OVERWRITE email ON TABLE user TYPE string
  ASSERT string::contains($value, "@")
  PERMISSIONS FOR update WHERE false;
DEFINE INDEX OVERWRITE unique_email ON TABLE user FIELDS email UNIQUE;

DEFINE FIELD OVERWRITE name ON TABLE user TYPE string
  ASSERT string::len($value) > 0;

DEFINE FIELD OVERWRITE password ON TABLE user TYPE string
  PERMISSIONS FOR select, update WHERE false;

DEFINE FIELD OVERWRITE created_at ON TABLE user TYPE datetime DEFAULT time::now() READONLY;

-- ##################################################################
-- COURSES
-- ##################################################################

DEFINE TABLE OVERWRITE course SCHEMAFULL
  PERMISSIONS
    FOR select, update, delete WHERE owner = $auth.id
    FOR create WHERE owner = $auth.id;

DEFINE FIELD OVERWRITE owner ON TABLE course TYPE record<user> READONLY; -- @parent
DEFINE FIELD OVERWRITE title ON TABLE course TYPE string ASSERT string::len($value) > 0;
DEFINE FIELD OVERWRITE description ON TABLE course TYPE option<string>;
-- Divider-tab color of the course in the sidebar (one of the UI's palette keys).
DEFINE FIELD OVERWRITE color ON TABLE course TYPE string DEFAULT 'blue';
-- Language the tutor speaks in calls of this course (ISO 639-1, e.g. "en",
-- "de"). Lecture material may be in any language.
DEFINE FIELD OVERWRITE language ON TABLE course TYPE string DEFAULT 'en'
  ASSERT string::len($value) = 2;
-- Extra instructions for the tutor in every call of this course ("the exam is
-- oral", "focus on proofs", ...).
DEFINE FIELD OVERWRITE tutor_notes ON TABLE course TYPE option<string>;
DEFINE FIELD OVERWRITE created_at ON TABLE course TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE updated_at ON TABLE course TYPE datetime VALUE time::now();

DEFINE INDEX OVERWRITE course_owner ON TABLE course FIELDS owner;

-- Deleting a course deletes everything in it. Events run without the caller's
-- permissions, so the server-only tables are reached too.
DEFINE EVENT OVERWRITE course_deleted ON TABLE course WHEN $event = "DELETE" THEN {
  DELETE material WHERE course = $before.id;
  DELETE call WHERE course = $before.id;
  DELETE fetcher WHERE course = $before.id;
  DELETE homework WHERE course = $before.id;
  DELETE document WHERE course = $before.id;
  DELETE site WHERE course = $before.id;
};

-- ##################################################################
-- MATERIALS
-- ##################################################################

DEFINE TABLE OVERWRITE material SCHEMAFULL
  PERMISSIONS
    FOR select, update, delete WHERE owner = $auth.id
    -- The course must be the user's own too: the API searches a course's
    -- material for the tutor, and nobody may plant text in someone else's.
    FOR create WHERE owner = $auth.id AND course.owner = $auth.id;

DEFINE FIELD OVERWRITE owner ON TABLE material TYPE record<user> READONLY;
DEFINE FIELD OVERWRITE course ON TABLE material TYPE record<course> READONLY; -- @parent
DEFINE FIELD OVERWRITE title ON TABLE material TYPE string ASSERT string::len($value) > 0;
-- Where the content came from: typed/pasted text, a markdown file, a PDF
-- that the API converted to markdown + images, or LaTeX source (`tex`, kept
-- as written and indexed through its Markdown, shared/latex.ts).
DEFINE FIELD OVERWRITE kind ON TABLE material TYPE string
  ASSERT $value IN ['text', 'markdown', 'pdf', 'tex']
  PERMISSIONS FOR update WHERE false;
-- The lecture itself, as markdown (LaTeX for `tex`). Images extracted from a PDF are referenced
-- as `![caption](asset:<id>)`. Can be long, so the sync engine only carries
-- the record and the client fetches the body directly (@opaque).
-- @opaque
DEFINE FIELD OVERWRITE content ON TABLE material TYPE string DEFAULT '';
-- Lifecycle of the server-side processing (PDF conversion, chunking,
-- embedding). `ready` means the tutor can search it.
-- Fields from here to `index_rev` are written by the API only (clients may
-- set them when creating the row, never change them).
DEFINE FIELD OVERWRITE status ON TABLE material TYPE string DEFAULT 'pending'
  ASSERT $value IN ['pending', 'processing', 'ready', 'failed']
  PERMISSIONS FOR update WHERE false;
-- Human-readable progress line while processing ("Converting pages 11-20").
DEFINE FIELD OVERWRITE status_detail ON TABLE material TYPE option<string>
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE error ON TABLE material TYPE option<string>
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE source_name ON TABLE material TYPE option<string>
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE source_size ON TABLE material TYPE option<int>
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE page_count ON TABLE material TYPE option<int>
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE chunk_count ON TABLE material TYPE int DEFAULT 0
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE image_count ON TABLE material TYPE int DEFAULT 0
  PERMISSIONS FOR update WHERE false;
-- Bumped by the API on every (re)index, so a stale run can tell it lost.
DEFINE FIELD OVERWRITE index_rev ON TABLE material TYPE int DEFAULT 0
  PERMISSIONS FOR update WHERE false;
-- Last section the student has reached, using the search index's zero-based
-- positions. Uploading material never marks it as reached or understood.
DEFINE FIELD OVERWRITE learned_through ON TABLE material TYPE int DEFAULT -1
  ASSERT $value >= -1
  PERMISSIONS FOR update WHERE false;
-- Revision of the material's AgentMemory graph, written by the API.
DEFINE FIELD OVERWRITE memory_rev ON TABLE material TYPE int DEFAULT 0
  PERMISSIONS FOR update WHERE false;
-- The source that imported it (see SOURCES below) and where a person opens
-- the original. NONE for material the student added. A source updates its
-- material when the original changes.
DEFINE FIELD OVERWRITE fetcher ON TABLE material TYPE option<record<fetcher>>
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE source_url ON TABLE material TYPE option<string>
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE created_at ON TABLE material TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE updated_at ON TABLE material TYPE datetime VALUE time::now();

DEFINE INDEX OVERWRITE material_course ON TABLE material FIELDS course;

DEFINE EVENT OVERWRITE material_deleted ON TABLE material WHEN $event = "DELETE" THEN {
  UPDATE homework SET sheet = NONE WHERE sheet = $before.id;
  UPDATE paper SET material = NONE WHERE material = $before.id;
  UPDATE document SET source = NONE WHERE source = $before.id;
  DELETE chunk WHERE material = $before.id;
  DELETE asset WHERE material = $before.id;
  DELETE source_part WHERE material = $before.id;
};

-- ##################################################################
-- CALLS
-- ##################################################################

-- One voice session with the tutor. Created by the API when a call starts,
-- so the client only reads it.
DEFINE TABLE OVERWRITE call SCHEMAFULL
  PERMISSIONS
    FOR select, delete WHERE owner = $auth.id
    FOR create, update WHERE false;

DEFINE FIELD OVERWRITE owner ON TABLE call TYPE record<user>;
DEFINE FIELD OVERWRITE course ON TABLE call TYPE record<course>; -- @parent
-- The lecture the call is about. NONE = the whole course.
DEFINE FIELD OVERWRITE material ON TABLE call TYPE option<record<material>>;
-- explain: walk me through the lecture. quiz: ask me questions. qa: I ask.
DEFINE FIELD OVERWRITE mode ON TABLE call TYPE string
  ASSERT $value IN ['explain', 'quiz', 'qa'];
DEFINE FIELD OVERWRITE status ON TABLE call TYPE string DEFAULT 'starting'
  ASSERT $value IN ['starting', 'live', 'ended'];
DEFINE FIELD OVERWRITE started_at ON TABLE call TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE ended_at ON TABLE call TYPE option<datetime>;
-- Written after the call: what was covered and what to review.
DEFINE FIELD OVERWRITE recap ON TABLE call TYPE option<string>;
-- The assistant a study session took place in ("ChatGPT"), when the student
-- studied there and it logged the session through the MCP server. Such a
-- session has a recap and no transcript. NONE for a voice call with Bolo.
DEFINE FIELD OVERWRITE assistant ON TABLE call TYPE option<string>;
-- SHA-256 of the per-call key the voice agent presents to the API's LLM
-- endpoint. Never readable.
DEFINE FIELD OVERWRITE key_hash ON TABLE call TYPE string
  PERMISSIONS FOR select WHERE false;
DEFINE FIELD OVERWRITE conversation_id ON TABLE call TYPE option<string>;

DEFINE INDEX OVERWRITE call_course ON TABLE call FIELDS course;

DEFINE EVENT OVERWRITE call_deleted ON TABLE call WHEN $event = "DELETE" THEN {
  DELETE call_turn WHERE call = $before.id;
  DELETE turn_source WHERE call = $before.id;
};

-- The transcript, one row per spoken turn. Written by the API as the call
-- runs, so the client shows it live through sync.
DEFINE TABLE OVERWRITE call_turn SCHEMAFULL
  PERMISSIONS
    FOR select WHERE owner = $auth.id
    FOR create, update, delete WHERE false;

DEFINE FIELD OVERWRITE owner ON TABLE call_turn TYPE record<user>;
DEFINE FIELD OVERWRITE call ON TABLE call_turn TYPE record<call>; -- @parent
DEFINE FIELD OVERWRITE seq ON TABLE call_turn TYPE int;
DEFINE FIELD OVERWRITE role ON TABLE call_turn TYPE string ASSERT $value IN ['student', 'tutor'];
DEFINE FIELD OVERWRITE text ON TABLE call_turn TYPE string;
DEFINE FIELD OVERWRITE created_at ON TABLE call_turn TYPE datetime DEFAULT time::now();

DEFINE INDEX OVERWRITE call_turn_call ON TABLE call_turn FIELDS call, seq UNIQUE;

-- The lecture passages a tutor turn cited, one row per citation. Written by
-- the API with the turn, so the transcript shows them as links into the
-- material (a PDF opens at the page).
DEFINE TABLE OVERWRITE turn_source SCHEMAFULL
  PERMISSIONS
    FOR select WHERE owner = $auth.id
    FOR create, update, delete WHERE false;

DEFINE FIELD OVERWRITE owner ON TABLE turn_source TYPE record<user>;
DEFINE FIELD OVERWRITE call ON TABLE turn_source TYPE record<call>; -- @parent
-- The tutor turn (call_turn.seq) and the passage number the tutor cited.
DEFINE FIELD OVERWRITE seq ON TABLE turn_source TYPE int;
DEFINE FIELD OVERWRITE ref ON TABLE turn_source TYPE int;
DEFINE FIELD OVERWRITE material ON TABLE turn_source TYPE record<material>;
-- The material's title when cited, so the link reads well after a rename or
-- deletion.
DEFINE FIELD OVERWRITE title ON TABLE turn_source TYPE string;
DEFINE FIELD OVERWRITE page ON TABLE turn_source TYPE option<int>;
DEFINE FIELD OVERWRITE page_end ON TABLE turn_source TYPE option<int>;
DEFINE FIELD OVERWRITE heading ON TABLE turn_source TYPE option<string>;
-- Where in the turn's text the citation was made. A reply cut short by an
-- interruption keeps only the citations it got to.
DEFINE FIELD OVERWRITE at ON TABLE turn_source TYPE int;
DEFINE FIELD OVERWRITE created_at ON TABLE turn_source TYPE datetime DEFAULT time::now();

DEFINE INDEX OVERWRITE turn_source_call ON TABLE turn_source FIELDS call, seq;

-- ##################################################################
-- HOMEWORK
--
-- The student's answers to an exercise sheet (a material), written in the
-- app: one `homework` per sheet and one `answer` per part of it, in sheet
-- order. The app splits the sheet into parts (shared/homework.ts) and creates
-- both itself; an answer row per part keeps edits on two devices apart.
-- ##################################################################

DEFINE TABLE OVERWRITE homework SCHEMAFULL
  PERMISSIONS
    FOR select, update, delete WHERE owner = $auth.id
    FOR create WHERE owner = $auth.id AND course.owner = $auth.id;

DEFINE FIELD OVERWRITE owner ON TABLE homework TYPE record<user> READONLY;
DEFINE FIELD OVERWRITE course ON TABLE homework TYPE record<course> READONLY; -- @parent
-- The exercise sheet. NONE once the sheet is deleted: the answers stay.
DEFINE FIELD OVERWRITE sheet ON TABLE homework TYPE option<record<material>>;
DEFINE FIELD OVERWRITE title ON TABLE homework TYPE string ASSERT string::len($value) > 0;
DEFINE FIELD OVERWRITE created_at ON TABLE homework TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE updated_at ON TABLE homework TYPE datetime VALUE time::now();

DEFINE INDEX OVERWRITE homework_course ON TABLE homework FIELDS course;

DEFINE EVENT OVERWRITE homework_deleted ON TABLE homework WHEN $event = "DELETE" THEN {
  DELETE answer WHERE homework = $before.id;
};

-- One part of the sheet and the student's answer to it.
DEFINE TABLE OVERWRITE answer SCHEMAFULL
  PERMISSIONS
    FOR select, update, delete WHERE owner = $auth.id
    FOR create WHERE owner = $auth.id AND homework.owner = $auth.id;

DEFINE FIELD OVERWRITE owner ON TABLE answer TYPE record<user> READONLY;
DEFINE FIELD OVERWRITE homework ON TABLE answer TYPE record<homework> READONLY; -- @parent
-- Order on the sheet, and the part's stable key there ("1.b.ii").
DEFINE FIELD OVERWRITE position ON TABLE answer TYPE int;
DEFINE FIELD OVERWRITE key ON TABLE answer TYPE string;
DEFINE FIELD OVERWRITE question ON TABLE answer TYPE string;
DEFINE FIELD OVERWRITE question_title ON TABLE answer TYPE option<string>;
-- The margin label within the question: "b (ii)", "(c)", "" for a whole question.
DEFINE FIELD OVERWRITE label ON TABLE answer TYPE string;
DEFINE FIELD OVERWRITE points ON TABLE answer TYPE option<float>;
-- The exercise: its own text, and the introduction above it.
DEFINE FIELD OVERWRITE prompt ON TABLE answer TYPE string DEFAULT '';
DEFINE FIELD OVERWRITE context ON TABLE answer TYPE option<string>;
-- Where the exercise is on a PDF sheet: the page, the label its excerpt
-- starts at, and once found, the excerpt itself as JSON
-- ([{ page, top, bottom }], fractions of the page height).
DEFINE FIELD OVERWRITE page ON TABLE answer TYPE option<int>;
DEFINE FIELD OVERWRITE anchor ON TABLE answer TYPE option<string>;
DEFINE FIELD OVERWRITE anchor_page ON TABLE answer TYPE option<int>;
DEFINE FIELD OVERWRITE region ON TABLE answer TYPE option<string>;
-- The answer, as Markdown with $math$. Pasted images are `answer-image:<id>`.
DEFINE FIELD OVERWRITE content ON TABLE answer TYPE string;
DEFINE FIELD OVERWRITE created_at ON TABLE answer TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE updated_at ON TABLE answer TYPE datetime VALUE time::now();

DEFINE INDEX OVERWRITE answer_homework ON TABLE answer FIELDS homework, position;

-- ##################################################################
-- SITES
--
-- A static website the student uploaded to a course, audio and all. The files
-- are server-only (`site_file`, apps/api/src/server-schema.surql) and served
-- publicly by the site's unguessable id at /site-files/<key>/ (apps/api/src/sites.ts).
-- The app creates the row, uploads the files through the API, and the API
-- marks it ready.
-- ##################################################################

DEFINE TABLE OVERWRITE site SCHEMAFULL
  PERMISSIONS
    FOR select, update, delete WHERE owner = $auth.id
    FOR create WHERE owner = $auth.id AND course.owner = $auth.id;

DEFINE FIELD OVERWRITE owner ON TABLE site TYPE record<user> READONLY;
DEFINE FIELD OVERWRITE course ON TABLE site TYPE record<course> READONLY; -- @parent
DEFINE FIELD OVERWRITE title ON TABLE site TYPE string ASSERT string::len($value) > 0;
DEFINE FIELD OVERWRITE status ON TABLE site TYPE string DEFAULT 'uploading'
  ASSERT $value IN ['uploading', 'ready'];
-- The page it opens on, and every HTML page of the site as a JSON array of paths.
DEFINE FIELD OVERWRITE entry ON TABLE site TYPE option<string>;
DEFINE FIELD OVERWRITE pages ON TABLE site TYPE string DEFAULT '[]';
DEFINE FIELD OVERWRITE file_count ON TABLE site TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE size ON TABLE site TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE created_at ON TABLE site TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE updated_at ON TABLE site TYPE datetime VALUE time::now();

DEFINE INDEX OVERWRITE site_course ON TABLE site FIELDS course;

-- ##################################################################
-- DOCUMENTS
--
-- Free writing in a course, a research proposal or an essay: one text in
-- the app's editor, Markdown or LaTeX, with the papers it draws on beside
-- it. A `paper` is an entry of a document's bibliography: a course material
-- (its PDF opens next to the text) or a reference typed in by hand. Markdown
-- cites one as `[@key]` or `[@key, p. 4]`, several as `[@a; @b]`; LaTeX as
-- `\cite{key}` or `\cite[p.~4]{key}`. The key is the paper's cite_key, or
-- its record key. Export PDF formats them and lists the cited papers.
-- ##################################################################

DEFINE TABLE OVERWRITE document SCHEMAFULL
  PERMISSIONS
    FOR select, update, delete WHERE owner = $auth.id
    FOR create WHERE owner = $auth.id AND course.owner = $auth.id;

DEFINE FIELD OVERWRITE owner ON TABLE document TYPE record<user> READONLY;
DEFINE FIELD OVERWRITE course ON TABLE document TYPE record<course> READONLY; -- @parent
DEFINE FIELD OVERWRITE title ON TABLE document TYPE string ASSERT string::len($value) > 0;
-- The material the document was started from (a draft uploaded to the course), if any.
DEFINE FIELD OVERWRITE source ON TABLE document TYPE option<record<material>>;
-- What the text is written in: Markdown with $math$, citing `[@key]`, or
-- LaTeX source, citing `\cite{key}`. Pasted images are `answer-image:<id>`.
DEFINE FIELD OVERWRITE format ON TABLE document TYPE string DEFAULT 'markdown'
  ASSERT $value IN ['markdown', 'tex'];
DEFINE FIELD OVERWRITE content ON TABLE document TYPE string;
-- What a save from the editor was built on: the SHA-256 of the text it started
-- from. The `document_stale_save` event (apps/api/src/server-schema.surql) refuses
-- a save whose text has changed meanwhile.
DEFINE FIELD OVERWRITE base ON TABLE document TYPE option<string>;
-- How citations print: (Author, 2024) or [1].
DEFINE FIELD OVERWRITE cite_style ON TABLE document TYPE string DEFAULT 'author-year'
  ASSERT $value IN ['author-year', 'numeric'];
DEFINE FIELD OVERWRITE created_at ON TABLE document TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE updated_at ON TABLE document TYPE datetime VALUE time::now();

DEFINE INDEX OVERWRITE document_course ON TABLE document FIELDS course;
DEFINE INDEX OVERWRITE document_source ON TABLE document FIELDS source;

DEFINE EVENT OVERWRITE document_deleted ON TABLE document WHEN $event = "DELETE" THEN {
  DELETE paper WHERE document = $before.id;
  DELETE document_version WHERE document = $before.id;
  DELETE document_delta WHERE document = $before.id;
};

-- A paper a document draws on, in the order it was added.
DEFINE TABLE OVERWRITE paper SCHEMAFULL
  PERMISSIONS
    FOR select, update, delete WHERE owner = $auth.id
    FOR create WHERE owner = $auth.id AND document.owner = $auth.id;

DEFINE FIELD OVERWRITE owner ON TABLE paper TYPE record<user> READONLY;
DEFINE FIELD OVERWRITE document ON TABLE paper TYPE record<document> READONLY; -- @parent
-- The course material holding the paper. NONE for a reference typed in, or once the material is deleted.
DEFINE FIELD OVERWRITE material ON TABLE paper TYPE option<record<material>>;
DEFINE FIELD OVERWRITE title ON TABLE paper TYPE string ASSERT string::len($value) > 0;
-- The key the text cites it by, BibTeX style ("stark2019b"). Papers added
-- before keys existed have none and are cited by their record key.
DEFINE FIELD OVERWRITE cite_key ON TABLE paper TYPE option<string>;
-- As the bibliography writes them: "Vaswani, A.; Shazeer, N.", "2017", "NeurIPS".
DEFINE FIELD OVERWRITE authors ON TABLE paper TYPE option<string>;
DEFINE FIELD OVERWRITE year ON TABLE paper TYPE option<string>;
DEFINE FIELD OVERWRITE venue ON TABLE paper TYPE option<string>;
DEFINE FIELD OVERWRITE url ON TABLE paper TYPE option<string>;
DEFINE FIELD OVERWRITE created_at ON TABLE paper TYPE datetime DEFAULT time::now() READONLY;

DEFINE INDEX OVERWRITE paper_document ON TABLE paper FIELDS document;

-- ##################################################################
-- DOCUMENT VERSIONS
--
-- Every change to a document's text is kept, whoever writes it: the event
-- below runs inside the write, so the web app, the MCP tools and anything
-- else get versions without doing anything. What is stored is changesets
-- running backwards from the current text (`document.content`), with a full
-- copy every 50 versions so that rebuilding one stays cheap. Reading a version
-- and going back to one live in apps/api/src/documents.ts. Going back writes
-- the old text as a new version, so nothing is ever lost by it.
--
-- A hunk is `{ at, len, old }`: the `len` characters at `at` were `old` before
-- it. Offsets count code points, like string::len. Not value::diff and
-- value::patch: text holding "@@" (LaTeX internals do) makes value::patch fail
-- or silently return the wrong text, and an event cannot catch an error, so
-- that would also block saving.
-- ##################################################################

-- One row per version, for the list in the app. The text is not here: the sync
-- node keeps every row of a synced table in memory.
DEFINE TABLE OVERWRITE document_version SCHEMAFULL
  PERMISSIONS
    FOR select WHERE owner = $auth.id
    FOR create, update, delete WHERE false;

DEFINE FIELD OVERWRITE owner ON TABLE document_version TYPE record<user> READONLY;
DEFINE FIELD OVERWRITE document ON TABLE document_version TYPE record<document> READONLY; -- @parent
-- Counts up from 1 per document.
DEFINE FIELD OVERWRITE n ON TABLE document_version TYPE int READONLY;
-- Who wrote it: the student in the app, an AI client through the MCP server, or
-- `system`, the text a document already had when versions began.
DEFINE FIELD OVERWRITE author ON TABLE document_version TYPE string
  ASSERT $value IN ['student', 'assistant', 'system'];
DEFINE FIELD OVERWRITE format ON TABLE document_version TYPE string;
-- Characters changed against the version before it.
DEFINE FIELD OVERWRITE added ON TABLE document_version TYPE int;
DEFINE FIELD OVERWRITE removed ON TABLE document_version TYPE int;
-- The version this one went back to.
DEFINE FIELD OVERWRITE restored_from ON TABLE document_version TYPE option<int>;
DEFINE FIELD OVERWRITE created_at ON TABLE document_version TYPE datetime;
DEFINE FIELD OVERWRITE updated_at ON TABLE document_version TYPE datetime;

DEFINE INDEX OVERWRITE document_version_document ON TABLE document_version FIELDS document, n;

-- What it takes to rebuild a version: same record key as its document_version.
-- `hunks` turn its text into the text of the version before it, last hunk first.
-- `snapshot` is its whole text, on every 50th once a newer version exists.
-- `sha` is the SHA-256 of its text, checked after rebuilding.
-- @nosync
DEFINE TABLE OVERWRITE document_delta SCHEMAFULL
  PERMISSIONS
    FOR select WHERE owner = $auth.id
    FOR create, update, delete WHERE false;

DEFINE FIELD OVERWRITE owner ON TABLE document_delta TYPE record<user> READONLY;
DEFINE FIELD OVERWRITE document ON TABLE document_delta TYPE record<document> READONLY;
DEFINE FIELD OVERWRITE n ON TABLE document_delta TYPE int READONLY;
DEFINE FIELD OVERWRITE hunks ON TABLE document_delta TYPE array<object>;
DEFINE FIELD OVERWRITE hunks.*.at ON TABLE document_delta TYPE int;
DEFINE FIELD OVERWRITE hunks.*.len ON TABLE document_delta TYPE int;
DEFINE FIELD OVERWRITE hunks.*.old ON TABLE document_delta TYPE string;
DEFINE FIELD OVERWRITE sha ON TABLE document_delta TYPE string;
DEFINE FIELD OVERWRITE snapshot ON TABLE document_delta TYPE option<string>;

DEFINE INDEX OVERWRITE document_delta_document ON TABLE document_delta FIELDS document, n;

-- How many characters two texts share at the start and, up to $max, at the end:
-- a binary search, 21 steps are enough for 2M characters. What it finds is always
-- shared, so a longer text only gets a larger hunk.
DEFINE FUNCTION OVERWRITE fn::common_prefix($a: string, $b: string) -> int {
  array::fold(array::range(0, 21), { lo: 0, hi: math::min([string::len($a), string::len($b)]) }, |$s, $step| {
    LET $mid = ($s.lo + $s.hi + 1) / 2;
    IF $s.lo >= $s.hi { $s }
    ELSE IF string::slice($a, $s.lo, $mid) = string::slice($b, $s.lo, $mid) { { lo: $mid, hi: $s.hi } }
    ELSE { { lo: $s.lo, hi: $mid - 1 } }
  }).lo
};
DEFINE FUNCTION OVERWRITE fn::common_suffix($a: string, $b: string, $max: int) -> int {
  LET $la = string::len($a);
  LET $lb = string::len($b);
  array::fold(array::range(0, 21), { lo: 0, hi: $max }, |$s, $step| {
    LET $mid = ($s.lo + $s.hi + 1) / 2;
    IF $s.lo >= $s.hi { $s }
    ELSE IF string::slice($a, $la - $mid, $la - $s.lo) = string::slice($b, $lb - $mid, $lb - $s.lo) { { lo: $mid, hi: $s.hi } }
    ELSE { { lo: $s.lo, hi: $mid - 1 } }
  }).lo
};

-- Saves a moment after each keystroke would make far too many versions, so the
-- student's saves extend the latest version while they keep writing. A new one
-- starts after 5 idle minutes or 30 minutes in all, when someone else wrote the
-- latest one or it went back to an older one, and for a change of 500 characters
-- or more at once (a paste, a selection typed over), so that what was there
-- before stays one click away. An AI client's writes are versions of their own,
-- so that each can be undone alone. A version that ends up as the text it started
-- from goes away again.
--
-- An error in here fails the write, so nothing may depend on what a row holds
-- (rows from before `format` have none). And no RETURN in here: where it ends in
-- a nested block differs between SurrealDB 3 (`spky dev`) and 4 (the deployment).
DEFINE EVENT OVERWRITE document_versions ON TABLE document
  WHEN $event = 'CREATE' OR ($event = 'UPDATE' AND (($before.content ?? '') != $after.content OR ($before.format ?? 'markdown') != ($after.format ?? 'markdown')))
  THEN {
    LET $doc = $after.id;
    LET $key = record::id($doc);
    LET $text = $after.content;
    LET $format = $after.format ?? 'markdown';
    LET $now = time::now();
    LET $version_id = |$n: int| type::record('document_version', $key + 'n' + <string> $n);
    LET $delta_id = |$n: int| type::record('document_delta', $key + 'n' + <string> $n);
    LET $make = |$n: int, $who: string, $as: string, $added: int, $removed: int, $hunks: array, $sha: string, $at: datetime| {
      CREATE $version_id($n) CONTENT {
        owner: $after.owner, document: $doc, n: $n, author: $who, format: $as, added: $added, removed: $removed, created_at: $at, updated_at: $at
      };
      CREATE $delta_id($n) CONTENT { owner: $after.owner, document: $doc, n: $n, hunks: $hunks, sha: $sha };
    };
    LET $author = IF $access = 'account' { 'student' } ELSE IF $access IN ['agent', 'oauth_agent'] { 'assistant' } ELSE { 'system' };
    IF $event = 'CREATE' {
      $make(1, $author, $format, string::len($text), 0, [], crypto::sha256($text), $now);
    } ELSE {
      LET $prior = $before.content ?? '';
      -- A document from before versions: what it held so far is version 1.
      LET $found = (SELECT n, author, format, created_at, updated_at, restored_from FROM document_version WHERE document = $doc ORDER BY n DESC LIMIT 1)[0];
      IF $found = NONE {
        $make(1, 'system', $before.format ?? 'markdown', string::len($prior), 0, [], crypto::sha256($prior), $before.updated_at ?? $now);
      };
      LET $last = $found ?? { n: 1, author: 'system' };

      -- What this write changed: the one stretch between what stayed at both ends.
      LET $la = string::len($prior);
      LET $lb = string::len($text);
      LET $p = fn::common_prefix($prior, $text);
      LET $s = fn::common_suffix($prior, $text, math::min([$la, $lb]) - $p);
      LET $d = $la - $p - $s;
      LET $i = $lb - $p - $s;
      LET $change = { at: $p, len: $i, old: string::slice($prior, $p, $p + $d) };

      LET $extend = $author = 'student' AND $last.author = 'student' AND $last.restored_from = NONE AND $last.format = $format
        AND $last.updated_at > $now - 5m AND $last.created_at > $now - 30m AND $d < 500 AND $i < 500;
      IF !$extend {
        IF $last.n % 50 = 0 {
          UPDATE $delta_id($last.n) SET snapshot = $prior;
        };
        $make($last.n + 1, $author, $format, $i, $d, IF $d = 0 AND $i = 0 { [] } ELSE { [$change] }, crypto::sha256($text), $now);
      } ELSE {
        -- Fold the change into the hunk it touches, so that typing along stays
        -- one hunk, or add it as the next one.
        LET $hunks = (SELECT VALUE hunks FROM ONLY $delta_id($last.n));
        LET $h = array::last($hunks);
        LET $next = IF $h != NONE AND $p <= $h.at + $h.len AND $p + $d >= $h.at {
          LET $start = math::min([$h.at, $p]);
          LET $end = math::max([$h.at + $h.len, $p + $d]);
          LET $old = string::slice($prior, $start, $h.at) + $h.old + string::slice($prior, $h.at + $h.len, $end);
          LET $len = $end - $start - $d + $i;
          array::concat(array::slice($hunks, 0, -1), IF $old = string::slice($text, $start, $start + $len) { [] } ELSE { [{ at: $start, len: $len, old: $old }] })
        } ELSE {
          array::push($hunks, $change)
        };
        IF $last.n > 1 AND array::len($next) = 0 {
          DELETE $version_id($last.n);
          DELETE $delta_id($last.n);
        } ELSE {
          -- It may have had its full copy before a newer version went away again.
          UPDATE $delta_id($last.n) SET hunks = $next, sha = crypto::sha256($text), snapshot = NONE;
          UPDATE $version_id($last.n) SET
            added = IF $last.n = 1 { $lb } ELSE { math::sum($next.len) },
            removed = IF $last.n = 1 { 0 } ELSE { math::sum($next.map(|$x| string::len($x.old))) },
            updated_at = $now;
        };
      };
    };
  };

-- ##################################################################
-- SOURCES
--
-- A source (`fetcher`) brings a course's material in by itself: the files
-- and pages of a bCourses (Canvas) course, or the PDFs linked from a course
-- website. The API checks it on a schedule and imports what is new or
-- changed (apps/api/src/fetch). Its Canvas token and what it imported are
-- server-only (apps/api/src/server-schema.surql).
-- ##################################################################

DEFINE TABLE OVERWRITE fetcher SCHEMAFULL
  PERMISSIONS
    FOR select, update, delete WHERE owner = $auth.id
    FOR create WHERE owner = $auth.id AND course.owner = $auth.id;

DEFINE FIELD OVERWRITE owner ON TABLE fetcher TYPE record<user> READONLY;
DEFINE FIELD OVERWRITE course ON TABLE fetcher TYPE record<course> READONLY; -- @parent
-- canvas: a bCourses (or other Canvas) course. web: one public web page.
DEFINE FIELD OVERWRITE kind ON TABLE fetcher TYPE string
  ASSERT $value IN ['canvas', 'web']
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE name ON TABLE fetcher TYPE string ASSERT string::len($value) > 0;
-- web: the page. canvas: the Canvas origin, e.g. https://bcourses.berkeley.edu.
DEFINE FIELD OVERWRITE url ON TABLE fetcher TYPE string
  PERMISSIONS FOR update WHERE false;
-- The Canvas course id. NONE for web.
DEFINE FIELD OVERWRITE canvas_course ON TABLE fetcher TYPE option<string>
  PERMISSIONS FOR update WHERE false;
-- Comma-separated words: only items whose name or URL contains one are
-- imported. NONE: every PDF.
DEFINE FIELD OVERWRITE include ON TABLE fetcher TYPE option<string>;
-- Also import pages (Canvas pages, the web page's own text) as Markdown.
DEFINE FIELD OVERWRITE import_pages ON TABLE fetcher TYPE bool DEFAULT false;
-- Also import LaTeX sources (.tex) as `tex` material. Optional: sources from
-- before it existed have none (false), and a plain bool would make every
-- UPDATE of such a row fail to coerce NONE.
DEFINE FIELD OVERWRITE import_tex ON TABLE fetcher TYPE option<bool> DEFAULT false;
DEFINE FIELD OVERWRITE enabled ON TABLE fetcher TYPE bool DEFAULT true;
-- Hours between scheduled checks.
DEFINE FIELD OVERWRITE every_hours ON TABLE fetcher TYPE int DEFAULT 6
  ASSERT $value >= 1 AND $value <= 168;
-- A custom hockeymask mapping (JSON) instead of the template built from the
-- fields above. Set through the API, which compiles it first.
DEFINE FIELD OVERWRITE mapping ON TABLE fetcher TYPE option<string>
  PERMISSIONS FOR update WHERE false;
-- Fields from here on are written by the API only.
-- idle: never ran. running. ok. partial: some items failed. failed.
DEFINE FIELD OVERWRITE status ON TABLE fetcher TYPE string DEFAULT 'idle'
  ASSERT $value IN ['idle', 'running', 'ok', 'partial', 'failed']
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE last_error ON TABLE fetcher TYPE option<string>
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE has_token ON TABLE fetcher TYPE bool DEFAULT false
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE imported_count ON TABLE fetcher TYPE int DEFAULT 0
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE run_started_at ON TABLE fetcher TYPE option<datetime>
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE last_run_at ON TABLE fetcher TYPE option<datetime>
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE next_run_at ON TABLE fetcher TYPE option<datetime>
  PERMISSIONS FOR update WHERE false;
DEFINE FIELD OVERWRITE created_at ON TABLE fetcher TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE updated_at ON TABLE fetcher TYPE datetime VALUE time::now();

DEFINE INDEX OVERWRITE fetcher_course ON TABLE fetcher FIELDS course;

-- Material a source imported stays: it is the student's now.
DEFINE EVENT OVERWRITE fetcher_deleted ON TABLE fetcher WHEN $event = "DELETE" THEN {
  DELETE fetch_run WHERE fetcher = $before.id;
  UPDATE material SET fetcher = NONE WHERE fetcher = $before.id;
};

-- One check of a source, for its history. Written by the API.
DEFINE TABLE OVERWRITE fetch_run SCHEMAFULL
  PERMISSIONS
    FOR select, delete WHERE owner = $auth.id
    FOR create, update WHERE false;

DEFINE FIELD OVERWRITE owner ON TABLE fetch_run TYPE record<user>;
DEFINE FIELD OVERWRITE fetcher ON TABLE fetch_run TYPE record<fetcher>; -- @parent
DEFINE FIELD OVERWRITE status ON TABLE fetch_run TYPE string DEFAULT 'running'
  ASSERT $value IN ['running', 'ok', 'partial', 'failed'];
-- Items the mapping kept, and what became of them.
DEFINE FIELD OVERWRITE found ON TABLE fetch_run TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE created ON TABLE fetch_run TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE updated ON TABLE fetch_run TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE failed ON TABLE fetch_run TYPE int DEFAULT 0;
-- Left for the next run (a run imports a bounded number of items).
DEFINE FIELD OVERWRITE deferred ON TABLE fetch_run TYPE int DEFAULT 0;
-- What went wrong, one line per problem.
DEFINE FIELD OVERWRITE detail ON TABLE fetch_run TYPE option<string>;
DEFINE FIELD OVERWRITE started_at ON TABLE fetch_run TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE finished_at ON TABLE fetch_run TYPE option<datetime>;

DEFINE INDEX OVERWRITE fetch_run_fetcher ON TABLE fetch_run FIELDS fetcher, started_at;

-- ##################################################################
-- API KEYS (MCP access for AI clients)
-- ##################################################################

-- The browser generates the key, shows it once, and stores only its SHA-256.
DEFINE TABLE OVERWRITE api_key SCHEMAFULL
  PERMISSIONS
    FOR select, update, delete WHERE owner = $auth.id
    FOR create WHERE owner = $auth.id;

DEFINE FIELD OVERWRITE owner ON TABLE api_key TYPE record<user> READONLY;
DEFINE FIELD OVERWRITE name ON TABLE api_key TYPE string ASSERT string::len($value) > 0;
-- First characters of the key, so the user can tell keys apart.
DEFINE FIELD OVERWRITE prefix ON TABLE api_key TYPE string READONLY;
DEFINE FIELD OVERWRITE key_hash ON TABLE api_key TYPE string READONLY
  PERMISSIONS FOR select WHERE false;
DEFINE FIELD OVERWRITE created_at ON TABLE api_key TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE revoked_at ON TABLE api_key TYPE option<datetime>;

-- Its lookup index lives in apps/api/src/server-schema.surql: sp00ky refuses an
-- index on a field clients cannot read.
