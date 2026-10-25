-- rTorrent: the second label-torrent client. The scope shape/option triggers and the label unique
-- index were replaced for the whole family in 0068, so only the registry row is added.
INSERT INTO provider_implementations(implementation,kind,protocol) VALUES ('rtorrent','download_client','torrent');
