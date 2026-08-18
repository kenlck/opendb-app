# Apply fails if the row changed under you

Staged Updates and deletes are optimistic. Apply targets Row Identity and the column values the grid last saw. If anything in that predicate no longer matches, the Staged Change fails and the whole transaction rolls back. Last-write-wins by primary key is rejected; a conflict UI is not v1. Reload, restage, Apply again.
