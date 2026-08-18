# Apply is one transaction for the whole batch

Staged Changes exist so Apply is an intentional unit. When someone Applies, the Client sends the whole batch in one transaction: any row fails, nothing is written. Partial success per row or per table would feel like the live cell writes we already rejected.
