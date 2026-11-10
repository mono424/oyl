-- Moves PDF parts and page images out of `bytes` fields into the `materials`
-- bucket (schema/buckets/materials.surql). Idempotent: run it repeatedly until
-- the final RETURN shows nothing left. Each run moves at most 5 rows per table
-- (a row can hold megabytes; one huge request wedged the shared database).
-- Run as a system user in the project's namespace and database.

-- The field must be optional before a row can lose it. The new server schema
-- (apps/api/src/server-schema.surql) removes it once nothing holds bytes.
IF (INFO FOR TABLE asset).fields.data { DEFINE FIELD OVERWRITE data ON TABLE asset TYPE option<bytes> };
IF (INFO FOR TABLE source_part).fields.data { DEFINE FIELD OVERWRITE data ON TABLE source_part TYPE option<bytes> };

LET $assets = SELECT id, data FROM asset WHERE data != NONE LIMIT 5;
-- 3.1.5 refuses a FOR over nothing.
IF array::len($assets) > 0 { FOR $a IN $assets {
  type::file('materials', '/assets/' + <string>record::id($a.id)).put($a.data);
  UPDATE $a.id UNSET data;
} };
LET $parts = SELECT id, data FROM source_part WHERE data != NONE LIMIT 5;
IF array::len($parts) > 0 { FOR $p IN $parts {
  type::file('materials', '/sources/' + <string>record::id($p.id)).put($p.data);
  UPDATE $p.id UNSET data;
} };

RETURN {
  assets_left: (SELECT VALUE count() FROM asset WHERE data != NONE GROUP ALL)[0] ?? 0,
  parts_left: (SELECT VALUE count() FROM source_part WHERE data != NONE GROUP ALL)[0] ?? 0,
};
