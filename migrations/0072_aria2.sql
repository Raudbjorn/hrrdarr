-- Aria2: the fourth label-torrent client (a domain is a subfolder of aria2's download directory).
-- The scope shape/option triggers and the label unique index were replaced for the whole family in
-- 0068, so only the registry row is added.
INSERT INTO provider_implementations(implementation,kind,protocol) VALUES ('aria2','download_client','torrent');
