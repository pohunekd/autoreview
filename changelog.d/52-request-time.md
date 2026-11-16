---
kind: added
---
`autoreview report` prints the time from the review request to its first publication, once per request (median and p90, `request_to_publication_median_seconds` and `request_to_publication_p90_seconds` in the JSON, whose `schema_version` is now 2) beside the time from the attempt's creation. The request time is stored with each review that a review request starts and kept through pruning; label triggers and reviews stored by earlier releases have none and are left out.
