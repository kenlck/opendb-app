# Schema forms are table-shaped only

v1 Schema Changes are forms for create/drop table, add/drop/rename column, and add/drop index, generating DDL you read before it runs. The rest of the catalog is browse-only (whatever the Engine has) or SQL. Users/roles and other cluster objects are not in the Client. A visual designer is out.
