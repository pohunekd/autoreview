---
kind: security
---
Model text in a published review can no longer publish a mention, raw HTML or an image through Markdown that GitHub reads as text where autoreview kept it as code: fenced blocks are written again with autoreview's own fence, an inline code span is kept only when its content is harmless as text, one-line fields such as titles keep no code, HTML entities are shown as written, a mention is defused whatever comes before it, and the location of a finding is a paragraph of its own.
