---
kind: changed
---
A review whose stored input names a base branch outside the names that discovery accepts (1 to 255 ASCII letters, digits, `.`, `_`, `/` and `-`, without `..`) now fails with `review_input_invalid` before it builds a reviewer or verifier request, so the trusted `Base branch:` line can never hold a line break, U+0085, U+2028 or U+2029. Discovery refused such pull requests already, so reviews of GitHub pull requests are unchanged; an empty base branch of an older stored review is still accepted.
