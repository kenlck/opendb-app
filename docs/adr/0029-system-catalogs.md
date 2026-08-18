# System Catalogs hidden by default, with a toggle

The Table list hides Engine internals (`pg_catalog`, `information_schema`, `sqlite_*`, and the like) so the daily loop is application Tables. A Client-wide toggle shows them. Queries can read them either way. Showing everything with no hide would make the front door a system catalog.
