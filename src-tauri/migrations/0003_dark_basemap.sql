-- Migration 0003: the default basemap changed from OpenFreeMap "liberty" (light) to
-- OpenFreeMap "dark". Installs still on the old default follow; custom URLs are untouched.
UPDATE settings
SET value = replace(
  value,
  '"style_url":"https://tiles.openfreemap.org/styles/liberty"',
  '"style_url":"https://tiles.openfreemap.org/styles/dark"'
)
WHERE key = 'app_settings';
