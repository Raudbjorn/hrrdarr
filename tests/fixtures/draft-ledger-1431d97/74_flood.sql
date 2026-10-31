-- Flood: the fifth label-torrent client (a domain is a Flood tag). Registry row only; the family
-- triggers and the label unique index came with 0070.
INSERT INTO provider_implementations(implementation,kind,protocol) VALUES ('flood','download_client','torrent');
