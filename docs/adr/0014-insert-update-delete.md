# Staged Changes are insert, update, and delete

Row mutations in the Client are not updates-only. Insert, update, and delete all become Staged Changes and Apply together in one transaction. Insert or delete as a side path would split the model we already chose for Apply.
