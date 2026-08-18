# Results can be staged only with one Table and Row Identity

A Query Result is not always a grid you can edit. Staged Changes from a Result are allowed only when the Client can prove the Result is one Table and every row carries Row Identity. Joins, aggregates, and missing identity are read-only. Best-effort UPDATE on arbitrary SQL is rejected.
