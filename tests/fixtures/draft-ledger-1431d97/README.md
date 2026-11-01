Rows 50 through 76 of the unpublished PR #31 draft migration ledger (draft head `1431d97`), one file
per ledger row, named `<row>_<name>.sql`, byte-identical to the files that commit includes. Rows 1
through 49 are the shared prefix and are read from `MIGRATIONS`. Used only by tests, which rebuild a
draft database from them and prove the schema fingerprint in `src/db/draft_conversion.rs`.
