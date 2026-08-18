# Table and Result grids are always bounded

Opening a Table or running a Query never loads an unbounded row set. The grid is a page; the next rows are an explicit next page. There is no “load all” that can freeze the Client. Seeking is filters or a Query, not `SELECT *`.
