-- PDF uploads and the page images extracted from them. Only the API Worker
-- reads and writes it (with system credentials); clients get images from the
-- API's /material-assets/:id route, so no record user may touch it.
--   /assets/<asset id>        one extracted page image (PNG)
--   /sources/<source_part id> one part (at most 1 MiB) of an uploaded PDF
--   /sites/<site_file id>_<n> one part (at most 1 MiB) of a file of an uploaded site
-- On Sp00ky Cloud's shared free database the control plane pins the backend to
-- this project's folder.
DEFINE BUCKET IF NOT EXISTS materials BACKEND "file:/buckets/materials"
  PERMISSIONS NONE;
