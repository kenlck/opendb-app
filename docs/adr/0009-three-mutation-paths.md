# Three mutation paths, never mixed

The Client mutates a Database in three ways, and they do not combine. Grid row edits become Staged Changes and Apply in one transaction. Mutating SQL runs immediately after confirm, as its own transaction, and is refused while Staged Changes exist. Schema Changes run immediately after confirm from generated DDL, and are refused while Staged Changes exist. Reads (Queries) always run immediately.
