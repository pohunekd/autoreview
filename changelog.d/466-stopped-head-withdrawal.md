---
kind: fixed
---
A stopped review whose pull request one discovery pass did not list, for example after a failed author-permission read, is now withdrawn for that pass instead of dropped, so its return handles the stopping run again without a second `review_stopped` notice and a head that the usage limit or the head fetch mismatch bound stopped stays stopped. An `autoreview retry` of a stopped head now still starts when a withdrawal or a retarget away and back comes before its first run; before, either one could stop the head again.
