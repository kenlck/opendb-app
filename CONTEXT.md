# OpenDB

A desktop Client for inspecting Databases and applying Staged Changes, so a known handful of people can stop paying for a closed GUI client.

The usual loop is: provide a Connection String, keep it on the Connection List, pick a Connection, open a Session, look at Table data.

## Language

**Client**:
The desktop program used to inspect a Database and apply Staged Changes to it.
_Avoid_: viewer, GUI, app, TablePlus clone

**Engine**:
A kind of Database the Client can speak. v1 Engines are Postgres, SQLite (a local file), and MySQL/MariaDB. Turso and other remote libSQL are not Engines in v1.
_Avoid_: driver, dialect, backend, data source

**Database**:
A live server or file the Client can open. One Connection points at exactly one Database.
_Avoid_: data source, backend, instance

**Namespace**:
A grouping of Tables inside a Database. Postgres catalogs are grouped by Namespace (`public`, `auth`, …). SQLite and MySQL Table lists are flat. System Catalogs are Namespaces/Tables the Client hides by default. This is not a Schema Change.
_Avoid_: schema, Postgres schema, owner

**System Catalog**:
Engine-internal Namespaces and Tables (`pg_catalog`, `information_schema`, `sqlite_*`, and the like). Hidden from the Table list by default; a Client-wide toggle shows them. Queries can still select them while they are hidden.
_Avoid_: system schema, internal tables, pg_

**Connection String**:
A URI (or SQLite path) that encodes how to open exactly one Database, including the secret in plaintext.
_Avoid_: DSN, URL, credentials

**Connection**:
One Connection String kept on the Connection List, plus a Name used in the list UI. Identity is the Connection String, not the Name. Names need not be unique.
_Avoid_: config, bookmark, source, data source

**Name**:
A label on a Connection, defaulting to the Database name or SQLite file stem. The list shows Names, not secrets.
_Avoid_: title, alias, nickname

**Connection List**:
The Client’s collection of Connections, stored in app data as one shareable bundle of Connection Strings. Sharing is explicit Export/Import. Import merges by exact Connection String (duplicates skip; same Database with a different secret is a second Connection).
_Avoid_: bundle, library, workspace, bookmarks, config

**Session**:
A live link to a Database, created by opening a Connection. Opening a Connection always creates a new Session, even if one already exists for that Connection. Staged Changes belong to that Session only. Several Sessions can be open at once.
_Avoid_: connection (the live socket), workspace

**Tab**:
An open view on a Session: a Table grid, a Query/Result, or structure. Unlimited per Session. Closing a Tab does not discard Staged Changes.
_Avoid_: panel, page, document

**Filter**:
A simple column predicate on a Table grid (equals, contains, or null). Unlimited. Not a Query.
_Avoid_: advanced filter, query builder, search

**Table**:
A named relation in a Database that can be opened as a grid. In Postgres it belongs to a Namespace.
_Avoid_: relation, collection, model, entity

**Query**:
A read-only SQL statement executed immediately on a Session.
_Avoid_: script, command, statement

**Result**:
The rows produced by a Query. It can produce Staged Changes only when it is one Table and every row carries Row Identity.
_Avoid_: dataset, recordset, output

**Row Identity**:
A primary key or unique index whose values are present on the grid row, including composite keys. Physical pointers (`ctid`, `rowid`) are not Row Identity. Without it, the grid cannot stage update or delete; insert is still allowed.
_Avoid_: row id, ctid, rowid, identity column (a serial column is not enough unless it is unique)

**Staged Change**:
An insert, update, or delete of a row, prepared from a Table grid or a qualifying Result, not yet Applied. All Staged Changes on a Session Apply together, from any Tab. Each can be Unstaged on its own, or the whole bag discarded. Ad-hoc mutating SQL and Schema Changes are not Staged Changes.
_Avoid_: edit, pending write, draft, dirty row

**Unstage**:
Remove one Staged Change from the Session bag without touching the Database. Not Apply, not discard-all.
_Avoid_: undo, revert, cancel edit

**Apply**:
The act of sending all current Staged Changes on a Session to that Database in a single transaction so they take effect together, or not at all. Apply also requires the targeted rows to still match what the grid read; a mismatch fails the whole transaction.
_Avoid_: commit, save, sync

**Schema Change**:
DDL generated from a form, run on a Session after confirm. Forms exist only for create/drop table, add/drop/rename column, and add/drop index. Create table uses the Namespace you are browsing; crossing Namespaces is SQL. Views, functions, triggers, sequences, users/roles, and extensions are not Schema Changes; they are SQL if at all. A Schema Change is never staged and must not run while Staged Changes exist. Not a Namespace.
_Avoid_: migration, staged schema, ALTER, schema (the Postgres object)
