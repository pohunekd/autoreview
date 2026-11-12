---
kind: security
---
Model text in a published review can no longer hide a mention, raw HTML or an image in Markdown that GitHub reads as text where autoreview read code, such as an escaped backtick, a code span or fence inside a list, quote or table cell, a lone carriage return, or an HTML entity: backticks and tilde runs outside code are escaped, entities are shown as written, a mention is defused whatever comes before it, and a finding path is shown in one code span.
