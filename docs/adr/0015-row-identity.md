# Row Identity is a primary key or unique index, never a physical pointer

Update and delete require Row Identity: a primary key or unique index whose values are on the row. The Client will not target rows via Postgres `ctid` or SQLite `rowid`. Tables (and Results) without that identity are read-only for update/delete; insert is still allowed. Physical pointers change under vacuum and are not something we share in a Connection-shaped workflow.
