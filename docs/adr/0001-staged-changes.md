# Staged Changes, not live cell writes

The Client lets people mutate rows from a grid, but live writes on cell blur are how production data gets destroyed. We stage mutations in the Client and send them only on an explicit Apply.
