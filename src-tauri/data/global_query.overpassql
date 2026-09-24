[out:json][timeout:600];
(
  node["man_made"="surveillance"]["surveillance:type"="ALPR"];
  way["man_made"="surveillance"]["surveillance:type"="ALPR"];
  node["man_made"="surveillance"]["surveillance:zone"="traffic"]["camera:type"="fixed"]["brand"~"[Ff]lock"];
);
out center tags;
