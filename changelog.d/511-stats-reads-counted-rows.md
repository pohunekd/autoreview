---
kind: fixed
---
`autoreview stats --since` reads only the rows it counts, so a damaged reviewer slot, verification or combined artifact no longer stops it with `store_state_invalid`; `autoreview report` still reads them.
