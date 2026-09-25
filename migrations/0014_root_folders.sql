-- Configured roots are declarations; accessibility and disk space are observations, never stored.
CREATE TABLE root_folders (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK(id BETWEEN 1 AND 9007199254740991),
    media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
    path TEXT NOT NULL CHECK(typeof(path)='text' AND length(CAST(path AS BLOB)) BETWEEN 2 AND 4096 AND substr(path,1,1)='/' AND substr(path,-1)!='/' AND instr(path,'//')=0 AND instr(path,char(0))=0 AND path NOT GLOB ('*['||char(1)||'-'||char(31)||char(127)||']*') AND instr(path,char(92))=0 AND instr(path||'/','/../')=0 AND instr(path||'/','/./')=0),
    UNIQUE(media_type,path)
);
