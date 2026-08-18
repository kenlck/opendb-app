# A Connection opens exactly one Database

A Connection is a recipe for one Database: one Postgres database name, one MySQL database, or one SQLite file. Switching `felicity_os` to `felicity_os_test` is another Connection. A server-level Connection would make Apply and “which Database is this grid on?” ambiguous.
