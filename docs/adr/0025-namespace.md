# Postgres catalogs group by Namespace; Schema Change stays DDL

Postgres `public` / `auth` groupings are Namespaces, not Schema Changes. The Table list is grouped by Namespace on Postgres and flat on SQLite and MySQL. We keep the term Schema Change for table-shaped DDL forms and do not rename it to match Postgres.
