-- Migration: document_versions
-- Created: 2026-11-03T21:03:03.487680+00:00

-- Modified
DEFINE EVENT OVERWRITE document_deleted ON document WHEN $event = 'DELETE' THEN { DELETE paper WHERE document = $before.id; DELETE document_version WHERE document = $before.id; DELETE document_delta WHERE document = $before.id; };

-- Added
DEFINE FUNCTION OVERWRITE fn::common_prefix($a: string, $b: string) -> int { array::fold(array::range(0, 21), { lo: 0, hi: math::min([string::len($a), string::len($b)]) }, |$s: any, $step: any| { LET $mid = ($s.lo + $s.hi + 1) / 2; IF $s.lo >= $s.hi { $s } ELSE IF string::slice($a, $s.lo, $mid) = string::slice($b, $s.lo, $mid) { { lo: $mid, hi: $s.hi } } ELSE { { lo: $s.lo, hi: $mid - 1 } }; }).lo } PERMISSIONS FULL;
DEFINE FUNCTION OVERWRITE fn::common_suffix($a: string, $b: string, $max: int) -> int { LET $la = string::len($a); LET $lb = string::len($b); array::fold(array::range(0, 21), { lo: 0, hi: $max }, |$s: any, $step: any| { LET $mid = ($s.lo + $s.hi + 1) / 2; IF $s.lo >= $s.hi { $s } ELSE IF string::slice($a, $la - $mid, $la - $s.lo) = string::slice($b, $lb - $mid, $lb - $s.lo) { { lo: $mid, hi: $s.hi } } ELSE { { lo: $s.lo, hi: $mid - 1 } }; }).lo; } PERMISSIONS FULL;
DEFINE TABLE OVERWRITE document_delta TYPE NORMAL SCHEMAFULL COMMENT 'sp00ky:nosync' PERMISSIONS FOR select WHERE owner = $auth.id, FOR create, update, delete WHERE false;
DEFINE TABLE OVERWRITE document_version TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select WHERE owner = $auth.id, FOR create, update, delete WHERE false;
DEFINE FIELD OVERWRITE document ON document_delta TYPE record<document> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE hunks ON document_delta TYPE array<object> PERMISSIONS FULL;
DEFINE FIELD OVERWRITE hunks.*.at ON document_delta TYPE int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE hunks.*.len ON document_delta TYPE int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE hunks.*.old ON document_delta TYPE string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE n ON document_delta TYPE int READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE owner ON document_delta TYPE record<user> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE sha ON document_delta TYPE string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE snapshot ON document_delta TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE added ON document_version TYPE int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE author ON document_version TYPE string ASSERT $value INSIDE ['student', 'assistant', 'system'] PERMISSIONS FULL;
DEFINE FIELD OVERWRITE created_at ON document_version TYPE datetime PERMISSIONS FULL;
DEFINE FIELD OVERWRITE document ON document_version TYPE record<document> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE format ON document_version TYPE string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE n ON document_version TYPE int READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE owner ON document_version TYPE record<user> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE removed ON document_version TYPE int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE restored_from ON document_version TYPE none | int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE updated_at ON document_version TYPE datetime PERMISSIONS FULL;
DEFINE INDEX OVERWRITE document_delta_document ON document_delta FIELDS document, n;
DEFINE INDEX OVERWRITE document_version_document ON document_version FIELDS document, n;
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
